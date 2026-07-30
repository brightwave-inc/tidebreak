//! The sandbox-resident `exec` tool: a model-authored command run *inside* the
//! container.
//!
//! In-container execution is the containment (see
//! [sandbox-providers.md](../../docs/sandbox-providers.md)). The container's
//! OS/VM boundary is what makes model-authored execution admissible at all:
//! model output can only move the sandbox, and the host consumes the run's
//! bounded event stream and result as untrusted input. So this tool does **not**
//! try to re-sandbox inside the container — that boundary already holds. What it
//! does is bound the invocation exactly the way the shipped foreground exec
//! adapter bounds its confined child (`openwave-code-execution`'s local
//! provider): a cleared environment with a fixed `HOME`/`TMPDIR`/`PATH`, stdin
//! wired to `/dev/null`, its own process group, a wall-time timeout that kills
//! that whole group, a captured-output cap, and rlimit ceilings — so a
//! runaway or wedged command is contained by resource bounds, not left to run.
//!
//! # NOT YET FOR CREDENTIAL-BEARING WORK
//!
//! A command run here can make network calls. The design routes egress through
//! the sandbox supervisor (credential separation + an egress proxy), which is a
//! **stub** in this crate. Nothing here reaches host authority — the containment
//! above holds — but egress *from the container* is not yet externally enforced.
//! This tool surface must not be routed to production credential-bearing work
//! until externally-enforced egress (the run's egress policy applied to the
//! container's network) and the transport-auth gate land. See the crate docs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The name the model uses to invoke the in-container exec tool.
pub const EXEC_TOOL: &str = "exec";

/// Default wall-time bound for one command; a command that outlives it has its
/// process group killed and the result is reported as timed out.
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum UTF-8 bytes in one model-authored command string.
pub const MAX_COMMAND_BYTES: usize = 64 * 1_024;

/// Maximum bytes captured from stdout and stderr together; beyond it the output
/// is truncated rather than buffered without bound.
pub const MAX_CAPTURE_BYTES: usize = 40_000;

/// The shell the command runs under. The container is the isolation boundary, so
/// a shell is admissible; `-c <command>` runs the model's text as one command.
const SHELL: &str = "/bin/sh";

/// A fixed, minimal search path handed to the command, so lookup does not depend
/// on an inherited environment (which is cleared).
const FIXED_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Stable location of the document helper scripts baked into the sandbox image.
const DOCUMENT_SCRIPTS_DIR: &str = "/opt/openwave/exec-scripts";

/// Arguments for [`ExecTool`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    /// The shell command to run inside the container's workspace directory.
    command: String,
}

/// Run a model-authored shell command inside the container, bounded.
///
/// The tool is rooted at the agent's in-container workspace directory: the
/// command runs with that directory as its working directory, `HOME`, and
/// `TMPDIR`. It holds a per-invocation wall-time bound.
pub struct ExecTool {
    workspace: PathBuf,
    timeout: Duration,
}

impl ExecTool {
    /// Build an exec tool rooted at `workspace`, bounding each command by
    /// `timeout`.
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            workspace: workspace.into(),
            timeout,
        }
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::for_args::<ExecArgs>(
            EXEC_TOOL,
            "Run a shell command inside the sandbox workspace and return its \
             output. The command runs with the workspace as its working \
             directory; it is bounded by a wall-time limit and a captured-output \
             cap. Document helpers live at $OPENWAVE_EXEC_SCRIPTS and write visual \
             results to preview/; overview/grid/thumbnail names are reviewed first. \
             Examples: python3 $OPENWAVE_EXEC_SCRIPTS/render_pdf.py report.pdf \
             --pages 1-2; python3 $OPENWAVE_EXEC_SCRIPTS/analyze_xlsx.py budget.xlsx.",
        )
    }

    fn approval_class(&self) -> ApprovalClass {
        // A command can escape the workspace and reach the network from inside
        // the container, so it is the most privileged class the sandbox surface
        // declares.
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        // Arguments are untrusted: a malformed call is a tool failure the model
        // sees and can correct, not a process error.
        let args: ExecArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(ToolOutput::error(format!("invalid arguments: {error}"))),
        };
        if args.command.is_empty() || args.command.len() > MAX_COMMAND_BYTES {
            return Ok(ToolOutput::error(format!(
                "command must be between 1 and {MAX_COMMAND_BYTES} bytes"
            )));
        }
        if args.command.as_bytes().contains(&0) {
            return Ok(ToolOutput::error(
                "command must not contain a NUL byte".to_owned(),
            ));
        }

        for directory in ["output", "preview", "documents"] {
            if let Err(error) = tokio::fs::create_dir_all(self.workspace.join(directory)).await {
                return Ok(ToolOutput::error(format!(
                    "workspace directory '{directory}/' is unavailable: {error}"
                )));
            }
        }
        let outcome = run_bounded(&args.command, &self.workspace, self.timeout).await;
        Ok(ToolOutput::text(outcome.render()))
    }
}

/// The normalized, bounded result of one in-container command.
#[derive(Debug, Default)]
pub(crate) struct ExecOutcome {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
    pub(crate) truncated: bool,
}

impl ExecOutcome {
    fn render(&self) -> String {
        let mut out = String::new();
        match (self.timed_out, self.exit_code) {
            (true, _) => out.push_str("command timed out and was killed\n"),
            (false, Some(code)) => out.push_str(&format!("exit code: {code}\n")),
            (false, None) => out.push_str("command was terminated by a signal\n"),
        }
        if self.truncated {
            out.push_str("(output truncated at the capture cap)\n");
        }
        out.push_str("stdout:\n");
        out.push_str(&self.stdout);
        if !self.stdout.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("stderr:\n");
        out.push_str(&self.stderr);
        out
    }
}

#[cfg(unix)]
mod capture {
    use super::MAX_CAPTURE_BYTES;

    #[derive(Clone, Copy)]
    pub(super) enum StreamKind {
        Stdout,
        Stderr,
    }

    /// A bounded stdout/stderr accumulator: once the shared cap is reached,
    /// further bytes are dropped and the capture is marked truncated.
    #[derive(Default)]
    pub(super) struct Capture {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        total: usize,
        pub(super) truncated: bool,
    }

    impl Capture {
        pub(super) fn append(&mut self, bytes: &[u8], kind: StreamKind) {
            let available = MAX_CAPTURE_BYTES.saturating_sub(self.total);
            let kept = available.min(bytes.len());
            let target = match kind {
                StreamKind::Stdout => &mut self.stdout,
                StreamKind::Stderr => &mut self.stderr,
            };
            target.extend_from_slice(&bytes[..kept]);
            self.total += kept;
            self.truncated |= kept < bytes.len();
        }

        pub(super) fn stdout(&self) -> String {
            String::from_utf8_lossy(&self.stdout).into_owned()
        }

        pub(super) fn stderr(&self) -> String {
            String::from_utf8_lossy(&self.stderr).into_owned()
        }
    }
}

#[cfg(unix)]
async fn run_bounded(command: &str, workspace: &Path, timeout: Duration) -> ExecOutcome {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};

    use capture::{Capture, StreamKind};
    use tokio::io::{AsyncRead, AsyncReadExt};
    use tokio::process::Command;

    /// Grace between the SIGTERM and SIGKILL sent to a timed-out group.
    const TERMINATE_GRACE: Duration = Duration::from_millis(250);
    /// How long to wait for the output readers to drain after the child exits.
    const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
    /// Ceiling on files the command may open, as defense in depth.
    const MAX_OPEN_FILES: u64 = 256;

    let cpu_seconds = timeout.as_secs().saturating_add(1).max(1);
    let mut cmd = Command::new(SHELL);
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace)
        .env("TMPDIR", workspace)
        .env("PATH", FIXED_PATH)
        .env("OPENWAVE_EXEC_SCRIPTS", DOCUMENT_SCRIPTS_DIR)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: `pre_exec` runs after fork and before exec. The closure performs
    // only async-signal-safe `setrlimit` calls with copied scalar values.
    unsafe {
        cmd.as_std_mut().pre_exec(move || {
            set_limit(libc::RLIMIT_CPU, cpu_seconds)?;
            set_limit(libc::RLIMIT_NOFILE, MAX_OPEN_FILES)?;
            Ok(())
        });
    }
    // Its own process group, so a wall-time kill reaps the whole tree, not just
    // the shell leader.
    cmd.as_std_mut().process_group(0);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ExecOutcome {
                stderr: format!("could not start the command: {error}"),
                ..ExecOutcome::default()
            };
        }
    };
    let group = child.id().map(|id| id as i32);
    let Some(stdout) = child.stdout.take() else {
        return ExecOutcome {
            stderr: "stdout capture is unavailable".to_owned(),
            ..ExecOutcome::default()
        };
    };
    let Some(stderr) = child.stderr.take() else {
        return ExecOutcome {
            stderr: "stderr capture is unavailable".to_owned(),
            ..ExecOutcome::default()
        };
    };

    async fn drain<R: AsyncRead + Unpin>(
        mut reader: R,
        capture: Arc<Mutex<Capture>>,
        kind: StreamKind,
    ) {
        let mut chunk = [0_u8; 8 * 1_024];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(read) => capture.lock().unwrap().append(&chunk[..read], kind),
            }
        }
    }

    let capture = Arc::new(Mutex::new(Capture::default()));
    let stdout_reader = tokio::spawn(drain(stdout, capture.clone(), StreamKind::Stdout));
    let stderr_reader = tokio::spawn(drain(stderr, capture.clone(), StreamKind::Stderr));

    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(waited) => (waited.ok(), false),
        Err(_) => {
            if let Some(group) = group {
                signal_group(group, libc::SIGTERM);
                tokio::time::sleep(TERMINATE_GRACE).await;
                signal_group(group, libc::SIGKILL);
            } else {
                let _ = child.kill().await;
            }
            (child.wait().await.ok(), true)
        }
    };
    if let Some(group) = group {
        // A command that daemonized must not outlive the invocation; sweep any
        // survivors after the leader is reaped.
        signal_group(group, libc::SIGKILL);
    }

    for reader in [stdout_reader, stderr_reader] {
        let mut reader = reader;
        if tokio::time::timeout(OUTPUT_DRAIN_GRACE, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }

    let capture = capture.lock().unwrap();
    ExecOutcome {
        exit_code: status.and_then(|status| status.code()),
        stdout: capture.stdout(),
        stderr: capture.stderr(),
        timed_out,
        truncated: capture.truncated,
    }
}

#[cfg(not(unix))]
async fn run_bounded(_command: &str, _workspace: &Path, _timeout: Duration) -> ExecOutcome {
    ExecOutcome {
        stderr: "in-container exec is only supported on unix".to_owned(),
        ..ExecOutcome::default()
    }
}

/// The integer type `libc::setrlimit` takes for the resource selector. glibc
/// types the `RLIMIT_*` constants as `__rlimit_resource_t` (a `u32`); every
/// other unix we build for (musl, macOS, the BSDs) uses a plain `c_int`, so a
/// single `c_int` signature fails to compile on the glibc Linux CI host.
#[cfg(all(unix, target_os = "linux", target_env = "gnu"))]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(all(unix, not(all(target_os = "linux", target_env = "gnu"))))]
type RlimitResource = libc::c_int;

#[cfg(unix)]
fn set_limit(resource: RlimitResource, value: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: `limit` is a valid initialized value and the resource constant is
    // supplied by libc for this target.
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn signal_group(group: i32, signal: i32) {
    // SAFETY: a negative pid addresses the child-owned process group. Failure
    // (normally ESRCH after a clean exit) is intentionally harmless.
    let _ = unsafe { libc::kill(-group, signal) };
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn echoes_and_runs_in_the_workspace() {
        let dir = workspace();
        let tool = ExecTool::new(dir.path(), DEFAULT_EXEC_TIMEOUT);
        // Bound the whole call so a wedged /bin/sh fails fast instead of hanging.
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(&ctx(), serde_json::json!({ "command": "printf hi; pwd" })),
        )
        .await
        .expect("exec returned within the outer bound")
        .unwrap();
        assert!(!out.is_error, "{out:?}");
        assert!(out.content.contains("exit code: 0"), "{}", out.content);
        assert!(out.content.contains("hi"), "{}", out.content);
        // `pwd` resolves through the canonicalized workspace root.
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            out.content.contains(canonical.to_str().unwrap()),
            "ran in the workspace: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn exposes_document_helpers_and_workspace_conventions() {
        let dir = workspace();
        let tool = ExecTool::new(dir.path(), DEFAULT_EXEC_TIMEOUT);
        let spec = tool.spec();
        assert!(spec.description.contains("OPENWAVE_EXEC_SCRIPTS"));
        assert!(spec.description.contains("render_pdf.py"));
        assert!(spec.description.contains("analyze_xlsx.py"));

        let out = tool
            .execute(
                &ctx(),
                serde_json::json!({
                    "command": "printf '%s' \"$OPENWAVE_EXEC_SCRIPTS\""
                }),
            )
            .await
            .unwrap();
        assert!(out.content.contains(DOCUMENT_SCRIPTS_DIR));
        for directory in ["output", "preview", "documents"] {
            assert!(dir.path().join(directory).is_dir());
        }
    }

    #[tokio::test]
    async fn a_timeout_kills_the_process_group() {
        let dir = workspace();
        let tool = ExecTool::new(dir.path(), Duration::from_millis(200));
        // Bound the whole assertion so a kill regression fails fast.
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(&ctx(), serde_json::json!({ "command": "sleep 30" })),
        )
        .await
        .expect("exec returned within the outer bound")
        .unwrap();
        assert!(out.content.contains("timed out"), "{}", out.content);
    }

    #[tokio::test]
    async fn output_past_the_cap_is_truncated() {
        let dir = workspace();
        let tool = ExecTool::new(dir.path(), DEFAULT_EXEC_TIMEOUT);
        // Emit well past the capture cap.
        let command = format!("yes x | head -c {}", MAX_CAPTURE_BYTES * 2);
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(&ctx(), serde_json::json!({ "command": command })),
        )
        .await
        .expect("exec returned within the outer bound")
        .unwrap();
        assert!(out.content.contains("truncated"), "{}", &out.content[..200]);
        assert!(out.content.len() < MAX_CAPTURE_BYTES * 2);
    }

    #[tokio::test]
    async fn an_empty_command_is_a_tool_failure() {
        let dir = workspace();
        let tool = ExecTool::new(dir.path(), DEFAULT_EXEC_TIMEOUT);
        let out = tool
            .execute(&ctx(), serde_json::json!({ "command": "" }))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    fn ctx() -> ToolCtx {
        ToolCtx::without_private_scratch(openwave_core::ChatId::new(), None)
    }
}

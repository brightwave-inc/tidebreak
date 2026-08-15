//! Setup and archive hooks for a workspace checkout.
//!
//! A failed hook preserves the checkout. The caller marks the workspace
//! `SetupFailed` (or leaves archive uncommitted) rather than deleting work.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Outcome of running a user-authored setup or archive script.
#[derive(Debug)]
pub(crate) struct ScriptRun {
    pub success: bool,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run `script` inside `worktree` via the user's login shell.
///
/// The script is a user-authored command string, so it goes through a shell.
/// Git operations in this crate never do.
pub(crate) async fn run_workspace_script(
    worktree: &Path,
    script: &str,
) -> Result<ScriptRun, String> {
    let script = script.trim();
    if script.is_empty() {
        return Ok(ScriptRun {
            success: true,
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        });
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut command = Command::new(shell);
    command
        .arg("-lc")
        .arg(script)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn workspace script: {err}"))?;
    let output = timeout(SCRIPT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "workspace script timed out".to_owned())?
        .map_err(|err| format!("workspace script failed: {err}"))?;
    Ok(ScriptRun {
        success: output.status.success(),
        status: output.status.code(),
        stdout: bound_text(&output.stdout),
        stderr: bound_text(&output.stderr),
    })
}

fn bound_text(bytes: &[u8]) -> String {
    const MAX: usize = 4_096;
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.chars().count() > MAX {
        text = text.chars().take(MAX).collect();
    }
    text
}

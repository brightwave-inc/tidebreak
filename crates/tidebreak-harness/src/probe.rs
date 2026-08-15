//! Login-shell binary resolution, version detection, and auth observation.
//!
//! GUI processes on macOS do not inherit the user's shell PATH. Resolution
//! therefore asks `$SHELL -lc 'command -v <bin>'` and accepts only an
//! absolute, executable result.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::is_absolute_executable;

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Host environment used for discovery. Tests inject a shim shell and PATH.
#[derive(Debug, Clone)]
pub struct HostEnv {
    /// Login shell. Defaults to `$SHELL`, then `/bin/sh`.
    pub shell: PathBuf,
    /// Extra environment for the probe child (typically a test PATH).
    pub env: Vec<(OsString, OsString)>,
    /// When set, replace the process environment entirely with `env`.
    pub clear_env: bool,
}

impl Default for HostEnv {
    fn default() -> Self {
        Self::from_process()
    }
}

impl HostEnv {
    /// The current process's login shell.
    #[must_use]
    pub fn from_process() -> Self {
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self {
            shell,
            env: Vec::new(),
            clear_env: false,
        }
    }
}

/// Probe failure.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The login shell printed nothing useful, or the binary is missing.
    #[error("binary {0} not found on the login-shell PATH")]
    NotFound(String),
    /// `command -v` returned a relative path; only absolute paths are accepted.
    #[error("binary resolution for {name} returned a relative path: {path}")]
    RelativePath {
        /// Binary name that was requested.
        name: String,
        /// The relative path the shell printed.
        path: String,
    },
    /// The resolved path exists but is not executable.
    #[error("resolved path {0} is not an executable file")]
    NotExecutable(PathBuf),
    /// The login shell itself failed.
    #[error("login shell failed: {0}")]
    Shell(String),
}

/// Resolve `name` through the user's login shell.
///
/// The command is `$SHELL -lc 'command -v <name>'`. The last non-empty line
/// of stdout is the answer, so a noisy profile that prints banners still
/// works as long as `command -v` prints last.
pub async fn resolve_binary(host: &HostEnv, name: &str) -> Result<PathBuf, ProbeError> {
    if name.is_empty() || name.contains(['/', '\\', '\0', '\'', '"', ';', '|', '&']) {
        return Err(ProbeError::NotFound(name.to_owned()));
    }
    let script = format!("command -v {name}");
    let output = run_login_shell(host, &script).await?;
    if !output.status_ok && output.stdout.trim().is_empty() {
        return Err(ProbeError::NotFound(name.to_owned()));
    }
    let last_line = output
        .stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    if last_line.is_empty() {
        return Err(ProbeError::NotFound(name.to_owned()));
    }
    let path = PathBuf::from(&last_line);
    if !path.is_absolute() {
        return Err(ProbeError::RelativePath {
            name: name.to_owned(),
            path: last_line,
        });
    }
    if !is_absolute_executable(&path) {
        return Err(ProbeError::NotExecutable(path));
    }
    Ok(path)
}

/// Run `<binary> --version` and return the first line, trimmed.
pub async fn observe_version(binary: &Path) -> Result<String, ProbeError> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| ProbeError::Shell("version probe timed out".into()))?
        .map_err(|err| ProbeError::Shell(err.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    if line.is_empty() {
        return Err(ProbeError::Shell("version probe produced no output".into()));
    }
    Ok(line)
}

struct ShellOutput {
    stdout: String,
    status_ok: bool,
}

async fn run_login_shell(host: &HostEnv, script: &str) -> Result<ShellOutput, ProbeError> {
    let mut command = Command::new(&host.shell);
    command
        .arg("-lc")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if host.clear_env {
        command.env_clear();
    }
    for (key, value) in &host.env {
        command.env(key, value);
    }
    let output = timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| ProbeError::Shell("login shell timed out".into()))?
        .map_err(|err| ProbeError::Shell(err.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if stdout.trim().is_empty() {
        if output.status.success() {
            return Ok(ShellOutput {
                stdout,
                status_ok: true,
            });
        }
        // `command -v` exits non-zero when the binary is missing. That is
        // NotFound, not a broken shell — unless stderr is the only output
        // and looks like a shell error (handled by the caller via empty
        // last-line → NotFound).
        return Ok(ShellOutput {
            stdout,
            status_ok: false,
        });
    }
    Ok(ShellOutput {
        stdout,
        status_ok: output.status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_exec(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn host_with_path(dir: &Path) -> HostEnv {
        HostEnv {
            shell: PathBuf::from("/bin/sh"),
            env: vec![("PATH".into(), dir.as_os_str().to_owned())],
            clear_env: true,
        }
    }

    #[tokio::test]
    async fn missing_binary_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_binary(&host_with_path(dir.path()), "claude")
            .await
            .unwrap_err();
        assert!(matches!(err, ProbeError::NotFound(_)));
    }

    #[tokio::test]
    async fn relative_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        write_exec(&shell, "#!/bin/sh\necho claude\n");
        let host = HostEnv {
            shell,
            env: Vec::new(),
            clear_env: false,
        };
        let err = resolve_binary(&host, "claude").await.unwrap_err();
        assert!(matches!(err, ProbeError::RelativePath { .. }));
    }

    #[tokio::test]
    async fn noisy_profile_still_resolves_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho 2.1.233\n");
        let shell = dir.path().join("shell");
        write_exec(
            &shell,
            &format!(
                "#!/bin/sh\necho 'welcome to my profile'\necho '{}'\n",
                bin.display()
            ),
        );
        let host = HostEnv {
            shell,
            env: Vec::new(),
            clear_env: false,
        };
        let resolved = resolve_binary(&host, "claude").await.unwrap();
        assert_eq!(resolved, bin);
    }

    #[tokio::test]
    async fn version_variants_are_first_nonempty_line() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho\necho '2.1.233 (Claude Code)'\n");
        let version = observe_version(&bin).await.unwrap();
        assert!(version.contains("2.1.233"));
    }
}

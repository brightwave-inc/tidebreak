//! Setup and archive hooks for a workspace checkout.
//!
//! A failed hook preserves the checkout. The caller marks the workspace
//! `SetupFailed` (or leaves archive uncommitted) rather than deleting work.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tidebreak_harness::ProcessTreeChild;
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

/// Spawn a user-authored script in the platform-native non-interactive shell.
///
/// Process ownership stays centralized in `tidebreak-harness`, where Windows
/// children are assigned to a Job Object before they can create descendants.
pub(crate) fn spawn_workspace_script(
    worktree: &Path,
    script: &str,
) -> io::Result<ProcessTreeChild> {
    let mut command = workspace_script_command(worktree, script);
    tidebreak_harness::spawn_process_tree(&mut command)
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
    let child = spawn_workspace_script(worktree, script)
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

fn workspace_script_command(worktree: &Path, script: &str) -> Command {
    let (program, args) = workspace_script_launcher(script, std::env::var_os("SHELL"));
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn workspace_script_launcher(
    script: &str,
    unix_shell: Option<OsString>,
) -> (OsString, Vec<OsString>) {
    #[cfg(windows)]
    {
        let _ = unix_shell;
        let script = format!(
            "$global:LASTEXITCODE = 0\n{script}\nif (-not $?) {{ if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}; exit 1 }}\nexit 0"
        );
        (
            OsString::from("powershell.exe"),
            [
                OsString::from("-NoLogo"),
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from(script),
            ]
            .into_iter()
            .collect(),
        )
    }
    #[cfg(not(windows))]
    {
        (
            unix_shell.unwrap_or_else(|| OsString::from("/bin/sh")),
            [OsString::from("-lc"), OsString::from(script)].into(),
        )
    }
}

fn bound_text(bytes: &[u8]) -> String {
    const MAX: usize = 4_096;
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.chars().count() > MAX {
        text = text.chars().take(MAX).collect();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_scripts_preserve_the_configured_login_shell_contract() {
        let (program, args) =
            workspace_script_launcher("printf ready", Some(OsString::from("/custom/shell")));
        assert_eq!(program, OsString::from("/custom/shell"));
        assert_eq!(
            args,
            [OsString::from("-lc"), OsString::from("printf ready")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_scripts_use_non_interactive_windows_powershell() {
        let (program, args) = workspace_script_launcher(
            "Write-Output ready",
            Some(OsString::from("ignored-shell.exe")),
        );
        assert_eq!(program, OsString::from("powershell.exe"));
        assert_eq!(
            &args[..4],
            ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
        );
        let command = args[4].to_string_lossy();
        assert!(command.contains("Write-Output ready"));
        assert!(command.contains("$LASTEXITCODE"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_powershell_runs_in_the_requested_worktree() {
        let directory = tempfile::tempdir().expect("worktree");
        let run = run_workspace_script(directory.path(), "(Get-Location).Path")
            .await
            .expect("run PowerShell");
        assert!(run.success, "{}", run.stderr);
        let reported = Path::new(run.stdout.trim())
            .canonicalize()
            .expect("reported worktree");
        assert_eq!(reported, directory.path().canonicalize().unwrap());

        let failed = run_workspace_script(directory.path(), "cmd.exe /c exit 7")
            .await
            .expect("run failing native command");
        assert!(!failed.success);
        assert_eq!(failed.status, Some(7));
    }
}

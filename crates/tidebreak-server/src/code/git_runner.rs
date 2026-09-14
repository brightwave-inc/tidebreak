//! Bounded subprocess runner for code-mode git (and `gh`) shell-outs.
//!
//! `worktree`, `gh`, and `checkpoint` used to each spawn git with their own
//! timeout and output-budget code. The bounds stay parameterized; this module
//! owns the one spawn/wait/finish path.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tidebreak_harness::{spawn_process_tree, BoundedProcessOutput, OutputBudget};
use tokio::process::Command;
use tokio::time::timeout;

pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const STDOUT_BYTES: usize = 8 * 1024 * 1024;
pub const STDOUT_LINES: usize = 200_000;
pub const STDERR_BYTES: usize = 64 * 1024;
pub const STDERR_LINES: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedCommandError {
    TimedOut,
    Failed(String),
}

impl BoundedCommandError {
    pub fn into_message(self, description: &str) -> String {
        match self {
            Self::TimedOut => format!("{description} timed out"),
            Self::Failed(message) => message,
        }
    }
}

pub fn default_stdout_budget() -> OutputBudget {
    OutputBudget::head(STDOUT_BYTES, STDOUT_LINES)
}

pub fn default_stderr_budget() -> OutputBudget {
    OutputBudget::tail(STDERR_BYTES, STDERR_LINES)
}

pub fn git_command(cwd: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
}

pub async fn wait_command_bounded(
    command: &mut Command,
    limit: Duration,
    stdout_budget: OutputBudget,
    stderr_budget: OutputBudget,
    description: &str,
) -> Result<BoundedProcessOutput, BoundedCommandError> {
    let child = spawn_process_tree(command).map_err(|err| {
        BoundedCommandError::Failed(format!("failed to spawn {description}: {err}"))
    })?;
    match timeout(
        limit,
        child.wait_with_bounded_output(stdout_budget, stderr_budget, true),
    )
    .await
    {
        Err(_) => Err(BoundedCommandError::TimedOut),
        Ok(Err(err)) => Err(BoundedCommandError::Failed(format!(
            "{description} failed: {err}"
        ))),
        Ok(Ok(output)) => Ok(output),
    }
}

pub fn finish_bounded_command(
    output: BoundedProcessOutput,
    accept_truncated_stdout: bool,
    exceeded_limit: &str,
    suffix_terminated_detail: bool,
) -> Result<(Vec<u8>, bool), String> {
    let stdout_truncated = output.stdout.truncated;
    let stderr_truncated = output.stderr.truncated;
    let stderr_empty = output.stderr.bytes.is_empty();
    if output.status.success() && !output.terminated_for_output {
        return if stdout_truncated && !accept_truncated_stdout {
            Err(exceeded_limit.to_owned())
        } else {
            Ok((output.stdout.bytes, stdout_truncated))
        };
    }
    if output.terminated_for_output
        && stdout_truncated
        && !stderr_truncated
        && stderr_empty
        && accept_truncated_stdout
    {
        return Ok((output.stdout.bytes, true));
    }
    let stdout = output.stdout.into_marked_text().trim().to_owned();
    let stderr = output.stderr.into_marked_text().trim().to_owned();
    if output.terminated_for_output {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            exceeded_limit.to_owned()
        } else if suffix_terminated_detail {
            format!("{exceeded_limit}: {detail}")
        } else {
            detail
        });
    }
    Err(if stderr.is_empty() { stdout } else { stderr })
}

pub async fn has_uncommitted_work(worktree: &Path) -> Result<bool, String> {
    let mut command = git_command(Some(worktree));
    command.args(["status", "--porcelain"]);
    let description = "git status --porcelain";
    let output = wait_command_bounded(
        &mut command,
        GIT_TIMEOUT,
        default_stdout_budget(),
        default_stderr_budget(),
        description,
    )
    .await
    .map_err(|err| err.into_message(description))?;
    let (bytes, _) = finish_bounded_command(output, false, "git output exceeded its limit", false)?;
    Ok(!bytes.trim_ascii().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::Stdio;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    async fn oversized_head_output() -> BoundedProcessOutput {
        let mut command = Command::new("head");
        command
            .args(["-c", "4096", "/dev/zero"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        wait_command_bounded(
            &mut command,
            Duration::from_secs(5),
            OutputBudget::head(64, 8),
            OutputBudget::tail(64, 8),
            "head",
        )
        .await
        .unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_output_beyond_the_cap_is_truncated() {
        let output = oversized_head_output().await;
        assert!(output.stdout.truncated);
        assert!(output.stdout.bytes.len() <= 64);
        let marked = output.stdout.into_marked_text();
        assert!(marked.contains("[output truncated]"));
        let accepted = oversized_head_output().await;
        let (finished, truncated) =
            finish_bounded_command(accepted, true, "git output exceeded its limit", false).unwrap();
        assert!(truncated);
        assert!(
            !String::from_utf8_lossy(&finished).contains("[output truncated]"),
            "callers parse raw bytes; the marker must not be spliced in"
        );
        let refused = finish_bounded_command(
            oversized_head_output().await,
            false,
            "git output exceeded its limit",
            false,
        )
        .unwrap_err();
        assert!(
            refused.contains("exceeded its limit") || refused.contains("[output truncated]"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn porcelain_status_reports_uncommitted_work() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        for args in [
            ["init", "-b", "main"].as_slice(),
            ["config", "user.email", "dev@example.com"].as_slice(),
            ["config", "user.name", "Dev"].as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        assert!(
            has_uncommitted_work(repo).await.unwrap(),
            "an untracked file is uncommitted work"
        );
        let add = std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(add.success());
        let commit = std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(commit.success());
        assert!(
            !has_uncommitted_work(repo).await.unwrap(),
            "a clean tree has no uncommitted work"
        );
    }
}

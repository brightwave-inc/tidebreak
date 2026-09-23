//! Repository trust: whether engines load the configuration a repository
//! carries for them.
//!
//! A repository can ship engine settings that run commands as you the moment
//! a session starts: Claude Code hooks, MCP servers, opencode plugins. Headless
//! engines load them without asking, outside Tidebreak's approvals and
//! sandbox. So every engine launches with project-level config turned off
//! ([`tidebreak_harness::ProjectConfig::Skip`]) until you trust the
//! repository.
//!
//! The decision lives in the repository's own git config as
//! `tidebreak.trusted`, beside the other `tidebreak.*` keys, so it needs no
//! migration and every worktree of the repository reads the same answer. Git
//! never clones config, so a repository cannot arrive already trusted, and
//! anyone who can write `.git/config` can already run commands through git
//! itself.

use std::path::Path;

use tidebreak_harness::ProjectConfig;

use super::git_runner::{
    default_stderr_budget, default_stdout_budget, git_command, wait_command_bounded, GIT_TIMEOUT,
};
use super::types::CodeRepoTrust;

/// The git config key that records the decision.
pub const TRUST_KEY: &str = "tidebreak.trusted";

/// The decision recorded in `repo_root`'s own git config.
///
/// Only an explicit `true` or `false` counts. A missing key is undecided, and
/// so is a config git cannot read: an error never trusts a repository.
pub async fn read_repo_trust(repo_root: &Path) -> CodeRepoTrust {
    let mut command = git_command(Some(repo_root));
    command.args(["config", "--local", "--type=bool", "--get", TRUST_KEY]);
    let description = "git config --get tidebreak.trusted";
    let output = match wait_command_bounded(
        &mut command,
        GIT_TIMEOUT,
        default_stdout_budget(),
        default_stderr_budget(),
        description,
    )
    .await
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                repo = %repo_root.display(),
                error = %error.into_message(description),
                "could not read the repository trust decision; treating it as undecided"
            );
            return CodeRepoTrust::Undecided;
        }
    };
    if !output.status.success() {
        // Exit 1 is git's "no such key". Anything else is a config git could
        // not read, which is no more a decision than a missing key.
        if output.status.code() != Some(1) {
            tracing::warn!(
                repo = %repo_root.display(),
                stderr = %output.stderr.into_marked_text().trim(),
                "could not read the repository trust decision; treating it as undecided"
            );
        }
        return CodeRepoTrust::Undecided;
    }
    match String::from_utf8_lossy(&output.stdout.bytes).trim() {
        "true" => CodeRepoTrust::Trusted,
        "false" => CodeRepoTrust::Untrusted,
        _ => CodeRepoTrust::Undecided,
    }
}

/// Record the decision in `repo_root`'s own git config.
pub async fn write_repo_trust(repo_root: &Path, trusted: bool) -> Result<(), String> {
    let mut command = git_command(Some(repo_root));
    command.args([
        "config",
        "--local",
        "--type=bool",
        TRUST_KEY,
        if trusted { "true" } else { "false" },
    ]);
    let description = "git config tidebreak.trusted";
    let output = wait_command_bounded(
        &mut command,
        GIT_TIMEOUT,
        default_stdout_budget(),
        default_stderr_budget(),
        description,
    )
    .await
    .map_err(|error| error.into_message(description))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = output.stderr.into_marked_text();
    Err(format!(
        "could not record the trust decision in the repository's git config: {}",
        stderr.trim()
    ))
}

/// What an engine launch in this repository may load.
#[must_use]
pub fn project_config_for(trust: CodeRepoTrust) -> ProjectConfig {
    match trust {
        CodeRepoTrust::Trusted => ProjectConfig::Load,
        CodeRepoTrust::Undecided | CodeRepoTrust::Untrusted => ProjectConfig::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[tokio::test]
    async fn only_an_explicit_decision_trusts_a_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        assert_eq!(read_repo_trust(repo).await, CodeRepoTrust::Undecided);
        assert_eq!(
            project_config_for(read_repo_trust(repo).await),
            ProjectConfig::Skip
        );

        write_repo_trust(repo, true).await.unwrap();
        assert_eq!(read_repo_trust(repo).await, CodeRepoTrust::Trusted);
        assert_eq!(
            project_config_for(read_repo_trust(repo).await),
            ProjectConfig::Load
        );

        write_repo_trust(repo, false).await.unwrap();
        assert_eq!(read_repo_trust(repo).await, CodeRepoTrust::Untrusted);
        assert_eq!(
            project_config_for(read_repo_trust(repo).await),
            ProjectConfig::Skip
        );

        // A value git cannot read as a boolean is no decision at all.
        git(repo, &["config", "--local", TRUST_KEY, "maybe"]);
        assert_eq!(read_repo_trust(repo).await, CodeRepoTrust::Undecided);
    }

    /// A linked worktree shares the repository's config, so trusting the
    /// main checkout covers every workspace, and the trust key never lands
    /// in a file the worktree's branch could commit.
    #[tokio::test]
    async fn a_worktree_reads_the_decision_recorded_on_its_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "dev@example.com"]);
        git(&repo, &["config", "user.name", "Dev"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        let worktree = dir.path().join("worktree");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );

        write_repo_trust(&repo, true).await.unwrap();
        assert_eq!(read_repo_trust(&worktree).await, CodeRepoTrust::Trusted);
        let tracked = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&worktree)
            .output()
            .unwrap();
        assert!(tracked.stdout.is_empty(), "the decision is not a file");
    }
}

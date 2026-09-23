//! Throwaway repositories for the restore and revert tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A repository with one commit holding `README.md` and `keep.txt`.
pub(super) fn init_repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let repo = dir.path().join("origin");
    std::fs::create_dir_all(&repo).unwrap();
    run(&repo, &["git", "init", "-q", "-b", "main"]);
    run(&repo, &["git", "config", "user.email", "dev@example.com"]);
    run(&repo, &["git", "config", "user.name", "Dev"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    std::fs::write(repo.join("keep.txt"), "keep\n").unwrap();
    run(&repo, &["git", "add", "README.md", "keep.txt"]);
    run(&repo, &["git", "commit", "-q", "-m", "init"]);
    (dir, repo)
}

/// A linked worktree on a new branch from `main`, the shape a workspace has.
pub(super) fn add_worktree(repo: &Path, label: &str) -> PathBuf {
    let path = repo.parent().unwrap().join(label);
    run(
        repo,
        &[
            "git",
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("tidebreak/{label}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

pub(super) fn run(cwd: &Path, args: &[&str]) {
    let status = Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .unwrap();
    assert!(status.success(), "{args:?} failed in {}", cwd.display());
}

/// Run git and return its trimmed stdout.
pub(super) fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

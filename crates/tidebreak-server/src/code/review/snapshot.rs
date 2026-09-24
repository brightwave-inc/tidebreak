//! The disposable copy a reviewer reads.
//!
//! A reviewer never runs in the person's worktree. It runs in a copy of the
//! reviewed state under the profile's data directory, which is deleted when
//! the review ends and swept at the next start if the process died first.
//!
//! The copy is its own git repository, not a worktree of the person's: a
//! linked worktree shares the person's refs, so a reviewer that moved a
//! branch or pushed would reach them. This one borrows the person's objects
//! read-only through `objects/info/alternates`, which git never writes to,
//! and has refs, an index, and a config of its own, with no remote. Its
//! `HEAD` is the state before the changes and its files and index are the
//! state after them, so `git diff HEAD` shows the reviewer exactly the diff
//! under review, new files included.
//!
//! The copy's git runs with no global or system config, so none of the
//! person's filters, hooks, or credential helpers apply to it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tidebreak_core::CodeReviewId;
use tidebreak_harness::OutputBudget;

use crate::code::git_runner;

/// Where review copies live, under the profile data directory.
const REVIEWS_DIR: &str = "reviews";
/// The directory the reviewer works in, inside one review's root.
const TREE_DIR: &str = "tree";
/// The empty file every copy's git reads as its global config.
const CONFIG_FILE: &str = "gitconfig";
/// A checkout of a large tree takes a while; nothing here waits on a person.
const GIT_LIMIT: Duration = Duration::from_secs(180);

/// The root holding every review copy.
#[must_use]
pub fn reviews_root(data_dir: &Path) -> PathBuf {
    data_dir.join("code").join(REVIEWS_DIR)
}

/// The root of one review's copy.
#[must_use]
pub fn review_root(data_dir: &Path, id: CodeReviewId) -> PathBuf {
    reviews_root(data_dir).join(id.to_string())
}

/// A materialized copy.
#[derive(Debug, Clone)]
pub struct ReviewCopy {
    /// Deleted as a whole when the review ends.
    pub root: PathBuf,
    /// The reviewer's working directory: the files after the changes.
    pub tree: PathBuf,
}

/// Build the copy of `to` (a tree-ish in the person's repository) with
/// `HEAD` at `from` (a commit-ish), for the worktree at `worktree`.
///
/// `root` must not exist yet. On failure, whatever was written is removed.
pub async fn materialize(
    worktree: &Path,
    from: &str,
    to: &str,
    root: &Path,
) -> Result<ReviewCopy, String> {
    let result = materialize_inner(worktree, from, to, root).await;
    if result.is_err() {
        remove(root).await;
    }
    result
}

async fn materialize_inner(
    worktree: &Path,
    from: &str,
    to: &str,
    root: &Path,
) -> Result<ReviewCopy, String> {
    // Everything the copy needs from the person's repository, read first.
    // Both revisions come from the diff range Tidebreak resolved, never from
    // a request, but an option-shaped one is refused all the same.
    if from.starts_with('-') || to.starts_with('-') {
        return Err("the reviewed revisions are not revisions".to_owned());
    }
    let objects = person_git(
        worktree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
    )
    .await?;
    let objects = absolute(worktree, &objects);
    let tree_oid = person_git(
        worktree,
        &["rev-parse", "--verify", &format!("{to}^{{tree}}")],
    )
    .await?;
    let base_oid = person_git(
        worktree,
        &["rev-parse", "--verify", &format!("{from}^{{commit}}")],
    )
    .await
    .ok();

    create_private_dir(root)?;
    let config = root.join(CONFIG_FILE);
    std::fs::write(&config, b"").map_err(|err| format!("could not write {CONFIG_FILE}: {err}"))?;
    let tree = root.join(TREE_DIR);
    create_private_dir(&tree)?;

    copy_git(&tree, &config, &["init", "--quiet"]).await?;
    let alternates = tree
        .join(".git")
        .join("objects")
        .join("info")
        .join("alternates");
    let line = format!("{}\n", objects.to_string_lossy());
    std::fs::write(&alternates, line)
        .map_err(|err| format!("could not link the repository's objects: {err}"))?;
    if let Some(base) = &base_oid {
        copy_git(&tree, &config, &["update-ref", "HEAD", base]).await?;
    }
    copy_git(&tree, &config, &["read-tree", &tree_oid]).await?;
    copy_git(&tree, &config, &["checkout-index", "--all", "--force"]).await?;
    Ok(ReviewCopy {
        root: root.to_path_buf(),
        tree,
    })
}

/// Delete one copy. Best effort: a copy a crash leaves behind is swept at the
/// next start.
pub async fn remove(root: &Path) {
    let root = root.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || remove_tree(&root)).await;
}

/// Delete every copy except the ones still in use. Called at start, before
/// any review runs, and safe to call while reviews run.
pub fn sweep(data_dir: &Path, keep: &std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(reviews_root(data_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) {
            continue;
        }
        remove_tree(&entry.path());
    }
}

fn remove_tree(path: &Path) {
    if std::fs::remove_dir_all(path).is_ok() || !path.exists() {
        return;
    }
    // Git writes some files read-only, which Windows will not delete as they
    // are. Make everything writable and try once more.
    make_writable(path);
    if let Err(error) = std::fs::remove_dir_all(path) {
        tracing::warn!(path = %path.display(), %error, "a review copy was not deleted");
    }
}

fn make_writable(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    let mut permissions = metadata.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    let _ = std::fs::set_permissions(path, permissions);
    if metadata.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

fn create_private_dir(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))
}

/// Git's answer as an absolute path. Not canonicalized: on Windows that
/// yields a `\\?\` path git cannot read back from an alternates file.
fn absolute(worktree: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    }
}

/// A read in the person's repository.
async fn person_git(worktree: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = git_runner::git_command(Some(worktree));
    command.args(args);
    run(command, args).await
}

/// A write in the copy, with none of the person's git config.
async fn copy_git(tree: &Path, config: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = git_runner::git_command(Some(tree));
    command
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .args(["-c", "core.hooksPath=", "-c", "core.fsmonitor=false"])
        .args(args);
    run(command, args).await
}

async fn run(mut command: tokio::process::Command, args: &[&str]) -> Result<String, String> {
    let label = format!("git {}", args.first().copied().unwrap_or_default());
    let output = git_runner::wait_command_bounded(
        &mut command,
        GIT_LIMIT,
        OutputBudget::head(64 * 1024, 1_000),
        git_runner::default_stderr_budget(),
        &label,
    )
    .await
    .map_err(|err| match err {
        git_runner::BoundedCommandError::TimedOut => format!("{label} timed out"),
        git_runner::BoundedCommandError::Failed(message) => message,
    })?;
    let stdout =
        git_runner::finish_bounded_command(output, true, &format!("{label} output"), true)?;
    Ok(String::from_utf8_lossy(&stdout.0).trim().to_owned())
}

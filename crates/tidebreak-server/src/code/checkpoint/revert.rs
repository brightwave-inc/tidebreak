//! Undo one change in the live worktree: a file's change, one hunk of it, or
//! a file's uncommitted edits.
//!
//! A revert rebuilds the patch from the range the diff view shows and applies
//! it in reverse with `git apply`, so it lands only where the worktree still
//! holds that change, and it never touches the user's index. A discard puts
//! files back to the last commit through git's own checkout and clean. Neither
//! runs a hook, and neither writes text a client sent.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use tidebreak_harness::OutputBudget;
use tracing::warn;

use super::{
    complete_nul_terminated_records, git_bytes_bounded, git_bytes_with_literal_paths_bounded,
    git_command, parse_name_status, review_diff_args, review_name_status_args, run_git_command,
    snapshot_tree, ChangedFile, CheckpointError, GitPath, GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES,
    GIT_SNAPSHOT_TIMEOUT, GIT_TIMEOUT, REVIEW_DIFF_FLAGS,
};

/// The largest patch a revert builds. A whole-file revert of a binary file
/// carries the file itself, base85-encoded, so this bounds the file too.
const MAX_PATCH_BYTES: usize = 32 * 1024 * 1024;
const MAX_PATCH_LINES: usize = 2_000_000;

/// Which hunk of a file's diff to revert, as the diff view showed it.
#[derive(Debug, Clone, Copy)]
pub struct HunkSelector<'a> {
    /// Zero-based position of the hunk in the file's diff.
    pub index: usize,
    /// The hunk as the view showed it, from its `@@` line through its last
    /// line. The revert refuses when the hunk the server finds differs,
    /// because the person confirmed the hunk they saw.
    pub text: &'a str,
}

/// The paths a revert or a discard changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertedChange {
    pub paths: Vec<GitPath>,
}

/// Undo `path`'s change between `from` and `to` in the worktree, or one hunk
/// of it.
///
/// For the workspace diff, `to` is a fresh snapshot of the worktree, so a
/// whole-file revert always lands: the file goes back to its version at
/// `from`. For a turn's diff, `from` and `to` are the turn's checkpoints, and
/// the revert lands only where the worktree still holds the turn's change. A
/// renamed file goes back to its old name.
pub async fn revert_change(
    worktree: &Path,
    from: &str,
    to: &str,
    path: &str,
    hunk: Option<HunkSelector<'_>>,
) -> Result<RevertedChange, CheckpointError> {
    let path = GitPath::from_wire(path)?;
    let change = find_change(worktree, from, to, &path)
        .await?
        .ok_or_else(|| {
            CheckpointError::conflict("no_change", "This file has no change to revert.")
        })?;
    let (patch, paths) = match hunk {
        None => {
            let mut paths = vec![change.path.clone()];
            paths.extend(change.previous_path.clone());
            (whole_file_patch(worktree, from, to, &paths).await?, paths)
        }
        Some(hunk) => (
            hunk_patch(worktree, from, to, &change.path, hunk).await?,
            vec![change.path.clone()],
        ),
    };
    apply_in_reverse(worktree, &patch).await?;
    Ok(RevertedChange { paths })
}

/// Put each path back to the last commit: its content and mode when `HEAD`
/// holds it, gone when it does not. The user's index follows, so a discarded
/// change is not left staged for the next commit.
///
/// Every path must be one git reports as changed since `HEAD`. That keeps a
/// discard to the files the person saw, and never lets a directory name
/// sweep up the untracked files inside it.
pub async fn discard_paths(
    worktree: &Path,
    paths: &[String],
) -> Result<RevertedChange, CheckpointError> {
    let mut wanted: Vec<GitPath> = Vec::new();
    for path in paths {
        let path = GitPath::from_wire(path)?;
        if !wanted.contains(&path) {
            wanted.push(path);
        }
    }
    if wanted.is_empty() {
        return Err(CheckpointError::user("name at least one file to discard"));
    }
    let snapshot = snapshot_tree(worktree).await?;
    let changed = uncommitted_paths(worktree, &snapshot).await?;
    if let Some(unchanged) = wanted.iter().find(|path| !changed.contains(path)) {
        return Err(CheckpointError::conflict(
            "no_change",
            format!(
                "{} has no uncommitted change to discard.",
                unchanged.to_wire()
            ),
        ));
    }

    let (listed, _) = git_bytes_with_literal_paths_bounded(
        worktree,
        &["ls-tree", "-z", "--name-only", "--full-tree", "HEAD", "--"],
        &wanted,
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    let committed: HashSet<GitPath> = listed
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(GitPath::from_bytes)
        .collect();
    let (restore, remove): (Vec<GitPath>, Vec<GitPath>) = wanted
        .iter()
        .cloned()
        .partition(|path| committed.contains(path));

    run_on_paths(worktree, &["reset", "-q", "HEAD", "--"], &wanted).await?;
    if !restore.is_empty() {
        run_on_paths(worktree, &["checkout-index", "-f", "-q", "--"], &restore).await?;
    }
    if !remove.is_empty() {
        run_on_paths(worktree, &["clean", "-f", "-q", "--"], &remove).await?;
    }
    Ok(RevertedChange { paths: wanted })
}

/// Every path that differs between `HEAD` and `snapshot`, which is what a
/// commit of the worktree would carry. A rename counts as both of its paths.
pub async fn uncommitted_paths(
    worktree: &Path,
    snapshot: &str,
) -> Result<HashSet<GitPath>, CheckpointError> {
    let mut args = vec!["diff"];
    args.extend_from_slice(REVIEW_DIFF_FLAGS);
    args.extend_from_slice(&["--name-only", "-z", "--no-renames", "HEAD", snapshot]);
    let (raw, truncated) = git_bytes_bounded(
        worktree,
        &args,
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    Ok(complete_nul_terminated_records(&raw, truncated)
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(GitPath::from_bytes)
        .collect())
}

/// The change the diff lists for `path`, by its new name or its old one.
async fn find_change(
    worktree: &Path,
    from: &str,
    to: &str,
    path: &GitPath,
) -> Result<Option<ChangedFile>, CheckpointError> {
    let args = review_name_status_args(from, to);
    let (raw, truncated) = git_bytes_bounded(
        worktree,
        &args[..args.len() - 1],
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    Ok(
        parse_name_status(complete_nul_terminated_records(&raw, truncated))
            .into_iter()
            .find(|file| &file.path == path || file.previous_path.as_ref() == Some(path)),
    )
}

/// The file's whole change, binary content and renames included, so the
/// reverse puts back exactly what `from` held.
async fn whole_file_patch(
    worktree: &Path,
    from: &str,
    to: &str,
    paths: &[GitPath],
) -> Result<Vec<u8>, CheckpointError> {
    let mut args = vec!["diff"];
    args.extend_from_slice(REVIEW_DIFF_FLAGS);
    args.extend_from_slice(&["--binary", "--full-index", "--find-renames", from, to, "--"]);
    let patch = bounded_patch(worktree, &args, paths).await?;
    if patch.is_empty() {
        return Err(CheckpointError::conflict(
            "no_change",
            "This file has no change to revert.",
        ));
    }
    Ok(patch)
}

/// One hunk of the file's diff, rebuilt with the flags the diff view uses so
/// it can be checked against the hunk the person saw.
async fn hunk_patch(
    worktree: &Path,
    from: &str,
    to: &str,
    path: &GitPath,
    hunk: HunkSelector<'_>,
) -> Result<Vec<u8>, CheckpointError> {
    let raw = bounded_patch(
        worktree,
        &review_diff_args(from, to),
        std::slice::from_ref(path),
    )
    .await?;
    let section = FileSection::parse(&raw);
    let shown = hunk.text.strip_suffix('\n').unwrap_or(hunk.text);
    let Some(lines) = section
        .hunks
        .get(hunk.index)
        .filter(|lines| String::from_utf8_lossy(&lines.join(&b'\n')) == shown)
    else {
        return Err(CheckpointError::conflict(
            "diff_changed",
            "This change is no longer in the diff. Review the diff again.",
        ));
    };
    // A new or deleted file is a single hunk: revert it whole, file mode and
    // all. Anything else keeps only the lines `git apply` needs to place the
    // hunk, so a rename or a mode change on the same file stays.
    let whole_file = section
        .header
        .iter()
        .any(|line| line.starts_with(b"new file mode") || line.starts_with(b"deleted file mode"));
    let mut patch = Vec::new();
    for line in &section.header {
        if whole_file
            || line.starts_with(b"diff --git ")
            || line.starts_with(b"--- ")
            || line.starts_with(b"+++ ")
        {
            patch.extend_from_slice(line);
            patch.push(b'\n');
        }
    }
    for line in lines {
        patch.extend_from_slice(line);
        patch.push(b'\n');
    }
    Ok(patch)
}

async fn bounded_patch(
    worktree: &Path,
    args: &[&str],
    paths: &[GitPath],
) -> Result<Vec<u8>, CheckpointError> {
    let (raw, truncated) = git_bytes_with_literal_paths_bounded(
        worktree,
        args,
        paths,
        GIT_SNAPSHOT_TIMEOUT,
        OutputBudget::head(MAX_PATCH_BYTES, MAX_PATCH_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Err(CheckpointError::conflict(
            "change_too_large",
            "This change is too large to revert here. Revert it in a terminal.",
        ));
    }
    Ok(raw)
}

/// Apply `patch` in reverse to the worktree only. `git apply` checks every
/// hunk before it writes a file, so a patch that no longer fits changes
/// nothing.
async fn apply_in_reverse(worktree: &Path, patch: &[u8]) -> Result<(), CheckpointError> {
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|err| CheckpointError::internal(format!("could not stage the revert: {err}")))?;
    file.write_all(patch)
        .and_then(|()| file.flush())
        .map_err(|err| CheckpointError::internal(format!("could not stage the revert: {err}")))?;
    let mut command = git_command(worktree);
    command
        .args(["apply", "-R", "--whitespace=nowarn"])
        .arg(file.path());
    run_git_command(command, "apply -R".to_owned(), GIT_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|err| {
            warn!(error = %err, "a revert did not apply");
            CheckpointError::conflict(
                "revert_conflict",
                "This change no longer matches the file, so nothing was reverted. The file \
                 changed after the diff you reviewed.",
            )
        })
}

async fn run_on_paths(
    worktree: &Path,
    args: &[&str],
    paths: &[GitPath],
) -> Result<(), CheckpointError> {
    git_bytes_with_literal_paths_bounded(
        worktree,
        args,
        paths,
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .map(|_| ())
    .map_err(CheckpointError::internal)
}

/// One file's section of a unified diff, split at its hunk headers.
///
/// A hunk's own lines start with a space, `+`, `-`, or `\`, so a line that
/// starts with `@@ ` always opens the next hunk.
struct FileSection<'a> {
    header: Vec<&'a [u8]>,
    hunks: Vec<Vec<&'a [u8]>>,
}

impl<'a> FileSection<'a> {
    fn parse(raw: &'a [u8]) -> Self {
        let body = raw.strip_suffix(b"\n").unwrap_or(raw);
        let mut header = Vec::new();
        let mut hunks: Vec<Vec<&[u8]>> = Vec::new();
        if body.is_empty() {
            return Self { header, hunks };
        }
        for line in body.split(|byte| *byte == b'\n') {
            if line.starts_with(b"@@ ") {
                hunks.push(vec![line]);
            } else if let Some(hunk) = hunks.last_mut() {
                hunk.push(line);
            } else {
                header.push(line);
            }
        }
        Self { header, hunks }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{add_worktree, git_stdout, init_repo, run};
    use super::super::{merge_base, produce_diff, DiffBounds};
    use super::*;

    /// Hunk `index` of `path` exactly as the diff view receives it.
    async fn shown_hunk(worktree: &Path, from: &str, to: &str, path: &str, index: usize) -> String {
        let diff = produce_diff(worktree, from, to, Some(path), DiffBounds::default())
            .await
            .unwrap();
        let mut hunks: Vec<Vec<&str>> = Vec::new();
        for line in diff.diff.split('\n').filter(|line| !line.is_empty()) {
            if line.starts_with("@@ ") {
                hunks.push(vec![line]);
            } else if let Some(hunk) = hunks.last_mut() {
                hunk.push(line);
            }
        }
        hunks[index].join("\n")
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn lines(count: usize, label: &str) -> String {
        (1..=count).map(|n| format!("{label} {n}\n")).collect()
    }

    #[tokio::test]
    async fn a_file_revert_puts_the_base_version_back_for_every_kind_of_change() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-file");
        std::fs::write(tree.join("README.md"), "edited\n").unwrap();
        std::fs::write(tree.join("added.txt"), "new\n").unwrap();
        std::fs::rename(tree.join("keep.txt"), tree.join("kept.txt")).unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let index_before = git_stdout(&tree, &["ls-files", "--stage"]);

        for path in ["README.md", "added.txt", "kept.txt"] {
            let to = snapshot_tree(&tree).await.unwrap();
            let reverted = revert_change(&tree, &from, &to, path, None).await.unwrap();
            assert!(reverted.paths.iter().any(|p| p.to_wire() == path));
        }

        assert_eq!(read(&tree.join("README.md")).as_deref(), Some("hello\n"));
        assert!(!tree.join("added.txt").exists(), "an added file is removed");
        assert_eq!(
            read(&tree.join("keep.txt")).as_deref(),
            Some("keep\n"),
            "a renamed file goes back to its old name"
        );
        assert!(!tree.join("kept.txt").exists());
        assert_eq!(
            git_stdout(&tree, &["ls-files", "--stage"]),
            index_before,
            "a revert never stages anything"
        );
        let to = snapshot_tree(&tree).await.unwrap();
        assert_eq!(
            to,
            git_stdout(&tree, &["rev-parse", &format!("{from}^{{tree}}")])
        );
    }

    #[tokio::test]
    async fn a_binary_file_reverts_whole() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-binary");
        std::fs::write(tree.join("logo.png"), b"\x89PNG\r\n\x1a\n\x00\x01").unwrap();
        run(&tree, &["git", "add", "logo.png"]);
        run(&tree, &["git", "commit", "-q", "-m", "logo"]);
        let from = snapshot_tree(&tree).await.unwrap();
        std::fs::write(tree.join("logo.png"), b"\x89PNG\r\n\x1a\n\x00\x02\x03").unwrap();
        let to = snapshot_tree(&tree).await.unwrap();

        revert_change(&tree, &from, &to, "logo.png", None)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(tree.join("logo.png")).unwrap(),
            b"\x89PNG\r\n\x1a\n\x00\x01"
        );
    }

    #[tokio::test]
    async fn a_hunk_revert_undoes_only_that_hunk() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-hunk");
        std::fs::write(tree.join("long.txt"), lines(40, "line")).unwrap();
        run(&tree, &["git", "add", "long.txt"]);
        run(&tree, &["git", "commit", "-q", "-m", "long"]);
        let base = snapshot_tree(&tree).await.unwrap();
        let edited = lines(40, "line")
            .replace("line 2\n", "line two\n")
            .replace("line 35\n", "line thirty-five\n");
        std::fs::write(tree.join("long.txt"), &edited).unwrap();
        let to = snapshot_tree(&tree).await.unwrap();
        let second = shown_hunk(&tree, &base, &to, "long.txt", 1).await;
        assert!(second.contains("+line thirty-five"), "{second}");

        revert_change(
            &tree,
            &base,
            &to,
            "long.txt",
            Some(HunkSelector {
                index: 1,
                text: &second,
            }),
        )
        .await
        .unwrap();

        let now = read(&tree.join("long.txt")).unwrap();
        assert!(now.contains("line two\n"), "the other hunk stays: {now}");
        assert!(
            now.contains("line 35\n"),
            "the chosen hunk is undone: {now}"
        );
        assert!(!now.contains("thirty-five"));
    }

    #[tokio::test]
    async fn a_hunk_that_changed_since_it_was_shown_is_refused() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-stale");
        let from = snapshot_tree(&tree).await.unwrap();
        std::fs::write(tree.join("README.md"), "the view showed this\n").unwrap();
        let shown_to = snapshot_tree(&tree).await.unwrap();
        let shown = shown_hunk(&tree, &from, &shown_to, "README.md", 0).await;
        std::fs::write(tree.join("README.md"), "then someone typed this\n").unwrap();
        let to = snapshot_tree(&tree).await.unwrap();

        let err = revert_change(
            &tree,
            &from,
            &to,
            "README.md",
            Some(HunkSelector {
                index: 0,
                text: &shown,
            }),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(
                err,
                CheckpointError::Conflict {
                    kind: "diff_changed",
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(
            read(&tree.join("README.md")).as_deref(),
            Some("then someone typed this\n")
        );
    }

    #[tokio::test]
    async fn a_turn_revert_keeps_later_edits_and_refuses_when_they_overlap() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-turn");
        std::fs::write(tree.join("long.txt"), lines(40, "line")).unwrap();
        let before_turn = snapshot_tree(&tree).await.unwrap();
        let after_turn_text = lines(40, "line").replace("line 3\n", "the turn wrote this\n");
        std::fs::write(tree.join("long.txt"), &after_turn_text).unwrap();
        let after_turn = snapshot_tree(&tree).await.unwrap();

        // A later edit far from the turn's change survives the revert.
        std::fs::write(
            tree.join("long.txt"),
            after_turn_text.replace("line 38\n", "a later edit\n"),
        )
        .unwrap();
        revert_change(&tree, &before_turn, &after_turn, "long.txt", None)
            .await
            .unwrap();
        let now = read(&tree.join("long.txt")).unwrap();
        assert!(
            now.contains("line 3\n") && now.contains("a later edit\n"),
            "{now}"
        );

        // A later edit on the turn's own line leaves nothing to revert cleanly.
        std::fs::write(
            tree.join("long.txt"),
            after_turn_text.replace("the turn wrote this\n", "and then I changed it\n"),
        )
        .unwrap();
        let err = revert_change(&tree, &before_turn, &after_turn, "long.txt", None)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CheckpointError::Conflict {
                    kind: "revert_conflict",
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(read(&tree.join("long.txt"))
            .unwrap()
            .contains("and then I changed it\n"));
    }

    #[tokio::test]
    async fn discard_puts_files_back_to_the_last_commit_and_unstages_them() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard");
        std::fs::write(tree.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(tree.join("committed.txt"), "committed\n").unwrap();
        run(&tree, &["git", "add", ".gitignore", "committed.txt"]);
        run(&tree, &["git", "commit", "-q", "-m", "branch work"]);
        std::fs::write(tree.join("README.md"), "uncommitted edit\n").unwrap();
        run(&tree, &["git", "add", "README.md"]);
        std::fs::write(tree.join("staged-new.txt"), "staged\n").unwrap();
        run(&tree, &["git", "add", "staged-new.txt"]);
        std::fs::create_dir_all(tree.join("scratch")).unwrap();
        std::fs::write(tree.join("scratch/untracked.txt"), "untracked\n").unwrap();
        std::fs::write(tree.join("scratch/other.txt"), "stays\n").unwrap();
        std::fs::write(tree.join("debug.log"), "ignored\n").unwrap();
        std::fs::remove_file(tree.join("keep.txt")).unwrap();

        discard_paths(
            &tree,
            &[
                "README.md".to_owned(),
                "staged-new.txt".to_owned(),
                "scratch/untracked.txt".to_owned(),
                "keep.txt".to_owned(),
            ],
        )
        .await
        .unwrap();

        assert_eq!(read(&tree.join("README.md")).as_deref(), Some("hello\n"));
        assert_eq!(read(&tree.join("keep.txt")).as_deref(), Some("keep\n"));
        assert!(!tree.join("staged-new.txt").exists());
        assert!(!tree.join("scratch/untracked.txt").exists());
        assert_eq!(
            read(&tree.join("scratch/other.txt")).as_deref(),
            Some("stays\n"),
            "only the named files go"
        );
        assert_eq!(read(&tree.join("debug.log")).as_deref(), Some("ignored\n"));
        assert_eq!(
            read(&tree.join("committed.txt")).as_deref(),
            Some("committed\n"),
            "committed branch work is not an uncommitted change"
        );
        assert_eq!(
            git_stdout(&tree, &["diff", "--cached", "--name-only"]),
            "",
            "nothing discarded stays staged"
        );
        assert_eq!(
            git_stdout(&tree, &["status", "--porcelain"]),
            "?? scratch/",
            "only the file nobody named is left"
        );
    }

    #[tokio::test]
    async fn discard_refuses_a_path_without_an_uncommitted_change() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard-refused");
        std::fs::create_dir_all(tree.join("scratch")).unwrap();
        std::fs::write(tree.join("scratch/a.txt"), "a\n").unwrap();
        for path in ["README.md", "scratch", "missing.txt"] {
            let err = discard_paths(&tree, &[path.to_owned()]).await.unwrap_err();
            assert!(
                matches!(
                    err,
                    CheckpointError::Conflict {
                        kind: "no_change",
                        ..
                    }
                ),
                "{path}: {err:?}"
            );
        }
        assert_eq!(
            read(&tree.join("scratch/a.txt")).as_deref(),
            Some("a\n"),
            "a directory name never sweeps up the files inside it"
        );
    }

    #[tokio::test]
    async fn uncommitted_paths_name_what_a_commit_would_carry() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "uncommitted");
        std::fs::write(tree.join("branch.txt"), "committed on the branch\n").unwrap();
        run(&tree, &["git", "add", "branch.txt"]);
        run(&tree, &["git", "commit", "-q", "-m", "branch"]);
        std::fs::write(tree.join("README.md"), "edited\n").unwrap();
        std::fs::write(tree.join("new.txt"), "new\n").unwrap();
        std::fs::rename(tree.join("keep.txt"), tree.join("kept.txt")).unwrap();
        let snapshot = snapshot_tree(&tree).await.unwrap();

        let mut listed: Vec<String> = uncommitted_paths(&tree, &snapshot)
            .await
            .unwrap()
            .into_iter()
            .map(|path| path.to_wire())
            .collect();
        listed.sort();
        assert_eq!(listed, ["README.md", "keep.txt", "kept.txt", "new.txt"]);
    }

    #[test]
    fn a_section_splits_at_hunk_headers_only() {
        let raw = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-a\n+@@ not a header\n@@ -9 +9 @@\n-x\n+y\n";
        let section = FileSection::parse(raw);
        assert_eq!(section.header.len(), 3);
        assert_eq!(section.hunks.len(), 2);
        assert_eq!(section.hunks[0].len(), 3);
    }
}

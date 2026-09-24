//! Undo one change in the live worktree: a file's change, one hunk of it, or
//! a file's uncommitted edits.
//!
//! Each works out the tree the worktree should hold next and moves the
//! worktree there through [`super::worktree`], so nothing outside the change
//! moves, nothing the snapshot does not hold is overwritten, and no hook runs.
//!
//! A revert carries the change backwards the way a merge would. The file as
//! the diff left it is the base, the file with the change undone is theirs,
//! and the file as it stands now is ours. A hunk is undone at exactly the
//! lines the diff names, never at another place that happens to read the
//! same. When a later edit overlaps the change, the revert refuses rather
//! than guess.

use std::collections::HashSet;
use std::path::Path;

use tidebreak_harness::OutputBudget;

use super::worktree::{
    apply, inspect, merge_blobs, name_paths, read_blob, refuse_sparse_checkout, tree_entry,
    tree_paths_under, tree_with_changes, unsaved_under, write_blob, ApplyFailure, PrivateIndex,
    TreeEntry, MAX_BLOB_BYTES,
};
use super::{
    complete_nul_terminated_records, git_bytes_bounded, git_bytes_with_literal_paths_bounded,
    parse_name_status, review_diff_args, review_name_status_args, ChangedFile, CheckpointError,
    GitPath, GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES, GIT_SNAPSHOT_TIMEOUT, GIT_TIMEOUT,
    REVIEW_DIFF_FLAGS,
};

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
/// whole-file revert puts the file back to its version at `from`. For a
/// turn's diff, `from` and `to` are the turn's checkpoints, and the revert
/// keeps later edits unless they overlap the turn's change. A renamed file
/// goes back to its old name; one hunk of it changes only those lines.
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
    let old = change
        .previous_path
        .clone()
        .unwrap_or_else(|| change.path.clone());
    let before = file_entry(worktree, from, &old).await?;
    let after = file_entry(worktree, to, &change.path).await?;
    refuse_sparse_checkout(worktree).await?;
    let current = PrivateIndex::new(worktree)
        .await?
        .snapshot(worktree)
        .await?;
    let ours = file_entry(worktree, &current, &change.path).await?;
    if [&before, &after, &ours]
        .into_iter()
        .flatten()
        .any(TreeEntry::is_submodule)
    {
        return Err(CheckpointError::conflict(
            "revert_unsupported",
            "Tidebreak cannot revert a submodule change. Revert it in a terminal.",
        ));
    }
    let edits = match hunk {
        Some(hunk) => {
            let section = file_section(worktree, from, to, &change).await?;
            let lines = section.hunk_matching(hunk)?;
            match (before, after) {
                (Some(_), Some(after)) => {
                    hunk_edits(worktree, &change.path, &lines, after, ours).await?
                }
                // A new or deleted file is one hunk: it goes whole.
                (before, after) => {
                    whole_file_edits(worktree, &change, &old, before, after, ours, &current).await?
                }
            }
        }
        None => whole_file_edits(worktree, &change, &old, before, after, ours, &current).await?,
    };
    let target = tree_with_changes(worktree, &current, &edits).await?;
    if target == current {
        return Err(CheckpointError::conflict(
            "no_change",
            "This change is already undone.",
        ));
    }
    let plan = inspect(worktree, &current, &target)
        .await?
        .into_plan("revert_blocked", "Reverting this change")?;
    apply(worktree, &plan)
        .await
        .map_err(ApplyFailure::into_error)?;
    Ok(RevertedChange {
        paths: edits.into_iter().map(|(path, _)| path).collect(),
    })
}

/// The file `path` names in `tree`: a file, a symlink, or a submodule, never
/// a folder.
async fn file_entry(
    worktree: &Path,
    tree: &str,
    path: &GitPath,
) -> Result<Option<TreeEntry>, CheckpointError> {
    Ok(tree_entry(worktree, tree, path)
        .await?
        .filter(|entry| !entry.is_folder()))
}

fn changed_since() -> CheckpointError {
    CheckpointError::conflict(
        "revert_conflict",
        "This change no longer matches the file, so nothing was reverted. The file changed \
         after the diff you reviewed.",
    )
}

fn diff_changed() -> CheckpointError {
    CheckpointError::conflict(
        "diff_changed",
        "This change is no longer in the diff. Review the diff again.",
    )
}

/// Undo one hunk of a file both sides hold.
async fn hunk_edits(
    worktree: &Path,
    path: &GitPath,
    lines: &[&[u8]],
    after: TreeEntry,
    ours: Option<TreeEntry>,
) -> Result<Vec<(GitPath, Option<TreeEntry>)>, CheckpointError> {
    let ours = ours.ok_or_else(changed_since)?;
    if !after.is_regular_file() || !ours.is_regular_file() {
        return Err(CheckpointError::conflict(
            "revert_unsupported",
            "This is not a text file, so only the whole file can be reverted.",
        ));
    }
    let after_bytes = read_blob(worktree, &after.oid).await?;
    let reverted = reverse_hunk(lines, &after_bytes)?;
    let result = if ours.oid == after.oid {
        reverted
    } else {
        let ours_bytes = read_blob(worktree, &ours.oid).await?;
        merge_blobs(worktree, &ours_bytes, &after_bytes, &reverted)
            .await?
            .ok_or_else(changed_since)?
    };
    let oid = write_blob(worktree, &result).await?;
    Ok(vec![(
        path.clone(),
        Some(TreeEntry {
            mode: ours.mode,
            oid,
        }),
    )])
}

/// Undo a file's whole change: put back what `from` held, carried onto any
/// later edits, under the file's old name.
async fn whole_file_edits(
    worktree: &Path,
    change: &ChangedFile,
    old: &GitPath,
    before: Option<TreeEntry>,
    after: Option<TreeEntry>,
    ours: Option<TreeEntry>,
    current: &str,
) -> Result<Vec<(GitPath, Option<TreeEntry>)>, CheckpointError> {
    let restored = match (before, after) {
        // The change added the file, so reverting it removes it. Only as the
        // change left it: later edits to it would go too.
        (None, after) => {
            if ours.is_none() {
                return Err(CheckpointError::conflict(
                    "no_change",
                    "This change is already undone.",
                ));
            }
            if ours != after {
                return Err(CheckpointError::conflict(
                    "revert_conflict",
                    format!(
                        "{} changed after this change was made. Reverting it would delete those \
                         edits too, so nothing was reverted.",
                        change.path.to_wire()
                    ),
                ));
            }
            None
        }
        // The change deleted the file, so it comes back, unless something
        // else stands there now.
        (Some(before), None) => {
            if ours.is_some() {
                return Err(CheckpointError::conflict(
                    "revert_conflict",
                    format!(
                        "A file stands at {} again, so nothing was reverted. Move it, then try \
                         again.",
                        change.path.to_wire()
                    ),
                ));
            }
            Some(before)
        }
        (Some(before), Some(after)) => {
            let ours = ours.ok_or_else(changed_since)?;
            if ours == after {
                Some(before)
            } else if before.is_regular_file() && after.is_regular_file() && ours.is_regular_file()
            {
                let ours_bytes = read_blob(worktree, &ours.oid).await?;
                let after_bytes = read_blob(worktree, &after.oid).await?;
                let before_bytes = read_blob(worktree, &before.oid).await?;
                let merged = merge_blobs(worktree, &ours_bytes, &after_bytes, &before_bytes)
                    .await?
                    .ok_or_else(changed_since)?;
                let mode = if ours.mode == after.mode {
                    before.mode
                } else {
                    ours.mode
                };
                Some(TreeEntry {
                    mode,
                    oid: write_blob(worktree, &merged).await?,
                })
            } else {
                return Err(changed_since());
            }
        }
    };
    if change.previous_path.is_none() {
        return Ok(vec![(change.path.clone(), restored)]);
    }
    // The file goes back to its old name, unless a different file took it.
    let standing = file_entry(worktree, current, old).await?;
    if standing.is_some() && standing != restored {
        return Err(CheckpointError::conflict(
            "revert_conflict",
            format!(
                "A file stands at {} again, so nothing was reverted. Move it, then try again.",
                old.to_wire()
            ),
        ));
    }
    Ok(vec![(change.path.clone(), None), (old.clone(), restored)])
}

/// `after` with one hunk of its diff put back, at exactly the lines the hunk
/// names. Anything else there means the diff moved on.
fn reverse_hunk(lines: &[&[u8]], after: &[u8]) -> Result<Vec<u8>, CheckpointError> {
    let (header, body) = lines.split_first().ok_or_else(diff_changed)?;
    let ((_, old_count), (new_start, new_count)) =
        parse_hunk_header(header).ok_or_else(diff_changed)?;
    let mut old_side: Vec<Vec<u8>> = Vec::new();
    let mut new_side: Vec<Vec<u8>> = Vec::new();
    let mut last = None;
    for line in body {
        let Some((&marker, content)) = line.split_first() else {
            return Err(diff_changed());
        };
        let mut owned = content.to_vec();
        owned.push(b'\n');
        match marker {
            b' ' => {
                old_side.push(owned.clone());
                new_side.push(owned);
            }
            b'-' => old_side.push(owned),
            b'+' => new_side.push(owned),
            // "\ No newline at end of file" belongs to the line before it.
            b'\\' => {
                let strip = |side: &mut Vec<Vec<u8>>| {
                    if let Some(line) = side.last_mut() {
                        line.pop();
                    }
                };
                match last {
                    Some(b' ') => {
                        strip(&mut old_side);
                        strip(&mut new_side);
                    }
                    Some(b'-') => strip(&mut old_side),
                    Some(b'+') => strip(&mut new_side),
                    _ => return Err(diff_changed()),
                }
                continue;
            }
            _ => return Err(diff_changed()),
        }
        last = Some(marker);
    }
    if old_side.len() != old_count || new_side.len() != new_count {
        return Err(diff_changed());
    }
    let file_lines: Vec<&[u8]> = after.split_inclusive(|byte| *byte == b'\n').collect();
    // `+c,0` names the gap after line `c`; any other range starts at line `c`.
    let start = if new_count == 0 {
        new_start
    } else {
        new_start.checked_sub(1).ok_or_else(diff_changed)?
    };
    let end = start + new_count;
    let placed = file_lines
        .get(start..end)
        .is_some_and(|found| found.iter().copied().eq(new_side.iter().map(Vec::as_slice)));
    if !placed {
        return Err(diff_changed());
    }
    let mut out = Vec::with_capacity(after.len());
    for line in &file_lines[..start] {
        out.extend_from_slice(line);
    }
    for line in &old_side {
        out.extend_from_slice(line);
    }
    for line in &file_lines[end..] {
        out.extend_from_slice(line);
    }
    Ok(out)
}

/// The two ranges of an `@@ -a,b +c,d @@` line, counts defaulting to 1.
fn parse_hunk_header(line: &[u8]) -> Option<((usize, usize), (usize, usize))> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |value: &str| -> Option<(usize, usize)> {
        match value.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((value.parse().ok()?, 1)),
        }
    };
    Some((range(old)?, range(new)?))
}

/// Put each named file back to the last commit: its content and mode when
/// `HEAD` holds it, gone when it does not. The user's index follows, so a
/// discarded change is not left staged for the next commit.
///
/// A discard touches exactly the paths it names and nothing else. It never
/// pairs a rename on its own: the Changes list pairs them against the base
/// branch, and a renamed file's row names both of its paths. A named path
/// that already matches the last commit, such as the old name of a rename
/// the branch already committed, is left as it is; at least one must have
/// something to discard. When a folder, or a file nobody named, stands where
/// a committed file goes, the discard refuses and names it. `expected_tree`
/// is the snapshot the person reviewed; a named file that changed since is
/// left alone.
pub async fn discard_paths(
    worktree: &Path,
    paths: &[String],
    expected_tree: Option<&str>,
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
    refuse_sparse_checkout(worktree).await?;
    let current = PrivateIndex::new(worktree)
        .await?
        .snapshot(worktree)
        .await?;
    let uncommitted = uncommitted_paths(worktree, &current).await?;
    let picked: Vec<GitPath> = wanted
        .iter()
        .filter(|path| uncommitted.contains(*path))
        .cloned()
        .collect();
    if picked.is_empty() {
        return Err(CheckpointError::conflict(
            "no_change",
            format!(
                "{} {} no uncommitted change to discard.",
                name_paths(&wanted),
                if wanted.len() == 1 { "has" } else { "have" }
            ),
        ));
    }
    if let Some(expected) = expected_tree {
        for path in &picked {
            if tree_entry(worktree, expected, path).await?
                != tree_entry(worktree, &current, path).await?
            {
                return Err(CheckpointError::conflict(
                    "worktree_changed",
                    format!(
                        "{} changed after you reviewed it, so nothing was discarded. Review the \
                         changes again.",
                        path.to_wire()
                    ),
                ));
            }
        }
    }

    let mut edits = Vec::new();
    for path in &picked {
        let committed = file_entry(worktree, "HEAD", path).await?;
        let now = file_entry(worktree, &current, path).await?;
        if committed
            .iter()
            .chain(now.iter())
            .any(TreeEntry::is_submodule)
        {
            return Err(CheckpointError::conflict(
                "revert_unsupported",
                "Tidebreak cannot discard a submodule change. Discard it in a terminal.",
            ));
        }
        if committed.is_some() {
            refuse_folder_in_the_way(worktree, &current, path, &picked).await?;
        }
        edits.push((path.clone(), committed));
    }
    let target = tree_with_changes(worktree, &current, &edits).await?;
    let plan = inspect(worktree, &current, &target)
        .await?
        .into_plan("discard_blocked", "Discarding these changes")?;
    apply(worktree, &plan)
        .await
        .map_err(ApplyFailure::into_error)?;
    run_on_paths(worktree, &["reset", "-q", "HEAD", "--"], &picked).await?;
    Ok(RevertedChange { paths: picked })
}

/// Refuse to put a committed file back where a folder now stands with files
/// in it that nobody named, or where a file stands in place of one of its
/// folders.
async fn refuse_folder_in_the_way(
    worktree: &Path,
    current: &str,
    path: &GitPath,
    picked: &[GitPath],
) -> Result<(), CheckpointError> {
    let held: Vec<GitPath> = tree_paths_under(worktree, current, path).await?;
    let saved: HashSet<GitPath> = held.iter().cloned().collect();
    let mut in_the_way: Vec<GitPath> = held
        .into_iter()
        .filter(|inside| !picked.contains(inside))
        .collect();
    in_the_way.extend(unsaved_under(worktree, path, &saved)?);
    if !in_the_way.is_empty() {
        in_the_way.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        return Err(CheckpointError::conflict(
            "discard_blocked",
            format!(
                "A folder now stands where {} was committed, and it holds files you did not \
                 pick: {}. Move or discard them first, then try again.",
                path.to_wire(),
                name_paths(&in_the_way)
            ),
        ));
    }
    for folder in path.ancestors() {
        let standing = file_entry(worktree, current, &folder).await?;
        if standing.is_some() && !picked.contains(&folder) {
            return Err(CheckpointError::conflict(
                "discard_blocked",
                format!(
                    "A file now stands at {}, where {} needs a folder. Move or discard it first, \
                     then try again.",
                    folder.to_wire(),
                    path.to_wire()
                ),
            ));
        }
    }
    Ok(())
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
pub(super) async fn find_change(
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

/// The file's section of the diff, with both of a renamed file's paths, so
/// git pairs them the way the diff view does.
async fn file_section(
    worktree: &Path,
    from: &str,
    to: &str,
    change: &ChangedFile,
) -> Result<OwnedSection, CheckpointError> {
    let mut paths = vec![change.path.clone()];
    paths.extend(change.previous_path.clone());
    let (raw, truncated) = git_bytes_with_literal_paths_bounded(
        worktree,
        &review_diff_args(from, to),
        &paths,
        GIT_SNAPSHOT_TIMEOUT,
        OutputBudget::head(MAX_BLOB_BYTES * 2, usize::MAX),
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Err(CheckpointError::conflict(
            "change_too_large",
            "This change is too large to revert here. Revert it in a terminal.",
        ));
    }
    Ok(OwnedSection { raw })
}

struct OwnedSection {
    raw: Vec<u8>,
}

impl OwnedSection {
    /// The lines of hunk `hunk.index`, when they read exactly as the person
    /// saw them.
    fn hunk_matching(&self, hunk: HunkSelector<'_>) -> Result<Vec<&[u8]>, CheckpointError> {
        let section = FileSection::parse(&self.raw);
        let shown = hunk.text.strip_suffix('\n').unwrap_or(hunk.text);
        section
            .hunks
            .into_iter()
            .nth(hunk.index)
            .filter(|lines| String::from_utf8_lossy(&lines.join(&b'\n')) == shown)
            .ok_or_else(diff_changed)
    }
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
    #[cfg_attr(not(test), allow(dead_code))]
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
    use super::super::{merge_base, produce_diff, snapshot_tree, DiffBounds};
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

    fn conflict_kind(err: &CheckpointError) -> &'static str {
        match err {
            CheckpointError::Conflict { kind, .. } => kind,
            _ => "not a conflict",
        }
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

        assert_eq!(conflict_kind(&err), "diff_changed", "{err:?}");
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
        assert_eq!(conflict_kind(&err), "revert_conflict", "{err:?}");
        assert!(read(&tree.join("long.txt"))
            .unwrap()
            .contains("and then I changed it\n"));
    }

    /// A renamed file with two edited lines: the diff pairs the rename, so
    /// reverting one hunk undoes those lines under the new name, and the file
    /// is neither deleted nor added under the old name.
    #[tokio::test]
    async fn a_hunk_of_a_renamed_file_reverts_only_its_lines() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-renamed");
        std::fs::write(tree.join("config.yml"), lines(40, "setting")).unwrap();
        run(&tree, &["git", "add", "config.yml"]);
        run(&tree, &["git", "commit", "-q", "-m", "config"]);
        let from = snapshot_tree(&tree).await.unwrap();
        std::fs::remove_file(tree.join("config.yml")).unwrap();
        let renamed = lines(40, "setting")
            .replace("setting 2\n", "setting two\n")
            .replace("setting 30\n", "setting thirty\n");
        std::fs::write(tree.join("config.yaml"), &renamed).unwrap();
        let to = snapshot_tree(&tree).await.unwrap();

        let diff = produce_diff(
            &tree,
            &from,
            &to,
            Some("config.yaml"),
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(
            diff.diff.contains("rename from config.yml"),
            "the diff pairs the rename: {}",
            diff.diff
        );
        let first = shown_hunk(&tree, &from, &to, "config.yaml", 0).await;
        assert!(first.contains("+setting two"), "{first}");
        assert!(!first.contains("+setting thirty"), "{first}");

        let reverted = revert_change(
            &tree,
            &from,
            &to,
            "config.yaml",
            Some(HunkSelector {
                index: 0,
                text: &first,
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            reverted
                .paths
                .iter()
                .map(GitPath::to_wire)
                .collect::<Vec<_>>(),
            ["config.yaml"]
        );
        assert_eq!(
            read(&tree.join("config.yaml")),
            Some(renamed.replace("setting two\n", "setting 2\n")),
            "only the chosen lines go back, under the new name"
        );
        assert!(!tree.join("config.yml").exists());
    }

    /// A turn changed the first of two identical blocks. After a later edit
    /// to that block, the turn's hunk no longer fits where it was, and the
    /// revert refuses instead of undoing the second block, which reads the
    /// same.
    #[tokio::test]
    async fn a_stale_turn_hunk_never_lands_on_another_identical_block() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "revert-identical");
        let block = |middle: &str| format!("fn run() {{\n    {middle}\n}}\n");
        // Both blocks are followed by the same lines, so the turn's hunk,
        // context and all, reads the same at the second block.
        let file = |first: &str, second: &str| {
            format!(
                "{}{}{}{}{}",
                block(first),
                lines(3, "// gap"),
                lines(6, "// middle"),
                block(second),
                lines(3, "// gap")
            )
        };
        std::fs::write(tree.join("blocks.rs"), file("step();", "step_twice();")).unwrap();
        let before_turn = snapshot_tree(&tree).await.unwrap();
        std::fs::write(
            tree.join("blocks.rs"),
            file("step_twice();", "step_twice();"),
        )
        .unwrap();
        let after_turn = snapshot_tree(&tree).await.unwrap();
        let hunk = shown_hunk(&tree, &before_turn, &after_turn, "blocks.rs", 0).await;
        // Later, someone rewrote the block the turn had changed.
        std::fs::write(
            tree.join("blocks.rs"),
            file("step_three_times();", "step_twice();"),
        )
        .unwrap();

        let err = revert_change(
            &tree,
            &before_turn,
            &after_turn,
            "blocks.rs",
            Some(HunkSelector {
                index: 0,
                text: &hunk,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(conflict_kind(&err), "revert_conflict", "{err:?}");
        assert_eq!(
            read(&tree.join("blocks.rs")),
            Some(file("step_three_times();", "step_twice();")),
            "the second block, which the turn never touched, stays"
        );
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
            None,
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
            let err = discard_paths(&tree, &[path.to_owned()], None)
                .await
                .unwrap_err();
            assert_eq!(conflict_kind(&err), "no_change", "{path}: {err:?}");
        }
        assert_eq!(
            read(&tree.join("scratch/a.txt")).as_deref(),
            Some("a\n"),
            "a directory name never sweeps up the files inside it"
        );
    }

    /// A sparse checkout's snapshot cannot tell a file outside the cone from
    /// a deleted one, so an undo refuses before it takes one, and an
    /// untracked file outside the cone stays.
    #[tokio::test]
    async fn an_undo_refuses_a_sparse_checkout() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "sparse");
        run(&tree, &["git", "sparse-checkout", "set", "docs"]);
        std::fs::write(tree.join("README.md"), "edited\n").unwrap();
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/outside.txt"), "outside the cone\n").unwrap();

        let err = discard_paths(&tree, &["README.md".to_owned()], None)
            .await
            .unwrap_err();
        assert_eq!(conflict_kind(&err), "sparse_checkout", "{err:?}");
        assert_eq!(read(&tree.join("README.md")).as_deref(), Some("edited\n"));
        assert_eq!(
            read(&tree.join("src/outside.txt")).as_deref(),
            Some("outside the cone\n")
        );
    }

    /// A committed file `config` was replaced by a folder holding a new file
    /// and an ignored one. Discarding `config` would delete that folder, so
    /// it refuses and names what is inside.
    #[tokio::test]
    async fn discard_refuses_when_a_folder_stands_where_the_file_was() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard-folder");
        std::fs::write(tree.join(".gitignore"), "*.local\n").unwrap();
        std::fs::write(tree.join("config"), "committed config\n").unwrap();
        run(&tree, &["git", "add", ".gitignore", "config"]);
        run(&tree, &["git", "commit", "-q", "-m", "config"]);
        std::fs::remove_file(tree.join("config")).unwrap();
        std::fs::create_dir_all(tree.join("config")).unwrap();
        std::fs::write(tree.join("config/app.py"), "print('new')\n").unwrap();
        std::fs::write(tree.join("config/dev.local"), "mine\n").unwrap();

        let err = discard_paths(&tree, &["config".to_owned()], None)
            .await
            .unwrap_err();

        let CheckpointError::Conflict {
            kind: "discard_blocked",
            message,
        } = &err
        else {
            panic!("{err:?}");
        };
        assert!(
            message.contains("config/app.py") && message.contains("config/dev.local"),
            "{message}"
        );
        assert_eq!(
            read(&tree.join("config/app.py")).as_deref(),
            Some("print('new')\n")
        );
        assert_eq!(
            read(&tree.join("config/dev.local")).as_deref(),
            Some("mine\n")
        );
    }

    /// A renamed file's row in the Changes list names both of its paths.
    /// Discarding it puts the file back under its committed name; when the
    /// branch already committed the rename, only the later edit goes and the
    /// old name, which has nothing to discard, is left alone.
    #[tokio::test]
    async fn discarding_a_renamed_row_touches_exactly_its_two_paths() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard-renamed");
        let notes = lines(20, "note");
        std::fs::write(tree.join("notes.txt"), &notes).unwrap();
        run(&tree, &["git", "add", "notes.txt"]);
        run(&tree, &["git", "commit", "-q", "-m", "notes"]);
        let row = ["notes.md".to_owned(), "notes.txt".to_owned()];
        // An uncommitted rename with an edit.
        std::fs::remove_file(tree.join("notes.txt")).unwrap();
        std::fs::write(tree.join("notes.md"), format!("{notes}and more\n")).unwrap();
        let discarded = discard_paths(&tree, &row, None).await.unwrap();
        let mut named: Vec<String> = discarded.paths.iter().map(GitPath::to_wire).collect();
        named.sort();
        assert_eq!(named, ["notes.md", "notes.txt"]);
        assert_eq!(read(&tree.join("notes.txt")), Some(notes.clone()));
        assert!(!tree.join("notes.md").exists());

        // A rename committed on the branch, then edited.
        run(&tree, &["git", "mv", "notes.txt", "notes.md"]);
        run(&tree, &["git", "commit", "-q", "-m", "rename"]);
        std::fs::write(tree.join("notes.md"), format!("{notes}edited after\n")).unwrap();
        let discarded = discard_paths(&tree, &row, None).await.unwrap();
        assert_eq!(
            discarded
                .paths
                .iter()
                .map(GitPath::to_wire)
                .collect::<Vec<_>>(),
            ["notes.md"]
        );
        assert_eq!(read(&tree.join("notes.md")), Some(notes));
        assert!(!tree.join("notes.txt").exists());
    }

    /// The branch rewrote `a.txt`, then someone deleted it and saved an
    /// edited copy as `b.txt`. The Changes list shows two rows: `a.txt`
    /// deleted and `b.txt` added. Against the last commit git would pair them
    /// as a rename, so a discard that paired on its own would delete `b.txt`,
    /// which nobody picked.
    #[tokio::test]
    async fn discard_never_touches_a_path_nobody_named() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard-unnamed");
        let rewritten = lines(20, "rewritten on the branch");
        std::fs::write(tree.join("a.txt"), &rewritten).unwrap();
        run(&tree, &["git", "add", "a.txt"]);
        run(&tree, &["git", "commit", "-q", "-m", "rewrite a"]);
        std::fs::remove_file(tree.join("a.txt")).unwrap();
        let copy = format!("{rewritten}one more line\n");
        std::fs::write(tree.join("b.txt"), &copy).unwrap();

        let discarded = discard_paths(&tree, &["a.txt".to_owned()], None)
            .await
            .unwrap();

        assert_eq!(
            discarded
                .paths
                .iter()
                .map(GitPath::to_wire)
                .collect::<Vec<_>>(),
            ["a.txt"]
        );
        assert_eq!(read(&tree.join("a.txt")), Some(rewritten));
        assert_eq!(
            read(&tree.join("b.txt")),
            Some(copy),
            "b.txt was not picked, so it stays"
        );
    }

    #[tokio::test]
    async fn discard_leaves_a_file_that_changed_after_the_review() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "discard-reviewed");
        std::fs::write(tree.join("README.md"), "reviewed\n").unwrap();
        let reviewed = snapshot_tree(&tree).await.unwrap();
        std::fs::write(tree.join("README.md"), "changed after the review\n").unwrap();

        let err = discard_paths(&tree, &["README.md".to_owned()], Some(&reviewed))
            .await
            .unwrap_err();

        assert_eq!(conflict_kind(&err), "worktree_changed", "{err:?}");
        assert_eq!(
            read(&tree.join("README.md")).as_deref(),
            Some("changed after the review\n")
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

    #[test]
    fn a_hunk_reverses_at_its_own_lines_with_or_without_a_final_newline() {
        let hunk: Vec<&[u8]> = vec![
            b"@@ -2,2 +2,2 @@",
            b" b",
            b"-c",
            b"\\ No newline at end of file",
            b"+C",
        ];
        assert_eq!(reverse_hunk(&hunk, b"a\nb\nC\n").unwrap(), b"a\nb\nc");
        // The same text one line lower is not the hunk's place.
        assert_eq!(
            conflict_kind(&reverse_hunk(&hunk, b"x\na\nb\nC\n").unwrap_err()),
            "diff_changed"
        );
        let insertion: Vec<&[u8]> = vec![b"@@ -1,0 +2 @@", b"+inserted"];
        assert_eq!(
            reverse_hunk(&insertion, b"a\ninserted\nb\n").unwrap(),
            b"a\nb\n"
        );
        let removal: Vec<&[u8]> = vec![b"@@ -2 +1,0 @@", b"-gone"];
        assert_eq!(reverse_hunk(&removal, b"a\nb\n").unwrap(), b"a\ngone\nb\n");
    }
}

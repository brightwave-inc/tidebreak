//! Change the live worktree the way git's own checkout does, from a snapshot.
//!
//! Restore, revert, and discard share one shape. Each snapshots the worktree
//! into a private index, works out the tree the worktree should hold next,
//! and has git move the worktree from the snapshot to that tree with a
//! two-tree `read-tree -m -u`. Git checks every path before it writes one.
//! It refuses when a file changed after the snapshot, and when an untracked
//! or ignored file, or a folder holding one, stands where a file must go. So
//! nothing the snapshot does not hold is overwritten or removed, and the
//! snapshot is always enough to put the worktree back.
//!
//! Before git runs, [`find_blockers`] names what is in the way, so a refusal
//! can say which files to move. When git stops partway anyway, a full disk or
//! a killed process, [`Switch::run`] puts back every path git wrote.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tidebreak_harness::OutputBudget;
use tokio::time::Instant;

use super::{
    checkpoint_index_path_before, git_bytes_bounded, git_bytes_with_literal_paths_bounded,
    git_command, git_text, git_text_env, run_git_command, run_git_command_bounded, CheckpointError,
    GitPath, GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES, GIT_SNAPSHOT_TIMEOUT, GIT_TIMEOUT,
};

/// How long one checkout that rewrites the worktree may run. A restore can
/// touch every file in a large repository, and a checkout stopped halfway is
/// worse than a slow one.
pub(super) const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(600);

/// The largest file content a revert reads or merges.
pub(super) const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;

/// How much `diff-tree` output a blocker scan or a rollback reads. Past this,
/// git's own checks still refuse what is in the way; only the names are lost.
const SCAN_BYTES: usize = 64 * 1024 * 1024;
const SCAN_LINES: usize = 4_000_000;

/// How many entries of a folder that must become a file are read before the
/// whole folder counts as in the way.
const MAX_FOLDER_WALK: usize = 10_000;

/// How many paths a refusal names before it says how many more there are.
pub(super) const NAMED_PATHS: usize = 10;

/// One entry of a tree: a file, a symlink, a folder, or a submodule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeEntry {
    pub mode: String,
    pub oid: String,
}

impl TreeEntry {
    pub fn is_regular_file(&self) -> bool {
        self.mode == "100644" || self.mode == "100755"
    }

    pub fn is_submodule(&self) -> bool {
        self.mode == "160000"
    }

    /// The tree holds this path as a folder.
    pub fn is_folder(&self) -> bool {
        self.mode == "040000"
    }
}

/// The entry `path` names in `tree`, or `None` when the tree does not hold it.
pub(super) async fn tree_entry(
    worktree: &Path,
    tree: &str,
    path: &GitPath,
) -> Result<Option<TreeEntry>, CheckpointError> {
    let (raw, _) = git_bytes_with_literal_paths_bounded(
        worktree,
        &["ls-tree", "-z", "--full-tree", tree, "--"],
        std::slice::from_ref(path),
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        if &record[tab + 1..] != path.as_bytes() {
            continue;
        }
        let mut fields = record[..tab].split(|byte| *byte == b' ');
        let (Some(mode), Some(_kind), Some(oid)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        return Ok(Some(TreeEntry {
            mode: String::from_utf8_lossy(mode).into_owned(),
            oid: String::from_utf8_lossy(oid).into_owned(),
        }));
    }
    Ok(None)
}

/// Every file path `tree` holds inside the folder `folder`.
pub(super) async fn tree_paths_under(
    worktree: &Path,
    tree: &str,
    folder: &GitPath,
) -> Result<Vec<GitPath>, CheckpointError> {
    let (raw, truncated) = git_bytes_with_literal_paths_bounded(
        worktree,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "--full-tree",
            tree,
            "--",
        ],
        std::slice::from_ref(folder),
        GIT_TIMEOUT,
        OutputBudget::head(SCAN_BYTES, SCAN_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    let prefix = folder.child_prefix();
    Ok(super::complete_nul_terminated_records(&raw, truncated)
        .split(|byte| *byte == 0)
        .filter(|name| name.starts_with(&prefix))
        .map(GitPath::from_bytes)
        .collect())
}

/// `base` with each path set to an entry, or removed. Removals go first, so
/// a file can take the place of a folder the same change empties.
pub(super) async fn tree_with_changes(
    worktree: &Path,
    base: &str,
    changes: &[(GitPath, Option<TreeEntry>)],
) -> Result<String, CheckpointError> {
    let index = TempIndex::new()?;
    let env = index.env();
    git_text_env(worktree, &["read-tree", base], &env, GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    let removals = changes.iter().filter(|(_, entry)| entry.is_none());
    let additions = changes.iter().filter(|(_, entry)| entry.is_some());
    for (path, entry) in removals.chain(additions) {
        let mut command = git_command(worktree);
        command.env("GIT_INDEX_FILE", &index.path);
        match entry {
            Some(entry) => {
                command
                    .args([
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &entry.mode,
                        &entry.oid,
                    ])
                    .arg(path.to_os_string().map_err(CheckpointError::internal)?);
            }
            None => {
                command
                    .args(["update-index", "--force-remove", "--"])
                    .arg(path.to_os_string().map_err(CheckpointError::internal)?);
            }
        }
        run_git_command(command, "update-index <1 path>".to_owned(), GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?;
    }
    git_text_env(worktree, &["write-tree"], &env, GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)
}

/// A file's content as the repository stores it.
pub(super) async fn read_blob(worktree: &Path, oid: &str) -> Result<Vec<u8>, CheckpointError> {
    let (bytes, truncated) = git_bytes_bounded(
        worktree,
        &["cat-file", "blob", oid],
        GIT_TIMEOUT,
        OutputBudget::head(MAX_BLOB_BYTES, usize::MAX),
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Err(CheckpointError::conflict(
            "change_too_large",
            "This file is too large to revert here. Revert it in a terminal.",
        ));
    }
    Ok(bytes)
}

/// Store `bytes` as a blob, as they are, and return its id.
pub(super) async fn write_blob(worktree: &Path, bytes: &[u8]) -> Result<String, CheckpointError> {
    let file = temp_file_with(bytes)?;
    let mut command = git_command(worktree);
    command
        .args(["hash-object", "-w", "--no-filters", "--"])
        .arg(file.path());
    let oid = run_git_command(command, "hash-object -w".to_owned(), GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    Ok(String::from_utf8_lossy(&oid).trim().to_owned())
}

/// Carry the change from `base` to `theirs` into `ours`, the way a merge
/// does. `None` when the two changes overlap or the files are not text.
pub(super) async fn merge_blobs(
    worktree: &Path,
    ours: &[u8],
    base: &[u8],
    theirs: &[u8],
) -> Result<Option<Vec<u8>>, CheckpointError> {
    let ours = temp_file_with(ours)?;
    let base = temp_file_with(base)?;
    let theirs = temp_file_with(theirs)?;
    let mut command = git_command(worktree);
    command
        .args(["merge-file", "-p", "--quiet", "--"])
        .arg(ours.path())
        .arg(base.path())
        .arg(theirs.path());
    let output = super::git_runner::wait_command_bounded(
        &mut command,
        GIT_TIMEOUT,
        OutputBudget::head(MAX_BLOB_BYTES, usize::MAX),
        super::git_runner::default_stderr_budget(),
        "git merge-file",
    )
    .await
    .map_err(|err| CheckpointError::internal(err.into_message("git merge-file")))?;
    // 0 is a clean merge; a positive status counts conflicts; anything else,
    // a binary file among them, is an error. Only a clean merge is used.
    if output.status.code() == Some(0) && !output.stdout.truncated {
        Ok(Some(output.stdout.bytes))
    } else {
        Ok(None)
    }
}

/// One change between two trees, as `diff-tree --raw` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawChange {
    pub path: GitPath,
    /// The entry `from` holds, when it holds one.
    pub before: Option<TreeEntry>,
    /// The entry `to` holds, when it holds one.
    pub after: Option<TreeEntry>,
}

/// Every file whose entry differs between two trees, renames split into a
/// removal and an addition. `None` when the list was too long to read whole.
pub(super) async fn raw_changes(
    worktree: &Path,
    from: &str,
    to: &str,
) -> Result<Option<Vec<RawChange>>, CheckpointError> {
    let (raw, truncated) = git_bytes_bounded(
        worktree,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            from,
            to,
        ],
        GIT_TIMEOUT,
        OutputBudget::head(SCAN_BYTES, SCAN_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Ok(None);
    }
    let mut changes = Vec::new();
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());
    while let Some(header) = parts.next() {
        let Some(path) = parts.next() else {
            break;
        };
        // `:<old mode> <new mode> <old oid> <new oid> <status>`
        let header = header.strip_prefix(b":").unwrap_or(header);
        let fields: Vec<&[u8]> = header.split(|byte| *byte == b' ').collect();
        let [old_mode, new_mode, old_oid, new_oid, ..] = fields[..] else {
            continue;
        };
        let entry = |mode: &[u8], oid: &[u8]| {
            (mode.iter().any(|byte| *byte != b'0')).then(|| TreeEntry {
                mode: String::from_utf8_lossy(mode).into_owned(),
                oid: String::from_utf8_lossy(oid).into_owned(),
            })
        };
        changes.push(RawChange {
            path: GitPath::from_bytes(path),
            before: entry(old_mode, old_oid),
            after: entry(new_mode, new_oid),
        });
    }
    Ok(Some(changes))
}

/// What in the worktree would be overwritten or removed on the way from the
/// snapshot `from` to `to`, although `from` does not hold it.
///
/// That is an ignored file where `to` puts a file, a file where `to` needs a
/// folder, and a folder that must become a file while it holds anything the
/// snapshot does not. No snapshot can bring any of those back, so a change
/// that would touch one is refused, naming it.
pub(super) async fn find_blockers(
    worktree: &Path,
    from: &str,
    to: &str,
) -> Result<Vec<GitPath>, CheckpointError> {
    let Some(changes) = raw_changes(worktree, from, to).await? else {
        // Too many changes to name. Git's own checks still refuse them.
        return Ok(Vec::new());
    };
    let removed: HashSet<&GitPath> = changes
        .iter()
        .filter(|change| change.after.is_none())
        .map(|change| &change.path)
        .collect();
    let mut blockers: Vec<GitPath> = Vec::new();
    for change in changes.iter().filter(|change| change.before.is_none()) {
        let path = &change.path;
        // A folder on the way to the new file must be a folder.
        let mut on_the_way = Vec::new();
        for ancestor in path.ancestors() {
            match symlink_metadata(worktree, &ancestor) {
                Ok(Some(meta)) if meta.is_dir() => continue,
                Ok(Some(_)) => {
                    if !removed.contains(&ancestor) {
                        on_the_way.push(ancestor);
                    }
                    break;
                }
                Ok(None) => break,
                Err(err) => return Err(err),
            }
        }
        blockers.extend(on_the_way);
        match symlink_metadata(worktree, path)? {
            None => {}
            Some(meta) if meta.is_dir() => {
                let saved: HashSet<GitPath> = tree_paths_under(worktree, from, path)
                    .await?
                    .into_iter()
                    .collect();
                blockers.extend(unsaved_under(worktree, path, &saved)?);
            }
            Some(_) => blockers.push(path.clone()),
        }
    }
    blockers.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    blockers.dedup();
    Ok(blockers)
}

/// Everything under the folder `folder` that `saved` does not hold, or the
/// folder itself when it holds too much to read.
pub(super) fn unsaved_under(
    worktree: &Path,
    folder: &GitPath,
    saved: &HashSet<GitPath>,
) -> Result<Vec<GitPath>, CheckpointError> {
    // Only a real folder has anything under it; a symlink to one does not.
    if !symlink_metadata(worktree, folder)?.is_some_and(|meta| meta.is_dir()) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    let mut pending = vec![folder.clone()];
    let mut seen = 0usize;
    while let Some(dir) = pending.pop() {
        let full = worktree.join(dir.to_os_string().map_err(CheckpointError::internal)?);
        let entries = match std::fs::read_dir(&full) {
            Ok(entries) => entries,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(err) => {
                return Err(CheckpointError::internal(format!(
                    "could not read {}: {err}",
                    dir.to_wire()
                )))
            }
        };
        for entry in entries {
            let entry = entry.map_err(|err| CheckpointError::internal(err.to_string()))?;
            seen += 1;
            if seen > MAX_FOLDER_WALK {
                return Ok(vec![folder.clone()]);
            }
            let child = dir.join_name(&entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|err| CheckpointError::internal(err.to_string()))?;
            if file_type.is_dir() {
                pending.push(child);
            } else if !saved.contains(&child) {
                found.push(child);
            }
        }
    }
    Ok(found)
}

/// The refusal for a change that would overwrite or remove unsaved files.
pub(super) fn blocked(kind: &'static str, action: &str, paths: &[GitPath]) -> CheckpointError {
    CheckpointError::conflict(
        kind,
        format!(
            "{action} would overwrite or remove files that Tidebreak cannot bring back: {}. \
             Move them out of the way, then try again.",
            name_paths(paths)
        ),
    )
}

/// A short, bounded list of paths for a sentence.
pub(super) fn name_paths(paths: &[GitPath]) -> String {
    let mut named: Vec<String> = paths
        .iter()
        .take(NAMED_PATHS)
        .map(GitPath::to_wire)
        .collect();
    if paths.len() > NAMED_PATHS {
        named.push(format!("and {} more", paths.len() - NAMED_PATHS));
    }
    named.join(", ")
}

/// How a two-tree checkout ended when it did not finish.
#[derive(Debug)]
pub(super) enum SwitchFailure {
    /// Git wrote nothing. The worktree most likely changed after the
    /// snapshot, and git refused rather than overwrite that change.
    Unchanged,
    /// Git stopped partway, and every path it wrote went back.
    RolledBack(String),
    /// Git stopped partway, and some paths it wrote could not go back.
    Partial(String),
}

impl SwitchFailure {
    /// The error a caller reports when it has nothing better to say.
    pub fn into_error(self) -> CheckpointError {
        match self {
            Self::Unchanged => CheckpointError::conflict(
                "worktree_changed",
                "The workspace changed while Tidebreak was changing it, so nothing changed. \
                 Review it again.",
            ),
            Self::RolledBack(reason) => CheckpointError::conflict(
                "worktree_write_failed",
                format!(
                    "Git could not finish writing the files, so Tidebreak put them back. Nothing \
                     changed. {reason}"
                ),
            ),
            Self::Partial(reason) => CheckpointError::internal(format!(
                "Git stopped partway and some files could not be put back: {reason}"
            )),
        }
    }
}

/// Move the worktree from the snapshot `from`, which `index` holds, to `to`.
pub(super) struct Switch<'a> {
    pub index: &'a PrivateIndex,
    pub from: &'a str,
    pub to: &'a str,
}

impl Switch<'_> {
    /// Run the checkout. When git stops partway, every path it wrote goes
    /// back to the snapshot, and nothing it did not write is touched.
    pub async fn run(self, worktree: &Path) -> Result<(), SwitchFailure> {
        let Err(reason) = self.index.switch(worktree, self.from, self.to).await else {
            return Ok(());
        };
        tracing::warn!(%reason, "git did not finish changing the worktree");
        match put_back_written(worktree, self.from, self.to).await {
            Ok(PutBack::NothingWritten) => Err(SwitchFailure::Unchanged),
            Ok(PutBack::Restored) => Err(SwitchFailure::RolledBack(reason)),
            Err(err) => Err(SwitchFailure::Partial(format!("{reason}; {err}"))),
        }
    }
}

enum PutBack {
    NothingWritten,
    Restored,
}

/// After a checkout from `from` to `to` stopped partway, put back every path
/// that now holds exactly what `to` holds there. A path that holds anything
/// else was not written by the checkout and stays as it is.
async fn put_back_written(worktree: &Path, from: &str, to: &str) -> Result<PutBack, String> {
    let index = PrivateIndex::new(worktree)
        .await
        .map_err(|err| err.to_string())?;
    let now = index
        .snapshot(worktree)
        .await
        .map_err(|err| err.to_string())?;
    let intended = raw_changes(worktree, from, to)
        .await
        .map_err(|err| err.to_string())?
        .ok_or("too many files changed to put back")?;
    let moved = raw_changes(worktree, from, &now)
        .await
        .map_err(|err| err.to_string())?
        .ok_or("too many files changed to put back")?;
    let moved: HashMap<&GitPath, &RawChange> =
        moved.iter().map(|change| (&change.path, change)).collect();
    let mut changes = Vec::new();
    for change in &intended {
        let Some(now_change) = moved.get(&change.path) else {
            continue;
        };
        if now_change.after == change.after {
            changes.push((change.path.clone(), change.before.clone()));
        }
    }
    if changes.is_empty() {
        return Ok(PutBack::NothingWritten);
    }
    let target = tree_with_changes(worktree, &now, &changes)
        .await
        .map_err(|err| err.to_string())?;
    let in_the_way = find_blockers(worktree, &now, &target)
        .await
        .map_err(|err| err.to_string())?;
    if !in_the_way.is_empty() {
        return Err(format!("{} are in the way", name_paths(&in_the_way)));
    }
    index.switch(worktree, &now, &target).await?;
    Ok(PutBack::Restored)
}

/// An index file of the caller's own, deleted when it drops.
///
/// It starts as a copy of the reusable checkpoint index when there is one, so
/// the snapshot re-hashes only what changed since the last checkpoint. The copy
/// is safe to take while another snapshot writes the original: git replaces an
/// index by renaming a finished file over it, and it marks an entry it could
/// not trust by its stat data, so a copied entry is re-read, never believed.
pub(super) struct PrivateIndex {
    path: PathBuf,
}

impl PrivateIndex {
    pub async fn new(worktree: &Path) -> Result<Self, CheckpointError> {
        let index = Self {
            path: TempIndex::new()?.keep(),
        };
        if let Ok(Some(reusable)) =
            checkpoint_index_path_before(worktree, Instant::now() + GIT_TIMEOUT).await
        {
            if tokio::fs::copy(&reusable, &index.path).await.is_err() {
                let _ = tokio::fs::remove_file(&index.path).await;
            }
        }
        Ok(index)
    }

    /// Snapshot the worktree into this index and return the tree.
    pub async fn snapshot(&self, worktree: &Path) -> Result<String, CheckpointError> {
        match self.try_snapshot(worktree).await {
            Ok(tree) => Ok(tree),
            // A seed git cannot read costs only its stat cache. Start cold.
            Err(_) if self.path.exists() => {
                let _ = tokio::fs::remove_file(&self.path).await;
                self.try_snapshot(worktree).await
            }
            Err(err) => Err(err),
        }
    }

    async fn try_snapshot(&self, worktree: &Path) -> Result<String, CheckpointError> {
        let index = self.path.to_string_lossy();
        let env = [("GIT_INDEX_FILE", index.as_ref())];
        if !self.path.exists() {
            git_text_env(worktree, &["read-tree", "HEAD"], &env, GIT_TIMEOUT)
                .await
                .map_err(CheckpointError::internal)?;
        }
        git_text_env(worktree, &["add", "-A"], &env, GIT_SNAPSHOT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?;
        git_text_env(worktree, &["write-tree"], &env, GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)
    }

    /// Make the worktree go from `from`, the snapshot this index holds, to
    /// `to`.
    ///
    /// A two-tree merge writes each file whose entry changes, removes each
    /// file `to` lacks, and leaves every other file alone, stat data and all,
    /// so a watcher sees only the files that really moved. It refuses, before
    /// it writes anything, when a file changed after the snapshot or when a
    /// file the snapshot does not hold is in the way. No hook runs.
    async fn switch(&self, worktree: &Path, from: &str, to: &str) -> Result<(), String> {
        let mut command = git_command(worktree);
        command
            .env("GIT_INDEX_FILE", &self.path)
            .args(["read-tree", "-m", "-u", from, to]);
        run_git_command_bounded(
            command,
            "read-tree -m -u".to_owned(),
            CHECKOUT_TIMEOUT,
            OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
            true,
        )
        .await
        .map(|_| ())
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        remove_index_files(&self.path);
    }
}

/// A scratch index path that nothing has written yet, removed when it drops.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn new() -> Result<Self, CheckpointError> {
        let temp = tempfile::NamedTempFile::new().map_err(|err| {
            CheckpointError::internal(format!("could not create a temporary index: {err}"))
        })?;
        let path = temp.path().to_path_buf();
        // Git wants to create the index itself; an empty file reads as corrupt.
        drop(temp);
        let _ = std::fs::remove_file(&path);
        Ok(Self { path })
    }

    fn env(&self) -> [(&str, &str); 1] {
        [("GIT_INDEX_FILE", self.path.to_str().unwrap_or_default())]
    }

    /// Hand the path to an owner that removes it itself.
    fn keep(self) -> PathBuf {
        let path = self.path.clone();
        std::mem::forget(self);
        path
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        remove_index_files(&self.path);
    }
}

fn remove_index_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let _ = std::fs::remove_file(PathBuf::from(lock));
}

fn temp_file_with(bytes: &[u8]) -> Result<tempfile::NamedTempFile, CheckpointError> {
    let mut file = tempfile::NamedTempFile::new()
        .map_err(|err| CheckpointError::internal(format!("could not stage a file: {err}")))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(|err| CheckpointError::internal(format!("could not stage a file: {err}")))?;
    Ok(file)
}

/// What stands at `path` in the worktree, without following a symlink.
fn symlink_metadata(
    worktree: &Path,
    path: &GitPath,
) -> Result<Option<std::fs::Metadata>, CheckpointError> {
    let full = worktree.join(path.to_os_string().map_err(CheckpointError::internal)?);
    match std::fs::symlink_metadata(&full) {
        Ok(meta) => Ok(Some(meta)),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(CheckpointError::internal(format!(
            "could not read {}: {err}",
            path.to_wire()
        ))),
    }
}

/// The tree a commit holds.
pub(super) async fn tree_of(worktree: &Path, commit: &str) -> Result<String, CheckpointError> {
    let spec = format!("{commit}^{{tree}}");
    git_text(
        worktree,
        &["rev-parse", "--verify", "--quiet", &spec],
        GIT_TIMEOUT,
    )
    .await
    .ok()
    .filter(|tree| !tree.is_empty())
    .ok_or_else(|| CheckpointError::not_found("That checkpoint is no longer in the repository."))
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{add_worktree, git_stdout, init_repo, run};
    use super::*;

    fn path(value: &str) -> GitPath {
        GitPath::from_bytes(value.as_bytes())
    }

    fn wires(paths: &[GitPath]) -> Vec<String> {
        paths.iter().map(GitPath::to_wire).collect()
    }

    #[tokio::test]
    async fn blockers_name_ignored_files_and_folders_in_the_way() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "blockers");
        std::fs::write(tree.join(".gitignore"), ".env\n*.local\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore"]);
        let index = PrivateIndex::new(&tree).await.unwrap();
        let empty = index.snapshot(&tree).await.unwrap();

        // The target holds `.env`, a file `utils`, and `notes/today.md`.
        std::fs::write(tree.join("utils"), "a file\n").unwrap();
        std::fs::create_dir_all(tree.join("notes")).unwrap();
        std::fs::write(tree.join("notes/today.md"), "note\n").unwrap();
        let env_blob = {
            std::fs::write(tree.join("env.tmp"), "PLACEHOLDER=1\n").unwrap();
            let oid = git_stdout(&tree, &["hash-object", "-w", "env.tmp"]);
            std::fs::remove_file(tree.join("env.tmp")).unwrap();
            oid
        };
        let target = index.snapshot(&tree).await.unwrap();
        let target = tree_with_changes(
            &tree,
            &target,
            &[(
                path(".env"),
                Some(TreeEntry {
                    mode: "100644".into(),
                    oid: env_blob,
                }),
            )],
        )
        .await
        .unwrap();

        // Now: `.env` is ignored and holds a real value, `utils` is a folder
        // holding an ignored file, and `notes` is an ignored file.
        std::fs::remove_file(tree.join("utils")).unwrap();
        std::fs::remove_dir_all(tree.join("notes")).unwrap();
        std::fs::write(tree.join(".env"), "the real value\n").unwrap();
        std::fs::create_dir_all(tree.join("utils")).unwrap();
        std::fs::write(tree.join("utils/settings.local"), "mine\n").unwrap();
        std::fs::write(tree.join("notes.local"), "x\n").unwrap();
        let now = index.snapshot(&tree).await.unwrap();
        assert_eq!(now, empty, "ignored files are in no snapshot");

        let blockers = find_blockers(&tree, &now, &target).await.unwrap();
        assert_eq!(wires(&blockers), [".env", "utils/settings.local"]);
    }

    #[tokio::test]
    async fn a_file_in_place_of_a_needed_folder_is_in_the_way() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "folder-needed");
        std::fs::write(tree.join(".gitignore"), "cache\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore"]);
        let index = PrivateIndex::new(&tree).await.unwrap();
        std::fs::create_dir_all(tree.join("cache")).unwrap();
        let before = index.snapshot(&tree).await.unwrap();
        let blob = git_stdout(&tree, &["hash-object", "-w", ".gitignore"]);
        let target = tree_with_changes(
            &tree,
            &before,
            &[(
                path("cache/entry"),
                Some(TreeEntry {
                    mode: "100644".into(),
                    oid: blob,
                }),
            )],
        )
        .await
        .unwrap();
        std::fs::remove_dir_all(tree.join("cache")).unwrap();
        std::fs::write(tree.join("cache"), "an ignored file\n").unwrap();

        let blockers = find_blockers(&tree, &before, &target).await.unwrap();
        assert_eq!(wires(&blockers), ["cache"]);
    }

    #[tokio::test]
    async fn a_checkout_that_stops_partway_puts_back_only_what_it_wrote() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "partway");
        std::fs::create_dir_all(tree.join("locked")).unwrap();
        std::fs::write(tree.join("locked/present.txt"), "present\n").unwrap();
        std::fs::write(tree.join("a.txt"), "target a\n").unwrap();
        std::fs::write(tree.join("locked/new.txt"), "target new\n").unwrap();
        let index = PrivateIndex::new(&tree).await.unwrap();
        let target = index.snapshot(&tree).await.unwrap();
        std::fs::write(tree.join("a.txt"), "current a\n").unwrap();
        std::fs::remove_file(tree.join("locked/new.txt")).unwrap();
        let current = index.snapshot(&tree).await.unwrap();
        // Git can write `a.txt` but not create a file in `locked/`.
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        std::fs::write(tree.join("elsewhere.txt"), "typed during the checkout\n").unwrap();

        let failure = Switch {
            index: &index,
            from: &current,
            to: &target,
        }
        .run(&tree)
        .await
        .unwrap_err();
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(
                failure,
                SwitchFailure::RolledBack(_) | SwitchFailure::Unchanged
            ),
            "{failure:?}"
        );
        assert_eq!(
            std::fs::read_to_string(tree.join("a.txt")).unwrap(),
            "current a\n",
            "the file git wrote goes back"
        );
        assert_eq!(
            std::fs::read_to_string(tree.join("elsewhere.txt")).unwrap(),
            "typed during the checkout\n",
            "a file git never wrote stays"
        );
        assert!(!tree.join("locked/new.txt").exists());
    }
}

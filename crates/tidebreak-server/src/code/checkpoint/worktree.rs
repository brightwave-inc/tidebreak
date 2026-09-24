//! Change the live worktree one path at a time, from a checked snapshot.
//!
//! Restore, revert, and discard share one shape. Each snapshots the worktree
//! into a private index, works out the tree the worktree should hold next, and
//! turns the difference into a [`Plan`]: every path that moves, what the
//! snapshot holds there, and what the next tree holds.
//!
//! [`inspect`] refuses what no saved state could bring back, and names it: a
//! file the snapshot does not hold where the next tree writes, ignored and
//! excluded files included; a folder holding such a file that must become a
//! file; a nested repository or submodule; two paths that name one file on
//! this disk; and a file that changed since the snapshot. It also stores each
//! file it would replace or remove exactly as its bytes stand, with no
//! filters, because a snapshot holds what git's clean filters made of it and
//! a lossy filter drops the rest.
//!
//! [`apply`] then moves one path at a time, and git writes nothing in the
//! worktree. Each new version is written to a temporary file in the
//! repository's own git folder first, under a short name, so no leftover of a
//! crash lands in the worktree. Right before it moves into place, the path
//! must still hold what the plan saw, or still be empty, or the apply stops.
//! It lands with a rename, or with a hard link where nothing stood, so a path
//! is never missing between its old and new content, and never overwrites a
//! file that appeared. When a path fails, every path already moved goes back
//! to its exact bytes, and the failure says whether each of them was then
//! verified to match.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use tidebreak_harness::OutputBudget;
use tokio::time::Instant;

use super::{
    checkpoint_index_path_before, git_bytes_bounded, git_bytes_with_literal_paths_bounded,
    git_command, git_text, git_text_env, run_git_command, CheckpointError, GitPath,
    GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES, GIT_SNAPSHOT_TIMEOUT, GIT_TIMEOUT,
};

/// How long writing one file's new content may take.
const WRITE_TIMEOUT: Duration = Duration::from_secs(600);

/// The largest file content a revert reads or merges.
pub(super) const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;

/// How much `diff-tree` output a plan reads. A change larger than this is
/// refused rather than applied in part.
const SCAN_BYTES: usize = 64 * 1024 * 1024;
const SCAN_LINES: usize = 4_000_000;

/// How many entries of a folder that must become a file are read before the
/// whole folder counts as in the way.
const MAX_FOLDER_WALK: usize = 10_000;

/// How many paths one `hash-object` call hashes.
const HASH_BATCH: usize = 256;

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

    fn is_executable(&self) -> bool {
        self.mode == "100755"
    }

    fn is_symlink(&self) -> bool {
        self.mode == "120000"
    }

    /// A submodule or a nested repository, which a tree holds only as a
    /// pointer to one commit, never as the files in it.
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

/// A tree holding exactly `entries`, however many there are, in one call.
pub(super) async fn tree_of_entries(
    worktree: &Path,
    entries: &[(GitPath, TreeEntry)],
) -> Result<String, CheckpointError> {
    let index = TempIndex::new()?;
    let mut records = Vec::new();
    for (path, entry) in entries {
        records.extend_from_slice(format!("{} {}\t", entry.mode, entry.oid).as_bytes());
        records.extend_from_slice(path.as_bytes());
        records.push(0);
    }
    let input = temp_file_with(&records)?;
    let stdin = std::fs::File::open(input.path())
        .map_err(|err| CheckpointError::internal(format!("could not stage a file: {err}")))?;
    let mut command = git_command(worktree);
    command
        .env("GIT_INDEX_FILE", &index.path)
        .args(["update-index", "-z", "--index-info"])
        .stdin(Stdio::from(stdin));
    run_git_command(
        command,
        format!("update-index --index-info <{} paths>", entries.len()),
        GIT_SNAPSHOT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    git_text_env(worktree, &["write-tree"], &index.env(), GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)
}

/// Every file entry `tree` holds, by path.
pub(super) async fn tree_entries(
    worktree: &Path,
    tree: &str,
) -> Result<HashMap<GitPath, TreeEntry>, CheckpointError> {
    let (raw, truncated) = git_bytes_bounded(
        worktree,
        &["ls-tree", "-r", "-z", "--full-tree", tree],
        GIT_TIMEOUT,
        OutputBudget::head(SCAN_BYTES, SCAN_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Err(CheckpointError::internal(
            "a saved state lists more files than Tidebreak can read",
        ));
    }
    let mut entries = HashMap::new();
    for record in raw
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let mut fields = record[..tab].split(|byte| *byte == b' ');
        let (Some(mode), Some(_kind), Some(oid)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        entries.insert(
            GitPath::from_bytes(&record[tab + 1..]),
            TreeEntry {
                mode: String::from_utf8_lossy(mode).into_owned(),
                oid: String::from_utf8_lossy(oid).into_owned(),
            },
        );
    }
    Ok(entries)
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

/// One path that moves between two trees, as `diff-tree --raw` reports it.
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

/// What stood at one path when it was checked, so a later look can tell
/// whether anything touched it since. Folders are never stamped: a folder's
/// times move whenever a file in it does.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    kind: StampKind,
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StampKind {
    File,
    Symlink,
    Folder,
    Other,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            StampKind::Symlink
        } else if file_type.is_file() {
            StampKind::File
        } else if file_type.is_dir() {
            StampKind::Folder
        } else {
            StampKind::Other
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                kind,
                len: meta.len(),
                modified: meta.modified().ok(),
                inode: meta.ino(),
                mode: meta.mode(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                kind,
                len: meta.len(),
                modified: meta.modified().ok(),
            }
        }
    }

    fn is_executable(&self) -> bool {
        #[cfg(unix)]
        {
            self.mode & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// Every path one change moves, and what each held when it was checked.
#[derive(Debug)]
pub(super) struct Plan {
    changes: Vec<RawChange>,
    /// What each path the plan replaces or removes held when it was checked.
    stamps: HashMap<GitPath, Stamp>,
    /// Each file the plan replaces or removes, stored exactly as its bytes
    /// stood, with no filters. Putting a path back writes these bytes, never
    /// what a clean filter made of them.
    exact_before: HashMap<GitPath, String>,
    /// Paths whose new version is written as these exact bytes, with no
    /// filters: a saved state brought back as it stood.
    exact_after: HashMap<GitPath, TreeEntry>,
    /// Whether the executable bit counts, as `core.fileMode` says.
    file_mode: bool,
}

impl Plan {
    /// Each file this plan replaces or removes whose exact bytes differ from
    /// what the snapshot holds for it, with the blob that holds those bytes.
    /// A saved state keeps these, or undoing it would lose what a clean
    /// filter dropped.
    pub fn exact_bytes_the_snapshot_lacks(&self) -> Vec<(GitPath, TreeEntry)> {
        self.changes
            .iter()
            .filter_map(|change| {
                let before = change.before.as_ref()?;
                let exact = self.exact_before.get(&change.path)?;
                (*exact != before.oid).then(|| {
                    (
                        change.path.clone(),
                        TreeEntry {
                            mode: before.mode.clone(),
                            oid: exact.clone(),
                        },
                    )
                })
            })
            .collect()
    }

    /// Write these paths' new versions as the exact bytes a saved state
    /// kept for them, instead of through the smudge filters.
    pub fn write_exact(&mut self, saved: HashMap<GitPath, TreeEntry>) {
        for change in &self.changes {
            let (Some(after), Some(exact)) = (&change.after, saved.get(&change.path)) else {
                continue;
            };
            if after.is_regular_file() && exact.mode == after.mode {
                self.exact_after.insert(change.path.clone(), exact.clone());
            }
        }
    }
}

/// What [`inspect`] found before anything moved.
#[derive(Debug)]
pub(super) struct Inspection {
    pub plan: Plan,
    /// Paths the change would overwrite or remove that no saved state could
    /// bring back: files the snapshot does not hold, folders holding them,
    /// and nested repositories and submodules.
    pub blocked: Vec<GitPath>,
    /// Pairs of paths that name one file on this disk, where the change
    /// touches one of them: changing one would change the other.
    pub aliased: Vec<(GitPath, GitPath)>,
    /// Paths that no longer hold what the snapshot holds.
    pub changed: Vec<GitPath>,
}

impl Inspection {
    /// The plan, when nothing stands in its way.
    pub fn into_plan(self, kind: &'static str, action: &str) -> Result<Plan, CheckpointError> {
        if !self.blocked.is_empty() {
            return Err(blocked(kind, action, &self.blocked));
        }
        if let Some((one, other)) = self.aliased.first() {
            return Err(CheckpointError::conflict(
                kind,
                format!(
                    "{} and {} name the same file on this disk, so changing one would change \
                     the other. {action} leaves both alone. Rename or remove one of them in a \
                     terminal, then try again.",
                    one.to_wire(),
                    other.to_wire()
                ),
            ));
        }
        if !self.changed.is_empty() {
            return Err(CheckpointError::conflict(
                "worktree_changed",
                format!(
                    "{} changed while Tidebreak was checking it, so nothing changed. Review it \
                     again.",
                    name_paths(&self.changed)
                ),
            ));
        }
        Ok(self.plan)
    }

    /// Every path a person must deal with before the change can run.
    pub fn in_the_way(&self) -> Vec<GitPath> {
        let mut paths = self.blocked.clone();
        for (one, other) in &self.aliased {
            paths.push(one.clone());
            paths.push(other.clone());
        }
        paths.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        paths.dedup();
        paths
    }
}

/// Check every path the change from the snapshot `from` to `to` moves.
///
/// Nothing is written. What a saved state could not bring back is
/// `blocked`; a path that no longer holds what the snapshot holds is
/// `changed`.
pub(super) async fn inspect(
    worktree: &Path,
    from: &str,
    to: &str,
) -> Result<Inspection, CheckpointError> {
    let changes = raw_changes(worktree, from, to).await?.ok_or_else(|| {
        CheckpointError::conflict(
            "change_too_large",
            "This change touches too many files to apply safely here. Use Git in a terminal.",
        )
    })?;
    let file_mode = core_file_mode(worktree).await;
    let removed: HashSet<&GitPath> = changes
        .iter()
        .filter(|change| change.before.is_some() && change.after.is_none())
        .map(|change| &change.path)
        .collect();

    let mut blocked: Vec<GitPath> = Vec::new();
    for change in &changes {
        // A tree holds a nested repository as one commit id, never its files,
        // so nothing could save what is in it.
        if [&change.before, &change.after]
            .into_iter()
            .flatten()
            .any(TreeEntry::is_submodule)
        {
            blocked.push(change.path.clone());
            continue;
        }
        if change.before.is_some() {
            continue;
        }
        // A new path: every folder on the way must be a folder, or a file
        // this same change removes.
        let mut on_the_way = false;
        for ancestor in change.path.ancestors() {
            match symlink_metadata(worktree, &ancestor)? {
                Some(meta) if meta.file_type().is_dir() => continue,
                Some(_) => {
                    if !removed.contains(&ancestor) {
                        blocked.push(ancestor);
                    }
                    on_the_way = true;
                    break;
                }
                None => break,
            }
        }
        // Past a file or symlink on the way, the path is made fresh once
        // that goes. Looking at it now would follow the symlink out of the
        // worktree.
        if on_the_way {
            continue;
        }
        match symlink_metadata(worktree, &change.path)? {
            None => {}
            Some(meta) if meta.file_type().is_dir() => {
                // A folder where a file goes: everything in it must be a file
                // this change removes.
                let saved: HashSet<GitPath> = removed.iter().map(|path| (*path).clone()).collect();
                blocked.extend(unsaved_under(worktree, &change.path, &saved)?);
            }
            Some(_) if !alias_of_removed(worktree, &change.path, &removed) => {
                blocked.push(change.path.clone());
            }
            Some(_) => {}
        }
    }
    let aliased = find_aliases(worktree, from, &changes).await?;

    // Every path the change replaces or removes must still hold what the
    // snapshot holds. Stamp first, then hash: a change after the stamp shows
    // in the hash, and one after the hash shows in the stamp.
    let mut stamps = HashMap::new();
    let mut changed = Vec::new();
    let mut files: Vec<(GitPath, String)> = Vec::new();
    let mut links: Vec<(GitPath, Vec<u8>, String)> = Vec::new();
    for change in &changes {
        let Some(before) = &change.before else {
            continue;
        };
        if before.is_submodule() {
            continue;
        }
        let Some(meta) = symlink_metadata(worktree, &change.path)? else {
            changed.push(change.path.clone());
            continue;
        };
        let stamp = Stamp::of(&meta);
        let expected = if before.is_symlink() {
            StampKind::Symlink
        } else {
            StampKind::File
        };
        if stamp.kind != expected
            || (file_mode
                && expected == StampKind::File
                && stamp.is_executable() != before.is_executable())
        {
            changed.push(change.path.clone());
            continue;
        }
        if expected == StampKind::Symlink {
            let target = read_link_bytes(worktree, &change.path)?;
            links.push((change.path.clone(), target, before.oid.clone()));
        } else {
            files.push((change.path.clone(), before.oid.clone()));
        }
        stamps.insert(change.path.clone(), stamp);
    }
    let paths: Vec<GitPath> = files.iter().map(|(path, _)| path.clone()).collect();
    for ((path, expected), actual) in files
        .iter()
        .zip(hash_worktree_files(worktree, &paths, Hashing::Cleaned).await?)
    {
        if *expected != actual {
            changed.push(path.clone());
        }
    }
    // The exact bytes too, stored, so putting a file back never depends on
    // what a clean filter kept of it.
    let exact_before: HashMap<GitPath, String> = paths
        .iter()
        .cloned()
        .zip(hash_worktree_files(worktree, &paths, Hashing::ExactAndStored).await?)
        .collect();
    let targets: Vec<Vec<u8>> = links.iter().map(|(_, target, _)| target.clone()).collect();
    for ((path, _, expected), actual) in links.iter().zip(hash_bytes(worktree, &targets).await?) {
        if *expected != actual {
            changed.push(path.clone());
        }
    }

    for list in [&mut blocked, &mut changed] {
        list.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        list.dedup();
    }
    Ok(Inspection {
        plan: Plan {
            changes,
            stamps,
            exact_before,
            exact_after: HashMap::new(),
            file_mode,
        },
        blocked,
        aliased,
        changed,
    })
}

/// Everything under the folder `folder` that `saved` does not hold, or the
/// folder itself when it holds too much to read. A nested repository counts
/// as in the way, named by its root.
pub(super) fn unsaved_under(
    worktree: &Path,
    folder: &GitPath,
    saved: &HashSet<GitPath>,
) -> Result<Vec<GitPath>, CheckpointError> {
    // Only a real folder has anything under it; a symlink to one does not.
    if !symlink_metadata(worktree, folder)?.is_some_and(|meta| meta.file_type().is_dir()) {
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
            if entry.file_name() == ".git" {
                found.push(dir.clone());
                continue;
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

/// Whether the file standing at the new path `path` is really a file this
/// same change removes, reached under another spelling: a disk that ignores
/// case or Unicode normalization finds `README.md` when asked for
/// `readme.md`. That file is not in the way: it goes first.
fn alias_of_removed(worktree: &Path, path: &GitPath, removed: &HashSet<&GitPath>) -> bool {
    let folder = path.folder();
    let Some(on_disk) = folder_names(worktree, folder.as_ref()) else {
        return false;
    };
    if on_disk.contains(path.name()) {
        // The exact name is there: a real file in the way.
        return false;
    }
    removed
        .iter()
        .any(|other| *other != path && other.folder() == folder && same_file(worktree, path, other))
}

/// Pairs of paths that name one file on this disk, where `changes` touches
/// one of them.
///
/// A disk that ignores case or Unicode normalization opens `README.md` when
/// asked for `readme.md`. When `core.ignorecase` or `core.precomposeunicode`
/// says otherwise, git keeps both spellings apart, so a snapshot can hold two
/// paths for one file, and changing one changes the other. A case-only
/// rename, one spelling removed and the other added, is not a problem: the
/// removal goes first.
async fn find_aliases(
    worktree: &Path,
    snapshot: &str,
    changes: &[RawChange],
) -> Result<Vec<(GitPath, GitPath)>, CheckpointError> {
    if changes.is_empty() || !may_hold_aliases(worktree).await {
        return Ok(Vec::new());
    }
    let touched: HashMap<&GitPath, &RawChange> = changes
        .iter()
        .map(|change| (&change.path, change))
        .collect();
    let mut folders: HashMap<Option<GitPath>, (HashSet<GitPath>, bool)> = HashMap::new();
    for change in changes {
        let (claimed, in_snapshot) = folders.entry(change.path.folder()).or_default();
        claimed.insert(change.path.clone());
        *in_snapshot |= change.before.is_some();
    }
    let mut pairs = Vec::new();
    for (folder, (mut claimed, in_snapshot)) in folders {
        if in_snapshot {
            claimed.extend(snapshot_names_in(worktree, snapshot, folder.as_ref()).await?);
        }
        let Some(on_disk) = folder_names(worktree, folder.as_ref()) else {
            continue;
        };
        let (spelled, other): (Vec<&GitPath>, Vec<&GitPath>) = claimed
            .iter()
            .partition(|path| on_disk.contains(path.name()));
        // A path whose exact name is not on disk, but which opens anyway, is
        // another spelling of a name that is.
        for alias in other {
            for exact in &spelled {
                if !same_file(worktree, alias, exact) {
                    continue;
                }
                let (one, two) = (touched.get(alias), touched.get(*exact));
                if (one.is_none() && two.is_none()) || is_rename_pair(one, two) {
                    continue;
                }
                let (first, second) = if alias.as_bytes() <= exact.as_bytes() {
                    (alias.clone(), (*exact).clone())
                } else {
                    ((*exact).clone(), alias.clone())
                };
                pairs.push((first, second));
            }
        }
    }
    pairs.sort_by(|a, b| (a.0.as_bytes(), a.1.as_bytes()).cmp(&(b.0.as_bytes(), b.1.as_bytes())));
    pairs.dedup();
    Ok(pairs)
}

/// One spelling removed and the other added: a case-only rename.
fn is_rename_pair(one: Option<&&RawChange>, two: Option<&&RawChange>) -> bool {
    let removed = |change: &RawChange| change.before.is_some() && change.after.is_none();
    let added = |change: &RawChange| change.before.is_none() && change.after.is_some();
    match (one, two) {
        (Some(one), Some(two)) => (removed(one) && added(two)) || (added(one) && removed(two)),
        _ => false,
    }
}

/// Whether a snapshot of this worktree can hold two spellings of one file:
/// the disk folds names in a way git's config does not.
async fn may_hold_aliases(worktree: &Path) -> bool {
    // The worktree's own `.git`, asked for in capitals.
    let folds_case = std::fs::symlink_metadata(worktree.join(".GIT")).is_ok();
    let folds_unicode = cfg!(target_os = "macos");
    if !folds_case && !folds_unicode {
        return false;
    }
    let setting = |name: &'static str| async move {
        git_text(worktree, &["config", "--bool", "--get", name], GIT_TIMEOUT)
            .await
            .is_ok_and(|value| value.trim() == "true")
    };
    (folds_case && !setting("core.ignorecase").await)
        || (folds_unicode && !setting("core.precomposeunicode").await)
}

/// Every path the snapshot holds directly inside `folder`, the top of the
/// worktree when `None`.
async fn snapshot_names_in(
    worktree: &Path,
    snapshot: &str,
    folder: Option<&GitPath>,
) -> Result<Vec<GitPath>, CheckpointError> {
    let mut spec = OsString::from(format!("{snapshot}:"));
    if let Some(folder) = folder {
        spec.push(folder.to_os_string().map_err(CheckpointError::internal)?);
    }
    let mut command = git_command(worktree);
    command.args(["ls-tree", "-z", "--name-only"]).arg(spec);
    let (raw, truncated) = super::run_git_command_bounded(
        command,
        "ls-tree <folder>".to_owned(),
        GIT_TIMEOUT,
        OutputBudget::head(SCAN_BYTES, SCAN_LINES),
        true,
    )
    .await
    .map_err(CheckpointError::internal)?;
    if truncated {
        return Err(CheckpointError::conflict(
            "change_too_large",
            "A folder this change touches holds too many files to check here. Use Git in a \
             terminal.",
        ));
    }
    Ok(raw
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| match folder {
            Some(folder) => GitPath::from_bytes(&[&folder.child_prefix()[..], name].concat()),
            None => GitPath::from_bytes(name),
        })
        .collect())
}

/// The exact names the disk holds in `folder`, the top of the worktree when
/// `None`. `None` when the folder cannot be read.
fn folder_names(worktree: &Path, folder: Option<&GitPath>) -> Option<HashSet<Vec<u8>>> {
    let full = match folder {
        Some(folder) => full_path(worktree, folder).ok()?,
        None => worktree.to_path_buf(),
    };
    let entries = std::fs::read_dir(full).ok()?;
    Some(
        entries
            .flatten()
            .map(|entry| os_bytes(&entry.file_name()))
            .collect(),
    )
}

fn os_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        name.to_string_lossy().as_bytes().to_vec()
    }
}

/// Whether two paths open one file, without following a symlink.
#[cfg(unix)]
fn same_file(worktree: &Path, one: &GitPath, other: &GitPath) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (
        symlink_metadata(worktree, one),
        symlink_metadata(worktree, other),
    ) {
        (Ok(Some(one)), Ok(Some(other))) => one.dev() == other.dev() && one.ino() == other.ino(),
        _ => false,
    }
}

/// Whether two paths open one file. This platform has no stable file
/// identity to compare, and its disks fold case.
#[cfg(not(unix))]
fn same_file(worktree: &Path, one: &GitPath, other: &GitPath) -> bool {
    one.as_bytes().eq_ignore_ascii_case(other.as_bytes())
        && matches!(symlink_metadata(worktree, one), Ok(Some(_)))
        && matches!(symlink_metadata(worktree, other), Ok(Some(_)))
}

/// The refusal for a change that would overwrite or remove unsaved files.
pub(super) fn blocked(kind: &'static str, action: &str, paths: &[GitPath]) -> CheckpointError {
    CheckpointError::conflict(
        kind,
        format!(
            "{action} would overwrite or remove files, or another repository, that no undo \
             could bring back: {}. Move them out of the way, then try again.",
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

/// How applying a plan ended when it did not finish.
#[derive(Debug)]
pub(super) enum ApplyFailure {
    /// A path could not be moved, and every path already moved went back.
    /// Each of those was then verified to hold what the snapshot holds; the
    /// apply never touched the rest. `changed` says the stop came from a
    /// path that changed after the check.
    RolledBack { reason: String, changed: bool },
    /// Some paths could not be put back, or did not verify. The snapshot
    /// still holds everything the plan replaced.
    Partial { reason: String },
}

impl ApplyFailure {
    /// The error a caller reports when it has nothing better to say.
    pub fn into_error(self) -> CheckpointError {
        match self {
            Self::RolledBack {
                reason,
                changed: true,
            } => CheckpointError::conflict(
                "worktree_changed",
                format!("{reason} Tidebreak put back what it had changed, so nothing changed."),
            ),
            Self::RolledBack {
                reason,
                changed: false,
            } => CheckpointError::conflict(
                "worktree_write_failed",
                format!("{reason} Tidebreak put back what it had changed, so nothing changed."),
            ),
            Self::Partial { reason } => CheckpointError::internal(format!(
                "{reason} Some files could not be put back as they were."
            )),
        }
    }
}

/// Why one path could not move.
#[derive(Debug)]
struct Stop {
    reason: String,
    /// The path no longer held what the plan saw.
    changed: bool,
}

impl Stop {
    fn changed(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            changed: true,
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            changed: false,
        }
    }
}

/// One path the apply moved, and what it left there.
#[derive(Debug)]
struct Moved {
    change: usize,
    left: Option<Stamp>,
}

/// Move every path in `plan`. When one fails, every path already moved goes
/// back, and the failure says whether the result was verified.
pub(super) async fn apply(worktree: &Path, plan: &Plan) -> Result<(), ApplyFailure> {
    let staging = Staging::prepare(worktree);
    match apply_steps(worktree, &staging, plan, |_| false).await {
        Ok(()) => Ok(()),
        Err((stop, moved)) => Err(roll_back(worktree, &staging, plan, moved, stop).await),
    }
}

/// The paths in the order they move: removals first, deepest first, so a
/// file can take a folder's place; then every write, parents first.
fn move_order(plan: &Plan) -> Vec<usize> {
    let mut order: Vec<usize> = (0..plan.changes.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&plan.changes[a], &plan.changes[b]);
        match (a.after.is_none(), b.after.is_none()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (true, true) => b.path.as_bytes().cmp(a.path.as_bytes()),
            (false, false) => a.path.as_bytes().cmp(b.path.as_bytes()),
        }
    });
    order
}

/// Move the paths before step `killed_at`, then stop the way a killed
/// process stops: nothing rolls back.
#[cfg(any(test, feature = "test-support"))]
pub(super) async fn apply_until_killed(worktree: &Path, plan: &Plan, killed_at: usize) {
    let staging = Staging::prepare(worktree);
    let _ = apply_steps(worktree, &staging, plan, |step| step == killed_at).await;
}

/// Move each path in turn. `halt` is asked before each step and ends the run
/// there, as a crash would, leaving what moved for the caller.
async fn apply_steps(
    worktree: &Path,
    staging: &Staging,
    plan: &Plan,
    halt: impl Fn(usize) -> bool,
) -> Result<(), (Stop, Vec<Moved>)> {
    let mut moved = Vec::new();
    for (step, index) in move_order(plan).into_iter().enumerate() {
        if halt(step) {
            return Err((Stop::failed("Stopped."), moved));
        }
        match move_one(worktree, staging, plan, &plan.changes[index]).await {
            Ok(left) => moved.push(Moved {
                change: index,
                left,
            }),
            Err(stop) => return Err((stop, moved)),
        }
    }
    Ok(())
}

/// Move one path, after checking it still holds what the plan saw.
async fn move_one(
    worktree: &Path,
    staging: &Staging,
    plan: &Plan,
    change: &RawChange,
) -> Result<Option<Stamp>, Stop> {
    let path = &change.path;
    let full = full_path(worktree, path).map_err(Stop::failed)?;
    folders_on_the_way(worktree, path, change.after.is_some())?;
    let now = stamp_at(&full).map_err(Stop::failed)?;
    let recorded = plan.stamps.get(path);
    match &change.before {
        Some(_) => {
            if now.as_ref() != recorded {
                return Err(Stop::changed(format!(
                    "{} changed after Tidebreak checked it.",
                    path.to_wire()
                )));
            }
        }
        None => {
            if now
                .as_ref()
                .is_some_and(|stamp| stamp.kind != StampKind::Folder)
            {
                return Err(appeared(path));
            }
        }
    }
    let Some(after) = &change.after else {
        std::fs::remove_file(&full)
            .map_err(|err| Stop::failed(format!("Could not remove {}: {err}.", path.to_wire())))?;
        remove_empty_folders(worktree, path);
        return Ok(None);
    };
    if now
        .as_ref()
        .is_some_and(|stamp| stamp.kind == StampKind::Folder)
    {
        // The folder's own files went first; anything left is not ours.
        std::fs::remove_dir(&full).map_err(|_| {
            Stop::changed(format!(
                "A folder with files in it stands at {}.",
                path.to_wire()
            ))
        })?;
    }
    let expect = match recorded {
        Some(stamp) if change.before.is_some() => Expect::Stamp(stamp),
        _ => Expect::Nothing,
    };
    let content = match plan.exact_after.get(path) {
        Some(exact) => Content::Exact(exact),
        None => Content::Smudged(after),
    };
    write_entry(worktree, staging, path, &full, content, expect).await?;
    // The path is written; a stamp that cannot be read only makes a later
    // roll back more careful, never less.
    Ok(stamp_at(&full).ok().flatten())
}

/// Every folder on the way to `path` must be a real folder, never a file or a
/// symlink, so nothing is written or removed outside the worktree. With
/// `create`, missing folders are made.
fn folders_on_the_way(worktree: &Path, path: &GitPath, create: bool) -> Result<(), Stop> {
    for ancestor in path.ancestors() {
        let full = full_path(worktree, &ancestor).map_err(Stop::failed)?;
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => {
                return Err(Stop::changed(format!(
                    "{} is no longer a folder.",
                    ancestor.to_wire()
                )))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(());
                }
                std::fs::create_dir(&full).map_err(|err| {
                    Stop::failed(format!(
                        "Could not create the folder {}: {err}.",
                        ancestor.to_wire()
                    ))
                })?;
            }
            Err(err) => {
                return Err(Stop::failed(format!(
                    "Could not read {}: {err}.",
                    ancestor.to_wire()
                )))
            }
        }
    }
    Ok(())
}

/// Remove the folders a removal emptied, deepest first, stopping at the first
/// one that still holds anything.
fn remove_empty_folders(worktree: &Path, path: &GitPath) {
    for folder in path.ancestors().into_iter().rev() {
        let Ok(full) = full_path(worktree, &folder) else {
            return;
        };
        if std::fs::remove_dir(full).is_err() {
            return;
        }
    }
}

/// What a write puts at a path.
#[derive(Debug, Clone, Copy)]
enum Content<'a> {
    /// A tree's version, through the smudge filters its path's attributes
    /// name, the way a checkout writes it.
    Smudged(&'a TreeEntry),
    /// These exact bytes, with no filters.
    Exact(&'a TreeEntry),
}

impl Content<'_> {
    fn entry(&self) -> &TreeEntry {
        match self {
            Self::Smudged(entry) | Self::Exact(entry) => entry,
        }
    }
}

/// The name of the staging folder inside a worktree's own git folder.
const STAGING_FOLDER: &str = "tidebreak-tmp";

/// Where new versions are written before they move into place.
///
/// The folder sits in the worktree's own git folder, which git never lists
/// and a snapshot never reads, so a crash mid-write leaves nothing in the
/// worktree. Its names are short and fixed-length, so a file whose own name
/// is as long as the disk allows still gets written.
struct Staging {
    folder: Option<PathBuf>,
}

impl Staging {
    /// The staging folder for `worktree`, emptied of anything an earlier
    /// crash left. Only one change holds a worktree at a time, so nothing
    /// in it belongs to anyone else.
    fn prepare(worktree: &Path) -> Self {
        let folder = staging_folder(worktree).and_then(|folder| {
            let _ = std::fs::remove_dir_all(&folder);
            std::fs::create_dir_all(&folder).ok().map(|()| folder)
        });
        Self { folder }
    }

    /// A fresh path for the new version of `full`: in the staging folder
    /// when it is on the same disk as `full`, so the move is one rename.
    /// Otherwise a short name beside `full`.
    fn temp_for(&self, full: &Path) -> PathBuf {
        let id = uuid::Uuid::new_v4().simple().to_string();
        if let (Some(folder), Some(parent)) = (&self.folder, full.parent()) {
            if same_disk(folder, parent) {
                return folder.join(&id[..16]);
            }
        }
        full.with_file_name(format!(".tb-{}.tmp", &id[..8]))
    }
}

/// The staging folder inside `worktree`'s own git folder: `.git/tidebreak-tmp`
/// in a main worktree, and the linked worktree's own folder under
/// `.git/worktrees/` for a linked one. `None` when `.git` cannot be read.
pub(crate) fn staging_folder(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let meta = std::fs::symlink_metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.join(STAGING_FOLDER));
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if git_dir.is_empty() {
        return None;
    }
    let git_dir = Path::new(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        worktree.join(git_dir)
    };
    Some(git_dir.join(STAGING_FOLDER))
}

/// Remove whatever a crash left in `worktree`'s staging folder.
pub(crate) fn clear_staging_folder(worktree: &Path) {
    if let Some(folder) = staging_folder(worktree) {
        let _ = std::fs::remove_dir_all(folder);
    }
}

#[cfg(unix)]
fn same_disk(one: &Path, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(one), std::fs::metadata(other)) {
        (Ok(one), Ok(other)) => one.dev() == other.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_disk(_one: &Path, _other: &Path) -> bool {
    true
}

/// What must stand at a path for a write to land there, checked again at the
/// last moment, after the new content is ready.
#[derive(Debug, Clone, Copy)]
enum Expect<'a> {
    /// Nothing. A file that appears first is never overwritten.
    Nothing,
    /// Exactly what this stamp recorded.
    Stamp(&'a Stamp),
}

/// Write `content` at `path`: to a temporary file first, then moved into
/// place in one step, once the path still holds what `expect` says.
async fn write_entry(
    worktree: &Path,
    staging: &Staging,
    path: &GitPath,
    full: &Path,
    content: Content<'_>,
    expect: Expect<'_>,
) -> Result<(), Stop> {
    let temp = staging.temp_for(full);
    let entry = content.entry();
    let written = if entry.is_symlink() {
        write_symlink(worktree, entry, &temp).await
    } else if entry.is_regular_file() {
        write_file(worktree, path, content, &temp).await
    } else {
        Err(format!(
            "{} is not a file Tidebreak can write.",
            path.to_wire()
        ))
    };
    let placed = written
        .map_err(Stop::failed)
        .and_then(|()| place(&temp, full, path, entry.is_regular_file(), expect));
    if placed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    placed
}

/// Move the finished `temp` to `full`, when `full` still holds what `expect`
/// says.
fn place(
    temp: &Path,
    full: &Path,
    path: &GitPath,
    regular_file: bool,
    expect: Expect<'_>,
) -> Result<(), Stop> {
    let now = stamp_at(full).map_err(Stop::failed)?;
    match expect {
        Expect::Nothing => {
            if now.is_some() {
                return Err(appeared(path));
            }
            // A hard link lands only where nothing stands, so a file that
            // appears after that look is never overwritten either. A
            // filesystem without hard links falls back to the rename.
            if regular_file {
                match std::fs::hard_link(temp, full) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(temp);
                        return Ok(());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        return Err(appeared(path));
                    }
                    Err(_) => {}
                }
            }
        }
        Expect::Stamp(stamp) => {
            if now.as_ref() != Some(stamp) {
                return Err(Stop::changed(format!(
                    "{} changed after Tidebreak checked it.",
                    path.to_wire()
                )));
            }
        }
    }
    std::fs::rename(temp, full)
        .map_err(|err| Stop::failed(format!("Could not write {}: {err}.", path.to_wire())))
}

fn appeared(path: &GitPath) -> Stop {
    Stop::changed(format!(
        "Something appeared at {} after Tidebreak checked it.",
        path.to_wire()
    ))
}

/// A file's content streamed straight to `temp`: through the smudge filters
/// its path's attributes name, or as its exact bytes.
async fn write_file(
    worktree: &Path,
    path: &GitPath,
    content: Content<'_>,
    temp: &Path,
) -> Result<(), String> {
    let entry = content.entry();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|err| format!("Could not write {}: {err}.", path.to_wire()))?;
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(worktree)
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("cat-file");
    match content {
        Content::Smudged(_) => {
            let mut filtered_path = OsString::from("--path=");
            filtered_path.push(path.to_os_string()?);
            command.arg("--filters").arg(filtered_path);
        }
        Content::Exact(_) => {
            command.arg("blob");
        }
    }
    command
        .arg(&entry.oid)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|err| format!("Could not start git: {err}."))?;
    let output = tokio::time::timeout(WRITE_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("Writing {} took too long.", path.to_wire()))?
        .map_err(|err| format!("Could not write {}: {err}.", path.to_wire()))?;
    if !output.status.success() {
        return Err(format!(
            "Could not write {}: {}",
            path.to_wire(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    set_executable(temp, entry.is_executable())
        .map_err(|err| format!("Could not set the mode of {}: {err}.", path.to_wire()))
}

async fn write_symlink(worktree: &Path, entry: &TreeEntry, temp: &Path) -> Result<(), String> {
    let target = read_blob(worktree, &entry.oid)
        .await
        .map_err(|err| err.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&target), temp)
            .map_err(|err| format!("Could not make a symlink: {err}."))
    }
    #[cfg(not(unix))]
    {
        // Git on this platform checks a symlink out as a file holding its
        // target.
        std::fs::write(temp, target).map_err(|err| format!("Could not write a file: {err}."))
    }
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    let mode = if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    };
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// Put back every path the apply moved, newest first, as its exact bytes,
/// then check each of them.
///
/// A path that no longer holds what the apply left there changed under it,
/// and stays as it is: putting it back would lose that change.
async fn roll_back(
    worktree: &Path,
    staging: &Staging,
    plan: &Plan,
    moved: Vec<Moved>,
    stop: Stop,
) -> ApplyFailure {
    let mut clean = true;
    for step in moved.iter().rev() {
        let change = &plan.changes[step.change];
        let Ok(full) = full_path(worktree, &change.path) else {
            clean = false;
            continue;
        };
        let now = stamp_at(&full).ok().flatten();
        if now != step.left {
            clean = false;
            continue;
        }
        let put_back = match &change.before {
            None => {
                let removed = match now {
                    Some(_) => std::fs::remove_file(&full).map_err(|err| err.to_string()),
                    None => Ok(()),
                };
                remove_empty_folders(worktree, &change.path);
                removed
            }
            Some(before) => {
                let exact = before
                    .is_regular_file()
                    .then(|| plan.exact_before.get(&change.path))
                    .flatten()
                    .map(|oid| TreeEntry {
                        mode: before.mode.clone(),
                        oid: oid.clone(),
                    });
                let content = match &exact {
                    Some(exact) => Content::Exact(exact),
                    None => Content::Smudged(before),
                };
                match folders_on_the_way(worktree, &change.path, true) {
                    Ok(()) => {
                        let expect = match &step.left {
                            Some(left) => Expect::Stamp(left),
                            None => Expect::Nothing,
                        };
                        write_entry(worktree, staging, &change.path, &full, content, expect)
                            .await
                            .map_err(|stop| stop.reason)
                    }
                    Err(stop) => Err(stop.reason),
                }
            }
        };
        if put_back.is_err() {
            clean = false;
        }
    }
    let moved: Vec<usize> = moved.iter().map(|step| step.change).collect();
    let verified = clean && holds_before(worktree, plan, &moved).await.unwrap_or(false);
    if verified {
        ApplyFailure::RolledBack {
            reason: stop.reason,
            changed: stop.changed,
        }
    } else {
        ApplyFailure::Partial {
            reason: stop.reason,
        }
    }
}

/// Whether every path the apply moved holds exactly the bytes it held before.
/// A path it never reached is as the check found it, or as someone else left
/// it since; either way the apply did not change it.
async fn holds_before(
    worktree: &Path,
    plan: &Plan,
    moved: &[usize],
) -> Result<bool, CheckpointError> {
    let mut files: Vec<(GitPath, String)> = Vec::new();
    let mut links: Vec<(Vec<u8>, String)> = Vec::new();
    for change in moved.iter().map(|&index| &plan.changes[index]) {
        let meta = symlink_metadata(worktree, &change.path)?;
        let Some(before) = &change.before else {
            if meta.is_some_and(|meta| !meta.file_type().is_dir()) {
                return Ok(false);
            }
            continue;
        };
        let Some(meta) = meta else {
            return Ok(false);
        };
        let stamp = Stamp::of(&meta);
        if before.is_symlink() {
            if stamp.kind != StampKind::Symlink {
                return Ok(false);
            }
            links.push((read_link_bytes(worktree, &change.path)?, before.oid.clone()));
        } else {
            if stamp.kind != StampKind::File
                || (plan.file_mode && stamp.is_executable() != before.is_executable())
            {
                return Ok(false);
            }
            let Some(exact) = plan.exact_before.get(&change.path) else {
                return Ok(false);
            };
            files.push((change.path.clone(), exact.clone()));
        }
    }
    let paths: Vec<GitPath> = files.iter().map(|(path, _)| path.clone()).collect();
    let hashed = hash_worktree_files(worktree, &paths, Hashing::Exact).await?;
    if files
        .iter()
        .zip(hashed)
        .any(|((_, expected), actual)| *expected != actual)
    {
        return Ok(false);
    }
    let targets: Vec<Vec<u8>> = links.iter().map(|(target, _)| target.clone()).collect();
    let hashed = hash_bytes(worktree, &targets).await?;
    Ok(!links
        .iter()
        .zip(hashed)
        .any(|((_, expected), actual)| *expected != actual))
}

/// How [`hash_worktree_files`] reads a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hashing {
    /// Through the clean filters its path's attributes name, the way
    /// `git add` hashes it.
    Cleaned,
    /// Its exact bytes, with no filters.
    Exact,
    /// Its exact bytes, stored as a blob so they can be written back.
    ExactAndStored,
}

/// The blob id git gives each worktree file, read as `hashing` says.
async fn hash_worktree_files(
    worktree: &Path,
    paths: &[GitPath],
    hashing: Hashing,
) -> Result<Vec<String>, CheckpointError> {
    let mut oids = Vec::with_capacity(paths.len());
    for batch in paths.chunks(HASH_BATCH) {
        let mut command = git_command(worktree);
        command.arg("hash-object");
        match hashing {
            Hashing::Cleaned => {}
            Hashing::Exact => {
                command.arg("--no-filters");
            }
            Hashing::ExactAndStored => {
                command.args(["-w", "--no-filters"]);
            }
        }
        command.arg("--");
        for path in batch {
            command.arg(path.to_os_string().map_err(CheckpointError::internal)?);
        }
        let raw = run_git_command(
            command,
            format!("hash-object <{} paths>", batch.len()),
            GIT_SNAPSHOT_TIMEOUT,
        )
        .await
        .map_err(CheckpointError::internal)?;
        oids.extend(
            String::from_utf8_lossy(&raw)
                .lines()
                .map(|line| line.trim().to_owned()),
        );
    }
    if oids.len() != paths.len() {
        return Err(CheckpointError::internal(
            "git hashed a different number of files than it was given",
        ));
    }
    Ok(oids)
}

/// The blob id of each byte string, as git stores it, with no filters.
///
/// Each string goes to a temporary file that is closed before the next one
/// opens, so many symlinks never run into the open-file limit.
async fn hash_bytes(worktree: &Path, contents: &[Vec<u8>]) -> Result<Vec<String>, CheckpointError> {
    let mut oids = Vec::with_capacity(contents.len());
    for batch in contents.chunks(HASH_BATCH) {
        let files = batch
            .iter()
            .map(|bytes| temp_file_with(bytes).map(tempfile::NamedTempFile::into_temp_path))
            .collect::<Result<Vec<_>, _>>()?;
        let mut command = git_command(worktree);
        command.args(["hash-object", "--no-filters", "--"]);
        for file in &files {
            command.arg(file.as_os_str());
        }
        let raw = run_git_command(command, "hash-object --no-filters".to_owned(), GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?;
        oids.extend(
            String::from_utf8_lossy(&raw)
                .lines()
                .map(|line| line.trim().to_owned()),
        );
    }
    if oids.len() != contents.len() {
        return Err(CheckpointError::internal(
            "git hashed a different number of strings than it was given",
        ));
    }
    Ok(oids)
}

/// Whether the repository counts the executable bit, as `core.fileMode`
/// says. Git's default is yes.
async fn core_file_mode(worktree: &Path) -> bool {
    git_text(
        worktree,
        &["config", "--bool", "--get", "core.fileMode"],
        GIT_TIMEOUT,
    )
    .await
    .map(|value| value.trim() != "false")
    .unwrap_or(true)
}

/// Refuse to change a sparse checkout. Its snapshots cannot tell a file left
/// out of the checkout from one that was deleted.
pub(super) async fn refuse_sparse_checkout(worktree: &Path) -> Result<(), CheckpointError> {
    let sparse = git_text(
        worktree,
        &["config", "--bool", "--get", "core.sparseCheckout"],
        GIT_TIMEOUT,
    )
    .await
    .is_ok_and(|value| value.trim() == "true");
    if sparse {
        return Err(CheckpointError::conflict(
            "sparse_checkout",
            "This workspace uses a sparse checkout, which Tidebreak cannot undo changes in yet. \
             Use Git in a terminal.",
        ));
    }
    Ok(())
}

/// Refuse a change that has no Undo when it would replace or remove a file
/// whose exact bytes git's filters do not give back.
///
/// A clean filter can drop part of a file on its way into git, such as a
/// filter that strips notebook output. A revert or a discard builds the new
/// version from what the filter kept, so the rest would be lost, and nothing
/// could bring it back. A filter that only changes line endings and gives
/// them back on checkout loses nothing, and passes.
pub(super) async fn refuse_lossy_filters(
    worktree: &Path,
    plan: &Plan,
    action: &str,
) -> Result<(), CheckpointError> {
    for change in &plan.changes {
        let Some(before) = change
            .before
            .as_ref()
            .filter(|entry| entry.is_regular_file())
        else {
            continue;
        };
        let Some(exact) = plan.exact_before.get(&change.path) else {
            continue;
        };
        if *exact == before.oid || smudges_back(worktree, &change.path, &before.oid, exact).await? {
            continue;
        }
        let path = change.path.to_wire();
        return Err(CheckpointError::conflict(
            "filter_lossy",
            match filter_name(worktree, &change.path).await {
                Some(filter) => format!(
                    "{action} would lose part of {path}: Git's \"{filter}\" filter drops it on \
                     the way into Git, so Tidebreak could not write it back. Change the file in \
                     your editor instead."
                ),
                None => format!(
                    "{action} would lose part of {path}: Git changes its bytes on the way into \
                     Git, such as its line endings, and does not give them back. Change the file \
                     in your editor instead."
                ),
            },
        ));
    }
    Ok(())
}

/// Whether the smudge filters turn `cleaned`, what the clean filters made of
/// the file at `path`, back into exactly the blob `exact`.
async fn smudges_back(
    worktree: &Path,
    path: &GitPath,
    cleaned: &str,
    exact: &str,
) -> Result<bool, CheckpointError> {
    let smudged = tempfile::NamedTempFile::new()
        .map_err(|err| CheckpointError::internal(format!("could not stage a file: {err}")))?
        .into_temp_path();
    std::fs::remove_file(&smudged)
        .map_err(|err| CheckpointError::internal(format!("could not stage a file: {err}")))?;
    write_file(
        worktree,
        path,
        Content::Smudged(&TreeEntry {
            mode: "100644".to_owned(),
            oid: cleaned.to_owned(),
        }),
        &smudged,
    )
    .await
    .map_err(CheckpointError::internal)?;
    let mut command = git_command(worktree);
    command
        .args(["hash-object", "--no-filters", "--"])
        .arg(smudged.as_os_str());
    let oid = run_git_command(command, "hash-object --no-filters".to_owned(), GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    Ok(String::from_utf8_lossy(&oid).trim() == exact)
}

/// The filter driver `.gitattributes` names for `path`, if any.
async fn filter_name(worktree: &Path, path: &GitPath) -> Option<String> {
    let (raw, _) = git_bytes_with_literal_paths_bounded(
        worktree,
        &["check-attr", "-z", "filter", "--"],
        std::slice::from_ref(path),
        GIT_TIMEOUT,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
    )
    .await
    .ok()?;
    // `<path> NUL filter NUL <value> NUL`
    let value = raw.split(|byte| *byte == 0).nth(2)?;
    let value = String::from_utf8_lossy(value).into_owned();
    (!matches!(value.as_str(), "" | "unspecified" | "unset" | "set")).then_some(value)
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

fn full_path(worktree: &Path, path: &GitPath) -> Result<PathBuf, String> {
    path.to_os_string().map(|path| worktree.join(path))
}

/// What stands at `full`, without following a symlink.
fn stamp_at(full: &Path) -> Result<Option<Stamp>, String> {
    match std::fs::symlink_metadata(full) {
        Ok(meta) => Ok(Some(Stamp::of(&meta))),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(format!("Could not read {}: {err}.", full.display())),
    }
}

/// What stands at `path` in the worktree, without following a symlink.
fn symlink_metadata(
    worktree: &Path,
    path: &GitPath,
) -> Result<Option<std::fs::Metadata>, CheckpointError> {
    let full = full_path(worktree, path).map_err(CheckpointError::internal)?;
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

/// A symlink's target, as the bytes git stores for it.
fn read_link_bytes(worktree: &Path, path: &GitPath) -> Result<Vec<u8>, CheckpointError> {
    let full = full_path(worktree, path).map_err(CheckpointError::internal)?;
    let target = std::fs::read_link(&full).map_err(|err| {
        CheckpointError::internal(format!("could not read {}: {err}", path.to_wire()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(target.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(target.to_string_lossy().replace('\\', "/").into_bytes())
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
    use std::os::unix::fs::PermissionsExt;

    fn path(value: &str) -> GitPath {
        GitPath::from_bytes(value.as_bytes())
    }

    fn wires(paths: &[GitPath]) -> Vec<String> {
        paths.iter().map(GitPath::to_wire).collect()
    }

    fn read(worktree: &Path, name: &str) -> Option<String> {
        std::fs::read_to_string(worktree.join(name)).ok()
    }

    /// A blob holding `content`, written to the object store.
    fn blob(worktree: &Path, content: &str) -> TreeEntry {
        std::fs::write(worktree.join(".blob.tmp"), content).unwrap();
        let oid = git_stdout(worktree, &["hash-object", "-w", ".blob.tmp"]);
        std::fs::remove_file(worktree.join(".blob.tmp")).unwrap();
        TreeEntry {
            mode: "100644".into(),
            oid,
        }
    }

    #[tokio::test]
    async fn blockers_name_ignored_files_and_folders_in_the_way() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "blockers");
        std::fs::write(tree.join(".gitignore"), ".env\n*.local\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore"]);
        let index = PrivateIndex::new(&tree).await.unwrap();
        let base = index.snapshot(&tree).await.unwrap();
        let target = tree_with_changes(
            &tree,
            &base,
            &[
                (path(".env"), Some(blob(&tree, "PLACEHOLDER=1\n"))),
                (path("utils"), Some(blob(&tree, "a file\n"))),
                (path("notes/today.md"), Some(blob(&tree, "note\n"))),
            ],
        )
        .await
        .unwrap();

        // Now: `.env` is ignored and holds a real value, and `utils` is a
        // folder holding an ignored file. Neither is in any snapshot.
        std::fs::write(tree.join(".env"), "the real value\n").unwrap();
        std::fs::create_dir_all(tree.join("utils")).unwrap();
        std::fs::write(tree.join("utils/settings.local"), "mine\n").unwrap();
        let now = index.snapshot(&tree).await.unwrap();
        assert_eq!(now, base, "ignored files are in no snapshot");

        let inspection = inspect(&tree, &now, &target).await.unwrap();
        assert_eq!(wires(&inspection.blocked), [".env", "utils/settings.local"]);
    }

    #[tokio::test]
    async fn a_file_in_place_of_a_needed_folder_is_in_the_way() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "folder-needed");
        std::fs::write(tree.join(".gitignore"), "cache\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore"]);
        let index = PrivateIndex::new(&tree).await.unwrap();
        let before = index.snapshot(&tree).await.unwrap();
        let target = tree_with_changes(
            &tree,
            &before,
            &[(path("cache/entry"), Some(blob(&tree, "entry\n")))],
        )
        .await
        .unwrap();
        std::fs::write(tree.join("cache"), "an ignored file\n").unwrap();

        let inspection = inspect(&tree, &before, &target).await.unwrap();
        assert_eq!(wires(&inspection.blocked), ["cache"]);
    }

    /// A clone inside the worktree is saved only as the commit it points at,
    /// never its files, so no change that would remove or replace it runs.
    #[tokio::test]
    async fn a_nested_repository_is_never_removed_or_replaced() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "nested");
        let vendor = tree.join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        run(&vendor, &["git", "init", "-q", "-b", "main"]);
        run(&vendor, &["git", "config", "user.email", "dev@example.com"]);
        run(&vendor, &["git", "config", "user.name", "Dev"]);
        run(&vendor, &["git", "config", "commit.gpgsign", "false"]);
        std::fs::write(vendor.join("lib.rs"), "committed\n").unwrap();
        run(&vendor, &["git", "add", "lib.rs"]);
        run(&vendor, &["git", "commit", "-q", "-m", "vendored"]);
        std::fs::write(vendor.join("lib.rs"), "uncommitted work\n").unwrap();
        let index = PrivateIndex::new(&tree).await.unwrap();
        let now = index.snapshot(&tree).await.unwrap();
        // The target holds a plain file where the clone stands.
        let target = tree_with_changes(
            &tree,
            &now,
            &[(path("vendor"), Some(blob(&tree, "a file\n")))],
        )
        .await
        .unwrap();

        let inspection = inspect(&tree, &now, &target).await.unwrap();
        assert_eq!(wires(&inspection.blocked), ["vendor"]);
        assert!(inspection
            .into_plan("restore_blocked", "The restore")
            .is_err());
        assert_eq!(
            read(&vendor, "lib.rs").as_deref(),
            Some("uncommitted work\n")
        );
        assert!(vendor.join(".git").is_dir());
    }

    /// Git stops partway through a plan: a folder it cannot write into. Every
    /// path already moved goes back, the result is verified, and a file
    /// nobody's plan named stays as it is.
    #[tokio::test]
    async fn a_plan_that_fails_partway_puts_back_only_what_it_moved() {
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
        let plan = inspect(&tree, &current, &target)
            .await
            .unwrap()
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        // `a.txt` can be written; nothing can be created in `locked/`.
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        std::fs::write(tree.join("elsewhere.txt"), "typed during the restore\n").unwrap();

        let failure = apply(&tree, &plan).await.unwrap_err();
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(failure, ApplyFailure::RolledBack { changed: false, .. }),
            "{failure:?}"
        );
        assert_eq!(read(&tree, "a.txt").as_deref(), Some("current a\n"));
        assert!(!tree.join("locked/new.txt").exists());
        assert_eq!(
            read(&tree, "elsewhere.txt").as_deref(),
            Some("typed during the restore\n")
        );
    }

    /// A folder that refuses a removal stops the plan, and the result is
    /// the state before it, verified, never a mix of the two.
    #[tokio::test]
    async fn a_removal_a_read_only_folder_refuses_leaves_the_state_before() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "read-only");
        std::fs::write(tree.join("a.txt"), "target a\n").unwrap();
        let index = PrivateIndex::new(&tree).await.unwrap();
        let target = index.snapshot(&tree).await.unwrap();
        std::fs::write(tree.join("a.txt"), "current a\n").unwrap();
        std::fs::create_dir_all(tree.join("sealed")).unwrap();
        std::fs::write(tree.join("sealed/added.txt"), "added since\n").unwrap();
        let current = index.snapshot(&tree).await.unwrap();
        let plan = inspect(&tree, &current, &target)
            .await
            .unwrap()
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        std::fs::set_permissions(tree.join("sealed"), std::fs::Permissions::from_mode(0o555))
            .unwrap();

        let failure = apply(&tree, &plan).await.unwrap_err();
        std::fs::set_permissions(tree.join("sealed"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(failure, ApplyFailure::RolledBack { .. }),
            "{failure:?}"
        );
        assert_eq!(read(&tree, "a.txt").as_deref(), Some("current a\n"));
        assert_eq!(
            read(&tree, "sealed/added.txt").as_deref(),
            Some("added since\n")
        );
        assert_eq!(index.snapshot(&tree).await.unwrap(), current);
    }

    /// A plan killed partway, the way a crash or a killed process stops it,
    /// leaves every path whole: its old version or its new one, never
    /// missing. A write it was in the middle of leaves its temporary file in
    /// the staging folder, where no snapshot sees it and the next change
    /// clears it. Restoring the snapshot then brings back exactly the state
    /// before.
    #[tokio::test]
    async fn a_plan_killed_partway_leaves_every_path_whole_and_can_be_undone() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "killed");
        for n in 0..5 {
            std::fs::write(tree.join(format!("f{n}.txt")), format!("target {n}\n")).unwrap();
        }
        let index = PrivateIndex::new(&tree).await.unwrap();
        let target = index.snapshot(&tree).await.unwrap();
        for n in 0..5 {
            std::fs::write(tree.join(format!("f{n}.txt")), format!("current {n}\n")).unwrap();
        }
        let before = index.snapshot(&tree).await.unwrap();
        let staging = staging_folder(&tree).unwrap();

        for killed_at in 0..=5 {
            let plan = inspect(&tree, &before, &target)
                .await
                .unwrap()
                .into_plan("restore_blocked", "The restore")
                .unwrap();
            apply_until_killed(&tree, &plan, killed_at).await;
            let leftover = staging.join("0123456789abcdef");
            std::fs::write(&leftover, "half\n").unwrap();
            for n in 0..5 {
                let now = read(&tree, &format!("f{n}.txt"));
                assert!(
                    now == Some(format!("current {n}\n")) || now == Some(format!("target {n}\n")),
                    "f{n}.txt is whole after a kill at step {killed_at}: {now:?}"
                );
            }
            // Undo: the saved snapshot comes back exactly, with nothing of
            // the killed write in the worktree.
            let now = index.snapshot(&tree).await.unwrap();
            if now != before {
                let undo = inspect(&tree, &now, &before)
                    .await
                    .unwrap()
                    .into_plan("restore_blocked", "The restore")
                    .unwrap();
                apply(&tree, &undo).await.unwrap();
            }
            assert_eq!(index.snapshot(&tree).await.unwrap(), before);
            let _ = std::fs::remove_file(&leftover);
        }
        // The next change clears what a crash left.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("0123456789abcdef"), "half\n").unwrap();
        let plan = inspect(&tree, &before, &target)
            .await
            .unwrap()
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        apply(&tree, &plan).await.unwrap();
        assert!(!staging.join("0123456789abcdef").exists());
    }

    /// An ignored file typed after the check, where the plan writes, is never
    /// overwritten: the plan stops at that path and puts back what it moved.
    #[tokio::test]
    async fn an_ignored_file_that_appears_after_the_check_is_never_overwritten() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "ignored-race");
        std::fs::write(tree.join(".gitignore"), ".env\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore"]);
        let index = PrivateIndex::new(&tree).await.unwrap();
        let current = index.snapshot(&tree).await.unwrap();
        let target = tree_with_changes(
            &tree,
            &current,
            &[
                (path(".env"), Some(blob(&tree, "PLACEHOLDER=1\n"))),
                (path("README.md"), Some(blob(&tree, "restored\n"))),
            ],
        )
        .await
        .unwrap();
        let plan = inspect(&tree, &current, &target)
            .await
            .unwrap()
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        std::fs::write(tree.join(".env"), "the real value\n").unwrap();

        let failure = apply(&tree, &plan).await.unwrap_err();

        assert!(
            matches!(failure, ApplyFailure::RolledBack { changed: true, .. }),
            "{failure:?}"
        );
        assert_eq!(read(&tree, ".env").as_deref(), Some("the real value\n"));
        assert_eq!(read(&tree, "README.md").as_deref(), Some("hello\n"));
    }

    /// The last look at a path comes after its new content is ready beside
    /// it, so a file that appears or changes while that content streams is
    /// never overwritten.
    #[test]
    fn a_file_that_moves_while_the_content_is_written_is_never_overwritten() {
        let dir = tempfile::TempDir::new().unwrap();
        let full = dir.path().join(".env");
        let temp = dir.path().join(".env.tidebreak-test.tmp");
        std::fs::write(&temp, "PLACEHOLDER=1\n").unwrap();

        // Nothing stood there at the check; something does now.
        std::fs::write(&full, "the real value\n").unwrap();
        for regular_file in [true, false] {
            let stop =
                place(&temp, &full, &path(".env"), regular_file, Expect::Nothing).unwrap_err();
            assert!(stop.changed, "{}", stop.reason);
            assert_eq!(std::fs::read_to_string(&full).unwrap(), "the real value\n");
        }

        // What stood there at the check changed since.
        let checked = stamp_at(&full).unwrap().unwrap();
        std::fs::write(&full, "a later edit, longer\n").unwrap();
        let stop = place(&temp, &full, &path(".env"), true, Expect::Stamp(&checked)).unwrap_err();
        assert!(stop.changed, "{}", stop.reason);
        assert_eq!(
            std::fs::read_to_string(&full).unwrap(),
            "a later edit, longer\n"
        );
    }

    /// A file excluded by `.git/info/exclude`, in a folder the plan turns back
    /// into a file, stops the plan whether it was there at the check or
    /// appeared after it.
    #[tokio::test]
    async fn an_excluded_file_in_a_folder_that_becomes_a_file_is_never_removed() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "excluded");
        let exclude = git_stdout(&tree, &["rev-parse", "--git-path", "info/exclude"]);
        let exclude = tree.join(exclude);
        std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        std::fs::write(&exclude, "*.session\n").unwrap();
        std::fs::write(tree.join("cache"), "the cache was a file\n").unwrap();
        let index = PrivateIndex::new(&tree).await.unwrap();
        let target = index.snapshot(&tree).await.unwrap();
        std::fs::remove_file(tree.join("cache")).unwrap();
        std::fs::create_dir_all(tree.join("cache")).unwrap();
        std::fs::write(tree.join("cache/data.bin"), "tracked data\n").unwrap();
        let current = index.snapshot(&tree).await.unwrap();

        // Excluded at the check: named.
        std::fs::write(tree.join("cache/login.session"), "signed in\n").unwrap();
        let inspection = inspect(&tree, &current, &target).await.unwrap();
        assert_eq!(wires(&inspection.blocked), ["cache/login.session"]);

        // Excluded after the check: the plan stops and puts back what it
        // moved.
        std::fs::remove_file(tree.join("cache/login.session")).unwrap();
        let plan = inspect(&tree, &current, &target)
            .await
            .unwrap()
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        std::fs::write(tree.join("cache/login.session"), "signed in\n").unwrap();
        let failure = apply(&tree, &plan).await.unwrap_err();
        assert!(
            matches!(failure, ApplyFailure::RolledBack { changed: true, .. }),
            "{failure:?}"
        );
        assert_eq!(
            read(&tree, "cache/login.session").as_deref(),
            Some("signed in\n")
        );
        assert_eq!(
            read(&tree, "cache/data.bin").as_deref(),
            Some("tracked data\n")
        );
    }

    /// A symlink that points out of the worktree is never followed: replacing
    /// it with a folder writes inside the worktree, and the folder it pointed
    /// at is untouched.
    #[tokio::test]
    async fn a_symlink_out_of_the_worktree_is_never_followed() {
        let (dir, repo) = init_repo();
        let tree = add_worktree(&repo, "symlink-out");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("theirs.txt"), "not the worktree's\n").unwrap();
        std::fs::create_dir_all(tree.join("out")).unwrap();
        std::fs::write(tree.join("out/theirs.txt"), "the worktree's own\n").unwrap();
        let index = PrivateIndex::new(&tree).await.unwrap();
        let target = index.snapshot(&tree).await.unwrap();
        std::fs::remove_dir_all(tree.join("out")).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("out")).unwrap();
        let current = index.snapshot(&tree).await.unwrap();

        let inspection = inspect(&tree, &current, &target).await.unwrap();
        assert!(inspection.blocked.is_empty(), "{:?}", inspection.blocked);
        let plan = inspection
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        apply(&tree, &plan).await.unwrap();

        assert!(!std::fs::symlink_metadata(tree.join("out"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            read(&tree, "out/theirs.txt").as_deref(),
            Some("the worktree's own\n")
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("theirs.txt")).unwrap(),
            "not the worktree's\n"
        );
    }

    /// A rename that changes only a letter's case goes through on any
    /// filesystem, the case-insensitive ones included.
    #[tokio::test]
    async fn a_case_only_rename_is_not_in_its_own_way() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "case");
        let index = PrivateIndex::new(&tree).await.unwrap();
        let before = index.snapshot(&tree).await.unwrap();
        let readme = tree_entry(&tree, &before, &path("README.md"))
            .await
            .unwrap()
            .unwrap();
        let target = tree_with_changes(
            &tree,
            &before,
            &[(path("README.md"), None), (path("Readme.md"), Some(readme))],
        )
        .await
        .unwrap();

        let inspection = inspect(&tree, &before, &target).await.unwrap();
        assert!(inspection.blocked.is_empty(), "{:?}", inspection.blocked);
        let plan = inspection
            .into_plan("restore_blocked", "The restore")
            .unwrap();
        apply(&tree, &plan).await.unwrap();

        let names: Vec<String> = std::fs::read_dir(&tree)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.eq_ignore_ascii_case("readme.md"))
            .collect();
        assert_eq!(names, ["Readme.md"]);
        assert_eq!(read(&tree, "Readme.md").as_deref(), Some("hello\n"));
    }
}

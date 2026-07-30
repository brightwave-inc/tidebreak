//! Per-turn write staging for granted host folders.
//!
//! Exec writes into a folder the user granted currently land on the user's real
//! files the moment the command runs. This module puts a per-turn overlay in
//! between: each writable granted root gets a private copy under the chat's
//! scratch tree, the sandbox profile makes that copy the only writable
//! location, and the turn's changes are applied to the real folder when the
//! turn ends.
//!
//! ## Why the overlay is a whole copy
//!
//! The agent drives an ordinary shell. It creates a file, greps for it, appends
//! to it, renames it, deletes it — and every one of those steps has to see what
//! the previous one did. A sparse overlay would need reads to fall through to
//! the real root, and on macOS there is no way to arrange that: Seatbelt is a
//! policy engine over real paths, not a mount namespace, so nothing can make
//! one path show two directories merged.
//!
//! Copying the root sidesteps the problem rather than solving it. Read-through
//! is not emulated, it is structural: the overlay *is* the tree, so every
//! filesystem operation works exactly as it would against the real folder, and
//! there is no operation to enumerate as unsupported. On APFS the copy is a
//! `clonefile`, which shares storage until a block is written, so this costs
//! directory metadata rather than the folder's bytes.
//!
//! The price is that exec addresses the staged tree at the overlay path rather
//! than the folder's own path. That is the one thing the user-visible surface
//! has to say out loud, and the operating prompt does.
//!
//! ## Why applying changes is manifest-driven
//!
//! At the end of the turn the overlay is compared against the digest manifest
//! recorded when it was made — not against whatever the real folder holds now.
//! A file whose bytes and mode still match the manifest was never touched and
//! is left alone. A file that differs is written back. A file the manifest
//! lists and the overlay no longer has was deleted, and is deleted from the
//! folder too.
//!
//! Judging deletions against the manifest rather than against the real folder
//! is deliberate: it is what keeps a partial copy, or a file the user added
//! while the turn ran, from being read as "the agent deleted this". Nothing is
//! removed unless this module watched it exist in the overlay first.
//!
//! Every step is issued against a descriptor pinned when the overlay was
//! prepared. Exec can write inside the overlay for the length of the turn, so
//! it can rename directories under the paths this module would otherwise walk;
//! pinning means the directory acted on is the directory that was checked.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use openwave_core::MAX_EXEC_SNAPSHOT_BYTES;
use sha2::{Digest as _, Sha256};

use crate::host_paths::{
    resolve_scratch_directory, FilePrecondition, FileStamp, ScratchDir, ScratchEntry,
    ScratchEntryKind,
};

/// Scratch-root-relative home for every live overlay. It sits beside the chat
/// workspaces rather than inside one, so it is never listed as a workspace
/// file, never synced, and never writable by exec except through the one
/// per-turn subdirectory the profile names.
pub const OVERLAY_DIR: &str = ".exec-overlays";

/// Ceiling on the entries one overlay may stage. A granted root larger than
/// this is left unstaged and keeps today's direct-write behavior rather than
/// making every turn pay an unbounded walk.
const MAX_OVERLAY_ENTRIES: usize = 20_000;

/// Ceiling on directory nesting inside one overlay, so a pathological tree
/// cannot turn the walk into unbounded recursion.
const MAX_OVERLAY_DEPTH: usize = 24;

/// Ceiling on one staged file's size when it is written back.
const MAX_STAGED_FILE_BYTES: u64 = 64 * 1_024 * 1_024;

/// What one regular file in the staged copy looked like before exec ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManifestEntry {
    stamp: FileStamp,
    sha256: [u8; 32],
}

/// One granted root and the private copy exec writes to instead.
pub struct OverlaySlot {
    /// The real folder, as granted.
    source: PathBuf,
    /// Where the staged copy lives. Exec sees this path.
    overlay: PathBuf,
    /// The real folder, pinned when the overlay was prepared.
    source_dir: ScratchDir,
    /// The staged copy, pinned when it was made.
    overlay_dir: ScratchDir,
    /// Every regular file the copy started with, by overlay-relative path.
    manifest: BTreeMap<String, ManifestEntry>,
}

impl std::fmt::Debug for OverlaySlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OverlaySlot")
            .field("source", &self.source)
            .field("overlay", &self.overlay)
            .field("staged", &self.manifest.len())
            .finish()
    }
}

impl OverlaySlot {
    /// The granted folder this slot stages writes for.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// The path exec writes to. This is what the operating prompt names and
    /// what the Seatbelt profile makes writable.
    #[must_use]
    pub fn overlay(&self) -> &Path {
        &self.overlay
    }
}

/// Every writable granted root staged for one turn.
#[derive(Debug)]
pub struct WriteOverlay {
    home: PathBuf,
    slots: Vec<OverlaySlot>,
}

/// A staged change that passed its precondition and reached the granted folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedChange {
    /// Granted folder the change landed in.
    pub folder: PathBuf,
    /// Changed file relative to `folder`.
    pub relative: String,
    /// Effect that reached the folder.
    pub change: MaterializedChangeKind,
}

/// The effect one successfully materialized file had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializedChangeKind {
    /// A previously absent file was written.
    Created,
    /// An existing file was replaced.
    Overwritten,
    /// An existing file was removed.
    Deleted,
}

/// A staged change that was deliberately left out of the granted folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedChange {
    /// Granted folder the change targeted.
    pub folder: PathBuf,
    /// Rejected file relative to `folder`.
    pub relative: String,
    /// Why the real folder was left untouched.
    pub reason: RejectedChangeReason,
}

/// Why one staged file was not materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedChangeReason {
    /// The real destination no longer has the bytes copied into the overlay.
    Stale,
    /// The prior bytes could not be retained for undo.
    SnapshotUnavailable,
    /// The staged file exceeds the write-back ceiling.
    StagedFileTooLarge,
    /// A filesystem operation failed or found an unsupported entry.
    Unavailable,
}

/// Per-file result of applying one overlay.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OverlayOutcome {
    /// Changes that reached their destinations.
    pub written: Vec<MaterializedChange>,
    /// Changes left staged because they could not safely be applied.
    pub rejected: Vec<RejectedChange>,
}

impl OverlayOutcome {
    fn written(&mut self, slot: &OverlaySlot, relative: &str, change: MaterializedChangeKind) {
        self.written.push(MaterializedChange {
            folder: slot.source.clone(),
            relative: relative.to_owned(),
            change,
        });
    }

    fn rejected(&mut self, slot: &OverlaySlot, relative: &str, reason: RejectedChangeReason) {
        self.rejected.push(RejectedChange {
            folder: slot.source.clone(),
            relative: relative.to_owned(),
            reason,
        });
    }
}

/// The bytes a granted folder held immediately before one staged change.
///
/// The distinction between "there was nothing" and "there was something we
/// could not keep" is the whole point of this type. Both leave no blob behind,
/// and only one of them means the change cannot be reverted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriorContents {
    /// The folder had no such file. Reverting means removing what was written.
    Absent,
    /// The prior bytes, small enough to retain.
    Bytes(Vec<u8>),
    /// The file exceeded [`MAX_EXEC_SNAPSHOT_BYTES`]. It is still written; the
    /// caller records that this one change cannot be undone.
    TooLarge { byte_len: u64 },
    /// The file existed but could not be read.
    Unreadable,
}

/// One file a turn's write-back is about to change, and what it is replacing.
#[derive(Debug)]
pub struct StagedChange<'a> {
    /// The granted folder, as the user attached it.
    pub folder: &'a Path,
    /// The changed file's path relative to that folder.
    pub relative: &'a str,
    /// What the folder held beforehand.
    pub prior: PriorContents,
    /// The bytes about to be written; absent when the file is being removed.
    pub next: Option<&'a [u8]>,
}

/// A snapshot prepared before a write and journaled only after it succeeds.
pub trait PreparedWriteSnapshot: Send {
    /// Confirm that the file mutation landed and make its journal row visible
    /// to the sink's turn-level commit.
    fn applied(self: Box<Self>);
}

/// Where a turn's file changes are journaled before they are materialized.
///
/// The sink runs *before* the folder is touched and may refuse: a change whose
/// prior bytes could not be retained is not applied, because an irreversible
/// overwrite is exactly what this whole path exists to prevent. Preparation is
/// per file so the prior bytes never all sit in memory at once; the receipt
/// joins the accumulated journal only after the conditional mutation succeeds.
#[async_trait::async_trait]
pub trait WriteSnapshotSink: Send + Sync {
    /// Retain `change`'s prior bytes and prepare its journal row.
    ///
    /// Dropping the receipt means the filesystem mutation was refused or
    /// failed. Calling [`PreparedWriteSnapshot::applied`] is the only action
    /// that includes the row in the turn-level journal.
    async fn prepare(
        &self,
        change: StagedChange<'_>,
    ) -> Result<Box<dyn PreparedWriteSnapshot>, String>;
}

impl WriteOverlay {
    /// Stage `sources` under a fresh per-turn home inside `scratch_root`.
    ///
    /// A source that cannot be copied — unreadable, too large, or a name that
    /// something already occupies — is simply not staged. It keeps direct write
    /// access, which is what it had before this module existed, so a folder
    /// this cannot handle degrades to the previous behavior instead of losing
    /// the agent's writes.
    pub async fn prepare(scratch_root: &Path, scope: &str, sources: &[PathBuf]) -> Option<Self> {
        if sources.is_empty() || scope.is_empty() || scope.contains('/') {
            return None;
        }
        // Anything already staged for this scope belongs to a turn that never
        // finished. Its writes are deliberately dropped rather than applied to
        // a folder that has moved on since.
        sweep_abandoned_overlays(scratch_root, scope).await;
        let relative = format!("{OVERLAY_DIR}/{scope}/{}", uuid::Uuid::new_v4());
        let home_dir = resolve_scratch_directory(scratch_root, &relative, true).await?;
        let home = scratch_root.join(&relative);

        let mut slots = Vec::new();
        let mut used = Vec::<String>::new();
        for source in sources {
            let name = slot_name(source, &used);
            used.push(name.clone());
            match stage_one(source, &home, &home_dir, &name).await {
                Some(slot) => slots.push(slot),
                None => {
                    tracing::warn!(
                        source = %source.display(),
                        "exec write overlay could not stage a granted folder; writes stay direct"
                    );
                }
            }
        }
        if slots.is_empty() {
            let _ = tokio::fs::remove_dir_all(&home).await;
            return None;
        }
        Some(Self { home, slots })
    }

    /// The staged roots, for the sandbox profile and the operating prompt.
    #[must_use]
    pub fn slots(&self) -> &[OverlaySlot] {
        &self.slots
    }

    /// Apply every staged change to the real folders and drop the overlay.
    ///
    /// Every change passes through `snapshots` first, so the bytes it replaces
    /// are retained before they stop existing. A sink that refuses one file
    /// leaves that file untouched and returns it in `rejected`.
    ///
    /// Errors are per file: one refused write does not abandon the rest, since
    /// leaving the remainder unapplied would strand work the agent believes it
    /// finished.
    pub async fn materialize(self, snapshots: Option<&dyn WriteSnapshotSink>) -> OverlayOutcome {
        let mut outcome = OverlayOutcome::default();
        for slot in &self.slots {
            let mut seen = Vec::new();
            apply_directory(
                slot,
                "",
                &slot.overlay_dir,
                snapshots,
                &mut seen,
                0,
                &mut outcome,
            )
            .await;
            apply_deletions(slot, snapshots, &seen, &mut outcome).await;
        }
        self.discard().await;
        outcome
    }

    /// Drop the overlay without applying anything.
    pub async fn discard(self) {
        let _ = tokio::fs::remove_dir_all(&self.home).await;
    }
}

/// Remove overlay homes left behind by a turn that did not finish cleanly.
///
/// Scoped to one caller's own subtree, so sweeping never touches an overlay a
/// concurrent chat is still writing into.
pub async fn sweep_abandoned_overlays(scratch_root: &Path, scope: &str) {
    if scope.is_empty() || scope.contains('/') {
        return;
    }
    let _ = tokio::fs::remove_dir_all(scratch_root.join(OVERLAY_DIR).join(scope)).await;
}

async fn stage_one(
    source: &Path,
    home: &Path,
    home_dir: &ScratchDir,
    name: &str,
) -> Option<OverlaySlot> {
    let source_dir = resolve_scratch_directory(source, "", false).await?;
    let destination = home.join(name);
    clone_tree(source, &destination).await.ok()?;
    // Reopening through the pinned home rather than by path is what ties the
    // handle to the directory just created: whatever the name refers to later,
    // this descriptor still refers to the copy.
    let overlay_dir = home_dir.open_dir(name).await.ok()?;

    let mut manifest = BTreeMap::new();
    if !record_manifest(&overlay_dir, "", &mut manifest, 0).await {
        let _ = tokio::fs::remove_dir_all(&destination).await;
        return None;
    }
    Some(OverlaySlot {
        source: source.to_path_buf(),
        overlay: destination,
        source_dir,
        overlay_dir,
        manifest,
    })
}

/// Walk the fresh copy and record what it started with. `false` means the tree
/// exceeded a bound, in which case the caller discards the slot.
///
/// An individual entry that cannot be inspected is left out rather than failing
/// the walk. Omission is the safe direction: an unrecorded file is never
/// deleted, and any attempted overwrite is treated as a create whose
/// absent-destination precondition rejects against the existing source file.
async fn record_manifest(
    directory: &ScratchDir,
    prefix: &str,
    manifest: &mut BTreeMap<String, ManifestEntry>,
    depth: usize,
) -> bool {
    if depth > MAX_OVERLAY_DEPTH {
        return false;
    }
    let Ok(entries) = directory.entries().await else {
        return false;
    };
    for ScratchEntry { name, kind } in entries {
        if manifest.len() >= MAX_OVERLAY_ENTRIES {
            return false;
        }
        let relative = join_relative(prefix, &name);
        match kind {
            ScratchEntryKind::File => {
                if let (Some(stamp), Ok(sha256)) = (
                    directory.file_stamp(&name).await,
                    directory.file_sha256(&name).await,
                ) {
                    manifest.insert(relative, ManifestEntry { stamp, sha256 });
                }
            }
            ScratchEntryKind::Directory => {
                let Ok(child) = directory.open_dir(&name).await else {
                    continue;
                };
                if !Box::pin(record_manifest(&child, &relative, manifest, depth + 1)).await {
                    return false;
                }
            }
            // Symlinks and everything exotic are copied by the clone but never
            // walked, written back, or deleted. They are inert.
            ScratchEntryKind::Other => {}
        }
    }
    true
}

async fn apply_directory(
    slot: &OverlaySlot,
    prefix: &str,
    directory: &ScratchDir,
    snapshots: Option<&dyn WriteSnapshotSink>,
    seen: &mut Vec<String>,
    depth: usize,
    outcome: &mut OverlayOutcome,
) {
    if depth > MAX_OVERLAY_DEPTH {
        return;
    }
    let Ok(entries) = directory.entries().await else {
        outcome.rejected(slot, prefix, RejectedChangeReason::Unavailable);
        return;
    };
    for ScratchEntry { name, kind } in entries {
        let relative = join_relative(prefix, &name);
        match kind {
            ScratchEntryKind::File => {
                seen.push(relative.clone());
                let Some(stamp) = directory.file_stamp(&name).await else {
                    outcome.rejected(slot, &relative, RejectedChangeReason::Unavailable);
                    continue;
                };
                let content = match changed_staged_content(
                    directory,
                    &name,
                    stamp,
                    slot.manifest.get(&relative),
                )
                .await
                {
                    Ok(Some(content)) => content,
                    Ok(None) => continue,
                    Err(reason) => {
                        outcome.rejected(slot, &relative, reason);
                        continue;
                    }
                };
                match write_back(
                    slot,
                    prefix,
                    &name,
                    snapshots,
                    stamp,
                    slot.manifest.get(&relative),
                    &content,
                )
                .await
                {
                    Ok(change) => outcome.written(slot, &relative, change),
                    Err(reason) => outcome.rejected(slot, &relative, reason),
                }
            }
            ScratchEntryKind::Directory => match directory.open_dir(&name).await {
                Ok(child) => {
                    Box::pin(apply_directory(
                        slot,
                        &relative,
                        &child,
                        snapshots,
                        seen,
                        depth + 1,
                        outcome,
                    ))
                    .await;
                }
                Err(_) => outcome.rejected(slot, &relative, RejectedChangeReason::Unavailable),
            },
            ScratchEntryKind::Other => {}
        }
    }
}

async fn changed_staged_content(
    directory: &ScratchDir,
    name: &str,
    stamp: FileStamp,
    manifest: Option<&ManifestEntry>,
) -> Result<Option<Vec<u8>>, RejectedChangeReason> {
    if stamp.len > MAX_STAGED_FILE_BYTES {
        let unchanged = match manifest {
            Some(manifest) if modes_match(stamp, manifest.stamp) => directory
                .file_sha256(name)
                .await
                .is_ok_and(|sha256| sha256 == manifest.sha256),
            _ => false,
        };
        return if unchanged {
            Ok(None)
        } else {
            Err(RejectedChangeReason::StagedFileTooLarge)
        };
    }
    let content = directory
        .read_file(name)
        .await
        .map_err(|_| RejectedChangeReason::Unavailable)?;
    let sha256: [u8; 32] = Sha256::digest(&content).into();
    if manifest
        .is_some_and(|manifest| sha256 == manifest.sha256 && modes_match(stamp, manifest.stamp))
    {
        Ok(None)
    } else {
        Ok(Some(content))
    }
}

#[cfg(unix)]
fn modes_match(left: FileStamp, right: FileStamp) -> bool {
    left.mode == right.mode
}

#[cfg(not(unix))]
fn modes_match(_: FileStamp, _: FileStamp) -> bool {
    true
}

async fn write_back(
    slot: &OverlaySlot,
    prefix: &str,
    name: &str,
    snapshots: Option<&dyn WriteSnapshotSink>,
    stamp: FileStamp,
    manifest: Option<&ManifestEntry>,
    content: &[u8],
) -> Result<MaterializedChangeKind, RejectedChangeReason> {
    let relative = join_relative(prefix, name);
    let target = descend(&slot.source_dir, prefix, true)
        .await
        .map_err(|_| RejectedChangeReason::Unavailable)?;
    let expected = manifest.map_or(FilePrecondition::Absent, |entry| {
        FilePrecondition::Sha256(entry.sha256)
    });
    let observed = observe_prior(&target, name).await?;
    if observed.precondition != expected {
        return Err(RejectedChangeReason::Stale);
    }
    let snapshot = match snapshots {
        Some(snapshots) => Some(
            snapshots
                .prepare(StagedChange {
                    folder: &slot.source,
                    relative: &relative,
                    prior: observed.prior,
                    next: Some(content),
                })
                .await
                .map_err(|_| RejectedChangeReason::SnapshotUnavailable)?,
        ),
        None => None,
    };
    // The staged file carries the mode the folder's copy had, so writing it
    // back does not turn a script into a non-executable file or narrow who can
    // read a document the user shares.
    #[cfg(unix)]
    let mode = Some(stamp.mode);
    #[cfg(not(unix))]
    let mode = {
        let _ = stamp;
        None
    };
    let applied = target
        .write_file_with_mode_if_matches(name, content, mode, expected)
        .await
        .map_err(|_| RejectedChangeReason::Unavailable)?;
    if !applied {
        return Err(RejectedChangeReason::Stale);
    }
    if let Some(snapshot) = snapshot {
        snapshot.applied();
    }
    Ok(if manifest.is_some() {
        MaterializedChangeKind::Overwritten
    } else {
        MaterializedChangeKind::Created
    })
}

struct ObservedPrior {
    prior: PriorContents,
    precondition: FilePrecondition,
}

/// Read what the user's folder holds for `name` and derive the exact
/// precondition from those same bytes.
async fn observe_prior(
    target: &ScratchDir,
    name: &str,
) -> Result<ObservedPrior, RejectedChangeReason> {
    match target.file_stamp(name).await {
        None => match target.open_file(name).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ObservedPrior {
                prior: PriorContents::Absent,
                precondition: FilePrecondition::Absent,
            }),
            _ => Err(RejectedChangeReason::SnapshotUnavailable),
        },
        Some(stamp) if stamp.len > MAX_EXEC_SNAPSHOT_BYTES => {
            let sha256 = target
                .file_sha256(name)
                .await
                .map_err(|_| RejectedChangeReason::SnapshotUnavailable)?;
            Ok(ObservedPrior {
                prior: PriorContents::TooLarge {
                    byte_len: stamp.len,
                },
                precondition: FilePrecondition::Sha256(sha256),
            })
        }
        Some(_) => {
            let bytes = match target.read_file(name).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(ObservedPrior {
                        prior: PriorContents::Absent,
                        precondition: FilePrecondition::Absent,
                    });
                }
                Err(_) => return Err(RejectedChangeReason::SnapshotUnavailable),
            };
            let sha256: [u8; 32] = Sha256::digest(&bytes).into();
            Ok(ObservedPrior {
                prior: PriorContents::Bytes(bytes),
                precondition: FilePrecondition::Sha256(sha256),
            })
        }
    }
}

/// Remove from the real folder every file the overlay started with and no
/// longer has. This is the only place anything is deleted, and it consults the
/// manifest rather than the folder, so a file that was never staged is never a
/// candidate.
async fn apply_deletions(
    slot: &OverlaySlot,
    snapshots: Option<&dyn WriteSnapshotSink>,
    seen: &[String],
    outcome: &mut OverlayOutcome,
) {
    let seen = seen.iter().collect::<std::collections::HashSet<_>>();
    for (relative, manifest) in &slot.manifest {
        if seen.contains(relative) {
            continue;
        }
        let (prefix, name) = split_relative(relative);
        let Ok(target) = descend(&slot.source_dir, prefix, false).await else {
            outcome.rejected(slot, relative, RejectedChangeReason::Unavailable);
            continue;
        };
        let expected = FilePrecondition::Sha256(manifest.sha256);
        let observed = match observe_prior(&target, name).await {
            Ok(observed) if observed.precondition == expected => observed,
            Ok(_) => {
                outcome.rejected(slot, relative, RejectedChangeReason::Stale);
                continue;
            }
            Err(reason) => {
                outcome.rejected(slot, relative, reason);
                continue;
            }
        };
        let snapshot = match snapshots {
            Some(snapshots) => match snapshots
                .prepare(StagedChange {
                    folder: &slot.source,
                    relative,
                    prior: observed.prior,
                    next: None,
                })
                .await
            {
                Ok(snapshot) => Some(snapshot),
                Err(_) => {
                    outcome.rejected(slot, relative, RejectedChangeReason::SnapshotUnavailable);
                    continue;
                }
            },
            None => None,
        };
        match target.remove_file_if_matches(name, expected).await {
            Ok(true) => {
                if let Some(snapshot) = snapshot {
                    snapshot.applied();
                }
                outcome.written(slot, relative, MaterializedChangeKind::Deleted);
            }
            Ok(false) => outcome.rejected(slot, relative, RejectedChangeReason::Stale),
            Err(_) => outcome.rejected(slot, relative, RejectedChangeReason::Unavailable),
        }
    }
}

/// Open `relative` under `directory` one pinned component at a time.
async fn descend(directory: &ScratchDir, relative: &str, create: bool) -> io::Result<ScratchDir> {
    let mut current = directory.clone();
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        current = if create {
            current.create_dir(component).await?
        } else {
            current.open_dir(component).await?
        };
    }
    Ok(current)
}

/// Copy `source` to `destination`, which must not exist.
///
/// On macOS this is `clonefile`, so an APFS volume shares the bytes until one
/// side writes and the copy costs directory metadata rather than the folder's
/// contents. Elsewhere — and if the clone is refused, which is what a
/// cross-volume or non-APFS source does — it falls back to reading and writing
/// the tree. Symlinks are copied as symlinks and never followed.
async fn clone_tree(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if clonefile(source, destination).await.is_ok() {
            return Ok(());
        }
    }
    copy_tree(source.to_path_buf(), destination.to_path_buf()).await
}

#[cfg(target_os = "macos")]
async fn clonefile(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    /// `CLONE_NOFOLLOW` from `<sys/clonefile.h>`.
    const CLONE_NOFOLLOW: u32 = 0x0001;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    tokio::task::spawn_blocking(move || {
        // SAFETY: both arguments are NUL-terminated C strings that outlive the
        // call, and `clonefile` reads them without retaining either.
        let result =
            unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), CLONE_NOFOLLOW) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
    .await
    .map_err(|_| io::Error::other("clone did not complete"))?
}

async fn copy_tree(source: PathBuf, destination: PathBuf) -> io::Result<()> {
    tokio::task::spawn_blocking(move || copy_tree_blocking(&source, &destination, 0))
        .await
        .map_err(|_| io::Error::other("copy did not complete"))?
}

fn copy_tree_blocking(source: &Path, destination: &Path, depth: usize) -> io::Result<()> {
    if depth > MAX_OVERLAY_DEPTH {
        return Err(io::Error::other("granted folder nests too deeply"));
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree_blocking(&entry.path(), &target, depth + 1)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
        // A symlink is left out rather than recreated. The fallback path only
        // runs where cloning is unavailable, and a dangling or escaping link is
        // not worth reproducing to serve it.
    }
    Ok(())
}

/// A readable, collision-free directory name for one granted root.
fn slot_name(source: &Path, used: &[String]) -> String {
    let base = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.contains('/') && *name != "." && *name != "..")
        .unwrap_or("folder");
    if !used.iter().any(|existing| existing == base) {
        return base.to_owned();
    }
    (2..)
        .map(|suffix| format!("{base}-{suffix}"))
        .find(|candidate| !used.iter().any(|existing| existing == candidate))
        .expect("an unused suffix always exists")
}

fn join_relative(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

fn split_relative(relative: &str) -> (&str, &str) {
    relative
        .rsplit_once('/')
        .map_or(("", relative), |(prefix, name)| (prefix, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn overlay_for(source: &Path) -> (tempfile::TempDir, WriteOverlay) {
        let scratch = tempfile::tempdir().unwrap();
        let overlay = WriteOverlay::prepare(scratch.path(), "chat", &[source.to_path_buf()])
            .await
            .expect("a readable granted folder stages");
        (scratch, overlay)
    }

    /// The invariant every later slice is built on: exec writing a granted path
    /// changes the overlay and leaves the user's folder alone until the turn
    /// ends, and a read of that path in between returns what was just written
    /// rather than the original.
    #[tokio::test]
    async fn a_write_lands_in_the_overlay_and_shadows_the_original_until_the_turn_ends() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::write(granted.path().join("notes.md"), "original").unwrap();
        std::fs::create_dir(granted.path().join("nested")).unwrap();
        std::fs::write(granted.path().join("nested/data.csv"), "a,b").unwrap();

        let (_scratch, overlay) = overlay_for(granted.path()).await;
        let staged = overlay.slots()[0].overlay().to_path_buf();

        // The copy is the whole tree, so the agent reads through it without
        // anything falling back to the real folder.
        assert_eq!(
            std::fs::read_to_string(staged.join("notes.md")).unwrap(),
            "original"
        );
        assert_eq!(
            std::fs::read_to_string(staged.join("nested/data.csv")).unwrap(),
            "a,b"
        );

        // What exec does during the turn: overwrite, create, delete.
        std::fs::write(staged.join("notes.md"), "revised").unwrap();
        std::fs::write(staged.join("nested/new.txt"), "fresh").unwrap();
        std::fs::remove_file(staged.join("nested/data.csv")).unwrap();

        // Read-back inside the turn sees the overlay, not the original.
        assert_eq!(
            std::fs::read_to_string(staged.join("notes.md")).unwrap(),
            "revised"
        );
        // None of it has reached the user's folder yet.
        assert_eq!(
            std::fs::read_to_string(granted.path().join("notes.md")).unwrap(),
            "original"
        );
        assert!(granted.path().join("nested/data.csv").exists());

        let outcome = overlay.materialize(None).await;
        assert_eq!(outcome.rejected, Vec::new());
        assert_eq!(outcome.written.len(), 3);
        assert_eq!(
            outcome
                .written
                .iter()
                .map(|file| (file.relative.as_str(), file.change))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("nested/data.csv", MaterializedChangeKind::Deleted),
                ("nested/new.txt", MaterializedChangeKind::Created),
                ("notes.md", MaterializedChangeKind::Overwritten),
            ])
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("notes.md")).unwrap(),
            "revised"
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("nested/new.txt")).unwrap(),
            "fresh"
        );
        assert!(!granted.path().join("nested/data.csv").exists());

        // A file written back keeps the mode it had rather than becoming a
        // private host-owned file the user can no longer share.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(granted.path().join("notes.md"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644);
        }
    }

    /// Deletions are judged against what the overlay started with, so a file
    /// the user adds mid-turn survives and an untouched file is not rewritten.
    #[tokio::test]
    async fn a_file_the_overlay_never_saw_is_left_alone() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::write(granted.path().join("kept.txt"), "kept").unwrap();

        let (_scratch, overlay) = overlay_for(granted.path()).await;
        std::fs::write(granted.path().join("arrived-later.txt"), "user").unwrap();

        let outcome = overlay.materialize(None).await;
        assert_eq!(outcome, OverlayOutcome::default());
        assert_eq!(
            std::fs::read_to_string(granted.path().join("arrived-later.txt")).unwrap(),
            "user"
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("kept.txt")).unwrap(),
            "kept"
        );
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Recorded {
        relative: String,
        prior: PriorContents,
        next: Option<Vec<u8>>,
    }

    /// A sink that records everything, and can be told to refuse one path.
    struct RecordingSink {
        refuse: Option<String>,
        seen: std::sync::Arc<std::sync::Mutex<Vec<Recorded>>>,
    }

    struct RecordingReceipt {
        seen: std::sync::Arc<std::sync::Mutex<Vec<Recorded>>>,
        recorded: Recorded,
    }

    impl PreparedWriteSnapshot for RecordingReceipt {
        fn applied(self: Box<Self>) {
            let Self { seen, recorded } = *self;
            seen.lock().unwrap().push(recorded);
        }
    }

    #[async_trait::async_trait]
    impl WriteSnapshotSink for RecordingSink {
        async fn prepare(
            &self,
            change: StagedChange<'_>,
        ) -> Result<Box<dyn PreparedWriteSnapshot>, String> {
            if self.refuse.as_deref() == Some(change.relative) {
                return Err("cannot retain".into());
            }
            Ok(Box::new(RecordingReceipt {
                seen: std::sync::Arc::clone(&self.seen),
                recorded: Recorded {
                    relative: change.relative.to_owned(),
                    prior: change.prior.clone(),
                    next: change.next.map(<[u8]>::to_vec),
                },
            }))
        }
    }

    /// The bytes a change destroys reach the journal before the folder is
    /// touched, and a change whose prior bytes could not be kept is not applied
    /// at all. An overwrite we cannot reverse is the failure this path exists to
    /// prevent, so refusing it has to beat completing it.
    #[tokio::test]
    async fn prior_bytes_are_offered_before_each_change_and_a_refusal_blocks_it() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::write(granted.path().join("notes.md"), "original").unwrap();
        std::fs::write(granted.path().join("gone.txt"), "doomed").unwrap();
        std::fs::write(granted.path().join("precious.md"), "irreplaceable").unwrap();

        let (_scratch, overlay) = overlay_for(granted.path()).await;
        let staged = overlay.slots()[0].overlay().to_path_buf();
        std::fs::write(staged.join("notes.md"), "revised").unwrap();
        std::fs::write(staged.join("fresh.txt"), "new").unwrap();
        std::fs::write(staged.join("precious.md"), "clobbered").unwrap();
        std::fs::remove_file(staged.join("gone.txt")).unwrap();

        let sink = RecordingSink {
            refuse: Some("precious.md".into()),
            seen: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        let outcome = overlay.materialize(Some(&sink)).await;
        assert_eq!(outcome.written.len(), 3);
        assert_eq!(
            outcome.rejected,
            vec![RejectedChange {
                folder: granted.path().to_path_buf(),
                relative: "precious.md".to_owned(),
                reason: RejectedChangeReason::SnapshotUnavailable,
            }]
        );

        let mut seen = std::mem::take(&mut *sink.seen.lock().unwrap());
        seen.sort_by(|left, right| left.relative.cmp(&right.relative));
        assert_eq!(
            seen,
            vec![
                Recorded {
                    relative: "fresh.txt".to_owned(),
                    prior: PriorContents::Absent,
                    next: Some(b"new".to_vec()),
                },
                Recorded {
                    relative: "gone.txt".to_owned(),
                    prior: PriorContents::Bytes(b"doomed".to_vec()),
                    next: None,
                },
                Recorded {
                    relative: "notes.md".to_owned(),
                    prior: PriorContents::Bytes(b"original".to_vec()),
                    next: Some(b"revised".to_vec()),
                },
            ]
        );

        // The refused file keeps the bytes the user had.
        assert_eq!(
            std::fs::read_to_string(granted.path().join("precious.md")).unwrap(),
            "irreplaceable"
        );
    }

    /// A user edit after the overlay is prepared wins every conflict: an
    /// overwrite, a delete, and a create collision are each rejected rather
    /// than replacing bytes the agent never saw.
    #[tokio::test]
    async fn stale_digest_preconditions_leave_user_files_untouched() {
        let granted = tempfile::tempdir().unwrap();
        std::fs::write(granted.path().join("notes.md"), "original").unwrap();
        std::fs::write(granted.path().join("gone.txt"), "original").unwrap();
        let original_modified = std::fs::metadata(granted.path().join("notes.md"))
            .unwrap()
            .modified()
            .unwrap();

        let (_scratch, overlay) = overlay_for(granted.path()).await;
        let staged = overlay.slots()[0].overlay().to_path_buf();
        std::fs::write(staged.join("notes.md"), "agent___").unwrap();
        std::fs::remove_file(staged.join("gone.txt")).unwrap();
        std::fs::write(staged.join("fresh.txt"), "agent").unwrap();

        // Keep both the length and mtime equal to the manifest. A stamp-only
        // precondition accepts this; the content digest must be what rejects
        // the overwrite.
        std::fs::write(granted.path().join("notes.md"), "useredit").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(granted.path().join("notes.md"))
            .unwrap()
            .set_modified(original_modified)
            .unwrap();
        std::fs::write(granted.path().join("gone.txt"), "user edit").unwrap();
        std::fs::write(granted.path().join("fresh.txt"), "user file").unwrap();

        let outcome = overlay.materialize(None).await;
        assert_eq!(outcome.written, Vec::new());
        assert_eq!(
            outcome
                .rejected
                .iter()
                .map(|file| (file.relative.as_str(), file.reason))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("fresh.txt", RejectedChangeReason::Stale),
                ("gone.txt", RejectedChangeReason::Stale),
                ("notes.md", RejectedChangeReason::Stale),
            ])
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("notes.md")).unwrap(),
            "useredit"
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("gone.txt")).unwrap(),
            "user edit"
        );
        assert_eq!(
            std::fs::read_to_string(granted.path().join("fresh.txt")).unwrap(),
            "user file"
        );
    }

    struct RacingSink {
        target: PathBuf,
        applied: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    struct RacingReceipt(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl PreparedWriteSnapshot for RacingReceipt {
        fn applied(self: Box<Self>) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl WriteSnapshotSink for RacingSink {
        async fn prepare(
            &self,
            _: StagedChange<'_>,
        ) -> Result<Box<dyn PreparedWriteSnapshot>, String> {
            // Simulate the user's editor saving after the prior bytes were read
            // and snapshotted but before the staged bytes are published.
            std::fs::write(&self.target, "user edit").unwrap();
            Ok(Box::new(RacingReceipt(std::sync::Arc::clone(
                &self.applied,
            ))))
        }
    }

    /// Snapshot publication can take real time. The destination is checked
    /// again afterwards, at the mutation boundary, and a receipt for a write
    /// that lost that race never enters the journal.
    #[tokio::test]
    async fn a_change_between_snapshot_and_publication_is_rejected_and_not_journaled() {
        let granted = tempfile::tempdir().unwrap();
        let target = granted.path().join("notes.md");
        std::fs::write(&target, "original").unwrap();

        let (_scratch, overlay) = overlay_for(granted.path()).await;
        std::fs::write(overlay.slots()[0].overlay().join("notes.md"), "agent").unwrap();

        let applied = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sink = RacingSink {
            target: target.clone(),
            applied: std::sync::Arc::clone(&applied),
        };
        let outcome = overlay.materialize(Some(&sink)).await;

        assert_eq!(outcome.written, Vec::new());
        assert_eq!(
            outcome.rejected,
            vec![RejectedChange {
                folder: granted.path().to_path_buf(),
                relative: "notes.md".to_owned(),
                reason: RejectedChangeReason::Stale,
            }]
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "user edit");
        assert!(!applied.load(std::sync::atomic::Ordering::SeqCst));
    }
}

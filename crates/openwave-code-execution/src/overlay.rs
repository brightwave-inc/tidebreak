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
//! At the end of the turn the overlay is compared against the manifest recorded
//! when it was made — not against whatever the real folder holds now. A file
//! whose length and modification time still match the manifest was never
//! touched and is left alone. A file that differs is written back. A file the
//! manifest lists and the overlay no longer has was deleted, and is deleted
//! from the folder too.
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

use crate::host_paths::{
    resolve_scratch_directory, FileStamp, ScratchDir, ScratchEntry, ScratchEntryKind,
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
    manifest: BTreeMap<String, FileStamp>,
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

/// What applying one overlay did, for the caller to report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OverlayOutcome {
    pub written: usize,
    pub deleted: usize,
    pub refused: usize,
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
    /// Errors are per file: one refused write does not abandon the rest, since
    /// leaving the remainder unapplied would strand work the agent believes it
    /// finished.
    pub async fn materialize(self) -> OverlayOutcome {
        let mut outcome = OverlayOutcome::default();
        for slot in &self.slots {
            let mut seen = Vec::new();
            apply_directory(slot, "", &slot.overlay_dir, &mut seen, 0, &mut outcome).await;
            apply_deletions(slot, &seen, &mut outcome).await;
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
/// deleted from the folder, and the worst it costs is writing identical bytes
/// back over itself.
async fn record_manifest(
    directory: &ScratchDir,
    prefix: &str,
    manifest: &mut BTreeMap<String, FileStamp>,
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
                if let Some(stamp) = directory.file_stamp(&name).await {
                    manifest.insert(relative, stamp);
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
    seen: &mut Vec<String>,
    depth: usize,
    outcome: &mut OverlayOutcome,
) {
    if depth > MAX_OVERLAY_DEPTH {
        return;
    }
    let Ok(entries) = directory.entries().await else {
        outcome.refused += 1;
        return;
    };
    for ScratchEntry { name, kind } in entries {
        let relative = join_relative(prefix, &name);
        match kind {
            ScratchEntryKind::File => {
                seen.push(relative.clone());
                let stamp = directory.file_stamp(&name).await;
                if stamp.is_some() && stamp == slot.manifest.get(&relative).copied() {
                    continue;
                }
                if stamp.is_some_and(|stamp| stamp.len > MAX_STAGED_FILE_BYTES) {
                    outcome.refused += 1;
                    continue;
                }
                match write_back(slot, prefix, &name, directory, stamp).await {
                    Ok(()) => outcome.written += 1,
                    Err(_) => outcome.refused += 1,
                }
            }
            ScratchEntryKind::Directory => match directory.open_dir(&name).await {
                Ok(child) => {
                    Box::pin(apply_directory(
                        slot,
                        &relative,
                        &child,
                        seen,
                        depth + 1,
                        outcome,
                    ))
                    .await;
                }
                Err(_) => outcome.refused += 1,
            },
            ScratchEntryKind::Other => {}
        }
    }
}

async fn write_back(
    slot: &OverlaySlot,
    prefix: &str,
    name: &str,
    directory: &ScratchDir,
    stamp: Option<FileStamp>,
) -> io::Result<()> {
    let content = directory.read_file(name).await?;
    let target = descend(&slot.source_dir, prefix, true).await?;
    // The staged file carries the mode the folder's copy had, so writing it
    // back does not turn a script into a non-executable file or narrow who can
    // read a document the user shares.
    #[cfg(unix)]
    let mode = stamp.map(|stamp| stamp.mode);
    #[cfg(not(unix))]
    let mode = {
        let _ = stamp;
        None
    };
    target.write_file_with_mode(name, &content, mode).await
}

/// Remove from the real folder every file the overlay started with and no
/// longer has. This is the only place anything is deleted, and it consults the
/// manifest rather than the folder, so a file that was never staged is never a
/// candidate.
async fn apply_deletions(slot: &OverlaySlot, seen: &[String], outcome: &mut OverlayOutcome) {
    let seen = seen.iter().collect::<std::collections::HashSet<_>>();
    for relative in slot.manifest.keys() {
        if seen.contains(relative) {
            continue;
        }
        let (prefix, name) = split_relative(relative);
        let Ok(target) = descend(&slot.source_dir, prefix, false).await else {
            outcome.refused += 1;
            continue;
        };
        match target.remove(name, ScratchEntryKind::File).await {
            Ok(()) => outcome.deleted += 1,
            // Already gone is the outcome we wanted, not a refusal.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => outcome.refused += 1,
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

        let outcome = overlay.materialize().await;
        assert_eq!(
            outcome,
            OverlayOutcome {
                written: 2,
                deleted: 1,
                refused: 0
            }
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

        let outcome = overlay.materialize().await;
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
}

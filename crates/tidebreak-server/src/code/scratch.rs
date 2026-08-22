//! `.tidebreak/`: the scratch directory Tidebreak writes into a worktree.
//!
//! Two things go here so far — a fork's transcript and a turn's attachments —
//! and both exist for the same reason: an engine reads a file off disk with
//! the tools it already has, so anything Tidebreak can put on disk reaches
//! every engine without a protocol for it.
//!
//! The directory hides itself. A `.gitignore` holding `*` covers the tree and
//! the marker with it, which keeps these files out of `git status` without
//! touching the repository's own tracked ignore file or the shared
//! `.git/info/exclude` that every other worktree of the same repository reads.

use std::path::{Path, PathBuf};

/// Root of the scratch tree, relative to the worktree.
pub(crate) const SCRATCH_DIR: &str = ".tidebreak";

/// Create the scratch tree, self-ignoring, and hand back an absolute path to
/// one directory inside it.
///
/// `relative` is the whole path from the worktree root, so callers name their
/// own subdirectory constant and this stays the only place that knows about
/// the marker.
pub(crate) async fn scratch_dir(worktree: &Path, relative: &str) -> std::io::Result<PathBuf> {
    let dir = worktree.join(relative);
    tokio::fs::create_dir_all(&dir).await?;
    ignore_scratch_dir(worktree).await?;
    Ok(dir)
}

/// Make `.tidebreak/` ignore itself.
async fn ignore_scratch_dir(worktree: &Path) -> std::io::Result<()> {
    let scratch: PathBuf = worktree.join(SCRATCH_DIR);
    let marker = scratch.join(".gitignore");
    if tokio::fs::try_exists(&marker).await? {
        return Ok(());
    }
    tokio::fs::write(&marker, "*\n").await
}

/// Put bytes at `path` in one step, leaving nothing behind if it fails.
///
/// A reader can already be on the file — a re-fork writes over a transcript
/// the last child is reading — so the write is published by rename and a
/// reader sees one whole version or another, never the middle of one. The
/// staged name carries a fresh id rather than a fixed `.part` suffix, so
/// concurrent writers stage separately and neither can be caught writing over
/// the other's half-written file.
pub(crate) async fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the published path names no file"))?;
    let staged = path.with_file_name(format!(
        "{}.{}.part",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let written = tokio::fs::write(&staged, bytes).await;
    let published = match written {
        Ok(()) => tokio::fs::rename(&staged, path).await,
        Err(err) => Err(err),
    };
    if published.is_err() {
        let _ = tokio::fs::remove_file(&staged).await;
    }
    published
}

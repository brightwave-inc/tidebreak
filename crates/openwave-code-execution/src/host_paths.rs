//! Host-side path resolution that never traverses a planted symlink.
//!
//! Local exec runs inside a chat's private scratch directory and can create
//! entries there, including a symlink aimed anywhere on the host. Everything
//! the host itself does to that same directory — creating the conventional
//! subdirectories, installing the bundled document helpers, mirroring a
//! workspace back — runs unsandboxed, and `create_dir_all` and a plain `write`
//! both follow a symlinked *parent*. So the host resolves one component at a
//! time, refuses anything that is not already a real directory, and writes
//! through a no-follow create rather than by path.

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;

/// Why a host directory under the private scratch tree was refused.
///
/// The distinction is the point: a planted symlink and a permissions failure
/// are the same `None` to a caller, but only one of them is the boundary being
/// probed, and collapsing them makes an attack read as noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchRefusal {
    /// A component tried to leave the root, or the resolved directory landed
    /// outside it. Nothing legitimate aims there.
    Escape,
    /// An intermediate component is a symlink. Following it is exactly the
    /// escape this resolver exists to refuse.
    SymlinkedComponent,
    /// An intermediate component exists and is neither a directory nor a
    /// symlink — a regular file where a directory was expected.
    NotADirectory,
    /// The directory is missing and could not be created, or could not be
    /// canonicalized: a permissions failure, or a lost race with another
    /// writer. Says nothing about the caller's intent.
    Unavailable,
}

impl ScratchRefusal {
    /// Whether the refusal is the containment boundary doing its job rather
    /// than the host failing at an ordinary filesystem operation.
    pub fn is_containment(self) -> bool {
        matches!(self, Self::Escape | Self::SymlinkedComponent)
    }
}

/// Resolve `relative` as a directory under `root` without walking through a
/// symlink at any component, discarding why a refusal happened.
///
/// Callers that report or log the refusal should use
/// [`try_resolve_scratch_directory`] instead.
pub async fn resolve_scratch_directory(
    root: &Path,
    relative: &str,
    create: bool,
) -> Option<PathBuf> {
    try_resolve_scratch_directory(root, relative, create)
        .await
        .ok()
}

/// Resolve `relative` as a directory under `root` without walking through a
/// symlink at any component.
///
/// Each intermediate component must already be a real directory; a symlink or
/// a regular file refuses. With `create`, a missing component is created one
/// level at a time, so a component planted between two host runs is seen
/// rather than followed. The canonical result must still sit inside the
/// canonical `root`.
pub async fn try_resolve_scratch_directory(
    root: &Path,
    relative: &str,
    create: bool,
) -> Result<PathBuf, ScratchRefusal> {
    // macOS puts chat scratch under a symlinked `/var`, so containment is
    // judged against the canonical root rather than the path as handed in.
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| ScratchRefusal::Unavailable)?;
    let mut dir = root.clone();
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        if component == "." || component == ".." {
            return Err(ScratchRefusal::Escape);
        }
        dir.push(component);
        match tokio::fs::symlink_metadata(&dir).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(metadata) if metadata.is_symlink() => {
                return Err(ScratchRefusal::SymlinkedComponent)
            }
            Ok(_) => return Err(ScratchRefusal::NotADirectory),
            Err(_) if create => tokio::fs::create_dir(&dir)
                .await
                .map_err(|_| ScratchRefusal::Unavailable)?,
            Err(_) => return Err(ScratchRefusal::Unavailable),
        }
    }
    let resolved = tokio::fs::canonicalize(&dir)
        .await
        .map_err(|_| ScratchRefusal::Unavailable)?;
    if !resolved.starts_with(&root) {
        return Err(ScratchRefusal::Escape);
    }
    Ok(resolved)
}

/// Write `content` at `host_path` without following a symlink at the final
/// component either: the bytes go to an unpredictable temp name opened with an
/// exclusive no-follow create, then a rename puts them in place. This is the
/// same shape the workspace-put path in `local.rs` uses.
pub async fn write_without_following(host_path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = host_path
        .parent()
        .ok_or_else(|| std::io::Error::other("host path has no parent"))?;
    let temporary = parent.join(format!(".openwave-write.{}", uuid::Uuid::new_v4()));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temporary).await?;
    let write = async {
        file.write_all(content).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temporary, host_path).await
    };
    match write.await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            Err(error)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_symlinked_component_refuses_instead_of_being_followed() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("planted")).unwrap();

        assert!(resolve_scratch_directory(root.path(), "planted", true)
            .await
            .is_none());
        assert!(
            resolve_scratch_directory(root.path(), "planted/deeper", true)
                .await
                .is_none()
        );
        assert!(!outside.path().join("deeper").exists());

        let made = resolve_scratch_directory(root.path(), "real/nested", true)
            .await
            .unwrap();
        assert!(made.is_dir());
        assert!(made.starts_with(root.path().canonicalize().unwrap()));
    }
}

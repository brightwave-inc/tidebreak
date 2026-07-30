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

/// Resolve `relative` as a directory under `root` without walking through a
/// symlink at any component.
///
/// Each intermediate component must already be a real directory; a symlink or
/// a regular file refuses. With `create`, a missing component is created one
/// level at a time, so a component planted between two host runs is seen
/// rather than followed. The canonical result must still sit inside the
/// canonical `root`. `None` means the path did not resolve inside the scratch
/// directory, or the directories could not be made.
pub async fn resolve_scratch_directory(
    root: &Path,
    relative: &str,
    create: bool,
) -> Option<PathBuf> {
    // macOS puts chat scratch under a symlinked `/var`, so containment is
    // judged against the canonical root rather than the path as handed in.
    let root = tokio::fs::canonicalize(root).await.ok()?;
    let mut dir = root.clone();
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        if component == "." || component == ".." {
            return None;
        }
        dir.push(component);
        match tokio::fs::symlink_metadata(&dir).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return None,
            Err(_) if create => tokio::fs::create_dir(&dir).await.ok()?,
            Err(_) => return None,
        }
    }
    let resolved = tokio::fs::canonicalize(&dir).await.ok()?;
    if !resolved.starts_with(&root) {
        return None;
    }
    Some(resolved)
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

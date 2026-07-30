//! Host-side path resolution that never traverses a planted symlink.
//!
//! Local exec runs inside a chat's private scratch directory and can create
//! entries there, including a symlink aimed anywhere on the host. Everything
//! the host itself does to that same directory — creating the conventional
//! subdirectories, installing the bundled document helpers, mirroring a
//! workspace back — runs unsandboxed, and `create_dir_all` and a plain `write`
//! both follow a symlinked *parent*.
//!
//! Resolving a path and then using that path is not enough, because the two
//! steps are separated by real time. A resolver that checks the components and
//! hands back a `PathBuf` invites every later `open` and `rename` to walk the
//! tree again from `/`, and a process with write access to scratch can rename
//! the verified directory aside and drop a symlink in its place in between.
//! `O_NOFOLLOW` does not help: it guards the final component, and the escape
//! happens at an intermediate one.
//!
//! So resolution here hands back an open descriptor, not a path. Each
//! component is opened relative to its parent with symlink following disabled,
//! and every subsequent read, write, and rename is issued relative to that
//! descriptor. The directory a caller acts on is then the exact directory that
//! was checked — renaming that name afterwards renames a directory the
//! descriptor no longer refers to. [`ScratchDir`] deliberately exposes no way
//! to recover the path it points at, so containment cannot be re-lost by a
//! caller reaching for `join`.

use std::io;
use std::path::Path;
use std::sync::Arc;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

/// Why a host directory under the private scratch tree was refused.
///
/// The distinction is the point: a planted symlink and a permissions failure
/// are the same `None` to a caller, but only one of them is the boundary being
/// probed, and collapsing them makes an attack read as noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchRefusal {
    /// A component tried to leave the root. Nothing legitimate aims there.
    Escape,
    /// An intermediate component is a symlink. Following it is exactly the
    /// escape this resolver exists to refuse.
    SymlinkedComponent,
    /// An intermediate component exists and is neither a directory nor a
    /// symlink — a regular file where a directory was expected.
    NotADirectory,
    /// The directory is missing and could not be created, or could not be
    /// opened: a permissions failure, or a lost race with another writer. Says
    /// nothing about the caller's intent.
    Unavailable,
}

impl ScratchRefusal {
    /// Whether the refusal is the containment boundary doing its job rather
    /// than the host failing at an ordinary filesystem operation.
    pub fn is_containment(self) -> bool {
        matches!(self, Self::Escape | Self::SymlinkedComponent)
    }
}

/// A directory inside a chat's private scratch tree, held open as a descriptor.
///
/// Obtained from [`try_resolve_scratch_directory`], which proves containment
/// while it walks. Every operation is relative to the pinned descriptor, so no
/// operation re-resolves the path and none can be steered elsewhere after the
/// walk.
#[derive(Clone)]
pub struct ScratchDir {
    directory: Arc<Dir>,
}

impl std::fmt::Debug for ScratchDir {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ScratchDir").finish_non_exhaustive()
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
) -> Option<ScratchDir> {
    try_resolve_scratch_directory(root, relative, create)
        .await
        .ok()
}

/// Resolve `relative` as a directory under `root` without walking through a
/// symlink at any component, and keep it open.
///
/// Each intermediate component must already be a real directory; a symlink or
/// a regular file refuses. With `create`, a missing component is created one
/// level at a time, so a component planted between two host runs is seen
/// rather than followed.
///
/// `root` itself is opened with ambient authority and its symlinks are
/// followed — macOS puts chat scratch under a symlinked `/var`, and the root is
/// host-owned rather than sandbox-writable. Everything below it is descriptor
/// relative, which is what makes containment structural: no later step consults
/// a path, so there is nothing left to compare against the root.
pub async fn try_resolve_scratch_directory(
    root: &Path,
    relative: &str,
    create: bool,
) -> Result<ScratchDir, ScratchRefusal> {
    let root = root.to_path_buf();
    let relative = relative.to_owned();
    let directory = tokio::task::spawn_blocking(move || resolve_blocking(&root, &relative, create))
        .await
        .map_err(|_| ScratchRefusal::Unavailable)??;
    Ok(ScratchDir {
        directory: Arc::new(directory),
    })
}

fn resolve_blocking(root: &Path, relative: &str, create: bool) -> Result<Dir, ScratchRefusal> {
    let mut directory = Dir::open_ambient_dir(root, ambient_authority())
        .map_err(|_| ScratchRefusal::Unavailable)?;
    for component in relative.split('/').filter(|part| !part.is_empty()) {
        if component == "." || component == ".." {
            return Err(ScratchRefusal::Escape);
        }
        directory = match directory.open_dir_nofollow(component) {
            Ok(child) => child,
            // The open is what refuses; the stat below only decides which
            // refusal to report, and it too is relative to the pinned parent.
            Err(_) => match directory.symlink_metadata(component) {
                Ok(metadata) if metadata.is_symlink() => {
                    return Err(ScratchRefusal::SymlinkedComponent)
                }
                Ok(metadata) if !metadata.is_dir() => return Err(ScratchRefusal::NotADirectory),
                Ok(_) => return Err(ScratchRefusal::Unavailable),
                Err(_) if create => {
                    // A racing writer that lands the name first makes this fail
                    // with `EEXIST`: closed, never adopted and never followed.
                    directory
                        .create_dir(component)
                        .map_err(|_| ScratchRefusal::Unavailable)?;
                    directory
                        .open_dir_nofollow(component)
                        .map_err(|_| ScratchRefusal::Unavailable)?
                }
                Err(_) => return Err(ScratchRefusal::Unavailable),
            },
        };
    }
    Ok(directory)
}

impl ScratchDir {
    /// Write `content` at `name` inside this directory without following a
    /// symlink at the final component either: the bytes go to an unpredictable
    /// temp name opened with an exclusive no-follow create, then a rename puts
    /// them in place. Both operations are relative to the pinned descriptor.
    pub async fn write_file(&self, name: &str, content: &[u8]) -> io::Result<()> {
        let name = single_component(name)?;
        let content = content.to_vec();
        self.blocking(move |directory| write_blocking(directory, &name, &content))
            .await
    }

    /// Read `name` inside this directory, refusing a symlink at the final
    /// component rather than following it.
    pub async fn read_file(&self, name: &str) -> io::Result<Vec<u8>> {
        let file = self.open_file(name).await?;
        tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut file = file;
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            Ok(content)
        })
        .await
        .map_err(|_| io::Error::other("scratch read did not complete"))?
    }

    /// Open `name` inside this directory for reading, refusing a symlink at the
    /// final component. The returned handle is already pinned, so judging it by
    /// `metadata()` reads the file that was opened rather than re-statting a
    /// path that may since have changed.
    pub async fn open_file(&self, name: &str) -> io::Result<std::fs::File> {
        let name = single_component(name)?;
        self.blocking(move |directory| {
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            Ok(directory.open_with(&name, &options)?.into_std())
        })
        .await
    }

    /// Whether `name` inside this directory is a symlink. Used only to explain
    /// a refusal — the no-follow open and write are what enforce it.
    pub async fn is_symlink(&self, name: &str) -> bool {
        let Ok(name) = single_component(name) else {
            return false;
        };
        self.blocking(move |directory| {
            Ok(directory
                .symlink_metadata(&name)
                .is_ok_and(|metadata| metadata.is_symlink()))
        })
        .await
        .unwrap_or(false)
    }

    async fn blocking<T, F>(&self, work: F) -> io::Result<T>
    where
        F: FnOnce(&Dir) -> io::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let directory = Arc::clone(&self.directory);
        tokio::task::spawn_blocking(move || work(&directory))
            .await
            .map_err(|_| io::Error::other("scratch operation did not complete"))?
    }
}

fn write_blocking(directory: &Dir, name: &str, content: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    // A process with write access to chat scratch could plant a symlink at a
    // guessable temp path, so the name is unpredictable and the create is
    // exclusive and no-follow: it fails rather than following anything that is
    // already there.
    let temporary = format!(".openwave-write.{}", uuid::Uuid::new_v4());
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = directory.open_with(&temporary, &options)?;
    let write = file
        .write_all(content)
        .and_then(|()| file.sync_all())
        .and_then(|()| directory.rename(&temporary, directory, name));
    if write.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    write
}

/// Names addressed inside a pinned directory must be a single ordinary
/// component. A nested path would be walked by the filesystem rather than by
/// [`try_resolve_scratch_directory`], which is exactly the resolution this
/// module exists to take over.
fn single_component(name: &str) -> io::Result<String> {
    let separated = name.contains('/') || (cfg!(windows) && name.contains(['\\', ':']));
    if name.is_empty() || name == "." || name == ".." || separated {
        return Err(io::Error::other("scratch entry name is not one component"));
    }
    Ok(name.to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_symlinked_component_refuses_instead_of_being_followed() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("planted")).unwrap();
        std::fs::write(root.path().join("file"), b"not a directory").unwrap();
        let root_path = root.path();

        let refusal = |relative: &'static str| async move {
            try_resolve_scratch_directory(root_path, relative, true)
                .await
                .err()
        };
        assert_eq!(
            refusal("planted/deeper").await,
            Some(ScratchRefusal::SymlinkedComponent)
        );
        assert_eq!(
            refusal("file/deeper").await,
            Some(ScratchRefusal::NotADirectory)
        );
        assert_eq!(refusal("../escape").await, Some(ScratchRefusal::Escape));
        assert!(!outside.path().join("deeper").exists());

        let made = resolve_scratch_directory(root.path(), "real/nested", true)
            .await
            .unwrap();
        made.write_file("f.txt", b"inside").await.unwrap();
        assert_eq!(
            std::fs::read(root.path().join("real/nested/f.txt")).unwrap(),
            b"inside"
        );
    }

    /// The property the pinning buys, without racing anything: swap the
    /// directory out from under a handle that has already been resolved, then
    /// write through the handle. A resolver that returned a path would rewalk
    /// the tree and land in the attacker's target; a pinned descriptor still
    /// refers to the directory that was checked.
    #[tokio::test]
    async fn a_resolved_directory_swapped_for_a_symlink_is_still_the_directory_checked() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("authorized_keys"), b"host secret").unwrap();
        let root = tempfile::tempdir().unwrap();

        let pinned = resolve_scratch_directory(root.path(), "out", true)
            .await
            .unwrap();

        // Exactly the move the resolve-then-use-by-path shape loses to: the
        // verified directory is renamed aside and a symlink takes its name.
        std::fs::rename(root.path().join("out"), root.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("out")).unwrap();

        pinned
            .write_file("authorized_keys", b"attacker key")
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(outside.path().join("authorized_keys")).unwrap(),
            b"host secret"
        );
        assert_eq!(
            std::fs::read(root.path().join("moved/authorized_keys")).unwrap(),
            b"attacker key"
        );
    }

    #[tokio::test]
    async fn reads_and_writes_refuse_a_symlink_at_the_final_component() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"host secret").unwrap();
        let root = tempfile::tempdir().unwrap();
        let pinned = resolve_scratch_directory(root.path(), "", false)
            .await
            .unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("link"))
            .unwrap();

        assert!(pinned.is_symlink("link").await);
        assert!(pinned.read_file("link").await.is_err());
        // The write replaces the link itself rather than writing through it.
        pinned.write_file("link", b"replacement").await.unwrap();
        assert_eq!(
            std::fs::read(outside.path().join("secret")).unwrap(),
            b"host secret"
        );
    }
}

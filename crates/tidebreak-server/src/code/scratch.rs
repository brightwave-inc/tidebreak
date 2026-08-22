//! `.tidebreak/`: files an engine reads from its worktree.
//!
//! Every path is opened relative to a pinned worktree directory. Each child
//! directory and final file refuses symlinks, so a repository cannot redirect
//! private bytes outside the checkout.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path};

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use tokio::io::AsyncWriteExt as _;

/// Root of the scratch tree, relative to the worktree.
pub(crate) const SCRATCH_DIR: &str = ".tidebreak";

/// An open scratch directory whose path components were all opened without
/// following symlinks.
pub(crate) struct ScratchDir {
    dir: Dir,
}

impl ScratchDir {
    /// Publish one file in this directory by atomic rename.
    pub(crate) async fn publish(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        publish_file(&self.dir, name, bytes).await
    }

    /// Read this directory through its pinned capability.
    pub(crate) fn read_dir(&self) -> io::Result<cap_std::fs::ReadDir> {
        self.dir.read_dir(".")
    }

    /// Inspect one direct child without following a symlink.
    pub(crate) fn symlink_metadata(&self, name: &OsStr) -> io::Result<cap_std::fs::Metadata> {
        self.dir.symlink_metadata(name)
    }

    /// Remove one direct child directory without escaping this capability.
    pub(crate) fn remove_dir_all(&self, name: &OsStr) -> io::Result<()> {
        self.dir.remove_dir_all(name)
    }

    /// Remove one direct child file or symlink.
    pub(crate) fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        self.dir.remove_file(name)
    }
}

/// A turn-specific scratch directory that removes itself when its owner exits.
///
/// The explicit cleanup reports failures on normal exits. `Drop` is the
/// cancellation and panic fallback, and runs before the caller releases the
/// worktree turn lock because the scope is created after that lock.
pub(crate) struct ScratchScope {
    dir: ScratchDir,
    session_parent: Dir,
    session_name: OsString,
    turn_name: OsString,
    cleaned: bool,
}

impl ScratchScope {
    pub(crate) async fn publish(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        self.dir.publish(name, bytes).await
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        let result = remove_scope(&self.session_parent, &self.session_name, &self.turn_name);
        if result.is_ok() {
            self.cleaned = true;
        }
        result
    }

    /// Keep a fully published scope. Its lifecycle owner removes it later.
    pub(crate) fn keep(mut self) {
        self.cleaned = true;
    }
}

impl Drop for ScratchScope {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = remove_scope(&self.session_parent, &self.session_name, &self.turn_name);
        }
    }
}

/// Open or create one directory below `.tidebreak`, refusing symlinks at
/// every component.
pub(crate) fn scratch_dir(worktree: &Path, relative: &str) -> io::Result<ScratchDir> {
    let components = scratch_components(relative)?;
    let worktree = open_worktree(worktree)?;
    let scratch = ensure_child_dir(&worktree, OsStr::new(SCRATCH_DIR))?;
    ignore_scratch_dir(&scratch)?;
    let mut current = scratch;
    for component in components.into_iter().skip(1) {
        current = ensure_child_dir(&current, &component)?;
    }
    Ok(ScratchDir { dir: current })
}

/// Open an existing scratch directory without creating any path component.
pub(crate) fn scratch_dir_if_exists(
    worktree: &Path,
    relative: &str,
) -> io::Result<Option<ScratchDir>> {
    let components = scratch_components(relative)?;
    let mut current = open_worktree(worktree)?;
    for component in components {
        let metadata = match current.symlink_metadata(&component) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "scratch path component is not a regular directory",
            ));
        }
        current = current.open_dir_nofollow(&component)?;
    }
    Ok(Some(ScratchDir { dir: current }))
}

/// Create a session-and-turn-specific directory below `relative`.
pub(crate) fn scratch_scope(
    worktree: &Path,
    relative: &str,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
) -> io::Result<ScratchScope> {
    let root = scratch_dir(worktree, relative)?;
    let session_name: OsString = session_id.to_string().into();
    let turn_name: OsString = turn_id.to_string().into();
    let session = ensure_child_dir(&root.dir, &session_name)?;
    let turn = ensure_child_dir(&session, &turn_name)?;
    Ok(ScratchScope {
        dir: ScratchDir { dir: turn },
        session_parent: root.dir,
        session_name,
        turn_name,
        cleaned: false,
    })
}

/// Remove attachment scopes left by a crashed worker.
///
/// Only UUID-named session directories and legacy UUID-named image files are
/// Tidebreak-owned. Unknown entries stay untouched. Symlinks are unlinked,
/// never followed.
pub(crate) fn sweep_scopes(worktree: &Path, relative: &str) -> io::Result<()> {
    let Some(root) = scratch_dir_if_exists(worktree, relative)? else {
        return Ok(());
    };
    for entry in root.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = root.symlink_metadata(&name)?;
        if metadata.file_type().is_symlink() {
            if is_scope_name(&name) || is_legacy_attachment_name(&name) {
                root.remove_file(&name)?;
            }
            continue;
        }
        if metadata.is_dir() && is_scope_name(&name) {
            root.remove_dir_all(&name)?;
        } else if metadata.is_file() && is_legacy_attachment_name(&name) {
            root.remove_file(&name)?;
        }
    }
    Ok(())
}

fn scratch_components(relative: &str) -> io::Result<Vec<OsString>> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch path must be relative",
        ));
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => components.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "scratch path contains an invalid component",
                ));
            }
        }
    }
    if components.first().is_none_or(|name| name != SCRATCH_DIR) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch path must start with .tidebreak",
        ));
    }
    Ok(components)
}

fn open_worktree(path: &Path) -> io::Result<Dir> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worktree is not a regular directory",
        ));
    }
    let directory = Dir::open_ambient_dir(path, ambient_authority())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory.dir_metadata()?;
        if metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worktree changed while it was opened",
            ));
        }
    }
    Ok(directory)
}

fn ensure_child_dir(parent: &Dir, name: &OsStr) -> io::Result<Dir> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "scratch path component is not a regular directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent.create_dir(name)?;
        }
        Err(error) => return Err(error),
    }
    parent.open_dir_nofollow(name)
}

fn ignore_scratch_dir(scratch: &Dir) -> io::Result<()> {
    // A repository may already track an unsafe marker. Replace every regular
    // marker before creating child storage so Git ignores every Tidebreak-owned
    // child.
    publish_file_blocking(scratch, OsStr::new(".gitignore"), b"*\n")
}

async fn publish_file(directory: &Dir, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
    validate_file_name(name)?;
    reject_non_regular_destination(directory, name)?;
    let staged: OsString =
        format!(".{}.{}.part", name.to_string_lossy(), uuid::Uuid::new_v4()).into();
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let file = directory.open_with(&staged, &options)?;
    if !file.metadata()?.is_file() {
        let _ = directory.remove_file(&staged);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "staged scratch path is not a regular file",
        ));
    }
    let mut file = tokio::fs::File::from_std(file.into_std());
    let result = async {
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        directory.rename(&staged, directory, name)
    }
    .await;
    if result.is_err() {
        let _ = directory.remove_file(&staged);
    }
    result
}

fn publish_file_blocking(directory: &Dir, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    validate_file_name(name)?;
    reject_non_regular_destination(directory, name)?;
    let staged: OsString =
        format!(".{}.{}.part", name.to_string_lossy(), uuid::Uuid::new_v4()).into();
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let result = (|| {
        let mut file = directory.open_with(&staged, &options)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "staged scratch path is not a regular file",
            ));
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        directory.rename(&staged, directory, name)
    })();
    if result.is_err() {
        let _ = directory.remove_file(&staged);
    }
    result
}

fn reject_non_regular_destination(directory: &Dir, name: &OsStr) -> io::Result<()> {
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "published scratch path is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_file_name(name: &OsStr) -> io::Result<()> {
    let path = Path::new(name);
    if path.components().count() != 1 || path.file_name() != Some(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "published scratch path must name one file",
        ));
    }
    Ok(())
}

fn remove_scope(parent: &Dir, session_name: &OsStr, turn_name: &OsStr) -> io::Result<()> {
    let session = match parent.open_dir_nofollow(session_name) {
        Ok(session) => session,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match session.symlink_metadata(turn_name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            session.remove_dir_all(turn_name)?;
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            session.remove_file(turn_name)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "turn scratch path is not a regular directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match parent.remove_dir(session_name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_scope_name(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| uuid::Uuid::parse_str(name).ok())
        .is_some()
}

fn is_legacy_attachment_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "png" | "jpg" | "webp" | "gif") && uuid::Uuid::parse_str(stem).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(worktree: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(worktree)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    }

    fn assert_git_success(worktree: &Path, args: &[&str]) {
        let output = git(worktree, args);
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlinked_scratch_root() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), worktree.path().join(SCRATCH_DIR)).unwrap();

        let result = scratch_dir(worktree.path(), ".tidebreak/attachments");

        assert!(result.is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlinked_nested_directory() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(worktree.path().join(SCRATCH_DIR)).unwrap();
        std::os::unix::fs::symlink(
            outside.path(),
            worktree.path().join(".tidebreak/attachments"),
        )
        .unwrap();

        let result = scratch_dir(worktree.path(), ".tidebreak/attachments");

        assert!(result.is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_dangling_gitignore_symlink() {
        let worktree = tempfile::tempdir().unwrap();
        std::fs::create_dir(worktree.path().join(SCRATCH_DIR)).unwrap();
        std::os::unix::fs::symlink(
            worktree.path().join("missing"),
            worktree.path().join(".tidebreak/.gitignore"),
        )
        .unwrap();

        assert!(scratch_dir(worktree.path(), ".tidebreak/attachments").is_err());
        assert!(!worktree.path().join("missing").exists());
    }

    #[tokio::test]
    async fn repairs_a_tracked_unsafe_marker_before_publishing_private_files() {
        let worktree = tempfile::tempdir().unwrap();
        assert_git_success(worktree.path(), &["init", "-b", "main"]);
        std::fs::create_dir(worktree.path().join(SCRATCH_DIR)).unwrap();
        std::fs::write(worktree.path().join(".tidebreak/.gitignore"), b"").unwrap();
        assert_git_success(worktree.path(), &["add", ".tidebreak/.gitignore"]);

        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let scope = scratch_scope(
            worktree.path(),
            ".tidebreak/attachments",
            session_id,
            turn_id,
        )
        .unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        scope.keep();

        assert_eq!(
            std::fs::read(worktree.path().join(".tidebreak/.gitignore")).unwrap(),
            b"*\n"
        );
        assert_git_success(worktree.path(), &["add", "-A"]);
        let indexed = git(
            worktree.path(),
            &["ls-files", "--cached", "--", ".tidebreak/attachments"],
        );
        assert!(indexed.status.success());
        assert!(
            indexed.stdout.is_empty(),
            "private scratch files entered the Git index: {}",
            String::from_utf8_lossy(&indexed.stdout)
        );
    }

    #[tokio::test]
    async fn scope_cleanup_removes_only_its_session_tree() {
        let worktree = tempfile::tempdir().unwrap();
        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let scope = scratch_scope(
            worktree.path(),
            ".tidebreak/attachments",
            session_id,
            turn_id,
        )
        .unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        let turn_path = worktree
            .path()
            .join(".tidebreak/attachments")
            .join(session_id.to_string())
            .join(turn_id.to_string());
        assert_eq!(
            std::fs::read(turn_path.join("image.png")).unwrap(),
            b"private"
        );

        let mut scope = scope;
        scope.cleanup().unwrap();

        assert!(!turn_path.exists());
        assert!(!turn_path.parent().unwrap().exists());
        assert!(worktree.path().join(".tidebreak/attachments").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_cleanup_can_be_retried_without_losing_the_scope() {
        use std::os::unix::fs::PermissionsExt as _;

        let worktree = tempfile::tempdir().unwrap();
        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let mut scope = scratch_scope(
            worktree.path(),
            ".tidebreak/attachments",
            session_id,
            turn_id,
        )
        .unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        let session_path = worktree
            .path()
            .join(".tidebreak/attachments")
            .join(session_id.to_string());
        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o500)).unwrap();

        assert!(scope.cleanup().is_err());
        assert!(session_path.join(turn_id.to_string()).exists());

        std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        scope.cleanup().unwrap();
        assert!(!session_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_unlinks_scoped_symlinks_without_touching_their_targets() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), b"keep").unwrap();
        let root = worktree.path().join(".tidebreak/attachments");
        std::fs::create_dir_all(&root).unwrap();
        let session_id = uuid::Uuid::new_v4();
        std::os::unix::fs::symlink(outside.path(), root.join(session_id.to_string())).unwrap();

        sweep_scopes(worktree.path(), ".tidebreak/attachments").unwrap();

        assert!(!root.join(session_id.to_string()).exists());
        assert_eq!(
            std::fs::read(outside.path().join("keep.txt")).unwrap(),
            b"keep"
        );
    }
}

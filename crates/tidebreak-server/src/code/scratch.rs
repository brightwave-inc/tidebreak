//! Private files that an engine reads outside its Git worktree.
//!
//! Every path is opened relative to the profile data directory. Each child
//! directory and final file refuses symlinks, so repository content cannot
//! redirect private bytes into a path that Git can index.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use tidebreak_core::WorkspaceId;
use tokio::io::AsyncWriteExt as _;

const CODE_DIR: &str = "code";
const PRIVATE_DIR: &str = "private";

/// A workspace's private storage root, pinned to the directory that passed
/// validation.
///
/// `path` is only the name that engines receive for reading published files.
/// Every Tidebreak mutation stays relative to `dir`, so replacing that name or
/// one of its ancestors cannot redirect the operation.
#[derive(Clone)]
pub(crate) struct ScratchRoot {
    dir: Arc<Dir>,
    path: PathBuf,
}

impl ScratchRoot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn open_for_test(path: &Path) -> io::Result<Self> {
        let path = absolute_path(path)?;
        Ok(Self {
            dir: Arc::new(open_root(&path)?),
            path,
        })
    }
}

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
    dir: Option<ScratchDir>,
    session_parent: Dir,
    session_name: OsString,
    turn_name: OsString,
    cleaned: bool,
}

impl ScratchScope {
    pub(crate) async fn publish(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let dir = self.dir.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "scratch scope is already closed")
        })?;
        dir.publish(name, bytes).await
    }

    pub(crate) fn cleanup(&mut self) -> io::Result<()> {
        drop(self.dir.take());
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
            drop(self.dir.take());
            let _ = remove_scope(&self.session_parent, &self.session_name, &self.turn_name);
        }
    }
}

/// Create the private root for one workspace under the profile data directory.
pub(crate) fn workspace_root(
    data_dir: &Path,
    workspace_id: WorkspaceId,
) -> io::Result<ScratchRoot> {
    let data_dir = absolute_path(data_dir)?;
    let root = open_root(&data_dir)?;
    reject_git_indexable_root(&std::fs::canonicalize(&data_dir)?)?;
    let code = ensure_child_dir(&root, OsStr::new(CODE_DIR))?;
    let private = ensure_child_dir(&code, OsStr::new(PRIVATE_DIR))?;
    let workspace_name = workspace_id.to_string();
    let workspace = ensure_child_dir(&private, OsStr::new(&workspace_name))?;
    Ok(ScratchRoot {
        dir: Arc::new(workspace),
        path: data_dir
            .join(CODE_DIR)
            .join(PRIVATE_DIR)
            .join(workspace_name),
    })
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn reject_git_indexable_root(root: &Path) -> io::Result<()> {
    for ancestor in root.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == ".git") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private storage cannot be inside a Git worktree",
            ));
        }
        match std::fs::symlink_metadata(ancestor.join(".git")) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private storage cannot be inside a Git worktree",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Open or create one directory below a workspace's private root.
pub(crate) fn scratch_dir(root: &ScratchRoot, relative: &str) -> io::Result<ScratchDir> {
    let components = scratch_components(relative)?;
    let mut current = root.dir.try_clone()?;
    for component in components {
        current = ensure_child_dir(&current, &component)?;
    }
    Ok(ScratchDir { dir: current })
}

/// Open an existing scratch directory without creating any path component.
pub(crate) fn scratch_dir_if_exists(
    root: &ScratchRoot,
    relative: &str,
) -> io::Result<Option<ScratchDir>> {
    let components = scratch_components(relative)?;
    let mut current = root.dir.try_clone()?;
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
    root: &ScratchRoot,
    relative: &str,
    session_id: uuid::Uuid,
    turn_id: uuid::Uuid,
) -> io::Result<ScratchScope> {
    let root = scratch_dir(root, relative)?;
    let session_name: OsString = session_id.to_string().into();
    let turn_name: OsString = turn_id.to_string().into();
    let session = ensure_child_dir(&root.dir, &session_name)?;
    let turn = ensure_child_dir(&session, &turn_name)?;
    Ok(ScratchScope {
        dir: Some(ScratchDir { dir: turn }),
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
pub(crate) fn sweep_scopes(root: &ScratchRoot, relative: &str) -> io::Result<()> {
    let Some(root) = scratch_dir_if_exists(root, relative)? else {
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
    if components.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scratch path must name a private directory",
        ));
    }
    Ok(components)
}

fn open_root(path: &Path) -> io::Result<Dir> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "private storage root is not a regular directory",
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
                "private storage root changed while it was opened",
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
            #[cfg(unix)]
            {
                use cap_std::fs::{DirBuilder, DirBuilderExt as _};

                let mut builder = DirBuilder::new();
                builder.mode(0o700);
                parent.create_dir_with(name, &builder)?;
            }
            #[cfg(not(unix))]
            parent.create_dir(name)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt as _};

        parent.set_permissions(name, Permissions::from_mode(0o700))?;
    }
    parent.open_dir_nofollow(name)
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
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
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
    drop(session);
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

    fn test_root(path: &Path) -> ScratchRoot {
        ScratchRoot::open_for_test(path).expect("scratch root")
    }

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
        let parent = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = parent.path().join("private");
        std::os::unix::fs::symlink(outside.path(), &root).unwrap();

        let result = ScratchRoot::open_for_test(&root);

        assert!(result.is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_symlinked_nested_directory() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("attachments")).unwrap();
        let root = test_root(directory.path());

        let result = scratch_dir(&root, "attachments");

        assert!(result.is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_rejects_a_symlinked_private_parent() {
        let data_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), data_dir.path().join(CODE_DIR)).unwrap();

        assert!(workspace_root(data_dir.path(), WorkspaceId::new()).is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_rejects_a_symlinked_git_marker() {
        let worktree = tempfile::tempdir().unwrap();
        let git_dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(git_dir.path(), worktree.path().join(".git")).unwrap();
        let data_dir = worktree.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();

        let error = workspace_root(&data_dir, WorkspaceId::new())
            .err()
            .expect("symlinked Git marker should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_directories_and_files_reject_group_and_world_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let data_dir = tempfile::tempdir().unwrap();
        let private_root = workspace_root(data_dir.path(), WorkspaceId::new()).unwrap();
        let scope = scratch_scope(
            &private_root,
            "attachments",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        )
        .unwrap();
        scope
            .publish(OsStr::new("private.png"), b"private")
            .await
            .unwrap();

        let private_mode = std::fs::metadata(private_root.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let attachments = private_root.path().join("attachments");
        let attachment_mode = std::fs::metadata(&attachments)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let session = std::fs::read_dir(&attachments)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let turn = std::fs::read_dir(&session)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let file_mode = std::fs::metadata(turn.join("private.png"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(private_mode, 0o700);
        assert_eq!(attachment_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }

    #[test]
    fn workspace_root_rejects_a_repository_local_data_directory() {
        let worktree = tempfile::tempdir().unwrap();
        assert_git_success(worktree.path(), &["init", "-b", "main"]);
        let data_dir = worktree.path().join(".tidebreak");
        std::fs::create_dir(&data_dir).unwrap();

        let error = workspace_root(&data_dir, WorkspaceId::new())
            .err()
            .expect("repository-local data directory should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_git_success(worktree.path(), &["add", "-A"]);
        let indexed = git(
            worktree.path(),
            &["ls-files", "--cached", "--", ".tidebreak/code/private"],
        );
        assert!(indexed.status.success());
        assert!(indexed.stdout.is_empty());
    }

    #[tokio::test]
    async fn a_tracked_unsafe_marker_cannot_make_private_files_indexable() {
        let worktree = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        assert_git_success(worktree.path(), &["init", "-b", "main"]);
        std::fs::create_dir(worktree.path().join(".tidebreak")).unwrap();
        std::fs::write(worktree.path().join(".tidebreak/.gitignore"), b"").unwrap();
        assert_git_success(worktree.path(), &["add", ".tidebreak/.gitignore"]);
        std::fs::write(worktree.path().join(".tidebreak/.gitignore"), b"*\n").unwrap();

        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let private_root = test_root(private.path());
        let scope = scratch_scope(&private_root, "attachments", session_id, turn_id).unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        scope.keep();

        assert_git_success(worktree.path(), &["restore", ".tidebreak/.gitignore"]);
        assert_git_success(worktree.path(), &["add", "-A"]);
        let indexed = git(
            worktree.path(),
            &["ls-files", "--cached", "--", ".tidebreak/attachments"],
        );
        assert!(indexed.status.success());
        assert!(
            indexed.stdout.is_empty(),
            "a private file entered the Git index: {}",
            String::from_utf8_lossy(&indexed.stdout)
        );
        assert_eq!(
            std::fs::read(
                private
                    .path()
                    .join("attachments")
                    .join(session_id.to_string())
                    .join(turn_id.to_string())
                    .join("image.png")
            )
            .unwrap(),
            b"private"
        );
    }

    #[tokio::test]
    async fn scope_cleanup_removes_only_its_session_tree() {
        let private = tempfile::tempdir().unwrap();
        let private_root = test_root(private.path());
        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let scope = scratch_scope(&private_root, "attachments", session_id, turn_id).unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        let turn_path = private
            .path()
            .join("attachments")
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
        assert!(private.path().join("attachments").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_cleanup_can_be_retried_without_losing_the_scope() {
        use std::os::unix::fs::PermissionsExt as _;

        let private = tempfile::tempdir().unwrap();
        let private_root = test_root(private.path());
        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let mut scope = scratch_scope(&private_root, "attachments", session_id, turn_id).unwrap();
        scope
            .publish(OsStr::new("image.png"), b"private")
            .await
            .unwrap();
        let session_path = private
            .path()
            .join("attachments")
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
        let private = tempfile::tempdir().unwrap();
        let private_root = test_root(private.path());
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep.txt"), b"keep").unwrap();
        let root = private.path().join("attachments");
        std::fs::create_dir_all(&root).unwrap();
        let session_id = uuid::Uuid::new_v4();
        std::os::unix::fs::symlink(outside.path(), root.join(session_id.to_string())).unwrap();

        sweep_scopes(&private_root, "attachments").unwrap();

        assert!(!root.join(session_id.to_string()).exists());
        assert_eq!(
            std::fs::read(outside.path().join("keep.txt")).unwrap(),
            b"keep"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_root_stays_bound_after_an_ancestor_is_replaced() {
        let parent = tempfile::tempdir().unwrap();
        let storage = parent.path().join("storage");
        let held = parent.path().join("held-storage");
        let redirected = parent.path().join("redirected-storage");
        std::fs::create_dir_all(storage.join("data")).unwrap();
        std::fs::create_dir_all(redirected.join("data")).unwrap();
        let workspace_id = WorkspaceId::new();
        let private_root = workspace_root(&storage.join("data"), workspace_id).unwrap();

        std::fs::rename(&storage, &held).unwrap();
        std::os::unix::fs::symlink(&redirected, &storage).unwrap();
        let redirected_workspace = redirected
            .join("data")
            .join(CODE_DIR)
            .join(PRIVATE_DIR)
            .join(workspace_id.to_string());
        std::fs::create_dir_all(&redirected_workspace).unwrap();

        let session_id = uuid::Uuid::new_v4();
        let turn_id = uuid::Uuid::new_v4();
        let scope = scratch_scope(&private_root, "attachments", session_id, turn_id).unwrap();
        scope
            .publish(OsStr::new("private.png"), b"private")
            .await
            .unwrap();
        scope.keep();

        let relative = Path::new("data")
            .join(CODE_DIR)
            .join(PRIVATE_DIR)
            .join(workspace_id.to_string())
            .join("attachments")
            .join(session_id.to_string())
            .join(turn_id.to_string())
            .join("private.png");
        assert_eq!(std::fs::read(held.join(&relative)).unwrap(), b"private");
        assert!(!redirected.join(relative).exists());
    }
}

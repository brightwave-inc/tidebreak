//! Saving one text file in a local workspace's worktree from the file viewer.
//!
//! The viewer loads a file through `GET /code/workspaces/{id}/blob`, which
//! returns its text and a hash of its bytes. A save names that hash as its
//! base and lands only while the file on disk still has it. An agent that
//! edited the same file in the meantime turns the save into
//! [`FileSaveError::Changed`], carrying the hash on disk now, so neither side
//! loses work without being asked.
//!
//! The path resolves exactly the way the read resolves it
//! ([`worktree::locate_worktree_file`]). On top of that, a save refuses what
//! the viewer never offers to edit: anything under `.git`, a file the viewer
//! reads as binary or cuts short, and text that is not UTF-8. The new bytes go
//! to a temporary file beside the original, take the original's permissions,
//! and replace it with one rename, so no reader sees half a file.
//!
//! The hash check and the rename are not one atomic step against other
//! processes. An agent that writes the file in the few milliseconds between
//! them loses that write. Closing the gap would take a lock every engine
//! honors, and none of them take one.

use std::io::Write as _;
use std::path::{Component, Path};

use tokio::io::AsyncReadExt;

use super::worktree::{self, WorktreeFileRefusal};

/// The most one save may write: the viewer's own cap, so every file the
/// editor writes is one the viewer shows whole.
pub const MAX_SAVE_BYTES: usize = worktree::MAX_BLOB_BYTES;

/// The file a save wrote, and the hash the next save names as its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedWorktreeFile {
    pub path: String,
    pub hash: String,
}

/// Why a save did not land. The file on disk is untouched in every case.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileSaveError {
    /// The path names nothing the editor may write: it is not relative, it
    /// leads outside the worktree, or it reaches into `.git`.
    #[error("{0}")]
    PathRefused(String),
    /// The path names a directory or another thing that is not a file.
    #[error("{0}")]
    NotAFile(String),
    /// Nothing is at the path. A save never creates a file.
    #[error("{0}")]
    NotFound(String),
    /// The file on disk is not one the viewer edits: binary, not UTF-8, over
    /// the size cap, or read-only.
    #[error("{0}")]
    NotEditable(String),
    /// The new text is over [`MAX_SAVE_BYTES`].
    #[error("{0}")]
    TooLarge(String),
    /// The new text would read back as binary.
    #[error("{0}")]
    ContentNotText(String),
    /// The base is not a hash the blob read could have returned.
    #[error("{0}")]
    InvalidBase(String),
    /// The file on disk no longer matches the base the editor loaded.
    #[error("{path} changed on disk since you opened it.")]
    Changed { path: String, current_hash: String },
    #[error("{0}")]
    Internal(String),
}

/// Replace one worktree file's text, if the file still matches `base_hash`.
///
/// `content` is written byte for byte, line endings included. See the module
/// documentation for what is refused and why.
pub async fn save_worktree_file(
    worktree_path: &Path,
    relative: &str,
    content: &str,
    base_hash: &str,
) -> Result<SavedWorktreeFile, FileSaveError> {
    if !is_content_hash(base_hash) {
        return Err(FileSaveError::InvalidBase(
            "The save does not name the version it replaces. Reload the file and try again."
                .to_owned(),
        ));
    }
    if content.len() > MAX_SAVE_BYTES {
        return Err(FileSaveError::TooLarge(format!(
            "The new text is over {}, the most the editor saves. Open the file in your editor instead.",
            size_label(MAX_SAVE_BYTES)
        )));
    }
    if content.contains('\0') {
        return Err(FileSaveError::ContentNotText(
            "The new text contains a NUL character, which would make the file binary.".to_owned(),
        ));
    }

    let located = worktree::locate_worktree_file(worktree_path, relative)
        .await
        .map_err(refused)?;
    let inside = located
        .canonical
        .strip_prefix(&located.canonical_root)
        .unwrap_or(&located.canonical);
    if names_git_metadata(&located.rel) || names_git_metadata(inside) {
        return Err(FileSaveError::PathRefused(
            "Files under .git are not editable here.".to_owned(),
        ));
    }
    let label = located.rel.to_string_lossy().replace('\\', "/");

    let current = read_current(&located.canonical, &label).await?;
    let current_hash = worktree::content_hash(&current);
    if current_hash != base_hash {
        return Err(FileSaveError::Changed {
            path: label,
            current_hash,
        });
    }
    // A base that matches can still name a file the viewer never offered to
    // edit, if the caller computed it rather than read it.
    if current.contains(&0) {
        return Err(FileSaveError::NotEditable(format!(
            "{label} is binary, so you cannot edit it here."
        )));
    }
    if std::str::from_utf8(&current).is_err() {
        return Err(FileSaveError::NotEditable(format!(
            "{label} is not UTF-8 text, so you cannot edit it here."
        )));
    }
    let metadata = tokio::fs::metadata(&located.canonical)
        .await
        .map_err(|err| io_failure(&label, &err))?;
    if metadata.permissions().readonly() {
        return Err(FileSaveError::NotEditable(format!(
            "{label} is read-only on disk."
        )));
    }

    write_replacing(
        &located.canonical,
        content.as_bytes().to_vec(),
        metadata.permissions(),
        &label,
    )
    .await?;
    Ok(SavedWorktreeFile {
        path: label,
        hash: worktree::content_hash(content.as_bytes()),
    })
}

/// Whether `value` has the shape [`worktree::content_hash`] produces.
pub fn is_content_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn refused(refusal: WorktreeFileRefusal) -> FileSaveError {
    match refusal {
        WorktreeFileRefusal::NotRelative(_) => FileSaveError::PathRefused(
            "Name a file inside the worktree with a relative path.".to_owned(),
        ),
        WorktreeFileRefusal::Outside => FileSaveError::PathRefused(
            "That path leads outside the worktree, so you cannot edit it here.".to_owned(),
        ),
        WorktreeFileRefusal::Missing(rel) => FileSaveError::NotFound(format!(
            "{} no longer exists.",
            rel.to_string_lossy().replace('\\', "/")
        )),
        WorktreeFileRefusal::NotAFile(rel) => FileSaveError::NotAFile(format!(
            "{} is not a file.",
            rel.to_string_lossy().replace('\\', "/")
        )),
        WorktreeFileRefusal::Unreadable(message) => FileSaveError::Internal(message),
    }
}

/// Whether any component of `path` is `.git`, in any letter case, since the
/// default macOS and Windows file systems ignore case.
fn names_git_metadata(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(name)
            if name.to_str().is_some_and(|name| name.eq_ignore_ascii_case(".git")))
    })
}

/// The file's bytes, refused when they run past the cap: the viewer showed
/// such a file cut short, so it never offered to edit it.
async fn read_current(path: &Path, label: &str) -> Result<Vec<u8>, FileSaveError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|err| io_failure(label, &err))?;
    let mut bytes = Vec::new();
    let read = file
        .take((MAX_SAVE_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|err| io_failure(label, &err))?;
    if read > MAX_SAVE_BYTES {
        return Err(FileSaveError::NotEditable(format!(
            "{label} is over {}, the most the editor saves. Open it in your editor instead.",
            size_label(MAX_SAVE_BYTES)
        )));
    }
    Ok(bytes)
}

/// Write `bytes` to a temporary file beside `target`, give it `permissions`,
/// and rename it over `target`.
///
/// The rename replaces the directory entry, so a hard link elsewhere keeps the
/// old bytes rather than taking an edit meant for this path.
async fn write_replacing(
    target: &Path,
    bytes: Vec<u8>,
    permissions: std::fs::Permissions,
    label: &str,
) -> Result<(), FileSaveError> {
    let target = target.to_owned();
    let label = label.to_owned();
    tokio::task::spawn_blocking(move || {
        let Some(parent) = target.parent() else {
            return Err(FileSaveError::Internal(format!(
                "Could not save {label}: it has no parent directory."
            )));
        };
        let mut temp = tempfile::Builder::new()
            .prefix(".tidebreak-save-")
            .tempfile_in(parent)
            .map_err(|err| io_failure(&label, &err))?;
        temp.write_all(&bytes)
            .map_err(|err| io_failure(&label, &err))?;
        temp.as_file()
            .set_permissions(permissions)
            .map_err(|err| io_failure(&label, &err))?;
        temp.as_file()
            .sync_all()
            .map_err(|err| io_failure(&label, &err))?;
        temp.persist(&target)
            .map_err(|err| io_failure(&label, &err.error))?;
        sync_directory(parent);
        Ok(())
    })
    .await
    .map_err(|err| FileSaveError::Internal(format!("The save stopped: {err}")))?
}

/// Make the rename itself durable. Best effort: the data is already synced,
/// and a failure here leaves a file that is either the old one or the new.
#[cfg(unix)]
fn sync_directory(dir: &Path) {
    if let Ok(dir) = std::fs::File::open(dir) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) {}

fn io_failure(label: &str, err: &std::io::Error) -> FileSaveError {
    match err.kind() {
        std::io::ErrorKind::PermissionDenied => {
            FileSaveError::NotEditable(format!("You do not have permission to write {label}."))
        }
        std::io::ErrorKind::NotFound => {
            FileSaveError::NotFound(format!("{label} no longer exists."))
        }
        _ => FileSaveError::Internal(format!("Could not save {label}: {err}")),
    }
}

fn size_label(bytes: usize) -> String {
    format!("{} KB", bytes / 1_024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A directory standing in for a worktree, with one text file in it.
    fn worktree_with(name: &str, text: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(name), text).unwrap();
        dir
    }

    fn hash_of(text: &str) -> String {
        worktree::content_hash(text.as_bytes())
    }

    #[tokio::test]
    async fn a_save_writes_the_text_byte_for_byte_and_returns_the_next_base() {
        let dir = worktree_with("notes.md", "# Notes\n");
        let loaded = worktree::read_worktree_file(dir.path(), "notes.md")
            .await
            .unwrap();
        let base = loaded.hash.expect("a whole UTF-8 file carries a hash");

        let text = "# Notes\r\n\r\nWindows line endings stay.\r\n";
        let saved = save_worktree_file(dir.path(), "notes.md", text, &base)
            .await
            .unwrap();

        assert_eq!(saved.path, "notes.md");
        assert_eq!(saved.hash, hash_of(text));
        assert_eq!(
            std::fs::read(dir.path().join("notes.md")).unwrap(),
            text.as_bytes()
        );
        let reread = worktree::read_worktree_file(dir.path(), "notes.md")
            .await
            .unwrap();
        assert_eq!(reread.hash.as_deref(), Some(saved.hash.as_str()));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("notes.md")]);
    }

    #[tokio::test]
    async fn a_stale_base_is_refused_with_the_hash_on_disk_and_leaves_the_file() {
        let dir = worktree_with("notes.md", "what the agent wrote\n");

        let error = save_worktree_file(
            dir.path(),
            "notes.md",
            "mine\n",
            &hash_of("what I loaded\n"),
        )
        .await
        .unwrap_err();

        assert_eq!(
            error,
            FileSaveError::Changed {
                path: "notes.md".to_owned(),
                current_hash: hash_of("what the agent wrote\n"),
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "what the agent wrote\n"
        );
    }

    #[tokio::test]
    async fn paths_that_leave_the_worktree_are_refused() {
        let dir = worktree_with("notes.md", "inside\n");
        let base = hash_of("inside\n");
        for path in ["../notes.md", "/etc/hosts", "", "docs/../../notes.md"] {
            let error = save_worktree_file(dir.path(), path, "changed\n", &base)
                .await
                .unwrap_err();
            assert!(
                matches!(error, FileSaveError::PathRefused(_)),
                "{path:?}: {error:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "inside\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_that_escapes_the_worktree_is_refused() {
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.md"), "not yours\n").unwrap();
        let dir = worktree_with("notes.md", "inside\n");
        std::os::unix::fs::symlink(
            outside.path().join("secret.md"),
            dir.path().join("secret.md"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("elsewhere")).unwrap();

        for path in ["secret.md", "elsewhere/secret.md"] {
            let error =
                save_worktree_file(dir.path(), path, "overwritten\n", &hash_of("not yours\n"))
                    .await
                    .unwrap_err();
            assert!(
                matches!(error, FileSaveError::PathRefused(_)),
                "{path}: {error:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret.md")).unwrap(),
            "not yours\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_to_another_worktree_file_saves_through_to_its_target() {
        let dir = worktree_with("README.md", "target\n");
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::os::unix::fs::symlink("../README.md", dir.path().join("docs/README.md")).unwrap();

        save_worktree_file(
            dir.path(),
            "docs/README.md",
            "edited\n",
            &hash_of("target\n"),
        )
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("README.md")).unwrap(),
            "edited\n"
        );
        assert!(std::fs::symlink_metadata(dir.path().join("docs/README.md"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    async fn git_metadata_is_refused_however_it_is_reached() {
        let dir = worktree_with("notes.md", "inside\n");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]\n").unwrap();
        std::fs::create_dir_all(dir.path().join("vendor/lib/.git")).unwrap();
        std::fs::write(dir.path().join("vendor/lib/.git/HEAD"), "ref: main\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(".git", dir.path().join("metadata")).unwrap();

        let mut paths = vec![
            (".git/config", "[core]\n"),
            ("vendor/lib/.git/HEAD", "ref: main\n"),
        ];
        if cfg!(unix) {
            paths.push(("metadata/config", "[core]\n"));
        }
        for (path, text) in paths {
            let error =
                save_worktree_file(dir.path(), path, "[core]\nbare = true\n", &hash_of(text))
                    .await
                    .unwrap_err();
            assert!(
                matches!(error, FileSaveError::PathRefused(ref message) if message.contains(".git")),
                "{path}: {error:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".git/config")).unwrap(),
            "[core]\n"
        );
    }

    #[tokio::test]
    async fn directories_binary_files_and_missing_files_are_refused() {
        let dir = worktree_with("notes.md", "inside\n");
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        let binary = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        std::fs::write(dir.path().join("logo.png"), binary).unwrap();
        std::fs::write(dir.path().join("latin1.txt"), b"caf\xe9\n").unwrap();

        let error = save_worktree_file(dir.path(), "docs", "text\n", &hash_of(""))
            .await
            .unwrap_err();
        assert!(matches!(error, FileSaveError::NotAFile(_)), "{error:?}");

        let error = save_worktree_file(
            dir.path(),
            "logo.png",
            "text\n",
            &worktree::content_hash(binary),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FileSaveError::NotEditable(_)), "{error:?}");
        assert_eq!(std::fs::read(dir.path().join("logo.png")).unwrap(), binary);

        let error = save_worktree_file(
            dir.path(),
            "latin1.txt",
            "café\n",
            &worktree::content_hash(b"caf\xe9\n"),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, FileSaveError::NotEditable(_)), "{error:?}");

        let error = save_worktree_file(dir.path(), "gone.md", "text\n", &hash_of(""))
            .await
            .unwrap_err();
        assert!(matches!(error, FileSaveError::NotFound(_)), "{error:?}");
        assert!(!dir.path().join("gone.md").exists(), "a save never creates");
    }

    #[tokio::test]
    async fn text_over_the_cap_is_refused_on_either_side_of_the_save() {
        let dir = worktree_with("notes.md", "small\n");
        let huge = "x".repeat(MAX_SAVE_BYTES + 1);

        let error = save_worktree_file(dir.path(), "notes.md", &huge, &hash_of("small\n"))
            .await
            .unwrap_err();
        assert!(matches!(error, FileSaveError::TooLarge(_)), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "small\n"
        );

        std::fs::write(dir.path().join("big.log"), &huge).unwrap();
        let error = save_worktree_file(dir.path(), "big.log", "short\n", &hash_of(&huge))
            .await
            .unwrap_err();
        assert!(matches!(error, FileSaveError::NotEditable(_)), "{error:?}");
    }

    #[tokio::test]
    async fn text_that_would_read_back_as_binary_is_refused() {
        let dir = worktree_with("notes.md", "small\n");
        let error = save_worktree_file(dir.path(), "notes.md", "a\0b", &hash_of("small\n"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, FileSaveError::ContentNotText(_)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_base_that_is_not_a_hash_is_refused() {
        let dir = worktree_with("notes.md", "small\n");
        for base in ["", "abc", &hash_of("small\n").to_uppercase()] {
            let error = save_worktree_file(dir.path(), "notes.md", "new\n", base)
                .await
                .unwrap_err();
            assert!(
                matches!(error, FileSaveError::InvalidBase(_)),
                "{base}: {error:?}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_mode_survives_a_save() {
        use std::os::unix::fs::PermissionsExt;

        let dir = worktree_with("deploy.sh", "#!/bin/sh\necho old\n");
        let script = dir.path().join("deploy.sh");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).unwrap();

        save_worktree_file(
            dir.path(),
            "deploy.sh",
            "#!/bin/sh\necho new\n",
            &hash_of("#!/bin/sh\necho old\n"),
        )
        .await
        .unwrap();

        let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o750);
        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            "#!/bin/sh\necho new\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_only_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = worktree_with("LOCKED.md", "locked\n");
        let locked = dir.path().join("LOCKED.md");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();

        let error = save_worktree_file(dir.path(), "LOCKED.md", "unlocked\n", &hash_of("locked\n"))
            .await
            .unwrap_err();

        assert!(matches!(error, FileSaveError::NotEditable(_)), "{error:?}");
        assert_eq!(std::fs::read_to_string(&locked).unwrap(), "locked\n");
    }

    #[test]
    fn only_lowercase_sha256_hex_reads_as_a_content_hash() {
        assert!(is_content_hash(&hash_of("anything")));
        assert!(!is_content_hash(&hash_of("anything").to_uppercase()));
        assert!(!is_content_hash("sha256:abc"));
        assert!(!is_content_hash(&"g".repeat(64)));
    }
}

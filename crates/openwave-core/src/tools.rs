//! Built-in filesystem tools: `read_file`, `list_dir`, `write_file`.
//!
//! These are the M0 Workspace-class tools. Every path is resolved **relative to
//! the chat's workspace directory** and may not escape it (no absolute paths,
//! no `..`). Filesystem operations are relative to a pinned directory
//! capability, so symlinks and path replacement cannot escape it. Failures the
//! model should see and react to (missing file, bad path)
//! come back as [`ToolOutput::error`], not `Err` — `Err` is reserved for
//! unexpected infrastructure faults.
//!
//! Enabled by the `tools` feature.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use cap_fs_ext::OpenOptionsSyncExt;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::Result;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

const MAX_READ_FILE_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_ENTRIES: usize = 4_096;

/// Validate a workspace-relative path before handing it to the capability API.
fn workspace_relative(rel: &str) -> std::result::Result<PathBuf, String> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(format!("path must be relative to the workspace: {rel}"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(format!("path may not contain `..`: {rel}"));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("path must be relative to the workspace: {rel}"));
            }
            _ => {}
        }
    }
    Ok(path.to_path_buf())
}

fn read_utf8_file(workspace: &Dir, path: &Path) -> std::result::Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true);
    let file = workspace
        .open_with(path, &options)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".into());
    }
    if metadata.len() > MAX_READ_FILE_BYTES as u64 {
        return Err(format!(
            "file is too large (maximum {MAX_READ_FILE_BYTES} bytes)"
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_READ_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_READ_FILE_BYTES {
        return Err(format!(
            "file is too large (maximum {MAX_READ_FILE_BYTES} bytes)"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".into())
}

fn list_directory(workspace: &Dir, path: &Path) -> std::result::Result<String, String> {
    let entries = workspace
        .read_dir(path)
        .map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    let mut output_bytes = 0usize;
    for entry in entries {
        if names.len() == MAX_LIST_DIR_ENTRIES {
            return Err(format!(
                "directory has too many entries (maximum {MAX_LIST_DIR_ENTRIES})"
            ));
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
        {
            name.push('/');
        }
        output_bytes = output_bytes.saturating_add(name.len() + usize::from(!names.is_empty()));
        if output_bytes > MAX_LIST_DIR_BYTES {
            return Err(format!(
                "directory listing is too large (maximum {MAX_LIST_DIR_BYTES} bytes)"
            ));
        }
        names.push(name);
    }
    names.sort();
    Ok(names.join("\n"))
}

fn write_utf8_file(workspace: &Dir, path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
    workspace.create_dir_all(parent_path)?;
    let parent = if parent_path.as_os_str().is_empty() {
        workspace.try_clone()?
    } else {
        workspace.open_dir(parent_path)?
    };
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path must name a file")
    })?;
    let permissions = match parent.symlink_metadata(file_name) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let temp_name = format!(".openwave-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).nonblock(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = parent.open_with(&temp_name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "temporary path is not a regular file",
            ));
        }
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        parent.rename(&temp_name, &parent, file_name)?;
        #[cfg(unix)]
        parent.open(".")?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temp_name);
    }
    result
}

/// Parse tool args, turning a bad shape into a model-facing error output.
fn parse_args<T: for<'de> Deserialize<'de>>(args: Value) -> std::result::Result<T, ToolOutput> {
    serde_json::from_value(args).map_err(|e| ToolOutput::error(format!("invalid arguments: {e}")))
}

/// `read_file` — read a UTF-8 text file from the workspace.
pub struct ReadFile;

#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file, relative to the workspace directory.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative file path." }
                },
                "required": ["path"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: ReadFileArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return Ok(output),
        };
        let path = match workspace_relative(&args.path) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let result = tokio::task::spawn_blocking(move || read_utf8_file(&workspace, &path)).await;
        match result {
            Ok(Ok(content)) => Ok(ToolOutput::text(content)),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not read {}: {error}",
                args.path
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not read {}: filesystem task failed: {error}",
                args.path
            ))),
        }
    }
}

/// `list_dir` — list the entries of a workspace directory.
pub struct ListDir;

#[derive(Deserialize)]
struct ListDirArgs {
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List the entries of a workspace directory (defaults to the root).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative directory (optional)." }
                }
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: ListDirArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return Ok(output),
        };
        let rel = args.path.as_deref().unwrap_or(".");
        let path = match workspace_relative(rel) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let result = tokio::task::spawn_blocking(move || list_directory(&workspace, &path)).await;
        match result {
            Ok(Ok(listing)) => Ok(ToolOutput::text(listing)),
            Ok(Err(error)) => Ok(ToolOutput::error(format!("could not list {rel}: {error}"))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not list {rel}: filesystem task failed: {error}"
            ))),
        }
    }
}

/// `write_file` — write a UTF-8 text file into the workspace (creating parent
/// directories). Workspace-class: auto-approved inside the workspace.
pub struct WriteFile;

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Write a UTF-8 text file into the workspace, creating parent \
                          directories. Overwrites an existing file."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative file path." },
                    "content": { "type": "string", "description": "File contents to write." }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Workspace
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: WriteFileArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return Ok(output),
        };
        let path = match workspace_relative(&args.path) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let content = args.content.into_bytes();
        let content_len = content.len();
        let result =
            tokio::task::spawn_blocking(move || write_utf8_file(&workspace, &path, &content)).await;
        match result {
            Ok(Ok(())) => Ok(ToolOutput::text(format!(
                "wrote {} bytes to {}",
                content_len, args.path
            ))),
            Ok(Err(error)) => Ok(ToolOutput::error(format!(
                "could not write {}: {error}",
                args.path
            ))),
            Err(error) => Ok(ToolOutput::error(format!(
                "could not write {}: filesystem task failed: {error}",
                args.path
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChatId;

    fn ctx(dir: &std::path::Path) -> ToolCtx {
        ToolCtx::try_new(ChatId::new(), None, dir.to_path_buf()).unwrap()
    }

    #[tokio::test]
    async fn write_then_read_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());

        let out = WriteFile
            .execute(&ctx, json!({"path": "notes/todo.txt", "content": "hello"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{out:?}");

        let read = ReadFile
            .execute(&ctx, json!({"path": "notes/todo.txt"}))
            .await
            .unwrap();
        assert_eq!(read.content, "hello");
        assert!(!read.is_error);

        let listing = ListDir
            .execute(&ctx, json!({"path": "notes"}))
            .await
            .unwrap();
        assert_eq!(listing.content, "todo.txt");
    }

    #[tokio::test]
    async fn confinement_rejects_escaping_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx(dir.path());

        let escape = ReadFile
            .execute(&ctx, json!({"path": "../secret"}))
            .await
            .unwrap();
        assert!(escape.is_error);

        let absolute = WriteFile
            .execute(&ctx, json!({"path": "/etc/passwd", "content": "x"}))
            .await
            .unwrap();
        assert!(absolute.is_error);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn confinement_rejects_symlink_escape() {
        // A directory outside the workspace, symlinked to from inside it.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("link")).unwrap();
        let ctx = ctx(ws.path());

        // Reading through the symlinked directory must be rejected...
        let read = ReadFile
            .execute(&ctx, json!({"path": "link/secret.txt"}))
            .await
            .unwrap();
        assert!(
            read.is_error,
            "symlinked-dir read should be rejected: {read:?}"
        );

        // ...and so must writing through it (no auto-approved arbitrary overwrite).
        let write = WriteFile
            .execute(&ctx, json!({"path": "link/pwn.txt", "content": "x"}))
            .await
            .unwrap();
        assert!(
            write.is_error,
            "symlinked-dir write should be rejected: {write:?}"
        );
        assert!(!outside.path().join("pwn.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_capability_survives_root_path_retargeting() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("note.txt"), "original").unwrap();
        let ctx = ctx(&workspace);

        let original = parent.path().join("original");
        std::fs::rename(&workspace, &original).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("note.txt"), "replacement").unwrap();

        let read = ReadFile
            .execute(&ctx, json!({"path": "note.txt"}))
            .await
            .unwrap();
        assert_eq!(read.content, "original");
        assert!(!read.is_error);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_rejects_a_fifo_without_blocking() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(dir.path().join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        let workspace = ctx(dir.path()).workspace().unwrap();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = read_utf8_file(&workspace, Path::new("pipe"));
            let _ = send.send(result);
        });

        let result = receive
            .recv_timeout(Duration::from_secs(2))
            .expect("FIFO read must not block");
        assert!(result.unwrap_err().contains("regular file"));
        worker.join().unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_rejects_a_fifo_without_blocking() {
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(dir.path().join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        let workspace = ctx(dir.path()).workspace().unwrap();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = write_utf8_file(&workspace, Path::new("pipe"), b"content");
            let _ = send.send(result);
        });

        let error = receive
            .recv_timeout(Duration::from_secs(2))
            .expect("FIFO write must not block")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        worker.join().unwrap();
        assert!(dir.path().join("pipe").exists());
    }

    #[tokio::test]
    async fn failed_atomic_write_preserves_the_target_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/keep.txt"), "keep").unwrap();

        let write = WriteFile
            .execute(
                &ctx(dir.path()),
                json!({"path": "target", "content": "replace"}),
            )
            .await
            .unwrap();
        assert!(write.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target/keep.txt")).unwrap(),
            "keep"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("target")]);
    }

    #[tokio::test]
    async fn atomic_write_replaces_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(dir.path().join("note.txt"))
                .unwrap()
                .permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(dir.path().join("note.txt"), permissions).unwrap();
        }

        let write = WriteFile
            .execute(
                &ctx(dir.path()),
                json!({"path": "note.txt", "content": "new"}),
            )
            .await
            .unwrap();
        assert!(!write.is_error, "{write:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("note.txt")).unwrap(),
            "new"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("note.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn read_rejects_files_over_the_output_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("large.txt"),
            vec![b'x'; MAX_READ_FILE_BYTES + 1],
        )
        .unwrap();

        let read = ReadFile
            .execute(&ctx(dir.path()), json!({"path": "large.txt"}))
            .await
            .unwrap();
        assert!(read.is_error);
        assert!(read.content.contains("too large"), "{read:?}");
    }

    #[tokio::test]
    async fn missing_file_is_a_model_facing_error_not_err() {
        let dir = tempfile::tempdir().unwrap();
        let out = ReadFile
            .execute(&ctx(dir.path()), json!({"path": "nope.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}

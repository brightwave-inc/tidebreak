//! Built-in filesystem tools: `read_file`, `list_dir`, `write_file`.
//!
//! These are the M0 Workspace-class tools. Every path is resolved **relative to
//! the chat's workspace directory** and may not escape it (no absolute paths,
//! no `..`); there is no sandbox yet, so that lexical confinement is the only
//! guard. Failures the model should see and react to (missing file, bad path)
//! come back as [`ToolOutput::error`], not `Err` — `Err` is reserved for
//! unexpected infrastructure faults.
//!
//! Enabled by the `tools` feature.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::Result;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};

/// Resolve `rel` under `workspace`, rejecting absolute paths and any `..` that
/// could escape. Returns the joined path, or an error message for the model.
fn resolve_in_workspace(workspace: &Path, rel: &str) -> std::result::Result<PathBuf, String> {
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
    let candidate = workspace.join(path);

    // The lexical check above only stops `..`/absolute paths; it can't stop a
    // symlink *inside* the workspace from pointing out. Canonicalize the root and
    // the nearest existing ancestor of the target and require the target to stay
    // under the root, and refuse a leaf that is itself a symlink (which a write
    // would otherwise follow out of the workspace).
    let root = workspace
        .canonicalize()
        .map_err(|e| format!("workspace directory unavailable: {e}"))?;
    if std::fs::symlink_metadata(&candidate)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "path is a symlink and may escape the workspace: {rel}"
        ));
    }
    let mut probe: &Path = &candidate;
    let real = loop {
        if let Ok(canonical) = probe.canonicalize() {
            break canonical;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Err(format!("path escapes the workspace: {rel}")),
        }
    };
    if !real.starts_with(&root) {
        return Err(format!("path escapes the workspace: {rel}"));
    }
    Ok(candidate)
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
        let path = match resolve_in_workspace(&ctx.workspace_dir, &args.path) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Ok(ToolOutput::text(content)),
            Err(err) => Ok(ToolOutput::error(format!(
                "could not read {}: {err}",
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
        let dir = match resolve_in_workspace(&ctx.workspace_dir, rel) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(err) => return Ok(ToolOutput::error(format!("could not list {rel}: {err}"))),
        };
        let mut names = Vec::new();
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                    names.push(if is_dir { format!("{name}/") } else { name });
                }
                Ok(None) => break,
                Err(err) => return Ok(ToolOutput::error(format!("could not list {rel}: {err}"))),
            }
        }
        names.sort();
        Ok(ToolOutput::text(names.join("\n")))
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
        let path = match resolve_in_workspace(&ctx.workspace_dir, &args.path) {
            Ok(path) => path,
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolOutput::error(format!(
                    "could not create parent of {}: {err}",
                    args.path
                )));
            }
        }
        match tokio::fs::write(&path, args.content.as_bytes()).await {
            Ok(()) => Ok(ToolOutput::text(format!(
                "wrote {} bytes to {}",
                args.content.len(),
                args.path
            ))),
            Err(err) => Ok(ToolOutput::error(format!(
                "could not write {}: {err}",
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
        ToolCtx {
            chat_id: ChatId::new(),
            project_id: None,
            workspace_dir: dir.to_path_buf(),
        }
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

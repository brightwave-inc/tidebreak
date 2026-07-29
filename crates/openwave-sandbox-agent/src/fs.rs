//! Sandbox-resident filesystem tools: read, write, and list files within the
//! agent's in-container workspace directory.
//!
//! These tools are scoped to a single workspace root. Every model-supplied path
//! goes through [`WorkspacePath`], which mirrors the `WorkspaceFilePath`
//! validation the shipped foreground provider uses: relative paths only, no
//! traversal (`..`), no absolute or prefix components, no control characters or
//! backslashes, and a bounded length, normalized to a `a/b/c` form. Resolution
//! then canonicalizes against the workspace root and refuses anything that
//! escapes it, so a symlinked intermediate directory cannot redirect a read or
//! write outside the workspace. Transfers and listings are bounded.
//!
//! The container is the isolation boundary; this path validation is what keeps a
//! model-authored path from wandering *within* the container's filesystem, not a
//! substitute for that boundary.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The name the model uses to read a workspace file.
pub const READ_FILE_TOOL: &str = "read_file";
/// The name the model uses to write a workspace file.
pub const WRITE_FILE_TOOL: &str = "write_file";
/// The name the model uses to list a workspace directory.
pub const LIST_DIR_TOOL: &str = "list_dir";

/// Maximum UTF-8 bytes in a workspace-relative path.
pub const MAX_PATH_BYTES: usize = 1_024;
/// Maximum bytes returned by one read.
pub const MAX_READ_BYTES: usize = 256 * 1_024;
/// Maximum bytes accepted by one write.
pub const MAX_WRITE_BYTES: usize = 1_024 * 1_024;
/// Maximum entries returned by one directory listing.
pub const MAX_LIST_ENTRIES: usize = 256;

/// A validated workspace-relative path.
///
/// Relative, normal components only (no `.`, `..`, root, or prefix), bounded
/// length, no control characters or backslashes, normalized to `a/b/c`. Absolute
/// host paths and traversal never cross this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePath(String);

impl WorkspacePath {
    /// Parse and normalize a workspace-relative path, or return why it is
    /// invalid.
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        let invalid = || "invalid workspace path".to_owned();
        if value.is_empty()
            || value.len() > MAX_PATH_BYTES
            || value.chars().any(char::is_control)
            || value.contains('\\')
        {
            return Err(invalid());
        }
        let path = Path::new(&value);
        if path.is_absolute() {
            return Err(invalid());
        }
        let mut parts = Vec::new();
        for component in path.components() {
            let Component::Normal(part) = component else {
                return Err(invalid());
            };
            parts.push(part.to_str().ok_or_else(invalid)?);
        }
        if parts.is_empty() {
            return Err(invalid());
        }
        Ok(Self(parts.join("/")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The final path component.
    #[must_use]
    fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

/// Canonicalize the workspace root so the `starts_with` containment checks
/// compare against the real directory, falling back to the given path when it
/// does not yet exist (the agent loop creates and canonicalizes it before use).
fn canonical_root(workspace: PathBuf) -> PathBuf {
    std::fs::canonicalize(&workspace).unwrap_or(workspace)
}

/// Resolve an existing path within `root`, refusing anything that escapes it.
///
/// Returns `Ok(None)` when the path does not exist. Canonicalization collapses
/// any symlinked intermediate directory, and the `starts_with` check is what
/// refuses an escape.
fn resolve_existing(
    root: &Path,
    rel: &WorkspacePath,
) -> std::result::Result<Option<PathBuf>, String> {
    let candidate = root.join(rel.as_str());
    match std::fs::canonicalize(&candidate) {
        Ok(resolved) if resolved.starts_with(root) => Ok(Some(resolved)),
        Ok(_) => Err("path escapes the workspace".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("could not resolve the path".to_owned()),
    }
}

/// Resolve a write target within `root`, creating parent directories inside the
/// workspace. The canonicalized parent must stay within the workspace; the final
/// component is rejoined so a not-yet-existing file resolves.
fn resolve_write_target(root: &Path, rel: &WorkspacePath) -> std::result::Result<PathBuf, String> {
    let target = root.join(rel.as_str());
    let parent = target
        .parent()
        .ok_or_else(|| "path has no parent".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|_| "could not create workspace directories".to_owned())?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|_| "could not resolve the workspace directory".to_owned())?;
    if !parent.starts_with(root) {
        return Err("path escapes the workspace".to_owned());
    }
    Ok(parent.join(rel.file_name()))
}

/// Arguments naming one workspace file.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    /// Workspace-relative path of the file to read.
    path: String,
}

/// Read one bounded file from the workspace.
pub struct ReadFileTool {
    workspace: PathBuf,
}

impl ReadFileTool {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: canonical_root(workspace.into()),
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::for_args::<ReadArgs>(
            READ_FILE_TOOL,
            "Read a UTF-8 text file from the sandbox workspace by its \
             workspace-relative path.",
        )
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: ReadArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(ToolOutput::error(format!("invalid arguments: {error}"))),
        };
        let path = match WorkspacePath::parse(args.path) {
            Ok(path) => path,
            Err(error) => return Ok(ToolOutput::error(error)),
        };
        let resolved = match resolve_existing(&self.workspace, &path) {
            Ok(Some(resolved)) => resolved,
            Ok(None) => return Ok(ToolOutput::error("no such file".to_owned())),
            Err(error) => return Ok(ToolOutput::error(error)),
        };
        let bytes = match read_bounded(&resolved) {
            Ok(bytes) => bytes,
            Err(error) => return Ok(ToolOutput::error(error)),
        };
        let truncated = bytes.len() > MAX_READ_BYTES;
        let kept = &bytes[..bytes.len().min(MAX_READ_BYTES)];
        let mut content = String::from_utf8_lossy(kept).into_owned();
        if truncated {
            content.push_str("\n(truncated at the read cap)");
        }
        Ok(ToolOutput::text(content))
    }
}

/// Read a regular file, refusing a symlinked final component and capping the
/// bytes read so an oversized file cannot exhaust memory.
fn read_bounded(path: &Path) -> std::result::Result<Vec<u8>, String> {
    use std::io::Read;

    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW);
    }
    let file = open
        .open(path)
        .map_err(|_| "could not read the file".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "could not read the file".to_owned())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".to_owned());
    }
    let mut content = Vec::new();
    file.take(MAX_READ_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|_| "could not read the file".to_owned())?;
    Ok(content)
}

/// Arguments for one workspace write.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    /// Workspace-relative path of the file to write.
    path: String,
    /// The UTF-8 content to write, replacing any existing file.
    content: String,
}

/// Write one bounded file within the workspace.
pub struct WriteFileTool {
    workspace: PathBuf,
}

impl WriteFileTool {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: canonical_root(workspace.into()),
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::for_args::<WriteArgs>(
            WRITE_FILE_TOOL,
            "Write a UTF-8 text file into the sandbox workspace at a \
             workspace-relative path, creating parent directories as needed and \
             replacing any existing file.",
        )
    }

    fn approval_class(&self) -> ApprovalClass {
        // A write stays inside the workspace, so it is a workspace mutation
        // rather than an escape.
        ApprovalClass::Workspace
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: WriteArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(ToolOutput::error(format!("invalid arguments: {error}"))),
        };
        if args.content.len() > MAX_WRITE_BYTES {
            return Ok(ToolOutput::error(format!(
                "content exceeds the {MAX_WRITE_BYTES}-byte write cap"
            )));
        }
        let path = match WorkspacePath::parse(args.path) {
            Ok(path) => path,
            Err(error) => return Ok(ToolOutput::error(error)),
        };
        let target = match resolve_write_target(&self.workspace, &path) {
            Ok(target) => target,
            Err(error) => return Ok(ToolOutput::error(error)),
        };
        match write_no_follow(&target, args.content.as_bytes()) {
            Ok(()) => Ok(ToolOutput::text(format!(
                "wrote {} bytes to {}",
                args.content.len(),
                path.as_str()
            ))),
            Err(error) => Ok(ToolOutput::error(error)),
        }
    }
}

/// Write `content` to `path`, refusing to follow a symlink already at the final
/// component so a planted link cannot redirect the write.
fn write_no_follow(path: &Path, content: &[u8]) -> std::result::Result<(), String> {
    use std::io::Write;

    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = open
        .open(path)
        .map_err(|_| "could not write the file".to_owned())?;
    file.write_all(content)
        .map_err(|_| "could not write the file".to_owned())
}

/// Arguments for one directory listing.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    /// Workspace-relative directory to list; omit to list the workspace root.
    #[serde(default)]
    path: Option<String>,
}

/// List one workspace directory, bounded.
pub struct ListDirTool {
    workspace: PathBuf,
}

impl ListDirTool {
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: canonical_root(workspace.into()),
        }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::for_args::<ListArgs>(
            LIST_DIR_TOOL,
            "List the entries of a directory in the sandbox workspace \
             (the workspace root when no path is given).",
        )
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args: ListArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(ToolOutput::error(format!("invalid arguments: {error}"))),
        };
        let (base, prefix) = match args.path {
            None => (self.workspace.clone(), None),
            Some(raw) => {
                let path = match WorkspacePath::parse(raw) {
                    Ok(path) => path,
                    Err(error) => return Ok(ToolOutput::error(error)),
                };
                match resolve_existing(&self.workspace, &path) {
                    Ok(Some(resolved)) => (resolved, Some(path.as_str().to_owned())),
                    Ok(None) => return Ok(ToolOutput::error("no such directory".to_owned())),
                    Err(error) => return Ok(ToolOutput::error(error)),
                }
            }
        };
        if !base.is_dir() {
            return Ok(ToolOutput::error("path is not a directory".to_owned()));
        }
        match list_bounded(&base, prefix.as_deref()) {
            Ok(listing) => Ok(ToolOutput::text(listing)),
            Err(error) => Ok(ToolOutput::error(error)),
        }
    }
}

/// List a directory into sorted, bounded text, skipping symlinks and special
/// files so a listing describes only regular files and directories.
fn list_bounded(base: &Path, prefix: Option<&str>) -> std::result::Result<String, String> {
    let reader = std::fs::read_dir(base).map_err(|_| "could not list the directory".to_owned())?;
    let mut entries = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|_| "could not list the directory".to_owned())?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            continue;
        }
        let relative = match prefix {
            None => name,
            Some(prefix) => format!("{prefix}/{name}"),
        };
        let line = if metadata.is_dir() {
            format!("{relative}/")
        } else {
            format!("{relative} ({} bytes)", metadata.len())
        };
        entries.push(line);
    }
    entries.sort();
    let truncated = entries.len() > MAX_LIST_ENTRIES;
    entries.truncate(MAX_LIST_ENTRIES);
    if truncated {
        entries.push(format!("(truncated at {MAX_LIST_ENTRIES} entries)"));
    }
    if entries.is_empty() {
        return Ok("(empty)".to_owned());
    }
    Ok(entries.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolCtx {
        ToolCtx::without_private_scratch(openwave_core::ChatId::new(), None)
    }

    #[test]
    fn path_validation_rejects_traversal_and_absolute_and_control() {
        assert_eq!(
            WorkspacePath::parse("reports//2026/summary.txt")
                .unwrap()
                .as_str(),
            "reports/2026/summary.txt"
        );
        for rejected in [
            "",
            "/etc/passwd",
            "../outside",
            "a/../b",
            "./a",
            "a\\b",
            "a\u{0}b",
        ] {
            assert!(
                WorkspacePath::parse(rejected).is_err(),
                "{rejected:?} must be rejected"
            );
        }
        assert!(WorkspacePath::parse("x".repeat(MAX_PATH_BYTES + 1)).is_err());
    }

    #[tokio::test]
    async fn write_then_read_round_trips_within_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let write = WriteFileTool::new(dir.path());
        let read = ReadFileTool::new(dir.path());

        let wrote = write
            .execute(
                &ctx(),
                serde_json::json!({ "path": "notes/hello.txt", "content": "hi there" }),
            )
            .await
            .unwrap();
        assert!(!wrote.is_error, "{wrote:?}");

        let got = read
            .execute(&ctx(), serde_json::json!({ "path": "notes/hello.txt" }))
            .await
            .unwrap();
        assert_eq!(got.content, "hi there");
    }

    #[tokio::test]
    async fn a_traversal_path_is_rejected_by_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let read = ReadFileTool::new(dir.path());
        let out = read
            .execute(&ctx(), serde_json::json!({ "path": "../secret" }))
            .await
            .unwrap();
        assert!(out.is_error, "traversal must be refused: {out:?}");
    }

    #[tokio::test]
    async fn an_oversized_write_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let write = WriteFileTool::new(dir.path());
        let content = "x".repeat(MAX_WRITE_BYTES + 1);
        let out = write
            .execute(
                &ctx(),
                serde_json::json!({ "path": "big.txt", "content": content }),
            )
            .await
            .unwrap();
        assert!(out.is_error, "oversized write must be refused");
        assert!(!dir.path().join("big.txt").exists(), "nothing was written");
    }

    #[tokio::test]
    async fn list_reflects_written_files() {
        let dir = tempfile::tempdir().unwrap();
        let write = WriteFileTool::new(dir.path());
        write
            .execute(
                &ctx(),
                serde_json::json!({ "path": "a.txt", "content": "1" }),
            )
            .await
            .unwrap();
        let list = ListDirTool::new(dir.path());
        let out = list.execute(&ctx(), serde_json::json!({})).await.unwrap();
        assert!(out.content.contains("a.txt"), "{}", out.content);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_escape_is_refused_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        // A symlink planted inside the workspace pointing outside it.
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("leak"))
            .unwrap();
        let read = ReadFileTool::new(dir.path());
        let out = read
            .execute(&ctx(), serde_json::json!({ "path": "leak" }))
            .await
            .unwrap();
        assert!(
            out.is_error,
            "a symlink escaping the workspace must be refused"
        );
    }
}

use std::path::{Component, Path};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum UTF-8 bytes in an executable name or path.
pub const MAX_COMMAND_BYTES: usize = 1_024;
/// Maximum number of arguments in one request.
pub const MAX_ARGUMENTS: usize = 128;
/// Maximum aggregate UTF-8 bytes across every argument.
pub const MAX_ARGUMENT_BYTES: usize = 32 * 1_024;
/// Maximum UTF-8 bytes in a private-workspace-relative current directory.
pub const MAX_CWD_BYTES: usize = 1_024;
/// Maximum bytes captured from stdout and stderr together.
pub const MAX_CAPTURE_BYTES: usize = 40_000;
/// Maximum bytes transferred for one workspace file in either direction.
pub const MAX_WORKSPACE_FILE_BYTES: usize = 16 * 1_024 * 1_024;
/// Maximum UTF-8 bytes in a workspace-relative file path.
pub const MAX_WORKSPACE_PATH_BYTES: usize = 1_024;
/// Maximum entries returned by one workspace listing.
pub const MAX_WORKSPACE_LIST_ENTRIES: usize = 256;
const MAX_ID_BYTES: usize = 128;

/// A configured code-execution backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodeExecutionProviderKind {
    /// The host's native process sandbox.
    Local,
    /// A managed E2B cloud sandbox.
    E2b,
    /// A managed Daytona cloud sandbox.
    Daytona,
}

impl CodeExecutionProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::E2b => "e2b",
            Self::Daytona => "daytona",
        }
    }
}

impl std::fmt::Display for CodeExecutionProviderKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable provider idempotency key for one canonical tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionId(String);

impl ExecutionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeExecutionError> {
        let value = value.into();
        validate_id(&value, "execution id")?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque workspace/session key interpreted only by the selected provider.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionWorkspaceId(String);

impl ExecutionWorkspaceId {
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeExecutionError> {
        let value = value.into();
        validate_id(&value, "workspace id")?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated workspace-relative file path.
///
/// Paths are relative, contain only normal components (no `.`, `..`, root, or
/// prefix components), and are stored in a normalized `a/b/c` form. Absolute
/// host paths and traversal never cross the workspace contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceFilePath(String);

impl WorkspaceFilePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, CodeExecutionError> {
        let value = value.into();
        let invalid = || CodeExecutionError::InvalidRequest("invalid workspace path".into());
        if value.is_empty()
            || value.len() > MAX_WORKSPACE_PATH_BYTES
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
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

/// One entry in a bounded workspace listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileEntry {
    /// Workspace-relative path in normalized `a/b/c` form.
    pub path: String,
    pub directory: bool,
    /// Byte length where the backend reports one; directories report none.
    pub size_bytes: Option<u64>,
}

/// A bounded listing of one workspace directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListing {
    pub entries: Vec<WorkspaceFileEntry>,
    /// True when the directory held more than [`MAX_WORKSPACE_LIST_ENTRIES`].
    pub truncated: bool,
}

/// Optional durable-workspace capability beside [`CodeExecutionProvider::execute`].
///
/// This is a host-internal surface: nothing here is model-facing, and no
/// credential or absolute host path crosses it. File transfers are bounded by
/// [`MAX_WORKSPACE_FILE_BYTES`] and every path is workspace-relative.
#[async_trait]
pub trait WorkspaceLifecycle: Send + Sync {
    /// Ensure the durable workspace exists, connecting when it already does.
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError>;

    /// Connect to an existing workspace without creating one. `Ok(false)`
    /// means no durable workspace is currently reachable.
    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<bool, CodeExecutionError>;

    /// Destroy the workspace. Destroying one that no longer exists succeeds.
    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> Result<(), CodeExecutionError>;

    /// Write one bounded file, creating parent directories inside the
    /// workspace and the workspace itself when needed.
    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> Result<(), CodeExecutionError>;

    /// Read one bounded file.
    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> Result<Vec<u8>, CodeExecutionError>;

    /// List one directory (the workspace root when `path` is `None`).
    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> Result<WorkspaceListing, CodeExecutionError>;
}

fn validate_id(value: &str, label: &str) -> Result<(), CodeExecutionError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CodeExecutionError::InvalidRequest(format!(
            "invalid {label}"
        )));
    }
    Ok(())
}

/// Provider-neutral request for one direct process invocation.
///
/// `command` is an executable plus an argument vector, not shell text. A caller
/// that deliberately needs a shell must name it explicitly (for example,
/// `command: "/bin/sh", arguments: ["-c", "…"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecutionRequest {
    pub execution_id: ExecutionId,
    pub workspace_id: ExecutionWorkspaceId,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    #[serde(default = "default_cwd")]
    pub cwd: String,
}

impl CodeExecutionRequest {
    pub fn new(
        execution_id: ExecutionId,
        workspace_id: ExecutionWorkspaceId,
        command: impl Into<String>,
        arguments: Vec<String>,
        cwd: impl Into<String>,
    ) -> Result<Self, CodeExecutionError> {
        let request = Self {
            execution_id,
            workspace_id,
            command: command.into(),
            arguments,
            cwd: cwd.into(),
        };
        request.validate()?;
        Ok(request)
    }

    /// Revalidate public/deserialized fields at the provider boundary.
    pub fn validate(&self) -> Result<(), CodeExecutionError> {
        validate_id(self.execution_id.as_str(), "execution id")?;
        validate_id(self.workspace_id.as_str(), "workspace id")?;
        if self.command.is_empty()
            || self.command.len() > MAX_COMMAND_BYTES
            || self.command.as_bytes().contains(&0)
        {
            return Err(CodeExecutionError::InvalidRequest(
                "invalid executable".into(),
            ));
        }
        if self.arguments.len() > MAX_ARGUMENTS
            || self
                .arguments
                .iter()
                .any(|argument| argument.as_bytes().contains(&0))
            || self
                .arguments
                .iter()
                .try_fold(0_usize, |total, argument| total.checked_add(argument.len()))
                .is_none_or(|total| total > MAX_ARGUMENT_BYTES)
        {
            return Err(CodeExecutionError::InvalidRequest(
                "invalid command arguments".into(),
            ));
        }
        if self.cwd.is_empty()
            || self.cwd.len() > MAX_CWD_BYTES
            || self.cwd.as_bytes().contains(&0)
            || !is_safe_relative_path(Path::new(&self.cwd))
        {
            return Err(CodeExecutionError::InvalidRequest(
                "invalid working directory".into(),
            ));
        }
        Ok(())
    }
}

fn default_cwd() -> String {
    ".".into()
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

/// Normalized, bounded result returned by every provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeExecutionResponse {
    pub provider: CodeExecutionProviderKind,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub output_truncated: bool,
    pub duration_ms: u64,
    /// What the scratch↔workspace mirror around this execution left behind
    /// (oversized files, truncated listings, a failed pull). Empty when the
    /// provider shares the host filesystem or the mirror was complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sync_notes: Vec<String>,
}

/// A configured execution provider. Implementations must treat
/// `request.execution_id` as an idempotency key and reject an identity reused
/// with different canonical arguments.
#[async_trait]
pub trait CodeExecutionProvider: Send + Sync {
    async fn execute(
        &self,
        request: CodeExecutionRequest,
    ) -> Result<CodeExecutionResponse, CodeExecutionError>;

    /// The provider's optional durable-workspace capability. `None` means the
    /// backend has no durable session surface; callers must degrade instead of
    /// treating the absence as an error.
    fn workspace_lifecycle(&self) -> Option<&dyn WorkspaceLifecycle> {
        None
    }
}

#[derive(Debug, Error)]
pub enum CodeExecutionError {
    #[error("invalid code execution request: {0}")]
    InvalidRequest(String),
    #[error("code execution is not configured")]
    NotConfigured,
    #[error("code execution provider is unavailable: {0}")]
    Unavailable(String),
    #[error("native sandbox setup failed: {0}")]
    Sandbox(String),
    #[error("could not start the sandboxed command")]
    Spawn,
    #[error("execution identity was reused with different arguments")]
    IdentityConflict,
    #[error("the requested workspace file does not exist")]
    WorkspaceFileNotFound,
    #[error("workspace file exceeds the transfer bound")]
    WorkspaceFileTooLarge,
    #[error("the outcome of this execution is ambiguous; it was not replayed")]
    AmbiguousExecution,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (ExecutionId, ExecutionWorkspaceId) {
        (
            ExecutionId::parse("call-123").unwrap(),
            ExecutionWorkspaceId::parse("chat-123").unwrap(),
        )
    }

    #[test]
    fn validates_bounded_direct_command_requests() {
        let (execution, workspace) = ids();
        assert!(CodeExecutionRequest::new(
            execution.clone(),
            workspace.clone(),
            "/usr/bin/python3",
            vec!["-c".into(), "print('ok')".into()],
            ".",
        )
        .is_ok());
        assert!(CodeExecutionRequest::new(
            execution.clone(),
            workspace.clone(),
            "/bin/sh",
            vec![],
            "../outside",
        )
        .is_err());
        assert!(CodeExecutionRequest::new(
            execution,
            workspace,
            "x".repeat(MAX_COMMAND_BYTES + 1),
            vec![],
            ".",
        )
        .is_err());
    }

    #[test]
    fn workspace_paths_are_normalized_and_reject_traversal() {
        assert_eq!(
            WorkspaceFilePath::parse("reports//2026/summary.csv")
                .unwrap()
                .as_str(),
            "reports/2026/summary.csv"
        );
        assert_eq!(
            WorkspaceFilePath::parse("a/b/c.txt").unwrap().file_name(),
            "c.txt"
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
                WorkspaceFilePath::parse(rejected).is_err(),
                "{rejected:?} must be rejected"
            );
        }
        assert!(WorkspaceFilePath::parse("x".repeat(MAX_WORKSPACE_PATH_BYTES + 1)).is_err());
    }

    #[test]
    fn ids_are_safe_single_path_components() {
        assert!(ExecutionId::parse("018f2c9a-4f42-7000-a000-000000000000").is_ok());
        assert!(ExecutionId::parse("../escape").is_err());
        assert!(ExecutionWorkspaceId::parse("chat/child").is_err());
    }
}

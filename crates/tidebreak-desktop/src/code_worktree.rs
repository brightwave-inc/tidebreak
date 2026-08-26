//! Native open action for a Code workspace's local worktree.
//!
//! The server owns workspace creation and stores the path. The desktop owns
//! the one thing a headless server cannot do: ask this operating system to
//! open that directory in its normal file manager. The renderer can propose a
//! path, but this command refuses remote attachments, relative paths, missing
//! paths, and non-directories before it starts a fixed platform launcher.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;
use tidebreak_core::{CodeWorkspaceStatus, WorkspaceId};

use crate::remote::RemoteAttachment;
use crate::{wait_server_info, AppState};

pub(crate) const REASON_AUTHORITY_UNAVAILABLE: &str = "code_worktree_authority_unavailable";
pub(crate) const REASON_PATH_INVALID: &str = "code_worktree_path_invalid";
pub(crate) const REASON_WORKSPACE_UNAVAILABLE: &str = "code_worktree_workspace_unavailable";
pub(crate) const REASON_WORKSPACE_INACTIVE: &str = "code_worktree_workspace_inactive";
pub(crate) const REASON_PATH_NOT_FOUND: &str = "code_worktree_path_not_found";
pub(crate) const REASON_NOT_DIRECTORY: &str = "code_worktree_not_directory";
pub(crate) const REASON_LAUNCHER_UNAVAILABLE: &str = "code_worktree_launcher_unavailable";
pub(crate) const REASON_OPEN_FAILED: &str = "code_worktree_open_failed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeWorktreeOpenError {
    /// Stable reason code that the renderer turns into user-facing copy.
    reason: &'static str,
    /// Native diagnostic for logs and support. The renderer does not display it.
    detail: Option<String>,
}

impl CodeWorktreeOpenError {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    fn detailed(reason: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            reason,
            detail: Some(detail.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CodeWorktreeOpenStatus {
    Opened,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeWorktreeOpenResult {
    status: CodeWorktreeOpenStatus,
}

/// Open one local worktree with this operating system's normal folder handler.
#[tauri::command]
pub(crate) async fn open_code_worktree(
    attachment: State<'_, Arc<RemoteAttachment>>,
    app_state: State<'_, Arc<AppState>>,
    workspace_id: String,
) -> Result<CodeWorktreeOpenResult, CodeWorktreeOpenError> {
    if attachment.current().await.is_some() {
        return Err(CodeWorktreeOpenError::new(REASON_AUTHORITY_UNAVAILABLE));
    }
    let workspace_id: WorkspaceId = workspace_id
        .parse()
        .map_err(|_| CodeWorktreeOpenError::new(REASON_WORKSPACE_UNAVAILABLE))?;
    let info = wait_server_info(app_state.inner())
        .await
        .map_err(|error| CodeWorktreeOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))?;
    let workspace = read_workspace(&info, workspace_id).await?;
    if !matches!(
        workspace.status,
        CodeWorkspaceStatus::Active | CodeWorkspaceStatus::SetupFailed
    ) {
        return Err(CodeWorktreeOpenError::new(REASON_WORKSPACE_INACTIVE));
    }
    open_directory(PathBuf::from(workspace.worktree_path))
}

#[derive(Deserialize)]
struct CodeWorktreeSnapshot {
    worktree_path: String,
    status: CodeWorkspaceStatus,
}

async fn read_workspace(
    info: &crate::NativeServerInfo,
    workspace_id: WorkspaceId,
) -> Result<CodeWorktreeSnapshot, CodeWorktreeOpenError> {
    let response = crate::documents::local_client()
        .get(format!("{}/code/workspaces/{workspace_id}", info.base_url))
        .bearer_auth(&info.token)
        .send()
        .await
        .map_err(|error| CodeWorktreeOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))?;
    if !response.status().is_success() {
        return Err(CodeWorktreeOpenError::detailed(
            REASON_WORKSPACE_UNAVAILABLE,
            response.status(),
        ));
    }
    response
        .json()
        .await
        .map_err(|error| CodeWorktreeOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))
}

fn open_directory(path: PathBuf) -> Result<CodeWorktreeOpenResult, CodeWorktreeOpenError> {
    validate_directory(&path)?;
    let plan = launcher_plan(current_platform(), &path)
        .ok_or_else(|| CodeWorktreeOpenError::new(REASON_LAUNCHER_UNAVAILABLE))?;
    let mut command = tokio::process::Command::new(plan.program);
    command
        .args(plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command.spawn().map_err(|error| {
        let reason = if error.kind() == std::io::ErrorKind::NotFound {
            REASON_LAUNCHER_UNAVAILABLE
        } else {
            REASON_OPEN_FAILED
        };
        CodeWorktreeOpenError::detailed(reason, error)
    })?;
    tauri::async_runtime::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(CodeWorktreeOpenResult {
        status: CodeWorktreeOpenStatus::Opened,
    })
}

fn validate_directory(path: &Path) -> Result<(), CodeWorktreeOpenError> {
    if !path.is_absolute() {
        return Err(CodeWorktreeOpenError::new(REASON_PATH_INVALID));
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(CodeWorktreeOpenError::new(REASON_NOT_DIRECTORY)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(CodeWorktreeOpenError::new(REASON_PATH_NOT_FOUND))
        }
        Err(error) => Err(CodeWorktreeOpenError::detailed(REASON_OPEN_FAILED, error)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesktopPlatform {
    Macos,
    Windows,
    Linux,
    Unsupported,
}

struct LauncherPlan {
    program: &'static str,
    args: Vec<OsString>,
}

fn launcher_plan(platform: DesktopPlatform, path: &Path) -> Option<LauncherPlan> {
    let (program, args) = match platform {
        DesktopPlatform::Macos => ("/usr/bin/open", vec![path.as_os_str().to_owned()]),
        DesktopPlatform::Windows => ("explorer.exe", vec![path.as_os_str().to_owned()]),
        DesktopPlatform::Linux => ("xdg-open", vec![path.as_os_str().to_owned()]),
        DesktopPlatform::Unsupported => return None,
    };
    Some(LauncherPlan { program, args })
}

const fn current_platform() -> DesktopPlatform {
    #[cfg(target_os = "macos")]
    {
        return DesktopPlatform::Macos;
    }
    #[cfg(windows)]
    {
        return DesktopPlatform::Windows;
    }
    #[cfg(target_os = "linux")]
    {
        return DesktopPlatform::Linux;
    }
    #[allow(unreachable_code)]
    DesktopPlatform::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_refuses_relative_missing_and_file_targets() {
        assert_eq!(
            validate_directory(Path::new("relative/worktree"))
                .unwrap_err()
                .reason,
            REASON_PATH_INVALID
        );

        let directory = tempfile::TempDir::new().unwrap();
        let missing = directory.path().join("missing");
        assert_eq!(
            validate_directory(&missing).unwrap_err().reason,
            REASON_PATH_NOT_FOUND
        );

        let file = directory.path().join("file");
        std::fs::write(&file, "not a worktree").unwrap();
        assert_eq!(
            validate_directory(&file).unwrap_err().reason,
            REASON_NOT_DIRECTORY
        );
        assert!(validate_directory(directory.path()).is_ok());
    }

    #[test]
    fn every_supported_platform_uses_a_fixed_folder_launcher() {
        let path = Path::new("/tmp/work tree");
        for (platform, program) in [
            (DesktopPlatform::Macos, "/usr/bin/open"),
            (DesktopPlatform::Windows, "explorer.exe"),
            (DesktopPlatform::Linux, "xdg-open"),
        ] {
            let plan = launcher_plan(platform, path).unwrap();
            assert_eq!(plan.program, program);
            assert_eq!(plan.args, vec![path.as_os_str().to_owned()]);
        }
        assert!(launcher_plan(DesktopPlatform::Unsupported, path).is_none());
    }

    #[test]
    fn failures_serialize_as_stable_reason_and_private_detail() {
        let value = serde_json::to_value(CodeWorktreeOpenError::detailed(
            REASON_OPEN_FAILED,
            "native detail",
        ))
        .unwrap();
        assert_eq!(value["reason"], REASON_OPEN_FAILED);
        assert_eq!(value["detail"], "native detail");
    }
}

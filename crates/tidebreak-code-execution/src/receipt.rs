use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ExecError, ExecRequest, ExecResponse};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ExecutionReceipt {
    Running {
        fingerprint: String,
    },
    Completed {
        fingerprint: String,
        response: ExecResponse,
    },
    Failed {
        fingerprint: String,
        #[serde(default)]
        error: ReceiptError,
        /// Present on receipts written before the error kind was stored.
        /// Ignored when [`Self::Failed::error`] is populated.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// The [`ExecError`] kind a failed receipt must replay as.
///
/// Older receipts stored only a display string and flattened every failure to
/// `Unavailable` / `Sandbox` at replay. The kind is the contract now.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ReceiptError {
    InvalidRequest { message: String },
    NotConfigured,
    Unavailable { message: String },
    Sandbox { message: String },
    Spawn,
    IdentityConflict,
    WorkspaceFileNotFound,
    WorkspaceFileTooLarge,
    AmbiguousExecution,
}

impl Default for ReceiptError {
    fn default() -> Self {
        Self::Unavailable {
            message: String::new(),
        }
    }
}

impl ReceiptError {
    fn from_exec(error: &ExecError) -> Self {
        match error {
            ExecError::InvalidRequest(message) => Self::InvalidRequest {
                message: message.clone(),
            },
            ExecError::NotConfigured => Self::NotConfigured,
            ExecError::Unavailable(message) => Self::Unavailable {
                message: message.clone(),
            },
            ExecError::Sandbox(message) => Self::Sandbox {
                message: message.clone(),
            },
            ExecError::Spawn => Self::Spawn,
            ExecError::IdentityConflict => Self::IdentityConflict,
            ExecError::WorkspaceFileNotFound => Self::WorkspaceFileNotFound,
            ExecError::WorkspaceFileTooLarge => Self::WorkspaceFileTooLarge,
            ExecError::AmbiguousExecution => Self::AmbiguousExecution,
        }
    }

    fn into_exec(self, legacy_message: Option<String>) -> ExecError {
        match self {
            Self::InvalidRequest { message } => ExecError::InvalidRequest(message),
            Self::NotConfigured => ExecError::NotConfigured,
            Self::Unavailable { message } => {
                let message = if message.is_empty() {
                    legacy_message.unwrap_or_default()
                } else {
                    message
                };
                ExecError::Unavailable(message)
            }
            Self::Sandbox { message } => ExecError::Sandbox(message),
            Self::Spawn => ExecError::Spawn,
            Self::IdentityConflict => ExecError::IdentityConflict,
            Self::WorkspaceFileNotFound => ExecError::WorkspaceFileNotFound,
            Self::WorkspaceFileTooLarge => ExecError::WorkspaceFileTooLarge,
            Self::AmbiguousExecution => ExecError::AmbiguousExecution,
        }
    }
}

#[derive(Debug)]
pub(crate) enum BeginExecution {
    Started,
    Cached(ExecResponse),
}

impl ExecutionReceipt {
    pub(crate) fn running(fingerprint: impl Into<String>) -> Self {
        Self::Running {
            fingerprint: fingerprint.into(),
        }
    }

    pub(crate) fn from_outcome(
        fingerprint: String,
        outcome: &Result<ExecResponse, ExecError>,
    ) -> Self {
        match outcome {
            Ok(response) => Self::Completed {
                fingerprint,
                response: response.clone(),
            },
            Err(error) => Self::Failed {
                fingerprint,
                error: ReceiptError::from_exec(error),
                message: None,
            },
        }
    }

    pub(crate) fn replay(&self, fingerprint: &str) -> Result<BeginExecution, ExecError> {
        match self {
            Self::Running {
                fingerprint: existing,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Err(ExecError::AmbiguousExecution)
            }
            Self::Completed {
                fingerprint: existing,
                response,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Ok(BeginExecution::Cached(response.clone()))
            }
            Self::Failed {
                fingerprint: existing,
                error,
                message,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Err(error.clone().into_exec(message.clone()))
            }
        }
    }
}

/// Hash of the model-authored command identity.
///
/// Host-injected fields such as per-turn overlay paths on folder grants are
/// excluded: a replay across turns must not report [`ExecError::IdentityConflict`]
/// merely because the overlay directory changed. The request has no model-set
/// environment or stdin today; those belong here if they are ever added.
pub(crate) fn request_fingerprint(request: &ExecRequest) -> Result<String, ExecError> {
    #[derive(Serialize)]
    struct Fingerprint<'a> {
        command: &'a str,
        arguments: &'a [String],
        cwd: &'a str,
        files: &'a [crate::WorkspaceFilePath],
    }
    let bytes = serde_json::to_vec(&Fingerprint {
        command: &request.command,
        arguments: &request.arguments,
        cwd: &request.cwd,
        files: &request.files,
    })
    .map_err(|_| ExecError::InvalidRequest("request is not serializable".into()))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ensure_same_fingerprint(existing: &str, expected: &str) -> Result<(), ExecError> {
    if existing == expected {
        Ok(())
    } else {
        Err(ExecError::IdentityConflict)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        ExecFolderAccess, ExecFolderGrant, ExecutionId, ExecutionWorkspaceId, WorkspaceFilePath,
    };

    fn request() -> ExecRequest {
        ExecRequest::new(
            ExecutionId::parse("call-1").unwrap(),
            ExecutionWorkspaceId::parse("chat-1").unwrap(),
            "/bin/echo",
            vec!["ok".into()],
            ".",
        )
        .unwrap()
    }

    #[test]
    fn replay_preserves_the_original_error_kind() {
        let fingerprint = "abc".to_owned();
        let receipt = ExecutionReceipt::from_outcome(
            fingerprint.clone(),
            &Err(ExecError::WorkspaceFileTooLarge),
        );
        let replayed = receipt.replay(&fingerprint).unwrap_err();
        assert!(matches!(replayed, ExecError::WorkspaceFileTooLarge));
    }

    #[test]
    fn overlay_path_is_not_part_of_the_fingerprint() {
        let left = request()
            .with_folder_grants(vec![ExecFolderGrant::new(
                PathBuf::from("/granted/folder"),
                ExecFolderAccess::ReadWrite,
            )
            .unwrap()
            .staged_at(PathBuf::from("/tmp/overlay-a"))
            .unwrap()])
            .unwrap();
        let right = request()
            .with_folder_grants(vec![ExecFolderGrant::new(
                PathBuf::from("/granted/folder"),
                ExecFolderAccess::ReadWrite,
            )
            .unwrap()
            .staged_at(PathBuf::from("/tmp/overlay-b"))
            .unwrap()])
            .unwrap();
        assert_eq!(
            request_fingerprint(&left).unwrap(),
            request_fingerprint(&right).unwrap()
        );
        let receipt = ExecutionReceipt::from_outcome(
            request_fingerprint(&left).unwrap(),
            &Err(ExecError::Unavailable("gone".into())),
        );
        let replayed = receipt.replay(&request_fingerprint(&right).unwrap());
        assert!(matches!(
            replayed,
            Err(ExecError::Unavailable(message)) if message == "gone"
        ));
    }

    #[test]
    fn model_authored_fields_are_part_of_the_fingerprint() {
        let changed = ExecRequest::new(
            ExecutionId::parse("call-1").unwrap(),
            ExecutionWorkspaceId::parse("chat-1").unwrap(),
            "/bin/echo",
            vec!["other".into()],
            ".",
        )
        .unwrap();
        assert_ne!(
            request_fingerprint(&request()).unwrap(),
            request_fingerprint(&changed).unwrap()
        );
        let staged = request()
            .with_staged_files(vec!["output/a.txt".into()])
            .unwrap();
        assert_ne!(
            request_fingerprint(&request()).unwrap(),
            request_fingerprint(&staged).unwrap()
        );
        let _ = WorkspaceFilePath::parse("output/a.txt").unwrap();
    }
}

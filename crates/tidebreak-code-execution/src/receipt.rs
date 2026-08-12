use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CodeExecutionError, CodeExecutionRequest, CodeExecutionResponse};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum ExecutionReceipt {
    Running {
        fingerprint: String,
    },
    Completed {
        fingerprint: String,
        response: CodeExecutionResponse,
    },
    Failed {
        fingerprint: String,
        message: String,
    },
}

pub(crate) enum BeginExecution {
    Started,
    Cached(CodeExecutionResponse),
}

impl ExecutionReceipt {
    pub(crate) fn running(fingerprint: impl Into<String>) -> Self {
        Self::Running {
            fingerprint: fingerprint.into(),
        }
    }

    pub(crate) fn from_outcome(
        fingerprint: String,
        outcome: &Result<CodeExecutionResponse, CodeExecutionError>,
    ) -> Self {
        match outcome {
            Ok(response) => Self::Completed {
                fingerprint,
                response: response.clone(),
            },
            Err(error) => Self::Failed {
                fingerprint,
                message: error.to_string(),
            },
        }
    }

    pub(crate) fn replay(
        &self,
        fingerprint: &str,
        failed_error: fn(String) -> CodeExecutionError,
    ) -> Result<BeginExecution, CodeExecutionError> {
        match self {
            Self::Running {
                fingerprint: existing,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Err(CodeExecutionError::AmbiguousExecution)
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
                message,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Err(failed_error(message.clone()))
            }
        }
    }
}

pub(crate) fn request_fingerprint(
    request: &CodeExecutionRequest,
) -> Result<String, CodeExecutionError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|_| CodeExecutionError::InvalidRequest("request is not serializable".into()))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn ensure_same_fingerprint(existing: &str, expected: &str) -> Result<(), CodeExecutionError> {
    if existing == expected {
        Ok(())
    } else {
        Err(CodeExecutionError::IdentityConflict)
    }
}

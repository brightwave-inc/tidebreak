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
        message: String,
    },
}

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
                message: error.to_string(),
            },
        }
    }

    pub(crate) fn replay(
        &self,
        fingerprint: &str,
        failed_error: fn(String) -> ExecError,
    ) -> Result<BeginExecution, ExecError> {
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
                message,
            } => {
                ensure_same_fingerprint(existing, fingerprint)?;
                Err(failed_error(message.clone()))
            }
        }
    }
}

pub(crate) fn request_fingerprint(request: &ExecRequest) -> Result<String, ExecError> {
    let bytes = serde_json::to_vec(request)
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

//! Host-proxied model inference over the reverse channel.
//!
//! An attached-only sandbox run carries no model credential (see
//! [credential separation](../../docs/sandbox-providers.md)); the host is the
//! model proxy. This module dials one model completion back to the host over the
//! reverse-RPC lane the transport carries, keyed by a durable
//! [`OperationId`](tidebreak_sandbox_protocol::ids::OperationId) so a completion
//! re-issued after a reconnect is answered from the host's recorded outcome
//! rather than executed twice.

use tidebreak_sandbox_protocol::{
    ids::OperationId,
    protocol::Response,
    reverse::{ModelInferenceParams, ReverseRequest, ReverseResult},
    ReverseOutcome, SandboxRun,
};

/// Why a host-proxied model completion did not return text.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// The host answered with a transport-stable error (denied, cancelled,
    /// version, too large, or an operation-log refusal).
    #[error("the host refused the model completion: {code:?}: {message}")]
    Host {
        /// The stable failure class.
        code: tidebreak_sandbox_protocol::protocol::ErrorCode,
        /// The transport-safe detail.
        message: String,
    },
    /// The connection dropped before the completion arrived. Re-issuing the same
    /// operation identity after the host reattaches returns the recorded outcome.
    #[error("the reverse connection dropped before the completion arrived")]
    Disconnected,
    /// The host answered a model-inference call with a non-inference result.
    #[error("the host answered with an unexpected reverse result")]
    UnexpectedResult,
}

/// The sandbox's model client: every completion is a reverse call to the host.
#[derive(Clone)]
pub struct HostModel {
    run: SandboxRun,
}

impl HostModel {
    /// Build a model client that dials completions back over `run`'s reverse lane.
    #[must_use]
    pub fn new(run: SandboxRun) -> Self {
        Self { run }
    }

    /// Complete `prompt` through the host under the durable `operation_id`.
    ///
    /// # Errors
    /// [`ModelError::Host`] on a transport-stable refusal, [`ModelError::Disconnected`]
    /// if the connection drops mid-flight, or [`ModelError::UnexpectedResult`] if
    /// the host answers with a non-inference result.
    pub async fn complete(
        &self,
        operation_id: OperationId,
        prompt: impl Into<String>,
    ) -> Result<String, ModelError> {
        let request = ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: prompt.into(),
        });
        match self.run.call(operation_id, request).await {
            ReverseOutcome::Settled(Response::Ok(ReverseResult::ModelInference(result))) => {
                Ok(result.completion)
            }
            ReverseOutcome::Settled(Response::Ok(_)) => Err(ModelError::UnexpectedResult),
            ReverseOutcome::Settled(Response::Error(error)) => Err(ModelError::Host {
                code: error.code,
                message: error.message,
            }),
            ReverseOutcome::Disconnected => Err(ModelError::Disconnected),
        }
    }
}

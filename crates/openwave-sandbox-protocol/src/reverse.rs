//! The reverse-RPC callback channel: wire types, capability grants with run
//! provenance, and the two-lane frame model.
//!
//! Direction rule (unchanged from the design): the host dials the sandbox and
//! holds the connection; "reverse" names only which side originates *requests*
//! over it. The sandbox supervisor originates reverse requests; the host
//! answers them, authorized deny-by-default by the run's grants and recorded
//! idempotently by [`OperationId`](crate::ids::OperationId).
//!
//! Two lanes multiplex over one connection. The **request lane**
//! ([`RequestFrame`]) carries reverse requests and their responses and is
//! subject to backpressure. The **control lane** ([`ControlFrame`]) carries
//! cancellation and liveness and is deliberately *separate*, so a cancel or a
//! heartbeat is never stuck behind the request backlog it is trying to relieve
//! — the gap the reverse-RPC spike flagged for this step to close.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    ids::{OperationId, RequestId, RunId},
    protocol::Response,
};

/// A host-mediated capability the sandbox may request back over the channel.
///
/// Model inference is the first and, for this protocol slice, only carried
/// capability: a sandbox-resident loop cannot take a single step without it.
/// The enum is `#[non_exhaustive]` so host search and consent prompts join as
/// variants without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Run one model completion through the host, which is the model proxy.
    ModelInference,
}

/// Parameters for one host-proxied model completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInferenceParams {
    pub prompt: String,
}

/// Bounded result of one host-proxied model completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInferenceResult {
    pub completion: String,
}

/// A capability-checked reverse request the sandbox issues to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "capability",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum ReverseRequest {
    ModelInference(ModelInferenceParams),
}

impl ReverseRequest {
    /// Capability this request is gated on, for deny-by-default authorization.
    #[must_use]
    pub const fn capability(&self) -> Capability {
        match self {
            ReverseRequest::ModelInference(_) => Capability::ModelInference,
        }
    }

    /// Whether this inbound request is within its declared per-capability bound.
    ///
    /// The host checks this before executing: a request is untrusted input, and
    /// an over-bound one is refused, not forwarded.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        match self {
            ReverseRequest::ModelInference(params) => params.within_bounds(),
        }
    }
}

/// A bounded typed success for one reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capability", content = "payload", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReverseResult {
    ModelInference(ModelInferenceResult),
}

/// A reverse request the sandbox multiplexes toward the host on the request
/// lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReverseEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub operation_id: OperationId,
    pub request: ReverseRequest,
}

/// The host's correlated answer to one reverse request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReverseResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub operation_id: OperationId,
    pub response: Response<ReverseResult>,
}

/// A unit of the request lane: a request sandbox -> host or a response host ->
/// sandbox, correlated by [`RequestId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "frame",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum RequestFrame {
    Request(ReverseEnvelope),
    Response(ReverseResponseEnvelope),
}

/// A unit of the **reserved control lane**, distinct from the request lane so
/// it is never subject to request backpressure.
///
/// Cancellation flows sandbox -> host; liveness flows in both directions. A
/// conforming transport MUST carry this lane independently of [`RequestFrame`]
/// (a separate stream, queue, or priority), so a [`ControlFrame::Cancel`] can
/// preempt a saturated request backlog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "control",
    content = "body",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum ControlFrame {
    /// Cancel an in-flight reverse operation by its durable identity.
    Cancel { operation_id: OperationId },
    /// Liveness probe.
    Ping { nonce: u64 },
    /// Liveness response.
    Pong { nonce: u64 },
}

/// Provenance carried on every reverse operation for audit.
///
/// Consent prompts arriving over the reverse channel are rendered with this —
/// which run, which provider — so the user knows who is asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProvenance {
    /// The run whose grants authorize the operation.
    pub run_id: RunId,
    /// The provider that stood up the sandbox, treated as untrusted attribution.
    pub provider: String,
}

/// The deny-by-default grant set resolved for one run at admission.
///
/// A capability absent from [`GrantSet::capabilities`] is refused and never
/// executes. The set is host state, never a claim the sandbox makes about
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantSet {
    provenance: RunProvenance,
    capabilities: BTreeSet<Capability>,
}

impl GrantSet {
    /// Build a grant set for `provenance` granting exactly `capabilities`.
    #[must_use]
    pub fn new(
        provenance: RunProvenance,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        Self {
            provenance,
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// A grant set that authorizes nothing — the deny-by-default default.
    #[must_use]
    pub fn none(provenance: RunProvenance) -> Self {
        Self {
            provenance,
            capabilities: BTreeSet::new(),
        }
    }

    /// Whether `capability` is granted to this run.
    #[must_use]
    pub fn allows(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The run provenance carried for audit.
    #[must_use]
    pub const fn provenance(&self) -> &RunProvenance {
        &self.provenance
    }

    /// The granted capabilities, for the attach handshake's advertisement.
    #[must_use]
    pub fn granted(&self) -> Vec<Capability> {
        self.capabilities.iter().copied().collect()
    }
}

/// The host's execution of a granted capability — its model proxy and, later,
/// host search and consent prompts.
///
/// Called at most once per [`OperationId`](crate::ids::OperationId); the
/// operation log, not this trait, enforces that. An implementation should be
/// cancel-safe: the host aborts the returned future when the sandbox cancels,
/// so partial work must not leave an unrecorded external effect the design
/// would then have to reconcile.
#[async_trait::async_trait]
pub trait CapabilityResponder: Send + Sync {
    /// Execute one reverse request and return its bounded outcome.
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult>;
}

impl ReverseResult {
    /// Whether this result is within its declared per-capability bound.
    ///
    /// The host checks this before recording: a responder that returns an
    /// over-bound completion has its result rejected rather than persisted.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        match self {
            ReverseResult::ModelInference(result) => result.within_bounds(),
        }
    }
}

impl ModelInferenceResult {
    /// Whether the completion is within its declared per-capability bound.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.completion.len() <= crate::protocol::MAX_MODEL_COMPLETION_BYTES
    }
}

impl ModelInferenceParams {
    /// Whether the prompt is within its declared per-capability bound.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.prompt.len() <= crate::protocol::MAX_MODEL_PROMPT_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;

    #[test]
    fn request_frame_roundtrips() {
        let frame = RequestFrame::Request(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id: OperationId::new(),
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "hello".to_owned(),
            }),
        });
        let encoded = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            serde_json::from_str::<RequestFrame>(&encoded).unwrap(),
            frame
        );
    }

    #[test]
    fn control_frame_is_a_distinct_wire_shape() {
        let cancel = ControlFrame::Cancel {
            operation_id: OperationId::new(),
        };
        let encoded = serde_json::to_value(cancel).unwrap();
        assert_eq!(encoded["control"], "cancel");
        assert_eq!(
            serde_json::from_value::<ControlFrame>(encoded).unwrap(),
            cancel
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let encoded = format!(
            r#"{{"protocol_version":1,"request_id":"{}","operation_id":"{}","request":{{"capability":"model_inference","payload":{{"prompt":"hi","extra":true}}}}}}"#,
            RequestId::new(),
            OperationId::new(),
        );
        assert!(serde_json::from_str::<ReverseEnvelope>(&encoded).is_err());
    }

    #[test]
    fn grant_set_denies_by_default() {
        let provenance = RunProvenance {
            run_id: RunId::new(),
            provider: "reference".to_owned(),
        };
        let empty = GrantSet::none(provenance.clone());
        assert!(!empty.allows(Capability::ModelInference));

        let granted = GrantSet::new(provenance, [Capability::ModelInference]);
        assert!(granted.allows(Capability::ModelInference));
        assert_eq!(granted.granted(), vec![Capability::ModelInference]);
    }
}

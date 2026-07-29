//! Versioned, transport-neutral host-broker request and response types.
//!
//! Control and operation envelopes are deliberately separate. A desktop host
//! may construct [`ControlEnvelope`] values after trusted user actions; an
//! agent executor receives only the [`OperationEnvelope`] surface.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Capability, ConsentMethod, ExecutionContext, GrantSubject, OperationId, RelativePath,
    RequestId, RootId,
};

/// Current pre-v1 broker protocol. Bump this for incompatible wire changes.
pub const PROTOCOL_VERSION: u32 = 8;

/// Largest file the broker returns as opaque bytes.
///
/// Binary reads exist so trusted native code can hand a document the agent
/// cannot read as text — a PDF or an Office file — to a product ingest
/// pipeline. The bytes travel base64-encoded over the newline-delimited JSON
/// transport, so [`crate::sidecar::MAX_RESPONSE_BYTES`] is derived from this
/// bound rather than chosen independently.
pub const MAX_READ_FILE_BINARY_BYTES: usize = 8 * 1024 * 1024;

/// Host-originated request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub request: ControlRequest,
}

/// Agent-operation request envelope.
///
/// `context` comes from trusted conversation execution state. It is not a
/// model-generated tool argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEnvelope {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub context: ExecutionContext,
    pub request: OperationRequest,
}

/// Trusted control actions. Agent code must not receive a [`crate::Controller`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "control",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum ControlRequest {
    /// Version negotiation. This request is accepted even when its envelope
    /// version differs so the host can discover the broker's version.
    Hello,
    /// List safe summaries of folders previously approved on this host.
    ///
    /// This is a trusted-host management operation, not an agent operation.
    /// It exposes neither absolute paths nor attachment authority.
    ListApprovedRoots,
    /// Register the exact folder returned by a native picker and attach it to
    /// the conversation that initiated the trusted interaction.
    RegisterRoot(RegisterRootRequest),
    /// Inspect a durable registration receipt without retrying the mutation.
    LookupRegisterRootReceipt(LookupRegisterRootReceiptRequest),
    /// Attach an already registered root to one conversation. Idempotent.
    AttachRoot(RootAttachmentMutationRequest),
    /// Detach one root from one conversation without globally revoking it.
    DetachRoot(RootAttachmentMutationRequest),
    /// Inspect a durable attach/detach receipt without retrying the mutation.
    LookupRootAttachmentReceipt(LookupRootAttachmentReceiptRequest),
    /// Disconnect a root owned by the exact subject. Idempotent.
    RevokeRoot(RevokeRootRequest),
}

/// Strict payload for a native-picker root registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRootRequest {
    /// Stable idempotency identity for this mutation, independent of transport
    /// request/retry correlation.
    pub operation_id: OperationId,
    pub subject: GrantSubject,
    pub conversation_id: Uuid,
    pub path: PathBuf,
    pub consent_method: ConsentMethod,
}

/// Strict payload for recovery-only registration receipt lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupRegisterRootReceiptRequest {
    /// Exact idempotency identity of the prior registration attempt.
    pub operation_id: OperationId,
    /// Trusted authority identity expected on the original registration.
    pub subject: GrantSubject,
    /// Trusted conversation expected on the original registration.
    pub conversation_id: Uuid,
}

/// Strict payload for an exact conversation attachment mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentMutationRequest {
    /// Stable idempotency identity for this mutation.
    pub operation_id: OperationId,
    /// Trusted product subject receiving or removing the conversation grant.
    pub subject: GrantSubject,
    /// Exact conversation whose live broker attachment changes.
    pub conversation_id: Uuid,
    pub root_id: RootId,
    /// Fresh trusted consent for an attach; detach requests carry no consent.
    pub consent_method: Option<ConsentMethod>,
}

/// Desired state used to fence recovery-only attachment receipt lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentMutationKind {
    Attach,
    Detach,
}

/// Strict payload for recovery-only attachment receipt lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupRootAttachmentReceiptRequest {
    pub operation_id: OperationId,
    pub subject: GrantSubject,
    pub conversation_id: Uuid,
    pub root_id: RootId,
    pub mutation: RootAttachmentMutationKind,
}

/// Strict payload for an idempotent root revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRootRequest {
    /// Stable idempotency identity for this mutation, independent of transport
    /// request/retry correlation.
    pub operation_id: OperationId,
    /// Original registering subject allowed to forget this host approval.
    pub subject: GrantSubject,
    pub root_id: RootId,
}

/// Capability-checked agent operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum OperationRequest {
    /// Discover safe summaries of roots available to the active conversation.
    ListRoots,
    /// List one directory under an explicitly selected root.
    ListDirectory(PathRequest),
    /// Read one bounded UTF-8 file under an explicitly selected root.
    ReadFile(PathRequest),
    /// Read one bounded file under an explicitly selected root as opaque bytes.
    ///
    /// This carries the same [`crate::Capability::ReadFiles`] authority as
    /// [`OperationRequest::ReadFile`] — the user consented to reading files
    /// below the root, not to a particular encoding. It exists so trusted
    /// native code can move a file the agent cannot read as text into a
    /// product pipeline; the bytes are not agent-readable.
    ReadFileBinary(PathRequest),
    /// Atomically publish caller-supplied, digest-bound bytes below an attached root.
    ///
    /// Only the trusted native output executor constructs this request. The
    /// model supplies an output identity, never these bytes or approval data.
    WriteFile(WriteFileRequest),
}

/// Strict root-relative payload shared by list and read operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRequest {
    pub root_id: RootId,
    pub path: RelativePath,
}

/// Whether an output publication may replace an existing regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteFileMode {
    Create,
    Replace,
}

/// Fresh native consent bound to one replacement request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteApproval {
    pub approval_id: Uuid,
}

/// Idempotent, bounded write request produced only by the trusted desktop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileRequest {
    pub operation_id: OperationId,
    pub root_id: RootId,
    pub path: RelativePath,
    pub mode: WriteFileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<WriteApproval>,
    pub content_base64: String,
    pub bytes: usize,
    pub sha256: [u8; 32],
}

impl std::fmt::Debug for WriteFileRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteFileRequest")
            .field("operation_id", &self.operation_id)
            .field("root_id", &self.root_id)
            .field("path", &self.path)
            .field("mode", &self.mode)
            .field("approval", &self.approval)
            .field("content_base64", &"[redacted]")
            .field("bytes", &self.bytes)
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// Correlated transport response. The broker version is returned on success and
/// failure, including a version-skewed Hello request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope<T> {
    pub protocol_version: u32,
    pub request_id: RequestId,
    pub response: Response<T>,
}

/// Typed success or safe, transport-stable error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "payload", rename_all = "snake_case")]
pub enum Response<T> {
    Ok(T),
    Error(ErrorResponse),
}

/// Stable failure classes exposed across the broker transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    ProtocolVersion,
    OperationIdConflict,
    OperationInProgress,
    Denied,
    InvalidRequest,
    InvalidRoot,
    TooLarge,
    UnsupportedContent,
    HostIo,
    Internal,
    AlreadyExists,
    NotFound,
    AmbiguousWrite,
}

/// Safe error payload; it never embeds an absolute path or raw OS error text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

pub type ControlResponseEnvelope = ResponseEnvelope<ControlResult>;
pub type OperationResponseEnvelope = ResponseEnvelope<OperationResult>;

/// Successful response to a control action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ControlResult {
    Hello(HelloResult),
    ListApprovedRoots { roots: Vec<RootSummary> },
    RegisterRoot(RegisterRootResult),
    LookupRegisterRootReceipt(LookupRegisterRootReceiptResult),
    AttachRoot(RootAttachmentMutationResult),
    DetachRoot(RootAttachmentMutationResult),
    LookupRootAttachmentReceipt(LookupRootAttachmentReceiptResult),
    RevokeRoot(RevokeRootResult),
}

/// Successful response to an agent operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationResult {
    ListRoots { roots: Vec<RootAccess> },
    ListDirectory { entries: Vec<DirectoryEntry> },
    ReadFile(ReadFileResult),
    ReadFileBinary(ReadFileBinaryResult),
    WriteFile(WriteFileResult),
}

/// Durable terminal receipt for one exact connected-root write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFileResult {
    pub operation_id: OperationId,
    pub bytes: usize,
    pub replaced: bool,
}

/// Version handshake response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelloResult {
    pub protocol_version: u32,
    pub operations: Vec<String>,
}

/// Safe agent-facing identity for a connected folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootSummary {
    pub root_id: RootId,
    pub display_name: String,
}

/// One reachable folder together with what the broker would actually allow on
/// it right now.
///
/// `capabilities` is not a stored description of the folder. It is produced by
/// asking the same authorization path that gates the real operations, for this
/// exact conversation, so a caller that renders it cannot claim access the
/// broker would refuse — or hide access it would allow. Only per-folder
/// capabilities appear here; subject-wide ones such as
/// [`crate::Capability::ListRoots`] are not properties of a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAccess {
    pub root_id: RootId,
    pub display_name: String,
    pub capabilities: Vec<Capability>,
}

/// Result of registering and attaching a selected folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRootResult {
    pub root: RootSummary,
}

/// Recovery-only view of one durable registration mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupRegisterRootReceiptResult {
    pub operation_id: OperationId,
    pub receipt: RegisterRootReceipt,
}

/// Durable state observed without starting or resuming registration work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RegisterRootReceipt {
    /// No mutation has used this operation identity.
    Unknown,
    /// A registration was durably claimed but has no known terminal outcome.
    Pending,
    /// Registration durably committed this safe root summary.
    Completed { root: RootSummary },
    /// Registration committed historically, but the root is no longer connected.
    Disconnected { root: RootSummary },
    /// Registration durably committed this transport-safe failure.
    Failed { error: ErrorResponse },
}

/// Durable result of one exact conversation attachment mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentMutationResult {
    pub root_id: RootId,
    pub mutation: RootAttachmentMutationKind,
    /// Whether this operation changed the live attachment set.
    pub changed: bool,
}

/// Recovery-only view of one durable attachment mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookupRootAttachmentReceiptResult {
    pub operation_id: OperationId,
    pub receipt: RootAttachmentMutationReceipt,
}

/// Durable state observed without starting or resuming attach/detach work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RootAttachmentMutationReceipt {
    Unknown,
    Completed {
        result: RootAttachmentMutationResult,
        /// Live state now; a later exact mutation may have changed it.
        currently_attached: bool,
    },
    Failed {
        error: ErrorResponse,
        /// Live state now, which a failed mutation does not describe. A rejected
        /// attach usually leaves nothing attached, and a caller that cannot tell
        /// that apart from "unknown" has to keep treating the folder as though
        /// authority might still exist.
        currently_attached: bool,
    },
}

/// Result of an idempotent root revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRootResult {
    pub revoked: bool,
}

/// Portable kind of one addressable directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

/// One entry whose name can safely be used in a later broker request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// Bounded UTF-8 file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileResult {
    pub content: String,
    pub bytes: usize,
}

/// Bounded opaque file content, base64-encoded for the JSON transport.
///
/// `bytes` is the decoded length, so a caller can bound its own work before
/// decoding. Content is deliberately not logged or `Debug`-printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFileBinaryResult {
    pub content_base64: String,
    pub bytes: usize,
}

impl std::fmt::Debug for ReadFileBinaryResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadFileBinaryResult")
            .field("content_base64", &"[redacted]")
            .field("bytes", &self.bytes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_and_operation_channels_have_distinct_wire_shapes() {
        let control = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::Hello,
        };
        let encoded = serde_json::to_value(&control).unwrap();
        assert_eq!(encoded["request"]["control"], "hello");
        assert!(encoded.get("context").is_none());

        let operation = OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            context: ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
            request: OperationRequest::ListRoots,
        };
        let encoded = serde_json::to_value(&operation).unwrap();
        assert_eq!(encoded["request"]["operation"], "list_roots");
        assert!(encoded.get("context").is_some());
    }

    #[test]
    fn protocol_paths_cannot_bypass_relative_path_validation() {
        let encoded = format!(
            r#"{{"protocol_version":2,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"read_file","payload":{{"root_id":"{}","path":"../secret"}}}}}}"#,
            RequestId::new(),
            Uuid::new_v4(),
            RootId::new(),
        );
        assert!(serde_json::from_str::<OperationEnvelope>(&encoded).is_err());
    }

    #[test]
    fn request_leaf_payloads_reject_unknown_fields() {
        let encoded = format!(
            r#"{{"protocol_version":2,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"read_file","payload":{{"root_id":"{}","path":"note.txt","unexpected":true}}}}}}"#,
            RequestId::new(),
            Uuid::new_v4(),
            RootId::new(),
        );
        assert!(serde_json::from_str::<OperationEnvelope>(&encoded).is_err());

        let conversation_id = Uuid::new_v4();
        let attachment = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            request: ControlRequest::AttachRoot(RootAttachmentMutationRequest {
                operation_id: OperationId::new(),
                subject: GrantSubject::conversation(conversation_id).unwrap(),
                conversation_id,
                root_id: RootId::new(),
                consent_method: Some(ConsentMethod::PermissionDialog),
            }),
        };
        let mut encoded = serde_json::to_value(attachment).unwrap();
        encoded["request"]["payload"]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ControlEnvelope>(encoded).is_err());
    }

    #[test]
    fn request_variants_reject_unknown_sibling_fields() {
        let control = format!(
            r#"{{"protocol_version":2,"request_id":"{}","request":{{"control":"hello","extra":true}}}}"#,
            RequestId::new()
        );
        assert!(serde_json::from_str::<ControlEnvelope>(&control).is_err());
        let operation = format!(
            r#"{{"protocol_version":2,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"list_roots","extra":true}}}}"#,
            RequestId::new(),
            Uuid::new_v4(),
        );
        assert!(serde_json::from_str::<OperationEnvelope>(&operation).is_err());
    }

    #[test]
    fn response_variants_roundtrip_without_ambiguous_payloads() {
        let control = ControlResult::RegisterRoot(RegisterRootResult {
            root: RootSummary {
                root_id: RootId::new(),
                display_name: "Documents".to_owned(),
            },
        });
        let encoded = serde_json::to_string(&control).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlResult>(&encoded).unwrap(),
            control
        );

        let operation_id = OperationId::new();
        let lookup = ControlResult::LookupRegisterRootReceipt(LookupRegisterRootReceiptResult {
            operation_id,
            receipt: RegisterRootReceipt::Pending,
        });
        let encoded = serde_json::to_string(&lookup).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlResult>(&encoded).unwrap(),
            lookup
        );

        let attachment_lookup =
            ControlResult::LookupRootAttachmentReceipt(LookupRootAttachmentReceiptResult {
                operation_id: OperationId::new(),
                receipt: RootAttachmentMutationReceipt::Completed {
                    result: RootAttachmentMutationResult {
                        root_id: RootId::new(),
                        mutation: RootAttachmentMutationKind::Attach,
                        changed: true,
                    },
                    currently_attached: false,
                },
            });
        let encoded = serde_json::to_string(&attachment_lookup).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlResult>(&encoded).unwrap(),
            attachment_lookup
        );

        let operation = OperationResult::ReadFile(ReadFileResult {
            content: "hello".to_owned(),
            bytes: 5,
        });
        let encoded = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<OperationResult>(&encoded).unwrap(),
            operation
        );

        let request_id = RequestId::new();
        let response: OperationResponseEnvelope = ResponseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            response: Response::Error(ErrorResponse {
                code: ErrorCode::Denied,
                message: "host operation was denied".to_owned(),
                retryable: false,
            }),
        };
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded = serde_json::from_str::<OperationResponseEnvelope>(&encoded).unwrap();
        assert_eq!(decoded, response);
        assert_eq!(decoded.request_id, request_id);
    }
}

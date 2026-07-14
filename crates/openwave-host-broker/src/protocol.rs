//! Versioned, transport-neutral host-broker request and response types.
//!
//! Control and operation envelopes are deliberately separate. A desktop host
//! may construct [`ControlEnvelope`] values after trusted user actions; an
//! agent executor receives only the [`OperationEnvelope`] surface.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    ConsentMethod, ExecutionContext, GrantSubject, OperationId, RelativePath, RequestId, RootId,
};

/// First public broker protocol. Bump this for incompatible wire changes.
pub const PROTOCOL_VERSION: u32 = 1;

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
    /// Register the exact folder returned by a native picker and attach it to
    /// the conversation that initiated the trusted interaction.
    RegisterRoot(RegisterRootRequest),
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

/// Strict payload for an idempotent root revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeRootRequest {
    /// Stable idempotency identity for this mutation, independent of transport
    /// request/retry correlation.
    pub operation_id: OperationId,
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
}

/// Strict root-relative payload shared by list and read operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRequest {
    pub root_id: RootId,
    pub path: RelativePath,
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
    RegisterRoot(RegisterRootResult),
    RevokeRoot(RevokeRootResult),
}

/// Successful response to an agent operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationResult {
    ListRoots { roots: Vec<RootSummary> },
    ListDirectory { entries: Vec<DirectoryEntry> },
    ReadFile(ReadFileResult),
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

/// Result of registering and attaching a selected folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRootResult {
    pub root: RootSummary,
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
            r#"{{"protocol_version":1,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"read_file","payload":{{"root_id":"{}","path":"../secret"}}}}}}"#,
            RequestId::new(),
            Uuid::new_v4(),
            RootId::new(),
        );
        assert!(serde_json::from_str::<OperationEnvelope>(&encoded).is_err());
    }

    #[test]
    fn request_leaf_payloads_reject_unknown_fields() {
        let encoded = format!(
            r#"{{"protocol_version":1,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"read_file","payload":{{"root_id":"{}","path":"note.txt","unexpected":true}}}}}}"#,
            RequestId::new(),
            Uuid::new_v4(),
            RootId::new(),
        );
        assert!(serde_json::from_str::<OperationEnvelope>(&encoded).is_err());
    }

    #[test]
    fn request_variants_reject_unknown_sibling_fields() {
        let control = format!(
            r#"{{"protocol_version":1,"request_id":"{}","request":{{"control":"hello","extra":true}}}}"#,
            RequestId::new()
        );
        assert!(serde_json::from_str::<ControlEnvelope>(&control).is_err());
        let operation = format!(
            r#"{{"protocol_version":1,"request_id":"{}","context":{{"conversation_id":"{}","project_id":null}},"request":{{"operation":"list_roots","extra":true}}}}"#,
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

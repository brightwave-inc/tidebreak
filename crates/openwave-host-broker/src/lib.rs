//! Capability and path-policy core for access to user-approved host resources.
//!
//! This crate defines policy values, a versioned protocol, and an owning broker
//! with separate trusted-control and agent-operation handles. The broker keeps
//! registry locks short: it authorizes and clones a pinned root, performs
//! bounded-result I/O, then reauthorizes before releasing bytes. A completed
//! revocation therefore fences in-flight results without waiting on host I/O.
//! No desktop runtime or concrete transport is referenced here.

pub mod audit;
pub mod broker;
pub mod capability;
pub mod id;
pub mod path_policy;
pub mod protocol;
pub mod relative_path;
pub mod sidecar;

pub use audit::{
    AuditActor, AuditError, AuditEvent, AuditLabel, AuditOperation, AuditOutcome, AuditSink,
    AuditTarget,
};
pub use broker::{Broker, BrokerError, Controller, Operator};
pub use capability::{
    Capability, ConsentMethod, ConsentRecord, Grant, GrantError, RootAttachment, Scope,
};
pub use id::{
    ExecutionContext, GrantId, GrantSubject, IdError, OperationId, ParseIdError, RequestId, RootId,
    SubjectKind,
};
pub use path_policy::{RootPolicy, RootPolicyError, ValidatedRoot};
pub use protocol::{
    ControlEnvelope, ControlRequest, ControlResponseEnvelope, ControlResult, DirectoryEntry,
    EntryKind, ErrorCode, ErrorResponse, HelloResult, LookupRegisterRootReceiptRequest,
    LookupRegisterRootReceiptResult, LookupRootAttachmentReceiptRequest,
    LookupRootAttachmentReceiptResult, OperationEnvelope, OperationRequest,
    OperationResponseEnvelope, OperationResult, PathRequest, ReadFileBinaryResult, ReadFileResult,
    RegisterRootReceipt, RegisterRootRequest, RegisterRootResult, Response, ResponseEnvelope,
    RevokeRootRequest, RevokeRootResult, RootAttachmentMutationKind, RootAttachmentMutationReceipt,
    RootAttachmentMutationRequest, RootAttachmentMutationResult, RootSummary,
    MAX_READ_FILE_BINARY_BYTES, PROTOCOL_VERSION,
};
pub use relative_path::{RelativePath, RelativePathError};

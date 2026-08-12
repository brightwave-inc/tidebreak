//! Capability and path-policy core for access to user-approved host resources.
//!
//! This crate defines policy values, a versioned protocol, and an owning broker
//! with separate trusted-control and agent-operation handles. The broker keeps
//! registry locks short: it authorizes and clones a pinned root, performs
//! bounded-result I/O, then reauthorizes before releasing bytes. A completed
//! revocation therefore fences in-flight results without waiting on host I/O.
//! No desktop runtime or concrete transport is referenced here.

pub mod audit;
pub mod blocklist;
pub mod broker;
pub mod capability;
pub mod computer_use;
pub mod consequential;
pub mod id;
pub mod path_policy;
pub mod protocol;
pub mod relative_path;
pub mod set_of_marks;
pub mod sidecar;

pub use audit::{
    AuditActor, AuditError, AuditEvent, AuditLabel, AuditOperation, AuditOutcome, AuditSink,
    AuditTarget,
};
pub use blocklist::{is_blocked_control_bundle, BLOCKED_CONTROL_BUNDLES};
pub use broker::{Broker, BrokerError, Controller, Operator};
pub use capability::{
    Capability, ConsentMethod, ConsentRecord, Grant, GrantError, RootAttachment, Scope,
};
pub use computer_use::{
    AxTree, BackendError, BackendErrorKind, CaptureMeta, CaptureTarget, ComputerUseBackend,
    ControlMeta, ElementDescription, ElementTarget, HelperBackend, PermissionStatus,
    UnsupportedBackend, WindowFrame, WindowInfo, HELPER_PATH_ENV,
};
pub use consequential::{classify, truncate_label, Consequence, ControlOp};
pub use id::{
    AppId, ExecutionContext, GrantId, GrantSubject, IdError, OperationId, ParseIdError, RequestId,
    RootId, SubjectKind,
};
pub use path_policy::{RootPolicy, RootPolicyError, ValidatedRoot};
pub use protocol::{
    AppFolderPathRequest, AppFolderWriteRequest, CaptureTargetWire, ControlEnvelope,
    ControlRequest, ControlResponseEnvelope, ControlResult, CuCaptureScreenResult,
    CuConfirmControlActionRequest, CuGrantAppRequest, CuGrantAppResult, CuListAppGrantsRequest,
    CuNeedsConfirmationResult, CuPermissionStatusResult, CuResolveHandoffRequest,
    CuResolveHandoffResult, CuRevokeAppRequest, DirectoryEntry, ElementTargetWire, EntryKind,
    ErrorCode, ErrorResponse, GrantRootCapabilityRequest, GrantRootCapabilityResult,
    GrantStatementSummary, HelloResult, LookupRegisterRootReceiptRequest,
    LookupRegisterRootReceiptResult, LookupRootAttachmentReceiptRequest,
    LookupRootAttachmentReceiptResult, OperationEnvelope, OperationRequest,
    OperationResponseEnvelope, OperationResult, PathRequest, PurgeConversationSubjectRequest,
    PurgeConversationSubjectResult, ReadFileBinaryResult, ReadFileResult, RegisterRootReceipt,
    RegisterRootRequest, RegisterRootResult, ResolveExecRootsRequest, ResolvedExecRoot, Response,
    ResponseEnvelope, RevokeGrantRequest, RevokeGrantResult, RevokeRootRequest, RevokeRootResult,
    RootAccess, RootAttachmentMutationKind, RootAttachmentMutationReceipt,
    RootAttachmentMutationRequest, RootAttachmentMutationResult, RootSummary,
    UnavailableRootReason, UnavailableRootSummary, WriteApproval, WriteFileMode, WriteFileRequest,
    WriteFileResult, MAX_HANDOFF_BYTES, MAX_READ_FILE_BINARY_BYTES, PROTOCOL_VERSION,
};
pub use relative_path::{RelativePath, RelativePathError};
pub use set_of_marks::{extract_marks, Mark, MarkFrame};

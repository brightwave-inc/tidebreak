//! Owning broker for consent mutations and capability-checked filesystem reads.
//!
//! The controller and operator share one registry. Operations take an authorized
//! clone of a pinned root handle under a short lock, perform bounded-result I/O,
//! then reauthorize before releasing bytes. Revocation therefore completes
//! without waiting on host I/O, prevents new operations, and fences results from
//! operations that were already in flight.

use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{Dir, OpenOptions};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    audit::{
        AuditActor, AuditError, AuditEvent, AuditOperation, AuditOutcome, AuditSink, AuditTarget,
        JsonlAuditSink, MemoryAuditSink,
    },
    path_policy::RootIdentity,
    protocol::{
        ControlEnvelope, ControlRequest, ControlResponseEnvelope, ControlResult, DirectoryEntry,
        EntryKind, ErrorCode, ErrorResponse, HelloResult, LookupRegisterRootReceiptRequest,
        LookupRegisterRootReceiptResult, LookupRootAttachmentReceiptRequest,
        LookupRootAttachmentReceiptResult, OperationEnvelope, OperationRequest,
        OperationResponseEnvelope, OperationResult, PathRequest, ReadFileBinaryResult,
        ReadFileResult, RegisterRootReceipt, RegisterRootRequest, RegisterRootResult,
        ResolveExecRootsRequest, ResolvedExecRoot, Response, ResponseEnvelope, RevokeRootRequest,
        RevokeRootResult, RootAccess, RootAttachmentMutationKind, RootAttachmentMutationReceipt,
        RootAttachmentMutationRequest, RootAttachmentMutationResult, RootSummary, WriteFileMode,
        WriteFileRequest, WriteFileResult, MAX_READ_FILE_BINARY_BYTES, PROTOCOL_VERSION,
    },
    Capability, ConsentMethod, ConsentRecord, ExecutionContext, Grant, GrantError, GrantId,
    GrantSubject, OperationId, RelativePath, RequestId, RootAttachment, RootId, RootPolicy,
    RootPolicyError, Scope, SubjectKind, ValidatedRoot,
};

mod state_file;
use state_file::StateFile;

const MAX_READ_FILE_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_ENTRIES: usize = 4_096;
const MAX_LIST_ROOTS: usize = 256;
const MAX_RESOLVE_EXEC_ROOTS: usize = 32;
const MAX_ROOT_DISPLAY_BYTES: usize = 1024;
pub(crate) const MAX_WRITE_FILE_BYTES: usize = 512 * 1024;

/// A broker request failed without widening host access.
#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("unsupported broker protocol version {received}; expected {expected}")]
    ProtocolVersion { received: u32, expected: u32 },
    #[error("operation identity was reused for a different control request")]
    OperationIdConflict,
    #[error("operation with this identity is already in progress")]
    OperationInProgress,
    #[error("the active conversation is not allowed to perform this host operation")]
    Denied,
    #[error("connected root does not exist")]
    UnknownRoot,
    #[error("conversation does not match the conversation-scoped grant subject")]
    SubjectConversationMismatch,
    #[error("folder-picker registration used an invalid consent method")]
    InvalidConsentMethod,
    #[error("broker state lock was poisoned")]
    StatePoisoned,
    #[error("broker state publication may have committed; restart is required")]
    PersistenceAmbiguous,
    #[error("broker state exceeds its durable size limit")]
    StateTooLarge,
    #[error("path is not a regular file")]
    NotRegularFile,
    #[error("file is too large (maximum {maximum} bytes)")]
    FileTooLarge { maximum: usize },
    #[error("file is not valid UTF-8")]
    NotUtf8,
    #[error("directory listing exceeds broker limits")]
    DirectoryTooLarge,
    #[error("connected root listing exceeds broker limits")]
    RootListTooLarge,
    #[error(transparent)]
    RootPolicy(#[from] RootPolicyError),
    #[error(transparent)]
    InvalidGrant(#[from] GrantError),
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error("host filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("write destination already exists")]
    DestinationExists,
    #[error("write destination does not exist")]
    DestinationMissing,
    #[error("replacement requires a fresh approval identity")]
    ReplacementApprovalRequired,
    #[error("write content does not match its bounded digest")]
    InvalidWriteContent,
    #[error("write dispatch may have partially completed")]
    AmbiguousWrite,
    #[error("a durable write receipt contains a terminal failure")]
    StoredWriteFailure(ErrorResponse),
}

/// Owner of the shared broker registry.
#[derive(Clone)]
pub struct Broker {
    shared: Arc<Shared>,
}

/// Trusted host-only control surface.
#[derive(Clone)]
pub struct Controller {
    shared: Arc<Shared>,
}

/// Capability-checked agent-operation surface.
#[derive(Clone)]
pub struct Operator {
    shared: Arc<Shared>,
}

struct Shared {
    policy: RootPolicy,
    state: Mutex<State>,
    state_file: Option<StateFile>,
    audit: Arc<dyn AuditSink>,
    failed_closed: AtomicBool,
}

impl Shared {
    /// Make the intent to mutate the host durable before anything is touched.
    ///
    /// The caller must refuse the operation when this fails. That is the whole
    /// contract: a file the user cannot see a record of was never overwritten,
    /// because the record comes first.
    fn record_intent(&self, event: &AuditEvent) -> Result<(), AuditError> {
        self.audit.record(event)
    }

    /// Record what an operation turned out to do.
    ///
    /// Nothing can be undone by the time this runs, so a failure here cannot
    /// refuse anything; it leaves the paired intent record standing alone,
    /// which reads as "this was attempted, the result is unknown". Read-only
    /// operations carry no intent record and are logged only here — an
    /// unrecorded read is worth less than the user losing access to their own
    /// files while the log is unwritable.
    fn record_completion(&self, event: &AuditEvent) {
        if let Err(error) = self.audit.record(event) {
            eprintln!("openwave host broker could not persist audit event: {error}");
        }
    }
}

#[derive(Clone, Default)]
struct State {
    roots: HashMap<RootId, RegisteredRoot>,
    grants: Vec<Grant>,
    attachments: Vec<RootAttachment>,
    mutations: HashMap<OperationId, MutationRecord>,
    active_mutations: HashSet<OperationId>,
    /// Persisted roots whose directory could not be reopened at load. They are
    /// held out of the live tables so the rest of the registry still works, and
    /// written back verbatim so the approval survives the outage.
    unavailable: Vec<UnavailableRoot>,
}

/// A registration that is dormant for the lifetime of this process.
///
/// The user approved this folder and nothing has withdrawn that approval; the
/// host simply cannot produce a pinned handle for it right now. Its grants and
/// attachments travel with it rather than being dropped, because the common
/// cause — an external volume that is not mounted — resolves on its own, and
/// re-consenting for a folder that was never disconnected is a poor trade for
/// the small amount of state this holds.
#[derive(Clone)]
struct UnavailableRoot {
    id: RootId,
    owner: GrantSubject,
    path: PathBuf,
    identity: RootIdentity,
    reason: UnavailableRootReason,
    grants: Vec<Grant>,
    attachments: Vec<RootAttachment>,
}

/// Why a persisted root could not be reopened.
///
/// The causes are recorded rather than acted on: an unmounted volume reports
/// itself as missing on some hosts and as an I/O failure on others, so no cause
/// here is reliable enough to justify destroying an approval on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnavailableRootReason {
    /// Nothing exists at the approved path.
    Missing,
    /// The path exists but the broker may no longer open it.
    PermissionDenied,
    /// Host I/O failed for some other reason, including a device that is
    /// present but not ready.
    HostIo,
    /// The path resolves to a directory the current policy would not approve.
    Rejected,
    /// A different directory now occupies the approved path. Consent named the
    /// original directory, and rebinding it to whatever replaced it would hand
    /// out authority the user never gave.
    Replaced,
}

impl UnavailableRootReason {
    fn from_policy_error(error: &RootPolicyError) -> Self {
        match error {
            RootPolicyError::Io(error) => match error.kind() {
                io::ErrorKind::NotFound => Self::Missing,
                io::ErrorKind::PermissionDenied => Self::PermissionDenied,
                _ => Self::HostIo,
            },
            _ => Self::Rejected,
        }
    }

    const fn error_code(self) -> ErrorCode {
        match self {
            Self::Missing => ErrorCode::NotFound,
            Self::PermissionDenied => ErrorCode::Denied,
            Self::HostIo => ErrorCode::HostIo,
            Self::Rejected | Self::Replaced => ErrorCode::InvalidRoot,
        }
    }
}

#[derive(Clone)]
struct RegisteredRoot {
    /// Subject that originally established the host approval. Other subjects
    /// may receive grants only through trusted attachment control actions.
    owner: GrantSubject,
    display_name: String,
    root: Arc<ValidatedRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum MutationRecord {
    Register {
        request: RegisterFingerprint,
        outcome: MutationOutcome<RegisterRootResult>,
    },
    Revoke {
        request: RevokeFingerprint,
        outcome: MutationOutcome<RevokeRootResult>,
    },
    Attachment {
        request: AttachmentFingerprint,
        outcome: MutationOutcome<RootAttachmentMutationResult>,
    },
    Write {
        request: WriteFingerprint,
        outcome: MutationOutcome<WriteFileResult>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum MutationOutcome<T> {
    Pending,
    Complete(Result<T, ErrorResponse>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterFingerprint {
    subject: GrantSubject,
    conversation_id: Uuid,
    path: PathBuf,
    consent_method: ConsentMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeFingerprint {
    subject: GrantSubject,
    root_id: RootId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentFingerprint {
    subject: GrantSubject,
    conversation_id: Uuid,
    root_id: RootId,
    mutation: RootAttachmentMutationKind,
    #[serde(default)]
    consent_method: Option<ConsentMethod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFingerprint {
    context: ExecutionContext,
    root_id: RootId,
    path: RelativePath,
    mode: WriteFileMode,
    approval_id: Option<Uuid>,
    byte_len: usize,
    sha256: [u8; 32],
}

struct PreparedRegistration {
    conversation_id: Uuid,
    root_id: RootId,
    root: RegisteredRoot,
    grants: [Grant; 3],
    attachment: RootAttachment,
}

struct ControlAudit {
    actor: AuditActor,
    operation: AuditOperation,
    operation_id: Option<OperationId>,
    target: AuditTarget,
    /// This request can change consent or host state, so its record must be
    /// durable before it runs.
    mutates: bool,
}

impl ControlAudit {
    fn from_request(request: &ControlRequest) -> Option<Self> {
        match request {
            // Neither reaches user data or changes anything. `Hello` is a
            // version handshake against a constant, and `ListApprovedRoots`
            // projects folders the trusted control surface already knows about
            // for the management UI. Recording them would add volume to a
            // bounded, rotating log without adding anything a reader could act
            // on, which costs the records that matter their retention.
            ControlRequest::Hello | ControlRequest::ListApprovedRoots => None,
            ControlRequest::ResolveExecRoots(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: match request.context.project_id() {
                        Some(project_id) => GrantSubject::project(project_id),
                        None => GrantSubject::conversation(request.context.conversation_id()),
                    }
                    .expect("execution contexts contain non-nil identities"),
                    conversation_id: Some(request.context.conversation_id()),
                },
                // A read of already-granted state. It hands absolute paths to
                // the local sandbox, but the broker cannot audit what happens
                // inside that sandbox either way, so gating it would cost the
                // user folder-aware execution while buying no coverage.
                mutates: false,
                operation: AuditOperation::ResolveExecRoots,
                operation_id: None,
                target: AuditTarget::Subject,
            }),
            ControlRequest::RegisterRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: true,
                operation: AuditOperation::RegisterRoot,
                operation_id: Some(request.operation_id),
                target: AuditTarget::selected_folder(&request.path),
            }),
            ControlRequest::LookupRegisterRootReceipt(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: false,
                operation: AuditOperation::LookupRegisterRootReceipt,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Subject,
            }),
            ControlRequest::AttachRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: true,
                operation: AuditOperation::AttachRoot,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::DetachRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: true,
                operation: AuditOperation::DetachRoot,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::LookupRootAttachmentReceipt(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: false,
                operation: AuditOperation::LookupRootAttachmentReceipt,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::RevokeRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: None,
                },
                mutates: true,
                operation: AuditOperation::RevokeRoot,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
        }
    }

    fn intent(&self, request_id: RequestId) -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id,
            operation_id: self.operation_id,
            actor: self.actor,
            operation: self.operation,
            target: self.target.clone(),
            outcome: AuditOutcome::Attempted,
            capability: None,
            grant_id: None,
            error_code: None,
            item_count: None,
            bytes: None,
        }
    }
}

struct OperationAudit {
    actor: AuditActor,
    operation: AuditOperation,
    capability: Capability,
    target: AuditTarget,
    /// This operation can change bytes on the host, so its record must be
    /// durable before it runs.
    mutates: bool,
}

impl OperationAudit {
    fn from_envelope(envelope: &OperationEnvelope) -> Self {
        let mutates = matches!(envelope.request, OperationRequest::WriteFile(_));
        let (operation, capability, target) = match &envelope.request {
            OperationRequest::ListRoots => (
                AuditOperation::ListRoots,
                Capability::ListRoots,
                AuditTarget::Subject,
            ),
            OperationRequest::ListDirectory(request) => (
                AuditOperation::ListDirectory,
                Capability::ReadFiles,
                AuditTarget::path(request.root_id, &request.path),
            ),
            OperationRequest::ReadFile(request) => (
                AuditOperation::ReadFile,
                Capability::ReadFiles,
                AuditTarget::path(request.root_id, &request.path),
            ),
            OperationRequest::ReadFileBinary(request) => (
                AuditOperation::ReadFileBinary,
                Capability::ReadFiles,
                AuditTarget::path(request.root_id, &request.path),
            ),
            OperationRequest::WriteFile(request) => (
                AuditOperation::WriteFile,
                Capability::WriteFiles,
                AuditTarget::path(request.root_id, &request.path),
            ),
        };
        Self {
            actor: AuditActor::Operation {
                context: envelope.context,
            },
            operation,
            capability,
            target,
            mutates,
        }
    }

    fn intent(&self, request_id: RequestId) -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id,
            operation_id: None,
            actor: self.actor,
            operation: self.operation,
            target: self.target.clone(),
            outcome: AuditOutcome::Attempted,
            capability: Some(self.capability),
            // The operation has not been authorized yet; the paired completion
            // record names the grant that allowed it, if any did.
            grant_id: None,
            error_code: None,
            item_count: None,
            bytes: None,
        }
    }
}

impl Broker {
    /// Create an empty broker using the reviewed host-root policy.
    pub fn new(policy: RootPolicy) -> Self {
        Self {
            shared: Arc::new(Shared {
                policy,
                state: Mutex::new(State::default()),
                state_file: None,
                audit: Arc::new(MemoryAuditSink::new()),
                failed_closed: AtomicBool::new(false),
            }),
        }
    }

    /// Create an ephemeral broker with an embedder-provided audit sink.
    pub fn with_audit_sink(policy: RootPolicy, audit: Arc<dyn AuditSink>) -> Self {
        Self {
            shared: Arc::new(Shared {
                policy,
                state: Mutex::new(State::default()),
                state_file: None,
                audit,
                failed_closed: AtomicBool::new(false),
            }),
        }
    }

    /// Open durable broker state under the application-private data directory.
    /// Persisted roots are revalidated and descriptor-pinned before this
    /// constructor returns, so stale state is never advertised.
    pub fn open(policy: RootPolicy, data_dir: &Path) -> Result<Self, BrokerError> {
        let state_file = StateFile::open(data_dir)?;
        let state = state_file.load(&policy)?;
        // A sink that could not be prepared is not replaced by one that discards
        // records: it stays the real sink, refusing until the host can hold the
        // log. Registration and writes fail with a stated reason, reads keep
        // working, and everything resumes without a restart.
        let audit: Arc<dyn AuditSink> = Arc::new(JsonlAuditSink::open(data_dir));
        let pruned = state
            .unavailable
            .iter()
            .map(unavailable_root_event)
            .collect::<Vec<_>>();
        let broker = Self {
            shared: Arc::new(Shared {
                policy,
                state: Mutex::new(state),
                state_file: Some(state_file),
                audit,
                failed_closed: AtomicBool::new(false),
            }),
        };
        for event in &pruned {
            broker.shared.record_completion(event);
        }
        Ok(broker)
    }

    /// Obtain the trusted host-only interface.
    pub fn controller(&self) -> Controller {
        Controller {
            shared: self.shared.clone(),
        }
    }

    /// Obtain the capability-checked operation interface.
    pub fn operator(&self) -> Operator {
        Operator {
            shared: self.shared.clone(),
        }
    }
}

impl Controller {
    /// Handle one trusted control request and always return a correlated,
    /// transport-safe response.
    pub fn handle(&self, envelope: ControlEnvelope) -> ControlResponseEnvelope {
        let request_id = envelope.request_id;
        let audit = ControlAudit::from_request(&envelope.request);
        if let Some(audit) = audit.as_ref().filter(|audit| audit.mutates) {
            if let Err(error) = self.shared.record_intent(&audit.intent(request_id)) {
                return response_envelope(
                    request_id,
                    Err(error_response(BrokerError::Audit(error))),
                );
            }
        }
        let result = self.execute(envelope);
        if let Some(audit) = audit {
            self.record_completion(request_id, audit, &result);
        }
        response_envelope(request_id, result)
    }

    fn record_completion(
        &self,
        request_id: crate::RequestId,
        metadata: ControlAudit,
        result: &Result<ControlResult, ErrorResponse>,
    ) {
        let error = result.as_ref().err();
        let operation = if error.is_some_and(|error| error.code == ErrorCode::ProtocolVersion) {
            AuditOperation::ProtocolVersionMismatch
        } else {
            metadata.operation
        };
        let event = AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id,
            operation_id: metadata.operation_id,
            actor: metadata.actor,
            operation,
            target: metadata.target,
            outcome: audit_outcome(error),
            capability: None,
            grant_id: None,
            error_code: error.map(|error| error.code),
            item_count: None,
            bytes: None,
        };
        self.shared.record_completion(&event);
    }

    fn execute(&self, envelope: ControlEnvelope) -> Result<ControlResult, ErrorResponse> {
        if matches!(envelope.request, ControlRequest::Hello) {
            return Ok(ControlResult::Hello(hello()));
        }
        require_version(envelope.protocol_version).map_err(error_response)?;
        match envelope.request {
            ControlRequest::Hello => Ok(ControlResult::Hello(hello())),
            ControlRequest::ListApprovedRoots => {
                let state = self.lock_state().map_err(error_response)?;
                list_approved_roots(&state).map(|roots| ControlResult::ListApprovedRoots { roots })
            }
            ControlRequest::ResolveExecRoots(request) => {
                let state = self.lock_state().map_err(error_response)?;
                resolve_exec_roots(&state, request)
                    .map(|roots| ControlResult::ResolveExecRoots { roots })
                    .map_err(error_response)
            }
            ControlRequest::RegisterRoot(request) => {
                self.register_root(request).map(ControlResult::RegisterRoot)
            }
            ControlRequest::LookupRegisterRootReceipt(request) => self
                .lookup_register_root_receipt(request)
                .map(ControlResult::LookupRegisterRootReceipt),
            ControlRequest::AttachRoot(request) => self
                .mutate_root_attachment(request, RootAttachmentMutationKind::Attach)
                .map(ControlResult::AttachRoot),
            ControlRequest::DetachRoot(request) => self
                .mutate_root_attachment(request, RootAttachmentMutationKind::Detach)
                .map(ControlResult::DetachRoot),
            ControlRequest::LookupRootAttachmentReceipt(request) => self
                .lookup_root_attachment_receipt(request)
                .map(ControlResult::LookupRootAttachmentReceipt),
            ControlRequest::RevokeRoot(request) => {
                self.revoke_root(request).map(ControlResult::RevokeRoot)
            }
        }
    }

    fn register_root(
        &self,
        request: RegisterRootRequest,
    ) -> Result<RegisterRootResult, ErrorResponse> {
        let operation_id = request.operation_id;
        let fingerprint = RegisterFingerprint {
            subject: request.subject,
            conversation_id: request.conversation_id,
            path: request.path.clone(),
            consent_method: request.consent_method,
        };
        {
            let mut state = self.lock_state().map_err(error_response)?;
            let mut next = state.clone();
            match claim_register(&mut next, operation_id, &fingerprint).map_err(error_response)? {
                Claim::Start => {}
                Claim::Complete(result) => return result,
            }
            self.commit_state(&mut state, next)
                .map_err(error_response)?;
        }

        let prepared = match self.prepare_registration(request) {
            Err(error) if retryable_registration_error(&error) => {
                let mut state = self.lock_state().map_err(error_response)?;
                state.active_mutations.remove(&operation_id);
                return Err(error_response(error));
            }
            result => result.map_err(error_response),
        };
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        let outcome = match prepared {
            Ok(prepared) => {
                let existing = preferred_root_alias(&next, &prepared.root);
                if let Some((root_id, display_name)) = existing {
                    ensure_subject_grants(
                        &mut next,
                        prepared.root.owner,
                        root_id,
                        prepared.grants[0].consent().clone(),
                    )
                    .map_err(error_response)?;
                    if !next.attachments.iter().any(|attachment| {
                        attachment.conversation_id() == prepared.conversation_id
                            && attachment.root_id() == root_id
                    }) {
                        next.attachments.push(
                            RootAttachment::new(prepared.conversation_id, root_id)
                                .map_err(BrokerError::from)
                                .map_err(error_response)?,
                        );
                    }
                    Ok(RegisterRootResult {
                        root: RootSummary {
                            root_id,
                            display_name,
                        },
                    })
                } else {
                    let result = RegisterRootResult {
                        root: RootSummary {
                            root_id: prepared.root_id,
                            display_name: prepared.root.display_name.clone(),
                        },
                    };
                    next.roots.insert(prepared.root_id, prepared.root);
                    next.grants.extend(prepared.grants);
                    next.attachments.push(prepared.attachment);
                    Ok(result)
                }
            }
            Err(error) => Err(error),
        };
        complete_register(&mut next, operation_id, &fingerprint, outcome.clone())
            .map_err(error_response)?;
        if let Err(error) = self.commit_state(&mut state, next) {
            state.active_mutations.remove(&operation_id);
            return Err(error_response(error));
        }
        outcome
    }

    fn lookup_register_root_receipt(
        &self,
        request: LookupRegisterRootReceiptRequest,
    ) -> Result<LookupRegisterRootReceiptResult, ErrorResponse> {
        if request.conversation_id.is_nil()
            || (request.subject.kind() == SubjectKind::Conversation
                && request.subject.id() != request.conversation_id)
        {
            return Err(error_response(BrokerError::SubjectConversationMismatch));
        }
        let state = self.lock_state().map_err(error_response)?;
        if let Some(record) = state.mutations.get(&request.operation_id) {
            match record {
                MutationRecord::Register {
                    request: registered,
                    ..
                } if registered.subject == request.subject
                    && registered.conversation_id == request.conversation_id => {}
                MutationRecord::Register { .. }
                | MutationRecord::Revoke { .. }
                | MutationRecord::Attachment { .. }
                | MutationRecord::Write { .. } => {
                    return Err(error_response(BrokerError::OperationIdConflict));
                }
            }
        }
        let receipt = match state.mutations.get(&request.operation_id) {
            None => RegisterRootReceipt::Unknown,
            Some(MutationRecord::Register {
                outcome: MutationOutcome::Pending,
                ..
            }) => RegisterRootReceipt::Pending,
            Some(MutationRecord::Register {
                request: registered,
                outcome: MutationOutcome::Complete(Ok(result)),
            }) => {
                let root = result.root.clone();
                if registration_is_connected(&state, registered, &root) {
                    RegisterRootReceipt::Completed { root }
                } else {
                    RegisterRootReceipt::Disconnected { root }
                }
            }
            Some(MutationRecord::Register {
                outcome: MutationOutcome::Complete(Err(error)),
                ..
            }) => RegisterRootReceipt::Failed {
                error: error.clone(),
            },
            Some(
                MutationRecord::Revoke { .. }
                | MutationRecord::Attachment { .. }
                | MutationRecord::Write { .. },
            ) => {
                unreachable!("non-registration operation was rejected above")
            }
        };
        Ok(LookupRegisterRootReceiptResult {
            operation_id: request.operation_id,
            receipt,
        })
    }

    fn prepare_registration(
        &self,
        request: RegisterRootRequest,
    ) -> Result<PreparedRegistration, BrokerError> {
        if request.subject.kind() == SubjectKind::Conversation
            && request.subject.id() != request.conversation_id
        {
            return Err(BrokerError::SubjectConversationMismatch);
        }
        if !matches!(
            request.consent_method,
            ConsentMethod::FolderPicker | ConsentMethod::OperatorConfig
        ) {
            return Err(BrokerError::InvalidConsentMethod);
        }
        let validated = self.shared.policy.open_root(&request.path)?;
        let display_name = root_display_name(validated.canonical_path());
        let root_id = RootId::new();
        let consent = ConsentRecord::new(request.consent_method, Utc::now());
        let list_grant = Grant::from_consent(
            GrantId::new(),
            request.subject,
            Capability::ListRoots,
            Scope::Subject,
            consent.clone(),
        )?;
        let read_grant = Grant::from_consent(
            GrantId::new(),
            request.subject,
            Capability::ReadFiles,
            Scope::Root { root_id },
            consent.clone(),
        )?;
        let write_grant = Grant::from_consent(
            GrantId::new(),
            request.subject,
            Capability::WriteFiles,
            Scope::Root { root_id },
            consent,
        )?;
        Ok(PreparedRegistration {
            conversation_id: request.conversation_id,
            root_id,
            root: RegisteredRoot {
                owner: request.subject,
                display_name,
                root: Arc::new(validated),
            },
            grants: [list_grant, read_grant, write_grant],
            attachment: RootAttachment::new(request.conversation_id, root_id)?,
        })
    }

    fn mutate_root_attachment(
        &self,
        request: RootAttachmentMutationRequest,
        mutation: RootAttachmentMutationKind,
    ) -> Result<RootAttachmentMutationResult, ErrorResponse> {
        let fingerprint = AttachmentFingerprint {
            subject: request.subject,
            conversation_id: request.conversation_id,
            root_id: request.root_id,
            mutation,
            consent_method: request.consent_method,
        };
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        match claim_attachment(&mut next, request.operation_id, fingerprint)
            .map_err(error_response)?
        {
            Claim::Complete(result) => return result,
            Claim::Start => {}
        }

        let result = apply_root_attachment(&mut next, fingerprint).map_err(error_response);
        complete_attachment(&mut next, request.operation_id, fingerprint, result.clone())
            .map_err(error_response)?;
        self.commit_state(&mut state, next)
            .map_err(error_response)?;
        result
    }

    fn lookup_root_attachment_receipt(
        &self,
        request: LookupRootAttachmentReceiptRequest,
    ) -> Result<LookupRootAttachmentReceiptResult, ErrorResponse> {
        validate_subject_conversation(request.subject, request.conversation_id)
            .map_err(error_response)?;
        let expected = AttachmentFingerprint {
            subject: request.subject,
            conversation_id: request.conversation_id,
            root_id: request.root_id,
            mutation: request.mutation,
            // Receipt lookup does not recreate consent. None is a wildcard for
            // both legacy v2 records and current method-bound mutations.
            consent_method: None,
        };
        let state = self.lock_state().map_err(error_response)?;
        let receipt = match state.mutations.get(&request.operation_id) {
            None => RootAttachmentMutationReceipt::Unknown,
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Pending,
            }) if attachment_lookup_matches(existing, &expected) => {
                return Err(error_response(BrokerError::OperationInProgress));
            }
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Complete(Ok(result)),
            }) if attachment_lookup_matches(existing, &expected) => {
                RootAttachmentMutationReceipt::Completed {
                    result: *result,
                    currently_attached: has_root_attachment(
                        &state,
                        request.conversation_id,
                        request.root_id,
                    ),
                }
            }
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Complete(Err(error)),
            }) if attachment_lookup_matches(existing, &expected) => {
                RootAttachmentMutationReceipt::Failed {
                    error: error.clone(),
                    currently_attached: has_root_attachment(
                        &state,
                        request.conversation_id,
                        request.root_id,
                    ),
                }
            }
            Some(_) => return Err(error_response(BrokerError::OperationIdConflict)),
        };
        Ok(LookupRootAttachmentReceiptResult {
            operation_id: request.operation_id,
            receipt,
        })
    }

    fn revoke_root(&self, request: RevokeRootRequest) -> Result<RevokeRootResult, ErrorResponse> {
        let operation_id = request.operation_id;
        let fingerprint = RevokeFingerprint {
            subject: request.subject,
            root_id: request.root_id,
        };
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        match claim_revoke(&mut next, operation_id, fingerprint).map_err(error_response)? {
            Claim::Complete(result) => return result,
            Claim::Start => {}
        }
        let owned = next
            .roots
            .get(&request.root_id)
            .is_some_and(|root| root.owner == request.subject);
        // State version 2 could persist one physical directory under multiple
        // product root IDs when separate subjects selected it. Removing only
        // the requested ID would falsely claim global approval was gone, while
        // deleting every alias would invalidate durable receipts and product
        // projections that still name those IDs. Preserve that legacy state
        // and report that no global revocation occurred.
        // Revocation is the one deliberate instruction to forget a root, so it
        // reaches the set-aside registrations too. Nothing else deletes them.
        let set_aside = next
            .unavailable
            .iter()
            .any(|root| root.id == request.root_id && root.owner == request.subject);
        let revoked = set_aside || (owned && !has_physical_root_alias(&next, request.root_id));
        if revoked {
            next.roots.remove(&request.root_id);
            next.unavailable.retain(|root| root.id != request.root_id);
            next.grants
                .retain(|grant| !scope_targets_root(grant.scope(), request.root_id));
            next.attachments
                .retain(|attachment| attachment.root_id() != request.root_id);
        }
        let result = Ok(RevokeRootResult { revoked });
        complete_revoke(&mut next, operation_id, fingerprint, result.clone())
            .map_err(error_response)?;
        self.commit_state(&mut state, next)
            .map_err(error_response)?;
        result
    }

    fn commit_state(
        &self,
        current: &mut MutexGuard<'_, State>,
        next: State,
    ) -> Result<(), BrokerError> {
        if let Some(state_file) = &self.shared.state_file {
            if let Err(error) = state_file.save(&next) {
                if matches!(error, BrokerError::PersistenceAmbiguous) {
                    self.shared.failed_closed.store(true, Ordering::SeqCst);
                }
                return Err(error);
            }
        }
        **current = next;
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, BrokerError> {
        if self.shared.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| BrokerError::StatePoisoned)?;
        if self.shared.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        Ok(state)
    }
}

impl Operator {
    /// Authorize and perform one agent operation, returning a correlated safe
    /// response rather than exposing raw host errors.
    pub fn handle(&self, envelope: OperationEnvelope) -> OperationResponseEnvelope {
        let request_id = envelope.request_id;
        let audit = OperationAudit::from_envelope(&envelope);
        if audit.mutates {
            if let Err(error) = self.shared.record_intent(&audit.intent(request_id)) {
                return response_envelope(
                    request_id,
                    Err(error_response(BrokerError::Audit(error))),
                );
            }
        }
        let (result, grant_id) = self.execute(envelope);
        let result = result.map_err(error_response);
        self.record_completion(request_id, audit, grant_id, &result);
        response_envelope(request_id, result)
    }

    fn execute(
        &self,
        envelope: OperationEnvelope,
    ) -> (Result<OperationResult, BrokerError>, Option<GrantId>) {
        let mut grant_id = None;
        let result = (|| {
            require_version(envelope.protocol_version)?;
            match envelope.request {
                OperationRequest::ListRoots => {
                    let state = self.lock_state()?;
                    let (result, authorized_by) = list_roots(&state, envelope.context)?;
                    grant_id = authorized_by;
                    Ok(result)
                }
                OperationRequest::ListDirectory(PathRequest { root_id, path }) => {
                    let (directory, authorized_by) =
                        self.authorized_root(envelope.context, root_id, &path)?;
                    grant_id = Some(authorized_by);
                    let result = list_directory(&directory, &path)?;
                    grant_id = Some(self.reauthorize(envelope.context, root_id, &path)?);
                    Ok(result)
                }
                OperationRequest::ReadFile(PathRequest { root_id, path }) => {
                    let (directory, authorized_by) =
                        self.authorized_root(envelope.context, root_id, &path)?;
                    grant_id = Some(authorized_by);
                    let result = read_file(&directory, &path)?;
                    grant_id = Some(self.reauthorize(envelope.context, root_id, &path)?);
                    Ok(result)
                }
                OperationRequest::ReadFileBinary(PathRequest { root_id, path }) => {
                    let (directory, authorized_by) =
                        self.authorized_root(envelope.context, root_id, &path)?;
                    grant_id = Some(authorized_by);
                    let result = read_file_binary(&directory, &path)?;
                    grant_id = Some(self.reauthorize(envelope.context, root_id, &path)?);
                    Ok(result)
                }
                OperationRequest::WriteFile(request) => {
                    let (result, authorized_by) = self.write_file(envelope.context, request)?;
                    grant_id = authorized_by;
                    Ok(OperationResult::WriteFile(result))
                }
            }
        })();
        (result, grant_id)
    }

    fn record_completion(
        &self,
        request_id: crate::RequestId,
        metadata: OperationAudit,
        grant_id: Option<GrantId>,
        result: &Result<OperationResult, ErrorResponse>,
    ) {
        let error = result.as_ref().err();
        let operation = if error.is_some_and(|error| error.code == ErrorCode::ProtocolVersion) {
            AuditOperation::ProtocolVersionMismatch
        } else {
            metadata.operation
        };
        let (item_count, bytes) = match result.as_ref().ok() {
            Some(OperationResult::ListRoots { roots }) => (Some(roots.len()), None),
            Some(OperationResult::ListDirectory { entries }) => (Some(entries.len()), None),
            Some(OperationResult::ReadFile(result)) => (None, Some(result.bytes)),
            Some(OperationResult::ReadFileBinary(result)) => (None, Some(result.bytes)),
            Some(OperationResult::WriteFile(result)) => (None, Some(result.bytes)),
            None => (None, None),
        };
        let event = AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id,
            operation_id: None,
            actor: metadata.actor,
            operation,
            target: metadata.target,
            outcome: audit_outcome(error),
            capability: Some(metadata.capability),
            grant_id,
            error_code: error.map(|error| error.code),
            item_count,
            bytes,
        };
        self.shared.record_completion(&event);
    }

    fn authorized_root(
        &self,
        context: ExecutionContext,
        root_id: RootId,
        path: &RelativePath,
    ) -> Result<(Dir, GrantId), BrokerError> {
        let state = self.lock_state()?;
        let grant_id = authorize(
            &state,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root_id,
                relative: path,
            },
        )?;
        let directory = state
            .roots
            .get(&root_id)
            .ok_or(BrokerError::Denied)?
            .root
            .directory()
            .try_clone()
            .map_err(BrokerError::from)?;
        Ok((directory, grant_id))
    }

    fn write_file(
        &self,
        context: ExecutionContext,
        request: WriteFileRequest,
    ) -> Result<(WriteFileResult, Option<GrantId>), BrokerError> {
        let content = decode_write_content(&request)?;
        let approval_id = request.approval.map(|approval| approval.approval_id);
        if matches!(request.mode, WriteFileMode::Replace)
            && approval_id.is_none_or(|id| id.is_nil())
        {
            return Err(BrokerError::ReplacementApprovalRequired);
        }
        if matches!(request.mode, WriteFileMode::Create) && approval_id.is_some() {
            return Err(BrokerError::ReplacementApprovalRequired);
        }
        let fingerprint = WriteFingerprint {
            context,
            root_id: request.root_id,
            path: request.path.clone(),
            mode: request.mode,
            approval_id,
            byte_len: request.bytes,
            sha256: request.sha256,
        };

        let recovering = {
            let mut state = self.lock_state()?;
            let mut next = state.clone();
            match claim_write(&mut next, request.operation_id, &fingerprint)? {
                WriteClaim::Complete(result) => {
                    return result
                        .map(|result| (result, None))
                        .map_err(BrokerError::StoredWriteFailure);
                }
                WriteClaim::Dispatch { recovering } => {
                    authorize_write(&next, context, request.root_id, &request.path)?;
                    self.commit_state(&mut state, next)?;
                    recovering
                }
            }
        };

        let mut state = self.lock_state()?;
        let grant_id = match authorize_write(&state, context, request.root_id, &request.path) {
            Ok(grant_id) => grant_id,
            Err(error) => {
                let terminal = Err(error_response(error));
                let mut next = state.clone();
                complete_write(
                    &mut next,
                    request.operation_id,
                    &fingerprint,
                    terminal.clone(),
                )?;
                self.commit_state(&mut state, next)?;
                return terminal
                    .map(|result| (result, None))
                    .map_err(BrokerError::StoredWriteFailure);
            }
        };
        let directory = state
            .roots
            .get(&request.root_id)
            .ok_or(BrokerError::Denied)?
            .root
            .directory()
            .try_clone()?;

        let terminal = if recovering {
            if destination_matches(&directory, &request.path, request.bytes, request.sha256) {
                Ok(WriteFileResult {
                    operation_id: request.operation_id,
                    bytes: request.bytes,
                    replaced: matches!(request.mode, WriteFileMode::Replace),
                })
            } else {
                Err(error_response(BrokerError::AmbiguousWrite))
            }
        } else {
            match atomic_write_connected_file(
                &directory,
                &request.path,
                &content,
                request.mode,
                request.operation_id,
            ) {
                Ok(replaced) => Ok(WriteFileResult {
                    operation_id: request.operation_id,
                    bytes: request.bytes,
                    replaced,
                }),
                Err(_error)
                    if destination_matches(
                        &directory,
                        &request.path,
                        request.bytes,
                        request.sha256,
                    ) =>
                {
                    Ok(WriteFileResult {
                        operation_id: request.operation_id,
                        bytes: request.bytes,
                        replaced: matches!(request.mode, WriteFileMode::Replace),
                    })
                }
                Err(error) => Err(error_response(error)),
            }
        };

        let mut next = state.clone();
        complete_write(
            &mut next,
            request.operation_id,
            &fingerprint,
            terminal.clone(),
        )?;
        self.commit_state(&mut state, next)?;
        terminal
            .map(|result| (result, Some(grant_id)))
            .map_err(BrokerError::StoredWriteFailure)
    }

    fn reauthorize(
        &self,
        context: ExecutionContext,
        root_id: RootId,
        path: &RelativePath,
    ) -> Result<GrantId, BrokerError> {
        let state = self.lock_state()?;
        let grant_id = authorize(
            &state,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root_id,
                relative: path,
            },
        )?;
        state.roots.get(&root_id).ok_or(BrokerError::Denied)?;
        Ok(grant_id)
    }

    fn commit_state(
        &self,
        current: &mut MutexGuard<'_, State>,
        next: State,
    ) -> Result<(), BrokerError> {
        if let Some(state_file) = &self.shared.state_file {
            if let Err(error) = state_file.save(&next) {
                if matches!(error, BrokerError::PersistenceAmbiguous) {
                    self.shared.failed_closed.store(true, Ordering::SeqCst);
                }
                return Err(error);
            }
        }
        **current = next;
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, BrokerError> {
        if self.shared.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| BrokerError::StatePoisoned)?;
        if self.shared.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        Ok(state)
    }
}

fn hello() -> HelloResult {
    HelloResult {
        protocol_version: PROTOCOL_VERSION,
        operations: vec![
            "list_roots".to_owned(),
            "list_directory".to_owned(),
            "read_file".to_owned(),
            "read_file_binary".to_owned(),
            "write_file".to_owned(),
        ],
    }
}

fn require_version(received: u32) -> Result<(), BrokerError> {
    if received == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(BrokerError::ProtocolVersion {
            received,
            expected: PROTOCOL_VERSION,
        })
    }
}

fn response_envelope<T>(
    request_id: crate::RequestId,
    result: Result<T, ErrorResponse>,
) -> ResponseEnvelope<T> {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        response: match result {
            Ok(result) => Response::Ok(result),
            Err(error) => Response::Error(error),
        },
    }
}

fn error_response(error: BrokerError) -> ErrorResponse {
    let (code, message, retryable) = match error {
        BrokerError::ProtocolVersion { .. } => (
            ErrorCode::ProtocolVersion,
            "broker protocol version mismatch",
            false,
        ),
        BrokerError::OperationIdConflict => (
            ErrorCode::OperationIdConflict,
            "operation identity was reused for a different mutation",
            false,
        ),
        BrokerError::OperationInProgress => (
            ErrorCode::OperationInProgress,
            "operation is already in progress",
            true,
        ),
        BrokerError::Denied => (ErrorCode::Denied, "host operation was denied", false),
        BrokerError::UnknownRoot => (
            ErrorCode::InvalidRoot,
            "connected root is unavailable",
            false,
        ),
        BrokerError::SubjectConversationMismatch
        | BrokerError::InvalidConsentMethod
        | BrokerError::InvalidGrant(_) => (
            ErrorCode::InvalidRequest,
            "host operation request is invalid",
            false,
        ),
        BrokerError::RootPolicy(RootPolicyError::Io(error)) => (
            ErrorCode::HostIo,
            "host filesystem operation failed",
            transient_io_kind(error.kind()),
        ),
        BrokerError::RootPolicy(_) => (
            ErrorCode::InvalidRoot,
            "selected folder is not an allowed connected root",
            false,
        ),
        BrokerError::FileTooLarge { .. }
        | BrokerError::DirectoryTooLarge
        | BrokerError::RootListTooLarge
        | BrokerError::StateTooLarge => (
            ErrorCode::TooLarge,
            "host operation exceeded its limit",
            false,
        ),
        BrokerError::NotRegularFile | BrokerError::NotUtf8 => (
            ErrorCode::UnsupportedContent,
            "host resource has an unsupported content type",
            false,
        ),
        BrokerError::Io(error) => (
            ErrorCode::HostIo,
            "host filesystem operation failed",
            transient_io_kind(error.kind()),
        ),
        BrokerError::DestinationExists => (
            ErrorCode::AlreadyExists,
            "write destination already exists",
            false,
        ),
        BrokerError::DestinationMissing => (
            ErrorCode::NotFound,
            "write destination does not exist",
            false,
        ),
        BrokerError::ReplacementApprovalRequired | BrokerError::InvalidWriteContent => {
            (ErrorCode::InvalidRequest, "write request is invalid", false)
        }
        BrokerError::AmbiguousWrite => (
            ErrorCode::AmbiguousWrite,
            "write outcome is ambiguous and will not be replayed",
            false,
        ),
        BrokerError::StoredWriteFailure(error) => return error,
        BrokerError::StatePoisoned => (
            ErrorCode::Internal,
            "broker state is unavailable; restart the broker",
            false,
        ),
        BrokerError::PersistenceAmbiguous => (
            ErrorCode::Internal,
            "broker state publication is ambiguous; restart the broker",
            false,
        ),
        BrokerError::Audit(_) => (
            ErrorCode::AuditUnavailable,
            "the host audit log could not be written, so the operation was refused",
            true,
        ),
    };
    ErrorResponse {
        code,
        message: message.to_owned(),
        retryable,
    }
}

fn retryable_registration_error(error: &BrokerError) -> bool {
    matches!(
        error,
        BrokerError::RootPolicy(RootPolicyError::Io(source))
            if transient_io_kind(source.kind())
    )
}

fn transient_io_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn audit_outcome(error: Option<&ErrorResponse>) -> AuditOutcome {
    match error.map(|error| error.code) {
        None => AuditOutcome::Allowed,
        Some(ErrorCode::Denied) => AuditOutcome::Denied,
        Some(_) => AuditOutcome::Failed,
    }
}

enum Claim<T> {
    Start,
    Complete(Result<T, ErrorResponse>),
}

fn registration_is_connected(
    state: &State,
    request: &RegisterFingerprint,
    root: &RootSummary,
) -> bool {
    state
        .roots
        .get(&root.root_id)
        .is_some_and(|registered| registered.display_name == root.display_name)
        && state.attachments.iter().any(|attachment| {
            attachment.conversation_id() == request.conversation_id
                && attachment.root_id() == root.root_id
        })
        && state.grants.iter().any(|grant| {
            grant.subject() == request.subject
                && grant.capability() == Capability::ListRoots
                && matches!(grant.scope(), Scope::Subject)
        })
        && state.grants.iter().any(|grant| {
            grant.subject() == request.subject
                && grant.capability() == Capability::ReadFiles
                && matches!(grant.scope(), Scope::Root { root_id } if *root_id == root.root_id)
        })
        && state.grants.iter().any(|grant| {
            grant.subject() == request.subject
                && grant.capability() == Capability::WriteFiles
                && matches!(grant.scope(), Scope::Root { root_id } if *root_id == root.root_id)
        })
}

fn claim_register(
    state: &mut State,
    operation_id: OperationId,
    request: &RegisterFingerprint,
) -> Result<Claim<RegisterRootResult>, BrokerError> {
    match state.mutations.get(&operation_id) {
        None => {
            state.mutations.insert(
                operation_id,
                MutationRecord::Register {
                    request: request.clone(),
                    outcome: MutationOutcome::Pending,
                },
            );
            state.active_mutations.insert(operation_id);
            Ok(Claim::Start)
        }
        Some(MutationRecord::Register {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if existing == request => Ok(Claim::Complete(result.clone())),
        Some(MutationRecord::Register {
            request: existing,
            outcome: MutationOutcome::Pending,
        }) if existing == request => {
            if state.active_mutations.insert(operation_id) {
                Ok(Claim::Start)
            } else {
                Err(BrokerError::OperationInProgress)
            }
        }
        Some(_) => Err(BrokerError::OperationIdConflict),
    }
}

fn complete_register(
    state: &mut State,
    operation_id: OperationId,
    request: &RegisterFingerprint,
    result: Result<RegisterRootResult, ErrorResponse>,
) -> Result<(), BrokerError> {
    match state.mutations.get_mut(&operation_id) {
        Some(MutationRecord::Register {
            request: existing,
            outcome,
        }) if existing == request && matches!(outcome, MutationOutcome::Pending) => {
            *outcome = MutationOutcome::Complete(result);
            state.active_mutations.remove(&operation_id);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

fn claim_revoke(
    state: &mut State,
    operation_id: OperationId,
    request: RevokeFingerprint,
) -> Result<Claim<RevokeRootResult>, BrokerError> {
    match state.mutations.get(&operation_id) {
        None => {
            state.mutations.insert(
                operation_id,
                MutationRecord::Revoke {
                    request,
                    outcome: MutationOutcome::Pending,
                },
            );
            state.active_mutations.insert(operation_id);
            Ok(Claim::Start)
        }
        Some(MutationRecord::Revoke {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if *existing == request => Ok(Claim::Complete(result.clone())),
        Some(MutationRecord::Revoke {
            request: existing,
            outcome: MutationOutcome::Pending,
        }) if *existing == request => {
            if state.active_mutations.insert(operation_id) {
                Ok(Claim::Start)
            } else {
                Err(BrokerError::OperationInProgress)
            }
        }
        Some(_) => Err(BrokerError::OperationIdConflict),
    }
}

fn complete_revoke(
    state: &mut State,
    operation_id: OperationId,
    request: RevokeFingerprint,
    result: Result<RevokeRootResult, ErrorResponse>,
) -> Result<(), BrokerError> {
    match state.mutations.get_mut(&operation_id) {
        Some(MutationRecord::Revoke {
            request: existing,
            outcome,
        }) if *existing == request && matches!(outcome, MutationOutcome::Pending) => {
            *outcome = MutationOutcome::Complete(result);
            state.active_mutations.remove(&operation_id);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

fn claim_attachment(
    state: &mut State,
    operation_id: OperationId,
    request: AttachmentFingerprint,
) -> Result<Claim<RootAttachmentMutationResult>, BrokerError> {
    match state.mutations.get(&operation_id) {
        None => {
            state.mutations.insert(
                operation_id,
                MutationRecord::Attachment {
                    request,
                    outcome: MutationOutcome::Pending,
                },
            );
            state.active_mutations.insert(operation_id);
            Ok(Claim::Start)
        }
        Some(MutationRecord::Attachment {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if attachment_mutation_matches(existing, &request) => {
            Ok(Claim::Complete(result.clone()))
        }
        Some(MutationRecord::Attachment {
            request: existing,
            outcome: MutationOutcome::Pending,
        }) if attachment_mutation_matches(existing, &request) => {
            if state.active_mutations.insert(operation_id) {
                Ok(Claim::Start)
            } else {
                Err(BrokerError::OperationInProgress)
            }
        }
        Some(_) => Err(BrokerError::OperationIdConflict),
    }
}

fn complete_attachment(
    state: &mut State,
    operation_id: OperationId,
    request: AttachmentFingerprint,
    result: Result<RootAttachmentMutationResult, ErrorResponse>,
) -> Result<(), BrokerError> {
    match state.mutations.get_mut(&operation_id) {
        Some(MutationRecord::Attachment {
            request: existing,
            outcome,
        }) if attachment_mutation_matches(existing, &request)
            && matches!(outcome, MutationOutcome::Pending) =>
        {
            *outcome = MutationOutcome::Complete(result);
            state.active_mutations.remove(&operation_id);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

enum WriteClaim {
    Dispatch { recovering: bool },
    Complete(Result<WriteFileResult, ErrorResponse>),
}

fn claim_write(
    state: &mut State,
    operation_id: OperationId,
    request: &WriteFingerprint,
) -> Result<WriteClaim, BrokerError> {
    match state.mutations.get(&operation_id) {
        None => {
            state.mutations.insert(
                operation_id,
                MutationRecord::Write {
                    request: request.clone(),
                    outcome: MutationOutcome::Pending,
                },
            );
            state.active_mutations.insert(operation_id);
            Ok(WriteClaim::Dispatch { recovering: false })
        }
        Some(MutationRecord::Write {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if existing == request => Ok(WriteClaim::Complete(result.clone())),
        Some(MutationRecord::Write {
            request: existing,
            outcome: MutationOutcome::Pending,
        }) if existing == request => {
            if state.active_mutations.insert(operation_id) {
                Ok(WriteClaim::Dispatch { recovering: true })
            } else {
                Err(BrokerError::OperationInProgress)
            }
        }
        Some(_) => Err(BrokerError::OperationIdConflict),
    }
}

fn complete_write(
    state: &mut State,
    operation_id: OperationId,
    request: &WriteFingerprint,
    result: Result<WriteFileResult, ErrorResponse>,
) -> Result<(), BrokerError> {
    match state.mutations.get_mut(&operation_id) {
        Some(MutationRecord::Write {
            request: existing,
            outcome,
        }) if existing == request && matches!(outcome, MutationOutcome::Pending) => {
            *outcome = MutationOutcome::Complete(result);
            state.active_mutations.remove(&operation_id);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

fn authorize_write(
    state: &State,
    context: ExecutionContext,
    root_id: RootId,
    path: &RelativePath,
) -> Result<GrantId, BrokerError> {
    let grant_id = authorize(
        state,
        context,
        Capability::WriteFiles,
        Resource::Path {
            root_id: &root_id,
            relative: path,
        },
    )?;
    state.roots.get(&root_id).ok_or(BrokerError::Denied)?;
    Ok(grant_id)
}

fn decode_write_content(request: &WriteFileRequest) -> Result<Vec<u8>, BrokerError> {
    if request.path.is_root()
        || request.bytes == 0
        || request.bytes > MAX_WRITE_FILE_BYTES
        || request.content_base64.len() > 4 * MAX_WRITE_FILE_BYTES / 3 + 8
    {
        return Err(BrokerError::InvalidWriteContent);
    }
    let content = BASE64
        .decode(&request.content_base64)
        .map_err(|_| BrokerError::InvalidWriteContent)?;
    if content.len() != request.bytes
        || <[u8; 32]>::from(Sha256::digest(&content)) != request.sha256
    {
        return Err(BrokerError::InvalidWriteContent);
    }
    Ok(content)
}

fn destination_parent(root: &Dir, path: &RelativePath) -> Result<(Dir, String), BrokerError> {
    let filesystem_path = Path::new(path.as_str());
    let filename = filesystem_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(BrokerError::InvalidWriteContent)?
        .to_owned();
    let parent = filesystem_path.parent().unwrap_or_else(|| Path::new(""));
    let directory = if parent.as_os_str().is_empty() {
        root.try_clone()?
    } else {
        root.open_dir_nofollow(parent)?
    };
    Ok((directory, filename))
}

fn atomic_write_connected_file(
    root: &Dir,
    path: &RelativePath,
    content: &[u8],
    mode: WriteFileMode,
    operation_id: OperationId,
) -> Result<bool, BrokerError> {
    let (directory, filename) = destination_parent(root, path)?;
    match directory.symlink_metadata(&filename) {
        Ok(metadata) => match mode {
            WriteFileMode::Create => return Err(BrokerError::DestinationExists),
            WriteFileMode::Replace if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(BrokerError::NotRegularFile);
            }
            WriteFileMode::Replace => {}
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if matches!(mode, WriteFileMode::Replace) {
                return Err(BrokerError::DestinationMissing);
            }
        }
        Err(error) => return Err(error.into()),
    }

    let temporary = format!(".openwave-write-{operation_id}.tmp");
    let result = (|| -> Result<bool, BrokerError> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No)
            .sync(true);
        let mut file = directory.open_with(&temporary, &options)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        match mode {
            WriteFileMode::Create => {
                directory
                    .hard_link(&temporary, &directory, &filename)
                    .map_err(|error| {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            BrokerError::DestinationExists
                        } else {
                            BrokerError::Io(error)
                        }
                    })?;
                directory.remove_file(&temporary)?;
                Ok(false)
            }
            WriteFileMode::Replace => {
                let metadata = directory.symlink_metadata(&filename)?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(BrokerError::NotRegularFile);
                }
                directory.rename(&temporary, &directory, &filename)?;
                Ok(true)
            }
        }
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}

fn destination_matches(root: &Dir, path: &RelativePath, byte_len: usize, sha256: [u8; 32]) -> bool {
    if byte_len == 0 || byte_len > MAX_WRITE_FILE_BYTES {
        return false;
    }
    let Ok((directory, filename)) = destination_parent(root, path) else {
        return false;
    };
    let Ok(metadata) = directory.symlink_metadata(&filename) else {
        return false;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != byte_len as u64
    {
        return false;
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let Ok(file) = directory.open_with(&filename, &options) else {
        return false;
    };
    let mut bytes = Vec::with_capacity(byte_len);
    if file
        .take((MAX_WRITE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    bytes.len() == byte_len && <[u8; 32]>::from(Sha256::digest(&bytes)) == sha256
}

fn apply_root_attachment(
    state: &mut State,
    request: AttachmentFingerprint,
) -> Result<RootAttachmentMutationResult, BrokerError> {
    validate_subject_conversation(request.subject, request.conversation_id)?;
    let changed = match request.mutation {
        RootAttachmentMutationKind::Attach => {
            state
                .roots
                .get(&request.root_id)
                .ok_or(BrokerError::UnknownRoot)?;
            let consent = ConsentRecord::new(
                request
                    .consent_method
                    .ok_or(BrokerError::InvalidConsentMethod)?,
                Utc::now(),
            );
            ensure_subject_grants(state, request.subject, request.root_id, consent)?;
            if has_root_attachment(state, request.conversation_id, request.root_id) {
                false
            } else {
                state.attachments.push(RootAttachment::new(
                    request.conversation_id,
                    request.root_id,
                )?);
                true
            }
        }
        RootAttachmentMutationKind::Detach => {
            if request.consent_method.is_some() {
                return Err(BrokerError::InvalidConsentMethod);
            }
            if state.roots.contains_key(&request.root_id)
                && !subject_has_root_grant(state, request.subject, request.root_id)
            {
                return Err(BrokerError::Denied);
            }
            let before = state.attachments.len();
            state.attachments.retain(|attachment| {
                attachment.conversation_id() != request.conversation_id
                    || attachment.root_id() != request.root_id
            });
            let mut detached = state.attachments.len() != before;
            // A conversation can detach a folder whose directory is currently
            // out of reach. Without this the attachment would come back the
            // moment the folder did.
            for root in &mut state.unavailable {
                if root.id != request.root_id {
                    continue;
                }
                let before = root.attachments.len();
                root.attachments
                    .retain(|attachment| attachment.conversation_id() != request.conversation_id);
                detached |= root.attachments.len() != before;
            }
            detached
        }
    };
    Ok(RootAttachmentMutationResult {
        root_id: request.root_id,
        mutation: request.mutation,
        changed,
    })
}

fn attachment_mutation_matches(
    stored: &AttachmentFingerprint,
    requested: &AttachmentFingerprint,
) -> bool {
    attachment_target_matches(stored, requested)
        && (stored.consent_method == requested.consent_method
            || (stored.mutation == RootAttachmentMutationKind::Attach
                && stored.consent_method.is_none()
                && requested.consent_method.is_some()))
}

fn attachment_lookup_matches(
    stored: &AttachmentFingerprint,
    requested: &AttachmentFingerprint,
) -> bool {
    attachment_target_matches(stored, requested)
}

fn attachment_target_matches(
    stored: &AttachmentFingerprint,
    requested: &AttachmentFingerprint,
) -> bool {
    stored.subject == requested.subject
        && stored.conversation_id == requested.conversation_id
        && stored.root_id == requested.root_id
        && stored.mutation == requested.mutation
}

fn validate_subject_conversation(
    subject: GrantSubject,
    conversation_id: Uuid,
) -> Result<(), BrokerError> {
    if conversation_id.is_nil()
        || (subject.kind() == SubjectKind::Conversation && subject.id() != conversation_id)
    {
        return Err(BrokerError::SubjectConversationMismatch);
    }
    Ok(())
}

fn has_root_attachment(state: &State, conversation_id: Uuid, root_id: RootId) -> bool {
    state.attachments.iter().any(|attachment| {
        attachment.conversation_id() == conversation_id && attachment.root_id() == root_id
    })
}

fn subject_has_root_grant(state: &State, subject: GrantSubject, root_id: RootId) -> bool {
    state.grants.iter().any(|grant| {
        grant.subject() == subject
            && grant.capability() == Capability::ReadFiles
            && matches!(grant.scope(), Scope::Root { root_id: granted } if *granted == root_id)
    })
}

fn ensure_subject_grants(
    state: &mut State,
    subject: GrantSubject,
    root_id: RootId,
    consent: ConsentRecord,
) -> Result<(), BrokerError> {
    if !state.grants.iter().any(|grant| {
        grant.subject() == subject
            && grant.capability() == Capability::ListRoots
            && matches!(grant.scope(), Scope::Subject)
    }) {
        state.grants.push(Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ListRoots,
            Scope::Subject,
            consent.clone(),
        )?);
    }
    if !subject_has_root_grant(state, subject, root_id) {
        state.grants.push(Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ReadFiles,
            Scope::Root { root_id },
            consent.clone(),
        )?);
    }
    if !state.grants.iter().any(|grant| {
        grant.subject() == subject
            && grant.capability() == Capability::WriteFiles
            && matches!(grant.scope(), Scope::Root { root_id: granted } if *granted == root_id)
    }) {
        state.grants.push(Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::WriteFiles,
            Scope::Root { root_id },
            consent,
        )?);
    }
    Ok(())
}

fn preferred_root_alias(state: &State, candidate: &RegisteredRoot) -> Option<(RootId, String)> {
    state
        .roots
        .iter()
        .filter(|(_, root)| root.root.identity() == candidate.root.identity())
        // Preserve an existing subject's legacy product root ID when possible,
        // then use the opaque UUID as the host-wide deterministic tie-breaker.
        .min_by_key(|(root_id, root)| (root.owner != candidate.owner, root_id.as_uuid()))
        .map(|(root_id, root)| (*root_id, root.display_name.clone()))
}

/// Record that an approved folder went dark, without naming its host path.
fn unavailable_root_event(root: &UnavailableRoot) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        request_id: RequestId::new(),
        operation_id: None,
        actor: AuditActor::Control {
            subject: root.owner,
            conversation_id: None,
        },
        operation: AuditOperation::PruneUnavailableRoot,
        target: AuditTarget::Root { root_id: root.id },
        outcome: AuditOutcome::Failed,
        capability: None,
        grant_id: None,
        error_code: Some(root.reason.error_code()),
        item_count: None,
        bytes: None,
    }
}

fn has_physical_root_alias(state: &State, root_id: RootId) -> bool {
    let Some(identity) = state.roots.get(&root_id).map(|root| root.root.identity()) else {
        return false;
    };
    state
        .roots
        .iter()
        .any(|(candidate_id, root)| *candidate_id != root_id && root.root.identity() == identity)
}

fn list_approved_roots(state: &State) -> Result<Vec<RootSummary>, ErrorResponse> {
    if state.roots.len() > MAX_LIST_ROOTS {
        return Err(error_response(BrokerError::RootListTooLarge));
    }
    let mut display_name_counts = HashMap::new();
    for root in state.roots.values() {
        *display_name_counts
            .entry(root.display_name.as_str())
            .or_insert(0usize) += 1;
    }
    let mut roots = state
        .roots
        .iter()
        // A basename is the only folder identity the native reuse prompt can
        // safely expose. If it is not unique, offer none of the colliding roots
        // and require the unambiguous native picker path instead.
        .filter(|(_, root)| display_name_counts.get(root.display_name.as_str()) == Some(&1))
        .map(|(root_id, root)| RootSummary {
            root_id: *root_id,
            display_name: root.display_name.clone(),
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.root_id.to_string().cmp(&right.root_id.to_string()))
    });
    Ok(roots)
}

fn list_roots(
    state: &State,
    context: ExecutionContext,
) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
    if !state
        .attachments
        .iter()
        .any(|attachment| attachment.conversation_id() == context.conversation_id())
    {
        return Ok((OperationResult::ListRoots { roots: Vec::new() }, None));
    }
    let grant_id = authorize(state, context, Capability::ListRoots, Resource::Subject)?;
    let mut roots = state
        .roots
        .iter()
        .filter(|(root_id, _root)| {
            authorize(
                state,
                context,
                Capability::ReadFiles,
                Resource::Root(root_id),
            )
            .is_ok()
        })
        .map(|(root_id, root)| RootAccess {
            root_id: *root_id,
            display_name: root.display_name.clone(),
            capabilities: root_capabilities(state, context, root_id),
        })
        .take(MAX_LIST_ROOTS + 1)
        .collect::<Vec<_>>();
    if roots.len() > MAX_LIST_ROOTS {
        return Err(BrokerError::RootListTooLarge);
    }
    roots.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.root_id.to_string().cmp(&right.root_id.to_string()))
    });
    Ok((OperationResult::ListRoots { roots }, Some(grant_id)))
}

fn resolve_exec_roots(
    state: &State,
    request: ResolveExecRootsRequest,
) -> Result<Vec<ResolvedExecRoot>, BrokerError> {
    if request.root_ids.len() > MAX_RESOLVE_EXEC_ROOTS {
        return Err(BrokerError::RootListTooLarge);
    }
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for root_id in request.root_ids {
        if !seen.insert(root_id)
            || authorize(
                state,
                request.context,
                Capability::ReadFiles,
                Resource::Root(&root_id),
            )
            .is_err()
        {
            continue;
        }
        let Some(root) = state.roots.get(&root_id) else {
            continue;
        };
        let path = root.root.canonical_path_if_current()?.to_path_buf();
        let writable = authorize(
            state,
            request.context,
            Capability::WriteFiles,
            Resource::Root(&root_id),
        )
        .is_ok();
        roots.push(ResolvedExecRoot {
            root_id,
            path,
            writable,
        });
    }
    Ok(roots)
}

/// Per-folder capabilities this conversation actually holds on one root.
///
/// Each candidate is put through the same [`authorize`] call that gates the
/// corresponding operation rather than read off the grant list, so a reported
/// capability and an allowed operation cannot disagree. Subject-wide
/// capabilities are excluded: they are not properties of a folder.
fn root_capabilities(
    state: &State,
    context: ExecutionContext,
    root_id: &RootId,
) -> Vec<Capability> {
    [Capability::ReadFiles, Capability::WriteFiles]
        .into_iter()
        .filter(|capability| {
            authorize(state, context, *capability, Resource::Root(root_id)).is_ok()
        })
        .collect()
}

fn root_display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Connected folder".to_owned());
    if name.len() <= MAX_ROOT_DISPLAY_BYTES {
        return name;
    }
    let mut boundary = MAX_ROOT_DISPLAY_BYTES;
    while !name.is_char_boundary(boundary) {
        boundary -= 1;
    }
    name[..boundary].to_owned()
}

fn list_directory(root: &Dir, path: &RelativePath) -> Result<OperationResult, BrokerError> {
    let entries = root.read_dir(Path::new(path.as_str()))?;
    let mut result = Vec::new();
    let mut output_bytes = 0usize;
    let mut examined_entries = 0usize;
    for entry in entries {
        examined_entries = examined_entries.saturating_add(1);
        if examined_entries > MAX_LIST_DIR_ENTRIES {
            return Err(BrokerError::DirectoryTooLarge);
        }
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if RelativePath::parse(&name).is_err() {
            continue;
        }
        output_bytes = output_bytes.saturating_add(name.len());
        if output_bytes > MAX_LIST_DIR_BYTES {
            return Err(BrokerError::DirectoryTooLarge);
        }
        let file_type = entry.file_type()?;
        let kind = if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::Other
        };
        result.push(DirectoryEntry { name, kind });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(OperationResult::ListDirectory { entries: result })
}

fn read_file(root: &Dir, path: &RelativePath) -> Result<OperationResult, BrokerError> {
    let bytes = read_bounded(root, path, MAX_READ_FILE_BYTES)?;
    let bytes_read = bytes.len();
    let content = String::from_utf8(bytes).map_err(|_| BrokerError::NotUtf8)?;
    Ok(OperationResult::ReadFile(ReadFileResult {
        content,
        bytes: bytes_read,
    }))
}

/// Read one file as opaque bytes under the same read authority as [`read_file`].
///
/// The UTF-8 requirement is deliberately absent: the caller is handing the file
/// to a trusted product pipeline rather than returning text to an agent. The
/// larger bound reflects that a PDF or Office document is routinely megabytes.
fn read_file_binary(root: &Dir, path: &RelativePath) -> Result<OperationResult, BrokerError> {
    let bytes = read_bounded(root, path, MAX_READ_FILE_BINARY_BYTES)?;
    Ok(OperationResult::ReadFileBinary(ReadFileBinaryResult {
        bytes: bytes.len(),
        content_base64: BASE64.encode(&bytes),
    }))
}

/// Read a whole regular file in one open, refusing anything over `maximum`.
///
/// The length is checked twice: once from metadata to avoid buffering a file the
/// caller will reject anyway, and once after reading one byte past the bound so
/// a file that grows between the two checks is refused rather than truncated.
fn read_bounded(root: &Dir, path: &RelativePath, maximum: usize) -> Result<Vec<u8>, BrokerError> {
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true);
    let file = root.open_with(Path::new(path.as_str()), &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(BrokerError::NotRegularFile);
    }
    if metadata.len() > maximum as u64 {
        return Err(BrokerError::FileTooLarge { maximum });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((maximum + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(BrokerError::FileTooLarge { maximum });
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum Resource<'a> {
    Subject,
    Root(&'a RootId),
    Path {
        root_id: &'a RootId,
        relative: &'a RelativePath,
    },
}

fn authorize(
    state: &State,
    context: ExecutionContext,
    capability: Capability,
    resource: Resource<'_>,
) -> Result<GrantId, BrokerError> {
    let attached = match resource {
        Resource::Subject => true,
        Resource::Root(root_id) | Resource::Path { root_id, .. } => {
            state.attachments.iter().any(|attachment| {
                attachment.conversation_id() == context.conversation_id()
                    && attachment.root_id() == *root_id
            })
        }
    };
    if !attached {
        return Err(BrokerError::Denied);
    }
    state
        .grants
        .iter()
        .find(|grant| {
            context.grant_subject_matches(grant.subject())
                && grant.capability() == capability
                && scope_covers(grant.scope(), resource)
        })
        .map(Grant::id)
        .ok_or(BrokerError::Denied)
}

fn scope_covers(scope: &Scope, resource: Resource<'_>) -> bool {
    match (scope, resource) {
        (Scope::Subject, Resource::Subject) => true,
        (Scope::Root { root_id }, Resource::Root(requested)) => root_id == requested,
        (
            Scope::Root { root_id },
            Resource::Path {
                root_id: requested, ..
            },
        ) => root_id == requested,
        (
            Scope::PathSubtree { root_id, relative },
            Resource::Path {
                root_id: requested,
                relative: requested_relative,
            },
        ) => root_id == requested && path_starts_with(requested_relative, relative),
        _ => false,
    }
}

fn path_starts_with(candidate: &RelativePath, prefix: &RelativePath) -> bool {
    let candidate = candidate.segments().collect::<Vec<_>>();
    let prefix = prefix.segments().collect::<Vec<_>>();
    candidate.len() >= prefix.len()
        && prefix
            .iter()
            .zip(candidate.iter())
            .all(|(left, right)| left == right)
}

fn scope_targets_root(scope: &Scope, requested: RootId) -> bool {
    match scope {
        Scope::Root { root_id } | Scope::PathSubtree { root_id, .. } => *root_id == requested,
        Scope::Subject => false,
    }
}

#[cfg(test)]
mod tests;

//! Owning broker for consent mutations and capability-checked filesystem reads.
//!
//! The controller and operator share one registry. Operations take an authorized
//! clone of a pinned root handle under a short lock, perform bounded-result I/O,
//! then reauthorize before releasing bytes. Revocation therefore completes
//! without waiting on host I/O, prevents new operations, and fences results from
//! operations that were already in flight.

use std::{
    collections::{HashMap, HashSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use cap_fs_ext::OpenOptionsSyncExt;
use cap_std::fs::{Dir, OpenOptions};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    audit::{
        AuditActor, AuditError, AuditEvent, AuditOperation, AuditOutcome, AuditSink, AuditTarget,
        JsonlAuditSink, MemoryAuditSink, UnavailableAuditSink,
    },
    protocol::{
        ControlEnvelope, ControlRequest, ControlResponseEnvelope, ControlResult, DirectoryEntry,
        EntryKind, ErrorCode, ErrorResponse, HelloResult, LookupRegisterRootReceiptRequest,
        LookupRegisterRootReceiptResult, LookupRootAttachmentReceiptRequest,
        LookupRootAttachmentReceiptResult, OperationEnvelope, OperationRequest,
        OperationResponseEnvelope, OperationResult, PathRequest, ReadFileResult,
        RegisterRootReceipt, RegisterRootRequest, RegisterRootResult, Response, ResponseEnvelope,
        RevokeRootRequest, RevokeRootResult, RootAttachmentMutationKind,
        RootAttachmentMutationReceipt, RootAttachmentMutationRequest, RootAttachmentMutationResult,
        RootSummary, PROTOCOL_VERSION,
    },
    Capability, ConsentMethod, ConsentRecord, ExecutionContext, Grant, GrantError, GrantId,
    GrantSubject, OperationId, RelativePath, RootAttachment, RootId, RootPolicy, RootPolicyError,
    Scope, SubjectKind, ValidatedRoot,
};

mod state_file;
use state_file::StateFile;

const MAX_READ_FILE_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_ENTRIES: usize = 4_096;
const MAX_LIST_ROOTS: usize = 256;
const MAX_ROOT_DISPLAY_BYTES: usize = 1024;

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
    #[error("file is too large (maximum {MAX_READ_FILE_BYTES} bytes)")]
    FileTooLarge,
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
    fn record_audit(&self, event: &AuditEvent) {
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
}

#[derive(Clone)]
struct RegisteredRoot {
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
}

struct PreparedRegistration {
    conversation_id: Uuid,
    root_id: RootId,
    root: RegisteredRoot,
    grants: [Grant; 2],
    attachment: RootAttachment,
}

struct ControlAudit {
    actor: AuditActor,
    operation: AuditOperation,
    operation_id: OperationId,
    target: AuditTarget,
}

impl ControlAudit {
    fn from_request(request: &ControlRequest) -> Option<Self> {
        match request {
            ControlRequest::Hello => None,
            ControlRequest::RegisterRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                operation: AuditOperation::RegisterRoot,
                operation_id: request.operation_id,
                target: AuditTarget::selected_folder(&request.path),
            }),
            ControlRequest::LookupRegisterRootReceipt(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                operation: AuditOperation::LookupRegisterRootReceipt,
                operation_id: request.operation_id,
                target: AuditTarget::Subject,
            }),
            ControlRequest::AttachRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                operation: AuditOperation::AttachRoot,
                operation_id: request.operation_id,
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::DetachRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                operation: AuditOperation::DetachRoot,
                operation_id: request.operation_id,
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::LookupRootAttachmentReceipt(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                operation: AuditOperation::LookupRootAttachmentReceipt,
                operation_id: request.operation_id,
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::RevokeRoot(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: None,
                },
                operation: AuditOperation::RevokeRoot,
                operation_id: request.operation_id,
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
        }
    }
}

struct OperationAudit {
    actor: AuditActor,
    operation: AuditOperation,
    capability: Capability,
    target: AuditTarget,
}

impl OperationAudit {
    fn from_envelope(envelope: &OperationEnvelope) -> Self {
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
        };
        Self {
            actor: AuditActor::Operation {
                context: envelope.context,
            },
            operation,
            capability,
            target,
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
        let audit: Arc<dyn AuditSink> = match JsonlAuditSink::open(data_dir) {
            Ok(audit) => Arc::new(audit),
            Err(error) => {
                eprintln!("openwave host broker audit unavailable at startup: {error}");
                Arc::new(UnavailableAuditSink)
            }
        };
        Ok(Self {
            shared: Arc::new(Shared {
                policy,
                state: Mutex::new(state),
                state_file: Some(state_file),
                audit,
                failed_closed: AtomicBool::new(false),
            }),
        })
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
        let result = self.execute(envelope);
        if let Some(audit) = audit {
            self.record_audit(request_id, audit, &result);
        }
        response_envelope(request_id, result)
    }

    fn record_audit(
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
            operation_id: Some(metadata.operation_id),
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
        self.shared.record_audit(&event);
    }

    fn execute(&self, envelope: ControlEnvelope) -> Result<ControlResult, ErrorResponse> {
        if matches!(envelope.request, ControlRequest::Hello) {
            return Ok(ControlResult::Hello(hello()));
        }
        require_version(envelope.protocol_version).map_err(error_response)?;
        match envelope.request {
            ControlRequest::Hello => Ok(ControlResult::Hello(hello())),
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
                let existing = next.roots.iter().find_map(|(root_id, root)| {
                    (root.owner == prepared.root.owner
                        && root.root.identity() == prepared.root.root.identity())
                    .then(|| (*root_id, root.display_name.clone()))
                });
                if let Some((root_id, display_name)) = existing {
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
                | MutationRecord::Attachment { .. } => {
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
            Some(MutationRecord::Revoke { .. } | MutationRecord::Attachment { .. }) => {
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
            grants: [list_grant, read_grant],
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
        };
        let state = self.lock_state().map_err(error_response)?;
        let receipt = match state.mutations.get(&request.operation_id) {
            None => RootAttachmentMutationReceipt::Unknown,
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Pending,
            }) if *existing == expected => {
                return Err(error_response(BrokerError::OperationInProgress));
            }
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Complete(Ok(result)),
            }) if *existing == expected => RootAttachmentMutationReceipt::Completed {
                result: *result,
                currently_attached: has_root_attachment(
                    &state,
                    request.conversation_id,
                    request.root_id,
                ),
            },
            Some(MutationRecord::Attachment {
                request: existing,
                outcome: MutationOutcome::Complete(Err(error)),
            }) if *existing == expected => RootAttachmentMutationReceipt::Failed {
                error: error.clone(),
            },
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
        if owned {
            next.roots.remove(&request.root_id);
            next.grants
                .retain(|grant| !scope_targets_root(grant.scope(), request.root_id));
            next.attachments
                .retain(|attachment| attachment.root_id() != request.root_id);
        }
        let result = Ok(RevokeRootResult { revoked: owned });
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
        let (result, grant_id) = self.execute(envelope);
        let result = result.map_err(error_response);
        self.record_audit(request_id, audit, grant_id, &result);
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
                    grant_id = Some(authorized_by);
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
            }
        })();
        (result, grant_id)
    }

    fn record_audit(
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
        self.shared.record_audit(&event);
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
        BrokerError::FileTooLarge
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
            ErrorCode::Internal,
            "broker audit storage is unavailable",
            false,
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
    state.roots.get(&root.root_id).is_some_and(|registered| {
        registered.owner == request.subject && registered.display_name == root.display_name
    }) && state.attachments.iter().any(|attachment| {
        attachment.conversation_id() == request.conversation_id
            && attachment.root_id() == root.root_id
    }) && state.grants.iter().any(|grant| {
        grant.subject() == request.subject
            && grant.capability() == Capability::ListRoots
            && matches!(grant.scope(), Scope::Subject)
    }) && state.grants.iter().any(|grant| {
        grant.subject() == request.subject
            && grant.capability() == Capability::ReadFiles
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
        }) if *existing == request => Ok(Claim::Complete(result.clone())),
        Some(MutationRecord::Attachment {
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
        }) if *existing == request && matches!(outcome, MutationOutcome::Pending) => {
            *outcome = MutationOutcome::Complete(result);
            state.active_mutations.remove(&operation_id);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

fn apply_root_attachment(
    state: &mut State,
    request: AttachmentFingerprint,
) -> Result<RootAttachmentMutationResult, BrokerError> {
    validate_subject_conversation(request.subject, request.conversation_id)?;
    let changed = match request.mutation {
        RootAttachmentMutationKind::Attach => {
            let root = state
                .roots
                .get(&request.root_id)
                .ok_or(BrokerError::UnknownRoot)?;
            if root.owner != request.subject {
                return Err(BrokerError::Denied);
            }
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
            if let Some(root) = state.roots.get(&request.root_id) {
                if root.owner != request.subject {
                    return Err(BrokerError::Denied);
                }
            }
            let before = state.attachments.len();
            state.attachments.retain(|attachment| {
                attachment.conversation_id() != request.conversation_id
                    || attachment.root_id() != request.root_id
            });
            state.attachments.len() != before
        }
    };
    Ok(RootAttachmentMutationResult {
        root_id: request.root_id,
        mutation: request.mutation,
        changed,
    })
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

fn list_roots(
    state: &State,
    context: ExecutionContext,
) -> Result<(OperationResult, GrantId), BrokerError> {
    let grant_id = authorize(state, context, Capability::ListRoots, Resource::Subject)?;
    let mut roots = state
        .roots
        .iter()
        .filter(|(root_id, root)| {
            context.grant_subject_matches(root.owner)
                && authorize(
                    state,
                    context,
                    Capability::ReadFiles,
                    Resource::Root(root_id),
                )
                .is_ok()
        })
        .map(|(root_id, root)| RootSummary {
            root_id: *root_id,
            display_name: root.display_name.clone(),
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
    Ok((OperationResult::ListRoots { roots }, grant_id))
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
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true);
    let file = root.open_with(Path::new(path.as_str()), &options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(BrokerError::NotRegularFile);
    }
    if metadata.len() > MAX_READ_FILE_BYTES as u64 {
        return Err(BrokerError::FileTooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_READ_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_READ_FILE_BYTES {
        return Err(BrokerError::FileTooLarge);
    }
    let bytes_read = bytes.len();
    let content = String::from_utf8(bytes).map_err(|_| BrokerError::NotUtf8)?;
    Ok(OperationResult::ReadFile(ReadFileResult {
        content,
        bytes: bytes_read,
    }))
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
mod tests {
    use super::*;

    #[derive(Default)]
    struct CollectingAudit {
        events: Mutex<Vec<AuditEvent>>,
    }

    impl AuditSink for CollectingAudit {
        fn record(&self, event: &AuditEvent) -> Result<(), AuditError> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    struct FailingAudit;

    impl AuditSink for FailingAudit {
        fn record(&self, _event: &AuditEvent) -> Result<(), AuditError> {
            Err(AuditError::Io(io::Error::other("injected audit failure")))
        }
    }

    fn test_policy(temp: &tempfile::TempDir) -> RootPolicy {
        RootPolicy::for_test(
            temp.path().join("home"),
            vec![temp.path().join("sensitive")],
            vec![temp.path().to_path_buf()],
            Vec::new(),
        )
    }

    fn setup() -> (tempfile::TempDir, Broker, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join("Documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
        std::fs::create_dir(root.join("reports")).unwrap();
        let policy = test_policy(&temp);
        (temp, Broker::new(policy), root)
    }

    fn durable_setup() -> (tempfile::TempDir, Broker, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("home/Documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
        let state_dir = temp.path().join("app-data/host-broker");
        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        (temp, broker, root, state_dir)
    }

    fn audited_setup() -> (tempfile::TempDir, Broker, PathBuf, Arc<CollectingAudit>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("home/Documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
        let audit = Arc::new(CollectingAudit::default());
        let broker = Broker::with_audit_sink(test_policy(&temp), audit.clone());
        (temp, broker, root, audit)
    }

    fn register(
        controller: &Controller,
        subject: GrantSubject,
        conversation_id: Uuid,
        path: PathBuf,
        operation_id: OperationId,
    ) -> RegisterRootResult {
        let result = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject,
                conversation_id,
                path,
                consent_method: ConsentMethod::FolderPicker,
            }),
        }))
        .unwrap();
        let ControlResult::RegisterRoot(result) = result else {
            panic!("unexpected control result")
        };
        result
    }

    fn mutate_attachment(
        controller: &Controller,
        operation_id: OperationId,
        subject: GrantSubject,
        conversation_id: Uuid,
        root_id: RootId,
        mutation: RootAttachmentMutationKind,
    ) -> Result<RootAttachmentMutationResult, ErrorResponse> {
        let request = RootAttachmentMutationRequest {
            operation_id,
            subject,
            conversation_id,
            root_id,
        };
        let control = match mutation {
            RootAttachmentMutationKind::Attach => ControlRequest::AttachRoot(request),
            RootAttachmentMutationKind::Detach => ControlRequest::DetachRoot(request),
        };
        let result = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: control,
        }))?;
        match result {
            ControlResult::AttachRoot(result) | ControlResult::DetachRoot(result) => Ok(result),
            _ => panic!("unexpected control result"),
        }
    }

    fn lookup_attachment_receipt(
        controller: &Controller,
        request: LookupRootAttachmentReceiptRequest,
    ) -> Result<RootAttachmentMutationReceipt, ErrorResponse> {
        let result = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::LookupRootAttachmentReceipt(request),
        }))?;
        let ControlResult::LookupRootAttachmentReceipt(result) = result else {
            panic!("unexpected control result")
        };
        Ok(result.receipt)
    }

    fn lookup_register_receipt(
        controller: &Controller,
        operation_id: OperationId,
        subject: GrantSubject,
        conversation_id: Uuid,
    ) -> Result<LookupRegisterRootReceiptResult, ErrorResponse> {
        let result = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::LookupRegisterRootReceipt(LookupRegisterRootReceiptRequest {
                operation_id,
                subject,
                conversation_id,
            }),
        }))?;
        let ControlResult::LookupRegisterRootReceipt(result) = result else {
            panic!("unexpected control result")
        };
        Ok(result)
    }

    fn operate(
        operator: &Operator,
        context: ExecutionContext,
        request: OperationRequest,
    ) -> Result<OperationResult, ErrorResponse> {
        unwrap_response(operator.handle(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            context,
            request,
        }))
    }

    fn unwrap_response<T>(envelope: ResponseEnvelope<T>) -> Result<T, ErrorResponse> {
        match envelope.response {
            Response::Ok(result) => Ok(result),
            Response::Error(error) => Err(error),
        }
    }

    #[test]
    fn register_list_read_and_revoke_are_one_live_authority_boundary() {
        let (_temp, broker, path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let registered = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            OperationId::new(),
        );
        let context = ExecutionContext::standalone(conversation).unwrap();

        let roots = operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap();
        assert_eq!(
            roots,
            OperationResult::ListRoots {
                roots: vec![registered.root.clone()]
            }
        );
        let listing = operate(
            &broker.operator(),
            context,
            OperationRequest::ListDirectory(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::root(),
            }),
        )
        .unwrap();
        let OperationResult::ListDirectory { entries } = listing else {
            panic!("unexpected listing result")
        };
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["note.txt", "reports"]
        );
        let read = operate(
            &broker.operator(),
            context,
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        )
        .unwrap();
        assert_eq!(
            read,
            OperationResult::ReadFile(ReadFileResult {
                content: "hello from broker".to_owned(),
                bytes: 17,
            })
        );

        let revoked = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id: registered.root.root_id,
            }),
        });
        assert_eq!(
            unwrap_response(revoked).unwrap(),
            ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
        );
        assert!(matches!(
            operate(
                &broker.operator(),
                context,
                OperationRequest::ReadFile(PathRequest {
                    root_id: registered.root.root_id,
                    path: RelativePath::parse("note.txt").unwrap(),
                })
            ),
            Err(ErrorResponse {
                code: ErrorCode::Denied,
                ..
            })
        ));
    }

    #[test]
    fn registration_receipt_lookup_never_starts_or_resumes_a_mutation() {
        let (temp, broker, path) = setup();
        let controller = broker.controller();
        let conversation_id = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation_id).unwrap();
        let unknown_id = OperationId::new();
        assert_eq!(
            lookup_register_receipt(&controller, unknown_id, subject, conversation_id).unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id: unknown_id,
                receipt: RegisterRootReceipt::Unknown,
            }
        );

        let operation_id = OperationId::new();
        broker.shared.state.lock().unwrap().mutations.insert(
            operation_id,
            MutationRecord::Register {
                request: RegisterFingerprint {
                    subject,
                    conversation_id,
                    path: path.clone(),
                    consent_method: ConsentMethod::FolderPicker,
                },
                outcome: MutationOutcome::Pending,
            },
        );
        assert_eq!(
            lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id,
                receipt: RegisterRootReceipt::Pending,
            }
        );
        let other_conversation = Uuid::new_v4();
        assert!(matches!(
            lookup_register_receipt(
                &controller,
                operation_id,
                GrantSubject::conversation(other_conversation).unwrap(),
                other_conversation,
            ),
            Err(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));
        let state = broker.shared.state.lock().unwrap();
        assert!(state.roots.is_empty());
        assert!(!state.active_mutations.contains(&operation_id));
        drop(state);

        let completed = register(&controller, subject, conversation_id, path, operation_id);
        let completed_root = completed.root.clone();
        assert_eq!(
            lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id,
                receipt: RegisterRootReceipt::Completed {
                    root: completed_root.clone(),
                },
            }
        );
        let revoke = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id: completed_root.root_id,
            }),
        }))
        .unwrap();
        assert_eq!(
            revoke,
            ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
        );
        assert_eq!(
            lookup_register_receipt(&controller, operation_id, subject, conversation_id).unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id,
                receipt: RegisterRootReceipt::Disconnected {
                    root: completed_root,
                },
            }
        );

        let failed_id = OperationId::new();
        let failure = unwrap_response(controller.handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id: failed_id,
                subject,
                conversation_id,
                path: temp.path().join("sensitive"),
                consent_method: ConsentMethod::FolderPicker,
            }),
        }))
        .unwrap_err();
        assert_eq!(
            lookup_register_receipt(&controller, failed_id, subject, conversation_id).unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id: failed_id,
                receipt: RegisterRootReceipt::Failed { error: failure },
            }
        );

        let revoke_id = OperationId::new();
        broker.shared.state.lock().unwrap().mutations.insert(
            revoke_id,
            MutationRecord::Revoke {
                request: RevokeFingerprint {
                    subject,
                    root_id: RootId::new(),
                },
                outcome: MutationOutcome::Pending,
            },
        );
        assert!(matches!(
            lookup_register_receipt(&controller, revoke_id, subject, conversation_id),
            Err(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));
    }

    #[test]
    fn registration_receipt_lookup_is_a_de_sensitized_audited_control_read() {
        let (_temp, broker, _path, audit) = audited_setup();
        let conversation_id = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation_id).unwrap();
        let operation_id = OperationId::new();
        lookup_register_receipt(&broker.controller(), operation_id, subject, conversation_id)
            .unwrap();

        let events = audit.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].operation,
            AuditOperation::LookupRegisterRootReceipt
        );
        assert_eq!(events[0].operation_id, Some(operation_id));
        assert_eq!(
            events[0].actor,
            AuditActor::Control {
                subject,
                conversation_id: Some(conversation_id),
            }
        );
        assert_eq!(events[0].target, AuditTarget::Subject);
    }

    #[test]
    fn repeated_registration_reuses_the_same_root_and_attaches_new_project_chat() {
        let (_temp, broker, path) = setup();
        let project = Uuid::new_v4();
        let first_chat = Uuid::new_v4();
        let second_chat = Uuid::new_v4();
        let subject = GrantSubject::project(project).unwrap();

        let first = register(
            &broker.controller(),
            subject,
            first_chat,
            path.clone(),
            OperationId::new(),
        );
        let repeated = register(
            &broker.controller(),
            subject,
            first_chat,
            path.clone(),
            OperationId::new(),
        );
        let attached = register(
            &broker.controller(),
            subject,
            second_chat,
            path,
            OperationId::new(),
        );

        assert_eq!(repeated.root, first.root);
        assert_eq!(attached.root, first.root);
        for chat in [first_chat, second_chat] {
            let roots = operate(
                &broker.operator(),
                ExecutionContext::project_chat(chat, project).unwrap(),
                OperationRequest::ListRoots,
            )
            .unwrap();
            assert_eq!(
                roots,
                OperationResult::ListRoots {
                    roots: vec![first.root.clone()]
                }
            );
        }
    }

    #[test]
    fn broker_audits_control_reads_denials_and_authorizing_grants() {
        let (_temp, broker, path, audit) = audited_setup();
        let hello = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::Hello,
        });
        assert!(matches!(hello.response, Response::Ok(_)));
        assert!(audit.events.lock().unwrap().is_empty());

        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let registered = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            OperationId::new(),
        );
        let request = OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        });
        operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            request.clone(),
        )
        .unwrap();
        assert!(matches!(
            operate(
                &broker.operator(),
                ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
                request,
            ),
            Err(ErrorResponse {
                code: ErrorCode::Denied,
                ..
            })
        ));

        let events = audit.events.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].operation, AuditOperation::RegisterRoot);
        assert!(matches!(
            &events[0].target,
            AuditTarget::SelectedFolder { display_name } if display_name.as_str() == "Documents"
        ));
        assert_eq!(events[1].operation, AuditOperation::ReadFile);
        assert_eq!(events[1].outcome, AuditOutcome::Allowed);
        assert!(events[1].grant_id.is_some());
        assert_eq!(events[1].bytes, Some(17));
        assert!(matches!(
            &events[1].target,
            AuditTarget::Path { root_id, relative }
                if *root_id == registered.root.root_id && relative.as_str() == "note.txt"
        ));
        assert_eq!(events[2].outcome, AuditOutcome::Denied);
        assert_eq!(events[2].error_code, Some(ErrorCode::Denied));
        assert!(events[2].grant_id.is_none());
        let encoded = serde_json::to_string(&*events).unwrap();
        assert!(!encoded.contains("home/Documents"));
        assert!(!encoded.contains("hello from broker"));
    }

    #[test]
    fn read_tier_audit_failure_does_not_block_user_access() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("home/Documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
        let broker = Broker::with_audit_sink(test_policy(&temp), Arc::new(FailingAudit));
        let conversation = Uuid::new_v4();
        let registered = register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            root,
            OperationId::new(),
        );
        let result = operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn project_grant_still_requires_an_exact_conversation_attachment() {
        let (_temp, broker, path) = setup();
        let project = Uuid::new_v4();
        let attached = Uuid::new_v4();
        let registered = register(
            &broker.controller(),
            GrantSubject::project(project).unwrap(),
            attached,
            path,
            OperationId::new(),
        );
        let request = OperationRequest::ReadFile(PathRequest {
            root_id: registered.root.root_id,
            path: RelativePath::parse("note.txt").unwrap(),
        });
        assert!(operate(
            &broker.operator(),
            ExecutionContext::project_chat(attached, project).unwrap(),
            request.clone(),
        )
        .is_ok());
        assert!(matches!(
            operate(
                &broker.operator(),
                ExecutionContext::project_chat(Uuid::new_v4(), project).unwrap(),
                request,
            ),
            Err(ErrorResponse {
                code: ErrorCode::Denied,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pinned_root_rejects_a_symlink_escape_at_operation_time() {
        use std::os::unix::fs::symlink;

        let (temp, broker, path) = setup();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "not connected").unwrap();
        symlink(&outside, path.join("escape")).unwrap();

        let conversation = Uuid::new_v4();
        let registered = register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path,
            OperationId::new(),
        );
        let result = operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("escape/secret.txt").unwrap(),
            }),
        );
        assert!(matches!(
            result,
            Err(ErrorResponse {
                code: ErrorCode::HostIo,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn directory_bound_counts_unaddressable_entries_examined() {
        let (_temp, broker, path) = setup();
        for index in 0..=MAX_LIST_DIR_ENTRIES {
            std::fs::write(path.join(format!("skip:{index}")), b"").unwrap();
        }
        let conversation = Uuid::new_v4();
        let registered = register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path,
            OperationId::new(),
        );
        let result = operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            OperationRequest::ListDirectory(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::root(),
            }),
        );
        assert!(matches!(
            result,
            Err(ErrorResponse {
                code: ErrorCode::TooLarge,
                ..
            })
        ));
    }

    #[test]
    fn connected_root_results_are_bounded_before_transport_serialization() {
        let (_temp, broker, path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let consent = ConsentRecord::new(ConsentMethod::FolderPicker, Utc::now());
        let pinned = Arc::new(broker.shared.policy.open_root(&path).unwrap());
        let mut state = State::default();
        state.grants.push(
            Grant::from_consent(
                GrantId::new(),
                subject,
                Capability::ListRoots,
                Scope::Subject,
                consent.clone(),
            )
            .unwrap(),
        );
        for _ in 0..=MAX_LIST_ROOTS {
            let root_id = RootId::new();
            state.roots.insert(
                root_id,
                RegisteredRoot {
                    owner: subject,
                    display_name: "Documents".to_owned(),
                    root: pinned.clone(),
                },
            );
            state
                .attachments
                .push(RootAttachment::new(conversation, root_id).unwrap());
            state.grants.push(
                Grant::from_consent(
                    GrantId::new(),
                    subject,
                    Capability::ReadFiles,
                    Scope::Root { root_id },
                    consent.clone(),
                )
                .unwrap(),
            );
        }
        assert!(matches!(
            list_roots(&state, ExecutionContext::standalone(conversation).unwrap()),
            Err(BrokerError::RootListTooLarge)
        ));
    }

    #[test]
    fn connected_root_display_names_are_bounded_on_utf8_boundaries() {
        let component = "é".repeat(MAX_ROOT_DISPLAY_BYTES);
        let display = root_display_name(Path::new(&component));
        assert!(display.len() <= MAX_ROOT_DISPLAY_BYTES);
        assert!(display.is_char_boundary(display.len()));
    }

    #[test]
    fn control_mutations_are_idempotent_and_reject_identity_reuse() {
        let (_temp, broker, path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let operation_id = OperationId::new();
        let first = register(
            &broker.controller(),
            subject,
            conversation,
            path.clone(),
            operation_id,
        );
        let retry = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            operation_id,
        );
        assert_eq!(retry, first);
        let conflict = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id,
                subject,
                root_id: first.root.root_id,
            }),
        });
        assert!(matches!(
            conflict.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));
    }

    #[test]
    fn attach_and_detach_are_exact_conversation_mutations() {
        let (_temp, broker, path) = setup();
        let project_id = Uuid::new_v4();
        let first_conversation = Uuid::new_v4();
        let second_conversation = Uuid::new_v4();
        let subject = GrantSubject::project(project_id).unwrap();
        let registered = register(
            &broker.controller(),
            subject,
            first_conversation,
            path,
            OperationId::new(),
        );
        let root_id = registered.root.root_id;

        let attach_id = OperationId::new();
        let attached = mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            second_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap();
        assert!(attached.changed);
        assert_eq!(
            mutate_attachment(
                &broker.controller(),
                attach_id,
                subject,
                second_conversation,
                root_id,
                RootAttachmentMutationKind::Attach,
            )
            .unwrap(),
            attached
        );
        assert!(matches!(
            mutate_attachment(
                &broker.controller(),
                attach_id,
                subject,
                second_conversation,
                root_id,
                RootAttachmentMutationKind::Detach,
            ),
            Err(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));

        let second_context =
            ExecutionContext::project_chat(second_conversation, project_id).unwrap();
        assert!(matches!(
            operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots == vec![registered.root.clone()]
        ));

        let detach_id = OperationId::new();
        let detached = mutate_attachment(
            &broker.controller(),
            detach_id,
            subject,
            first_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap();
        assert!(detached.changed);
        let first_context = ExecutionContext::project_chat(first_conversation, project_id).unwrap();
        assert!(matches!(
            operate(&broker.operator(), first_context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots.is_empty()
        ));
        assert!(matches!(
            operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots == vec![registered.root]
        ));
        assert!(
            !mutate_attachment(
                &broker.controller(),
                OperationId::new(),
                subject,
                first_conversation,
                root_id,
                RootAttachmentMutationKind::Detach,
            )
            .unwrap()
            .changed
        );
    }

    #[test]
    fn attachment_receipts_report_historical_result_and_current_state() {
        let (_temp, broker, path) = setup();
        let project_id = Uuid::new_v4();
        let registered_conversation = Uuid::new_v4();
        let attached_conversation = Uuid::new_v4();
        let subject = GrantSubject::project(project_id).unwrap();
        let root_id = register(
            &broker.controller(),
            subject,
            registered_conversation,
            path,
            OperationId::new(),
        )
        .root
        .root_id;
        let attach_id = OperationId::new();
        let attach = mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            attached_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap();
        let lookup = LookupRootAttachmentReceiptRequest {
            operation_id: attach_id,
            subject,
            conversation_id: attached_conversation,
            root_id,
            mutation: RootAttachmentMutationKind::Attach,
        };
        assert_eq!(
            lookup_attachment_receipt(&broker.controller(), lookup).unwrap(),
            RootAttachmentMutationReceipt::Completed {
                result: attach,
                currently_attached: true,
            }
        );

        let detach_id = OperationId::new();
        mutate_attachment(
            &broker.controller(),
            detach_id,
            subject,
            attached_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap();
        assert!(matches!(
            lookup_attachment_receipt(&broker.controller(), lookup).unwrap(),
            RootAttachmentMutationReceipt::Completed {
                currently_attached: false,
                ..
            }
        ));
        let conflicting_lookup = LookupRootAttachmentReceiptRequest {
            mutation: RootAttachmentMutationKind::Detach,
            ..lookup
        };
        assert!(matches!(
            lookup_attachment_receipt(&broker.controller(), conflicting_lookup),
            Err(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));
    }

    #[test]
    fn failed_attachment_mutation_is_durable_and_cannot_widen_authority() {
        let (_temp, broker, _path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let operation_id = OperationId::new();
        let root_id = RootId::new();
        let first = mutate_attachment(
            &broker.controller(),
            operation_id,
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap_err();
        let retry = mutate_attachment(
            &broker.controller(),
            operation_id,
            subject,
            conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap_err();
        assert_eq!(first, retry);
        assert_eq!(first.code, ErrorCode::InvalidRoot);
        assert!(matches!(
            lookup_attachment_receipt(
                &broker.controller(),
                LookupRootAttachmentReceiptRequest {
                    operation_id,
                    subject,
                    conversation_id: conversation,
                    root_id,
                    mutation: RootAttachmentMutationKind::Attach,
                },
            )
            .unwrap(),
            RootAttachmentMutationReceipt::Failed { error } if error == first
        ));
    }

    #[test]
    fn failed_control_mutation_still_binds_its_operation_identity() {
        let (_temp, broker, path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let operation_id = OperationId::new();
        let invalid = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                path: path.clone(),
                consent_method: ConsentMethod::PermissionDialog,
            }),
        };
        let first = broker.controller().handle(invalid.clone());
        let retry = broker.controller().handle(invalid);
        assert_eq!(first.response, retry.response);
        assert!(matches!(
            first.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                ..
            })
        ));

        let conflict = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                path,
                consent_method: ConsentMethod::FolderPicker,
            }),
        });
        assert!(matches!(
            conflict.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::OperationIdConflict,
                ..
            })
        ));
    }

    #[test]
    fn in_flight_mutation_identity_cannot_be_reused() {
        let operation_id = OperationId::new();
        let subject = GrantSubject::conversation(Uuid::new_v4()).unwrap();
        let request = RegisterFingerprint {
            subject,
            conversation_id: subject.id(),
            path: PathBuf::from("/selected/folder"),
            consent_method: ConsentMethod::FolderPicker,
        };
        let mut state = State::default();
        assert!(matches!(
            claim_register(&mut state, operation_id, &request).unwrap(),
            Claim::Start
        ));
        assert!(matches!(
            claim_register(&mut state, operation_id, &request),
            Err(BrokerError::OperationInProgress)
        ));
        let mut different = request.clone();
        different.path = PathBuf::from("/other/folder");
        assert!(matches!(
            claim_register(&mut state, operation_id, &different),
            Err(BrokerError::OperationIdConflict)
        ));
    }

    #[test]
    fn completed_revocation_fences_an_in_flight_read_result() {
        let (_temp, broker, path) = setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let registered = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            OperationId::new(),
        );
        let context = ExecutionContext::standalone(conversation).unwrap();
        let relative = RelativePath::parse("note.txt").unwrap();
        let operator = broker.operator();
        let (directory, _) = operator
            .authorized_root(context, registered.root.root_id, &relative)
            .unwrap();
        let buffered = read_file(&directory, &relative).unwrap();

        let revoked = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: OperationId::new(),
                subject,
                root_id: registered.root.root_id,
            }),
        });
        assert!(matches!(revoked.response, Response::Ok(_)));
        assert!(matches!(
            operator.reauthorize(context, registered.root.root_id, &relative),
            Err(BrokerError::Denied)
        ));
        drop(buffered);
    }

    #[test]
    fn hello_negotiates_across_version_skew_but_operations_do_not() {
        let (_temp, broker, _path) = setup();
        let hello_request_id = crate::RequestId::new();
        let hello = broker.controller().handle(ControlEnvelope {
            protocol_version: PROTOCOL_VERSION + 1,
            request_id: hello_request_id,
            request: ControlRequest::Hello,
        });
        assert_eq!(hello.request_id, hello_request_id);
        assert_eq!(
            unwrap_response(hello).unwrap(),
            ControlResult::Hello(super::hello())
        );

        let error = broker.operator().handle(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION + 1,
            request_id: crate::RequestId::new(),
            context: ExecutionContext::standalone(Uuid::new_v4()).unwrap(),
            request: OperationRequest::ListRoots,
        });
        assert!(matches!(
            error.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::ProtocolVersion,
                retryable: false,
                ..
            })
        ));
    }

    #[test]
    fn transport_retryability_is_an_explicit_transient_allowlist() {
        for kind in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::TimedOut,
        ] {
            let response = error_response(BrokerError::Io(io::Error::from(kind)));
            assert_eq!(response.code, ErrorCode::HostIo);
            assert!(response.retryable, "{kind:?}");
        }
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidInput] {
            let response = error_response(BrokerError::Io(io::Error::from(kind)));
            assert_eq!(response.code, ErrorCode::HostIo);
            assert!(!response.retryable, "{kind:?}");
        }
        let poisoned = error_response(BrokerError::StatePoisoned);
        assert_eq!(poisoned.code, ErrorCode::Internal);
        assert!(!poisoned.retryable);
    }

    #[test]
    fn durable_registry_receipts_and_revocation_survive_restart() {
        let (temp, broker, path, state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let register_id = OperationId::new();
        let registered = register(
            &broker.controller(),
            subject,
            conversation,
            path.clone(),
            register_id,
        );
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        let retry = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            register_id,
        );
        assert_eq!(retry, registered);
        let context = ExecutionContext::standalone(conversation).unwrap();
        assert!(operate(
            &broker.operator(),
            context,
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        )
        .is_ok());

        let revoke_id = OperationId::new();
        let revoke = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RevokeRoot(RevokeRootRequest {
                operation_id: revoke_id,
                subject,
                root_id: registered.root.root_id,
            }),
        };
        let first = unwrap_response(broker.controller().handle(revoke.clone())).unwrap();
        assert_eq!(
            first,
            ControlResult::RevokeRoot(RevokeRootResult { revoked: true })
        );
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        let retry = unwrap_response(broker.controller().handle(revoke)).unwrap();
        assert_eq!(retry, first);
        assert_eq!(
            operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots: Vec::new() }
        );
        assert!(matches!(
            operate(
                &broker.operator(),
                context,
                OperationRequest::ReadFile(PathRequest {
                    root_id: registered.root.root_id,
                    path: RelativePath::parse("note.txt").unwrap(),
                }),
            ),
            Err(ErrorResponse {
                code: ErrorCode::Denied,
                ..
            })
        ));
    }

    #[test]
    fn conversation_attachments_and_receipts_survive_restart() {
        let (temp, broker, path, state_dir) = durable_setup();
        let project_id = Uuid::new_v4();
        let first_conversation = Uuid::new_v4();
        let second_conversation = Uuid::new_v4();
        let subject = GrantSubject::project(project_id).unwrap();
        let root_id = register(
            &broker.controller(),
            subject,
            first_conversation,
            path,
            OperationId::new(),
        )
        .root
        .root_id;
        let attach_id = OperationId::new();
        let attach = mutate_attachment(
            &broker.controller(),
            attach_id,
            subject,
            second_conversation,
            root_id,
            RootAttachmentMutationKind::Attach,
        )
        .unwrap();
        let detach_id = OperationId::new();
        let detach = mutate_attachment(
            &broker.controller(),
            detach_id,
            subject,
            first_conversation,
            root_id,
            RootAttachmentMutationKind::Detach,
        )
        .unwrap();
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        assert_eq!(
            mutate_attachment(
                &broker.controller(),
                attach_id,
                subject,
                second_conversation,
                root_id,
                RootAttachmentMutationKind::Attach,
            )
            .unwrap(),
            attach
        );
        assert_eq!(
            mutate_attachment(
                &broker.controller(),
                detach_id,
                subject,
                first_conversation,
                root_id,
                RootAttachmentMutationKind::Detach,
            )
            .unwrap(),
            detach
        );
        let first_context = ExecutionContext::project_chat(first_conversation, project_id).unwrap();
        let second_context =
            ExecutionContext::project_chat(second_conversation, project_id).unwrap();
        assert_eq!(
            operate(
                &broker.operator(),
                first_context,
                OperationRequest::ListRoots
            )
            .unwrap(),
            OperationResult::ListRoots { roots: Vec::new() }
        );
        assert!(matches!(
            operate(&broker.operator(), second_context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots.len() == 1 && roots[0].root_id == root_id
        ));
        assert!(matches!(
            lookup_attachment_receipt(
                &broker.controller(),
                LookupRootAttachmentReceiptRequest {
                    operation_id: detach_id,
                    subject,
                    conversation_id: first_conversation,
                    root_id,
                    mutation: RootAttachmentMutationKind::Detach,
                }
            )
            .unwrap(),
            RootAttachmentMutationReceipt::Completed {
                result,
                currently_attached: false,
            } if result == detach
        ));
    }

    #[test]
    fn unavailable_audit_does_not_block_restart_or_read_access() {
        let (temp, broker, path, state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        let registered = register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path,
            OperationId::new(),
        );
        drop(broker);
        std::fs::write(
            state_dir.join("host-broker-audit.previous.jsonl"),
            vec![b'x'; 8 * 1024 * 1024 + 1],
        )
        .unwrap();

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        let result = operate(
            &broker.operator(),
            ExecutionContext::standalone(conversation).unwrap(),
            OperationRequest::ReadFile(PathRequest {
                root_id: registered.root.root_id,
                path: RelativePath::parse("note.txt").unwrap(),
            }),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn pending_registration_resumes_after_completion_save_failure_and_restart() {
        let (temp, broker, path, state_dir) = durable_setup();
        broker
            .shared
            .state_file
            .as_ref()
            .unwrap()
            .fail_after_saves(1);
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let operation_id = OperationId::new();
        let request = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                path: path.clone(),
                consent_method: ConsentMethod::FolderPicker,
            }),
        };
        let failed = broker.controller().handle(request);
        assert!(matches!(
            failed.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::HostIo,
                ..
            })
        ));
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        assert_eq!(
            lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,)
                .unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id,
                receipt: RegisterRootReceipt::Pending,
            }
        );
        assert!(broker.shared.state.lock().unwrap().roots.is_empty());
        let completed = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            operation_id,
        );
        assert_eq!(completed.root.display_name, "Documents");
    }

    #[test]
    fn ambiguous_state_publication_fails_closed_until_restart() {
        let (temp, broker, path, state_dir) = durable_setup();
        broker
            .shared
            .state_file
            .as_ref()
            .unwrap()
            .fail_once_after_publish();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let operation_id = OperationId::new();
        let request = ControlEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            request: ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject,
                conversation_id: conversation,
                path: path.clone(),
                consent_method: ConsentMethod::FolderPicker,
            }),
        };
        let ambiguous = broker.controller().handle(request);
        assert!(matches!(
            ambiguous.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::Internal,
                retryable: false,
                ..
            })
        ));
        let unavailable = broker.operator().handle(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: crate::RequestId::new(),
            context: ExecutionContext::standalone(conversation).unwrap(),
            request: OperationRequest::ListRoots,
        });
        assert!(matches!(
            unavailable.response,
            Response::Error(ErrorResponse {
                code: ErrorCode::Internal,
                retryable: false,
                ..
            })
        ));
        assert!(matches!(
            lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,),
            Err(ErrorResponse {
                code: ErrorCode::Internal,
                retryable: false,
                ..
            })
        ));
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        assert_eq!(
            lookup_register_receipt(&broker.controller(), operation_id, subject, conversation,)
                .unwrap(),
            LookupRegisterRootReceiptResult {
                operation_id,
                receipt: RegisterRootReceipt::Pending,
            }
        );
        let completed = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            operation_id,
        );
        assert_eq!(completed.root.display_name, "Documents");
    }

    #[test]
    fn attachment_publication_failures_recover_from_the_durable_boundary() {
        let (temp, broker, path, state_dir) = durable_setup();
        let project_id = Uuid::new_v4();
        let registered_conversation = Uuid::new_v4();
        let attached_conversation = Uuid::new_v4();
        let subject = GrantSubject::project(project_id).unwrap();
        let root_id = register(
            &broker.controller(),
            subject,
            registered_conversation,
            path,
            OperationId::new(),
        )
        .root
        .root_id;

        let unpublished_id = OperationId::new();
        broker
            .shared
            .state_file
            .as_ref()
            .unwrap()
            .fail_after_saves(0);
        assert!(matches!(
            mutate_attachment(
                &broker.controller(),
                unpublished_id,
                subject,
                attached_conversation,
                root_id,
                RootAttachmentMutationKind::Attach,
            ),
            Err(ErrorResponse {
                code: ErrorCode::HostIo,
                ..
            })
        ));
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        assert_eq!(
            lookup_attachment_receipt(
                &broker.controller(),
                LookupRootAttachmentReceiptRequest {
                    operation_id: unpublished_id,
                    subject,
                    conversation_id: attached_conversation,
                    root_id,
                    mutation: RootAttachmentMutationKind::Attach,
                },
            )
            .unwrap(),
            RootAttachmentMutationReceipt::Unknown
        );
        let context = ExecutionContext::project_chat(attached_conversation, project_id).unwrap();
        assert!(matches!(
            operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots.is_empty()
        ));

        let published_id = OperationId::new();
        broker
            .shared
            .state_file
            .as_ref()
            .unwrap()
            .fail_once_after_publish();
        assert!(matches!(
            mutate_attachment(
                &broker.controller(),
                published_id,
                subject,
                attached_conversation,
                root_id,
                RootAttachmentMutationKind::Attach,
            ),
            Err(ErrorResponse {
                code: ErrorCode::Internal,
                retryable: false,
                ..
            })
        ));
        assert!(matches!(
            lookup_attachment_receipt(
                &broker.controller(),
                LookupRootAttachmentReceiptRequest {
                    operation_id: published_id,
                    subject,
                    conversation_id: attached_conversation,
                    root_id,
                    mutation: RootAttachmentMutationKind::Attach,
                },
            ),
            Err(ErrorResponse {
                code: ErrorCode::Internal,
                retryable: false,
                ..
            })
        ));
        drop(broker);

        let broker = Broker::open(test_policy(&temp), &state_dir).unwrap();
        let receipt = lookup_attachment_receipt(
            &broker.controller(),
            LookupRootAttachmentReceiptRequest {
                operation_id: published_id,
                subject,
                conversation_id: attached_conversation,
                root_id,
                mutation: RootAttachmentMutationKind::Attach,
            },
        )
        .unwrap();
        assert!(matches!(
            receipt,
            RootAttachmentMutationReceipt::Completed {
                result: RootAttachmentMutationResult { changed: true, .. },
                currently_attached: true,
            }
        ));
        assert!(
            mutate_attachment(
                &broker.controller(),
                published_id,
                subject,
                attached_conversation,
                root_id,
                RootAttachmentMutationKind::Attach,
            )
            .unwrap()
            .changed
        );
        assert!(matches!(
            operate(&broker.operator(), context, OperationRequest::ListRoots).unwrap(),
            OperationResult::ListRoots { roots } if roots.len() == 1
        ));
    }

    #[test]
    fn one_process_exclusively_owns_a_durable_broker_directory() {
        let (temp, broker, _path, state_dir) = durable_setup();
        assert!(matches!(
            Broker::open(test_policy(&temp), &state_dir),
            Err(BrokerError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock
        ));
        drop(broker);
        assert!(Broker::open(test_policy(&temp), &state_dir).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn durable_state_uses_private_directory_and_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, broker, path, state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path,
            OperationId::new(),
        );
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(state_dir.join("host-broker-state.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn restart_revalidates_every_persisted_root_before_advertising_state() {
        let (temp, broker, path, state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path.clone(),
            OperationId::new(),
        );
        drop(broker);
        std::fs::remove_file(path.join("note.txt")).unwrap();
        std::fs::remove_dir(path).unwrap();

        assert!(matches!(
            Broker::open(test_policy(&temp), &state_dir),
            Err(BrokerError::RootPolicy(_))
        ));
    }

    #[test]
    fn restart_refuses_to_rebind_a_grant_to_a_replaced_folder() {
        let (temp, broker, path, state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path.clone(),
            OperationId::new(),
        );
        drop(broker);
        let original = path.with_file_name("Documents-original");
        std::fs::rename(&path, original).unwrap();
        std::fs::create_dir(&path).unwrap();

        assert!(matches!(
            Broker::open(test_policy(&temp), &state_dir),
            Err(BrokerError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
        ));
    }

    #[test]
    fn persisted_receipts_must_match_authoritative_state() {
        let (_temp, broker, path, _state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        let subject = GrantSubject::conversation(conversation).unwrap();
        let registered = register(
            &broker.controller(),
            subject,
            conversation,
            path,
            OperationId::new(),
        );
        let state = broker.shared.state.lock().unwrap().clone();

        let mut inconsistent_revoke = state.clone();
        inconsistent_revoke.mutations.insert(
            OperationId::new(),
            MutationRecord::Revoke {
                request: RevokeFingerprint {
                    subject,
                    root_id: registered.root.root_id,
                },
                outcome: MutationOutcome::Complete(Ok(RevokeRootResult { revoked: true })),
            },
        );
        assert!(state_file::validate_loaded_state(&inconsistent_revoke).is_err());

        let mut inconsistent_register = state;
        let record = inconsistent_register
            .mutations
            .values_mut()
            .find(|record| matches!(record, MutationRecord::Register { .. }))
            .unwrap();
        let MutationRecord::Register {
            outcome: MutationOutcome::Complete(Ok(result)),
            ..
        } = record
        else {
            panic!("expected successful registration receipt")
        };
        result.root.root_id = RootId::new();
        assert!(state_file::validate_loaded_state(&inconsistent_register).is_err());
    }

    #[test]
    fn persisted_attachments_must_be_unique_and_respect_conversation_ownership() {
        let (_temp, broker, path, _state_dir) = durable_setup();
        let conversation = Uuid::new_v4();
        register(
            &broker.controller(),
            GrantSubject::conversation(conversation).unwrap(),
            conversation,
            path,
            OperationId::new(),
        );
        let state = broker.shared.state.lock().unwrap().clone();

        let mut duplicate = state.clone();
        duplicate.attachments.push(duplicate.attachments[0]);
        assert!(state_file::validate_loaded_state(&duplicate).is_err());

        let mut wrong_conversation = state;
        wrong_conversation.attachments[0] =
            RootAttachment::new(Uuid::new_v4(), wrong_conversation.attachments[0].root_id())
                .unwrap();
        assert!(state_file::validate_loaded_state(&wrong_conversation).is_err());

        let mut pending = broker.shared.state.lock().unwrap().clone();
        let root_id = pending.attachments[0].root_id();
        pending.mutations.insert(
            OperationId::new(),
            MutationRecord::Attachment {
                request: AttachmentFingerprint {
                    subject: GrantSubject::conversation(conversation).unwrap(),
                    conversation_id: conversation,
                    root_id,
                    mutation: RootAttachmentMutationKind::Detach,
                },
                outcome: MutationOutcome::Pending,
            },
        );
        assert!(state_file::validate_loaded_state(&pending).is_err());
    }

    #[test]
    fn persisted_attachment_ledger_rejects_unknown_nested_fields() {
        let conversation_id = Uuid::new_v4();
        let record = MutationRecord::Attachment {
            request: AttachmentFingerprint {
                subject: GrantSubject::conversation(conversation_id).unwrap(),
                conversation_id,
                root_id: RootId::new(),
                mutation: RootAttachmentMutationKind::Attach,
            },
            outcome: MutationOutcome::Pending,
        };
        let mut encoded = serde_json::to_value(record).unwrap();
        encoded["Attachment"]["request"]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<MutationRecord>(encoded).is_err());
    }

    #[test]
    fn negative_revoke_receipt_is_valid_when_the_subject_did_not_own_the_root() {
        let mut state = State::default();
        state.mutations.insert(
            OperationId::new(),
            MutationRecord::Revoke {
                request: RevokeFingerprint {
                    subject: GrantSubject::conversation(Uuid::new_v4()).unwrap(),
                    root_id: RootId::new(),
                },
                outcome: MutationOutcome::Complete(Ok(RevokeRootResult { revoked: false })),
            },
        );
        assert!(state_file::validate_loaded_state(&state).is_ok());
    }

    #[test]
    fn transient_root_open_failures_remain_retryable() {
        let error = BrokerError::RootPolicy(RootPolicyError::Io(io::Error::from(
            io::ErrorKind::WouldBlock,
        )));
        assert!(retryable_registration_error(&error));
        let response = error_response(error);
        assert_eq!(response.code, ErrorCode::HostIo);
        assert!(response.retryable);
    }

    #[test]
    fn restart_rejects_an_oversized_state_file_before_parsing_it() {
        let (temp, broker, _path, state_dir) = durable_setup();
        drop(broker);
        std::fs::write(
            state_dir.join("host-broker-state.json"),
            vec![b' '; state_file::MAX_STATE_FILE_BYTES + 1],
        )
        .unwrap();

        assert!(matches!(
            Broker::open(test_policy(&temp), &state_dir),
            Err(BrokerError::StateTooLarge)
        ));
    }
}

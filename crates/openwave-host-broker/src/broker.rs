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
            ControlRequest::Hello | ControlRequest::ListApprovedRoots => None,
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
            ControlRequest::ListApprovedRoots => {
                let state = self.lock_state().map_err(error_response)?;
                list_approved_roots(&state).map(|roots| ControlResult::ListApprovedRoots { roots })
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
        let revoked = owned && !has_physical_root_alias(&next, request.root_id);
        if revoked {
            next.roots.remove(&request.root_id);
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
            state.attachments.len() != before
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
    Ok((OperationResult::ListRoots { roots }, Some(grant_id)))
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
mod tests;

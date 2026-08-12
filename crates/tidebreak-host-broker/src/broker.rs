//! Owning broker for consent mutations and capability-checked filesystem reads.
//!
//! The controller and operator share one registry. Operations take an authorized
//! clone of a pinned root handle under a short lock, perform bounded-result I/O,
//! then reauthorize before releasing bytes. Revocation therefore completes
//! without waiting on host I/O, prevents new operations, and fences results from
//! operations that were already in flight.

use std::{
    collections::{HashMap, HashSet, VecDeque},
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
        AuditActor, AuditError, AuditEvent, AuditLabel, AuditOperation, AuditOutcome, AuditSink,
        AuditTarget, JsonlAuditSink, MemoryAuditSink,
    },
    blocklist::is_blocked_control_bundle,
    computer_use::{
        BackendError, BackendErrorKind, ComputerUseBackend, ControlMeta, HelperBackend,
        UnsupportedBackend,
    },
    consequential::{classify, key_press_needs_confirmation, truncate_label},
    path_policy::RootIdentity,
    protocol::{
        AppFolderWriteRequest, CaptureTargetWire, ControlEnvelope, ControlRequest,
        ControlResponseEnvelope, ControlResult, CuCaptureScreenResult,
        CuConfirmControlActionRequest, CuGrantAppRequest, CuGrantAppResult,
        CuNeedsConfirmationResult, CuResolveHandoffRequest, CuResolveHandoffResult,
        CuRevokeAppRequest, DirectoryEntry, ElementTargetWire, EntryKind, ErrorCode, ErrorResponse,
        GrantRootCapabilityRequest, GrantRootCapabilityResult, GrantStatementSummary, HelloResult,
        LookupRegisterRootReceiptRequest, LookupRegisterRootReceiptResult,
        LookupRootAttachmentReceiptRequest, LookupRootAttachmentReceiptResult, OperationEnvelope,
        OperationRequest, OperationResponseEnvelope, OperationResult, PathRequest,
        PurgeConversationSubjectRequest, PurgeConversationSubjectResult, ReadFileBinaryResult,
        ReadFileResult, RegisterRootReceipt, RegisterRootRequest, RegisterRootResult,
        ResolveExecRootsRequest, ResolvedExecRoot, Response, ResponseEnvelope, RevokeGrantRequest,
        RevokeGrantResult, RevokeRootRequest, RevokeRootResult, RootAccess,
        RootAttachmentMutationKind, RootAttachmentMutationReceipt, RootAttachmentMutationRequest,
        RootAttachmentMutationResult, RootSummary, UnavailableRootReason, UnavailableRootSummary,
        WriteFileMode, WriteFileRequest, WriteFileResult, MAX_HANDOFF_BYTES,
        MAX_READ_FILE_BINARY_BYTES, PROTOCOL_VERSION,
    },
    set_of_marks::extract_marks,
    Capability, CaptureTarget, ConsentMethod, ConsentRecord, Consequence, ControlOp, ElementTarget,
    ExecutionContext, Grant, GrantError, GrantId, GrantSubject, OperationId, RelativePath,
    RequestId, RootAttachment, RootId, RootPolicy, RootPolicyError, Scope, SubjectKind,
    ValidatedRoot,
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

/// Upper clamp for a `cu_wait` request. The pause is a broker-side sleep on
/// the synchronous dispatch loop, so an unbounded one would wedge the whole
/// sidecar; ten seconds covers "wait for the sheet to animate" without
/// letting a runaway value stall every other request.
const MAX_CU_WAIT_SECONDS: f64 = 10.0;
/// Upper clamp on `cu_read_app_content` depth / node budgets. The helper
/// applies its own bounds; these keep a wild request from reaching it.
const MAX_CU_AX_DEPTH: u32 = 16;
const MAX_CU_AX_NODES: u32 = 4_096;
/// Largest `cu_type_text` payload. Well beyond any realistic field entry.
const MAX_CU_TYPE_TEXT_BYTES: usize = 8 * 1024;
/// Bound on the `cu_key_press` vocabulary reaching the helper.
const MAX_CU_KEY_BYTES: usize = 64;
const MAX_CU_MODIFIERS: usize = 8;
/// Cap on Set-of-Marks badges extracted for one annotated capture.
const MAX_CAPTURE_MARKS: usize = 100;
/// Pending consequential-action confirmations retained at once, oldest
/// evicted first. Each is single-use and small; the cap only bounds a client
/// that never confirms.
const MAX_PENDING_CONFIRMATIONS: usize = 64;
/// Staged captures awaiting redemption, oldest evicted (and their staging
/// files deleted) first.
const MAX_PENDING_HANDOFFS: usize = 32;
/// Subdirectory of the broker's durable data directory used for capture
/// staging, or a unique owner-only directory under the system temp dir for an
/// ephemeral broker.
const CU_STAGING_DIR_NAME: &str = "cu-staging";

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
    #[error("this application is on the computer-use blocklist")]
    BlockedApp,
    #[error("computer-use backend failed: {}", .0.message)]
    ComputerUse(BackendError),
    #[error("no held control action matches this confirmation identity")]
    UnknownConfirmation,
    #[error("the target element changed after confirmation")]
    StaleTarget,
    #[error("computer-use request parameters are invalid")]
    InvalidCuRequest,
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
    execute_commands: bool,
    state: Mutex<State>,
    state_file: Option<StateFile>,
    audit: Arc<dyn AuditSink>,
    failed_closed: AtomicBool,
    /// The computer-use native backend. Policy lives here in the broker; the
    /// backend only performs an already-authorized op.
    computer_use: Arc<dyn ComputerUseBackend>,
    /// Owner-only directory staged captures are written to before the trusted
    /// desktop redeems them. `None` only when the host would not provide one;
    /// capture then refuses as a host I/O failure.
    cu_staging: Option<PathBuf>,
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
            eprintln!("tidebreak host broker could not persist audit event: {error}");
        }
    }

    /// Drop a one-shot grant after the operation it authorized finished.
    /// Missing or standing grants are no-ops. Persistence failure is
    /// reported but must not rewrite the operation's own result: the host
    /// effect already happened.
    fn consume_single_use_grant(&self, grant_id: Option<GrantId>) -> Result<(), BrokerError> {
        let Some(grant_id) = grant_id else {
            return Ok(());
        };
        if self.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        let mut current = self.state.lock().map_err(|_| BrokerError::StatePoisoned)?;
        if self.failed_closed.load(Ordering::SeqCst) {
            return Err(BrokerError::PersistenceAmbiguous);
        }
        let matches_shot = current
            .grants
            .iter()
            .any(|grant| grant.id() == grant_id && grant.is_single_use());
        if !matches_shot {
            return Ok(());
        }
        let mut next = current.clone();
        next.grants.retain(|grant| grant.id() != grant_id);
        if let Some(state_file) = &self.state_file {
            if let Err(error) = state_file.save(&next) {
                if matches!(error, BrokerError::PersistenceAmbiguous) {
                    self.failed_closed.store(true, Ordering::SeqCst);
                }
                return Err(error);
            }
        }
        *current = next;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct State {
    roots: HashMap<RootId, RegisteredRoot>,
    grants: Vec<Grant>,
    attachments: Vec<RootAttachment>,
    mutations: HashMap<OperationId, MutationRecord>,
    /// Completed attachment and write records in the order they finished,
    /// oldest first. This is what makes the mutation table bounded: it names
    /// which records may be dropped, and which of them is the least useful one
    /// to drop next. See [`prune_mutation_receipts`].
    receipt_order: VecDeque<OperationId>,
    active_mutations: HashSet<OperationId>,
    /// Persisted roots whose directory could not be reopened at load. They are
    /// held out of the live tables so the rest of the registry still works, and
    /// written back verbatim so the approval survives the outage.
    unavailable: Vec<UnavailableRoot>,
    /// Subject/folder pairs whose access the user has already settled once.
    ///
    /// Revocation deletes rows, so the grant table cannot tell "narrowed to
    /// nothing" apart from "never granted" — and every other trace is keyed on
    /// the wrong entity. Attachments are per conversation while grants are per
    /// subject, so a chat's own attachment cannot answer for a project subject
    /// its siblings share, and detaching removes even that. This is the one
    /// record kept on the same key the grants use, and it only ever answers
    /// one question: has this subject been given this folder's default access
    /// before? Once it has, access is the user's to widen through the
    /// permission dialog, never something an arrival re-mints.
    ///
    /// It is dropped when the folder's registration is revoked or the
    /// conversation subject is purged — the points where the position itself
    /// stops existing.
    settled: HashSet<(GrantSubject, RootId)>,
    /// Consequential control actions held for explicit user confirmation,
    /// keyed by their single-use confirmation identity. Session-only: a
    /// pending confirmation never survives a restart (the desktop re-issues
    /// the action instead), so this table is deliberately not persisted.
    pending_confirmations: HashMap<Uuid, PendingControlAction>,
    /// Insertion order of `pending_confirmations`, for bounded eviction.
    confirmation_order: VecDeque<Uuid>,
    /// Captures staged on disk awaiting trusted-desktop redemption, keyed by
    /// handoff identity. Session-only like the confirmations: the staged file
    /// is cleared on broker startup, so an identity from a previous run
    /// resolves to `NotFound` rather than stale pixels.
    handoffs: HashMap<Uuid, StagedCapture>,
    /// Insertion order of `handoffs`, for bounded eviction.
    handoff_order: VecDeque<Uuid>,
}

/// A consequential control action the consequential gate held back, waiting
/// on the user's explicit confirmation. The broker owns the parameters: the
/// agent cannot substitute a different target or text at confirm time, only
/// redeem the exact held action (or let it lapse).
#[derive(Clone)]
struct PendingControlAction {
    /// Execution authority the original request arrived with; re-authorized
    /// against the live grants at act time.
    context: ExecutionContext,
    action: PendingActionKind,
    /// The bounded label shown in the confirmation. The held action is
    /// honored only while the live element still reports this label — a UI
    /// that changed under the prompt voids it.
    expected_label: Option<String>,
    /// The element's fingerprint read when the confirmation was raised. The
    /// act-time re-check compares it to the live fingerprint, so an element
    /// swapped out for another with the same (possibly truncated) label still
    /// voids the confirmation. Absent for an action with no element.
    expected_fingerprint: Option<String>,
}

#[derive(Clone)]
enum PendingActionKind {
    Click {
        bundle_id: String,
        target: ElementTarget,
        button: Option<String>,
        click_count: Option<u32>,
    },
    TypeText {
        bundle_id: String,
        text: String,
        target: ElementTarget,
    },
    /// A key press held for confirmation — a chord or a bare Return, the
    /// keyboard paths that commit (send / delete / quit) without any element
    /// label the consequential classifier could read.
    KeyPress {
        bundle_id: String,
        key: String,
        modifiers: Option<Vec<String>>,
    },
}

/// A capture written into the staging directory, awaiting one redemption.
#[derive(Clone)]
struct StagedCapture {
    width: u32,
    height: u32,
    media_type: String,
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

/// The transport reason vocabulary, derived here where the policy error is
/// observed. The causes are recorded rather than acted on — see the enum's
/// own docs in [`crate::protocol`].
fn unavailable_reason(error: &RootPolicyError) -> UnavailableRootReason {
    match error {
        RootPolicyError::Io(error) => match error.kind() {
            io::ErrorKind::NotFound => UnavailableRootReason::Missing,
            io::ErrorKind::PermissionDenied => UnavailableRootReason::PermissionDenied,
            _ => UnavailableRootReason::HostIo,
        },
        _ => UnavailableRootReason::Rejected,
    }
}

const fn unavailable_error_code(reason: UnavailableRootReason) -> ErrorCode {
    match reason {
        UnavailableRootReason::Missing => ErrorCode::NotFound,
        UnavailableRootReason::PermissionDenied => ErrorCode::Denied,
        UnavailableRootReason::HostIo => ErrorCode::HostIo,
        UnavailableRootReason::Rejected | UnavailableRootReason::Replaced => ErrorCode::InvalidRoot,
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
    grants: Vec<Grant>,
    attachment: RootAttachment,
}

struct ControlAudit {
    actor: AuditActor,
    operation: AuditOperation,
    operation_id: Option<OperationId>,
    target: AuditTarget,
    /// Grant this request names directly, for single-grant revocation.
    grant_id: Option<GrantId>,
    /// This request can change consent or host state, so its record must be
    /// durable before it runs.
    mutates: bool,
}

impl ControlAudit {
    fn from_request(request: &ControlRequest) -> Option<Self> {
        match request {
            // None of these reaches user data or changes anything. `Hello` is
            // a version handshake against a constant, and the two listings
            // project state the trusted control surface already knows about —
            // approved folders and standing grants — for the management UI.
            // Recording them would add volume to a bounded, rotating log
            // without adding anything a reader could act on, which costs the
            // records that matter their retention.
            ControlRequest::Hello
            | ControlRequest::ListApprovedRoots
            | ControlRequest::ListGrantStatements
            | ControlRequest::ListUnavailableRoots => None,
            // The app-folder trio is consented server-side — the app grant
            // names the folder and access level, and dispatch is admitted
            // there before the broker is reached — but the broker's own
            // ledger still sees the I/O, attributed to the app actor the
            // request names. No capability or grant id applies: the broker
            // holds no grant row for an app, so implying one would overstate
            // what this record proves.
            ControlRequest::ListAppFolder(request) => Some(Self {
                actor: AuditActor::App {
                    app_id: request.app_id,
                },
                mutates: false,
                operation: AuditOperation::ListAppFolder,
                grant_id: None,
                operation_id: None,
                target: AuditTarget::path(request.root_id, &request.path),
            }),
            ControlRequest::ReadAppFolderFile(request) => Some(Self {
                actor: AuditActor::App {
                    app_id: request.app_id,
                },
                mutates: false,
                operation: AuditOperation::ReadAppFolderFile,
                grant_id: None,
                operation_id: None,
                target: AuditTarget::path(request.root_id, &request.path),
            }),
            // A host mutation: like the agent write operation, the intent
            // record must be durable before any bytes change. No operation
            // id — the digest-bound write reconciles a same-content retry by
            // itself, so there is no separate mutation identity to correlate.
            ControlRequest::WriteAppFolderFile(request) => Some(Self {
                actor: AuditActor::App {
                    app_id: request.app_id,
                },
                mutates: true,
                operation: AuditOperation::WriteAppFolderFile,
                grant_id: None,
                operation_id: None,
                target: AuditTarget::path(request.root_id, &request.path),
            }),
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
                grant_id: None,
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
                grant_id: None,
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
                grant_id: None,
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
                grant_id: None,
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
                grant_id: None,
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
                grant_id: None,
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
                grant_id: None,
                operation_id: Some(request.operation_id),
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            ControlRequest::RevokeGrant(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: None,
                },
                mutates: true,
                operation: AuditOperation::RevokeGrant,
                grant_id: Some(request.grant_id),
                // Naturally idempotent: the grant id names the exact row, so
                // there is no separate mutation identity to correlate.
                operation_id: None,
                target: AuditTarget::Subject,
            }),
            ControlRequest::PurgeConversationSubject(request) => {
                let Ok(subject) = GrantSubject::conversation(request.conversation_id) else {
                    // Nil subjects are rejected in execute; skip audit construction
                    // rather than panicking on a malformed trusted request.
                    return None;
                };
                Some(Self {
                    actor: AuditActor::Control {
                        subject,
                        conversation_id: Some(request.conversation_id),
                    },
                    mutates: true,
                    operation: AuditOperation::PurgeConversationSubject,
                    grant_id: None,
                    operation_id: None,
                    target: AuditTarget::Subject,
                })
            }
            ControlRequest::GrantRootCapability(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: Some(request.conversation_id),
                },
                mutates: true,
                operation: AuditOperation::GrantRootCapability,
                grant_id: None,
                // Naturally idempotent: an equivalent live grant makes a retry
                // a no-op, so there is no separate mutation identity.
                operation_id: None,
                target: AuditTarget::Root {
                    root_id: request.root_id,
                },
            }),
            // Permission preflight / request and the grants listing expose
            // state the trusted surface already holds; like the other
            // management listings they are not recorded. Handoff redemption
            // returns pixels the capture op already recorded staging, and
            // confirming a held action records its own intent + completion
            // pair inside the handler — the pending entry, not the request,
            // carries the actor and target.
            ControlRequest::CuPermissionStatus
            | ControlRequest::CuRequestPermissions
            | ControlRequest::CuListAppGrants(_)
            | ControlRequest::CuResolveHandoff(_)
            | ControlRequest::CuConfirmControlAction(_) => None,
            // Consent mutations: durable intent before the grant store
            // changes, exactly like the folder grant mutations.
            ControlRequest::CuGrantApp(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: None,
                },
                mutates: true,
                operation: AuditOperation::CuGrantApp,
                grant_id: None,
                operation_id: None,
                target: cu_scope_audit_target(request.bundle_id.as_deref()),
            }),
            ControlRequest::CuRevokeApp(request) => Some(Self {
                actor: AuditActor::Control {
                    subject: request.subject,
                    conversation_id: None,
                },
                mutates: true,
                operation: AuditOperation::CuRevokeApp,
                grant_id: None,
                // Naturally idempotent: the (subject, capability, scope) tuple
                // names the exact rows, so there is no separate mutation
                // identity to correlate.
                operation_id: None,
                target: cu_scope_audit_target(request.bundle_id.as_deref()),
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
            grant_id: self.grant_id,
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
        // Input synthesis (click / type / key / scroll / focus) is a host
        // mutation: its intent record must be durable before any event is
        // synthesized, and the op refuses when the record cannot be made
        // durable. Scroll warps the cursor; focus raises a window.
        let mutates = matches!(
            envelope.request,
            OperationRequest::WriteFile(_)
                | OperationRequest::CuClick { .. }
                | OperationRequest::CuTypeText { .. }
                | OperationRequest::CuKeyPress { .. }
                | OperationRequest::CuScroll { .. }
                | OperationRequest::CuFocusWindow { .. }
        );
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
            OperationRequest::CuListWindows { bundle_id } => (
                AuditOperation::CuListWindows,
                cu_list_windows_capability(bundle_id.as_deref()),
                cu_scope_audit_target(bundle_id.as_deref()),
            ),
            OperationRequest::CuCaptureScreen { target } => (
                AuditOperation::CuCaptureScreen,
                Capability::CaptureScreen,
                match target {
                    CaptureTargetWire::App { bundle_id } => AuditTarget::app(bundle_id),
                    CaptureTargetWire::Display { .. } => AuditTarget::Screen,
                },
            ),
            OperationRequest::CuReadAppContent { bundle_id, .. } => (
                AuditOperation::CuReadAppContent,
                Capability::ReadAppContent,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuClick { bundle_id, .. } => (
                AuditOperation::CuClick,
                Capability::ControlApp,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuTypeText { bundle_id, .. } => (
                AuditOperation::CuTypeText,
                Capability::ControlApp,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuKeyPress { bundle_id, .. } => (
                AuditOperation::CuKeyPress,
                Capability::ControlApp,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuScroll { bundle_id, .. } => (
                AuditOperation::CuScroll,
                Capability::ReadAppContent,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuFocusWindow { bundle_id, .. } => (
                AuditOperation::CuFocusWindow,
                Capability::ReadAppContent,
                AuditTarget::app(bundle_id),
            ),
            OperationRequest::CuWait { .. } => (
                AuditOperation::CuWait,
                Capability::ListRoots,
                AuditTarget::Subject,
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

    /// Completion record paired with [`OperationAudit::intent`], for paths
    /// that record both inside one handler (the consequential-action confirm
    /// path). `error` is the transport error the attempt ended in, if any.
    fn completion(&self, request_id: RequestId, error: Option<&ErrorResponse>) -> AuditEvent {
        AuditEvent {
            event_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            request_id,
            operation_id: None,
            actor: self.actor,
            operation: self.operation,
            target: self.target.clone(),
            outcome: audit_outcome(error),
            capability: Some(self.capability),
            grant_id: None,
            error_code: error.map(|error| error.code),
            item_count: None,
            bytes: None,
        }
    }
}

impl Broker {
    /// Create an empty broker using the reviewed host-root policy.
    pub fn new(policy: RootPolicy) -> Self {
        Self::new_with_execute_commands(policy, true)
    }

    /// Create an empty broker with the host's local command reach declared.
    pub fn new_with_execute_commands(policy: RootPolicy, execute_commands: bool) -> Self {
        Self::with_components(
            policy,
            execute_commands,
            Arc::new(MemoryAuditSink::new()),
            default_computer_use_backend(),
            ephemeral_cu_staging(),
        )
    }

    /// Create an ephemeral broker with an embedder-provided audit sink.
    pub fn with_audit_sink(policy: RootPolicy, audit: Arc<dyn AuditSink>) -> Self {
        Self::with_components(
            policy,
            true,
            audit,
            default_computer_use_backend(),
            ephemeral_cu_staging(),
        )
    }

    fn with_components(
        policy: RootPolicy,
        execute_commands: bool,
        audit: Arc<dyn AuditSink>,
        computer_use: Arc<dyn ComputerUseBackend>,
        cu_staging: Option<PathBuf>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                policy,
                execute_commands,
                state: Mutex::new(State::default()),
                state_file: None,
                audit,
                failed_closed: AtomicBool::new(false),
                computer_use,
                cu_staging,
            }),
        }
    }

    /// Test hook: an ephemeral broker with an injected computer-use backend
    /// (a stub, never the real helper) and a staging dir under the test's
    /// own tempdir.
    #[cfg(test)]
    pub(crate) fn test_with_computer_use(
        policy: RootPolicy,
        audit: Arc<dyn AuditSink>,
        backend: Arc<dyn ComputerUseBackend>,
        staging: PathBuf,
    ) -> Self {
        Self::with_components(policy, true, audit, backend, prepare_cu_staging(staging))
    }

    /// Open durable broker state under the application-private data directory.
    /// Persisted roots are revalidated and descriptor-pinned before this
    /// constructor returns, so stale state is never advertised.
    pub fn open(policy: RootPolicy, data_dir: &Path) -> Result<Self, BrokerError> {
        Self::open_with_execute_commands(policy, data_dir, true)
    }

    /// Open durable broker state with the host's local command reach declared.
    pub fn open_with_execute_commands(
        policy: RootPolicy,
        data_dir: &Path,
        execute_commands: bool,
    ) -> Result<Self, BrokerError> {
        let state_file = StateFile::open(data_dir)?;
        let state = state_file.load(&policy, execute_commands)?;
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
                execute_commands,
                state: Mutex::new(state),
                state_file: Some(state_file),
                audit,
                failed_closed: AtomicBool::new(false),
                computer_use: default_computer_use_backend(),
                cu_staging: open_cu_staging(data_dir),
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
            grant_id: metadata.grant_id,
            error_code: error.map(|error| error.code),
            item_count: None,
            bytes: None,
        };
        self.shared.record_completion(&event);
    }

    fn execute(&self, envelope: ControlEnvelope) -> Result<ControlResult, ErrorResponse> {
        if matches!(envelope.request, ControlRequest::Hello) {
            return Ok(ControlResult::Hello(self.hello()));
        }
        require_version(envelope.protocol_version).map_err(error_response)?;
        match envelope.request {
            ControlRequest::Hello => Ok(ControlResult::Hello(self.hello())),
            ControlRequest::ListApprovedRoots => {
                let state = self.lock_state().map_err(error_response)?;
                list_approved_roots(&state).map(|roots| ControlResult::ListApprovedRoots { roots })
            }
            ControlRequest::ListGrantStatements => {
                let state = self.lock_state().map_err(error_response)?;
                list_grant_statements(&state)
                    .map(|grants| ControlResult::ListGrantStatements { grants })
            }
            ControlRequest::ListUnavailableRoots => {
                let state = self.lock_state().map_err(error_response)?;
                list_unavailable_roots(&state)
                    .map(|roots| ControlResult::ListUnavailableRoots { roots })
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
            ControlRequest::RevokeGrant(request) => {
                self.revoke_grant(request).map(ControlResult::RevokeGrant)
            }
            ControlRequest::PurgeConversationSubject(request) => self
                .purge_conversation_subject(request)
                .map(ControlResult::PurgeConversationSubject),
            ControlRequest::GrantRootCapability(request) => self
                .grant_root_capability(request)
                .map(ControlResult::GrantRootCapability),
            ControlRequest::ListAppFolder(request) => {
                let root = {
                    let state = self.lock_state().map_err(error_response)?;
                    app_folder_root(&state, request.root_id).map_err(error_response)?
                };
                match list_directory(&root, &request.path).map_err(error_response)? {
                    OperationResult::ListDirectory { entries } => {
                        Ok(ControlResult::ListAppFolder { entries })
                    }
                    _ => Err(error_response(BrokerError::Denied)),
                }
            }
            ControlRequest::ReadAppFolderFile(request) => {
                let root = {
                    let state = self.lock_state().map_err(error_response)?;
                    app_folder_root(&state, request.root_id).map_err(error_response)?
                };
                match read_file_binary(&root, &request.path).map_err(error_response)? {
                    OperationResult::ReadFileBinary(result) => {
                        Ok(ControlResult::ReadAppFolderFile(result))
                    }
                    _ => Err(error_response(BrokerError::Denied)),
                }
            }
            ControlRequest::WriteAppFolderFile(request) => {
                let root = {
                    let state = self.lock_state().map_err(error_response)?;
                    app_folder_root(&state, request.root_id).map_err(error_response)?
                };
                write_app_folder_file(&root, request).map_err(error_response)
            }
            ControlRequest::CuPermissionStatus => self
                .shared
                .computer_use
                .permission_status()
                .map(ControlResult::CuPermissionStatus)
                .map_err(|error| error_response(BrokerError::ComputerUse(error))),
            ControlRequest::CuRequestPermissions => self
                .shared
                .computer_use
                .request_permissions()
                .map(ControlResult::CuRequestPermissions)
                .map_err(|error| error_response(BrokerError::ComputerUse(error))),
            ControlRequest::CuGrantApp(request) => self
                .cu_grant_app(request)
                .map(ControlResult::CuGrantApp)
                .map_err(error_response),
            ControlRequest::CuRevokeApp(request) => self
                .cu_revoke_app(request)
                .map(|revoked| ControlResult::CuRevokeApp { revoked })
                .map_err(error_response),
            ControlRequest::CuListAppGrants(request) => {
                let state = self.lock_state().map_err(error_response)?;
                Ok(ControlResult::CuListAppGrants {
                    grants: list_cu_app_grants(&state, request.subject),
                })
            }
            ControlRequest::CuResolveHandoff(request) => self
                .cu_resolve_handoff(request)
                .map(ControlResult::CuResolveHandoff)
                .map_err(error_response),
            ControlRequest::CuConfirmControlAction(request) => self
                .cu_confirm_control_action(envelope.request_id, request)
                .map(ControlResult::CuConfirmControlAction),
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
                    // Picking a folder this conversation already has is the
                    // same non-event as attaching one it already has: it says
                    // where the folder may be used, and the chat is already
                    // there. Only the pick that actually brings the folder into
                    // a conversation carries the access a pick describes — and
                    // then only for a subject that holds nothing over the root,
                    // which is what keeps a narrowed position narrowed.
                    if !has_root_attachment(&next, prepared.conversation_id, root_id) {
                        ensure_default_subject_grants(
                            &mut next,
                            prepared.root.owner,
                            root_id,
                            prepared.grants[0].consent().clone(),
                            self.shared.execute_commands,
                        )
                        .map_err(error_response)?;
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
                    // A first registration is the first mint for this subject,
                    // so it settles the position too. Without this, revoking
                    // the whole folder and picking it again would count as a
                    // first arrival and hand the access back.
                    next.settled.insert((fingerprint.subject, prepared.root_id));
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
            consent.clone(),
        )?;
        // Choosing a folder is how the user says the agent may work in it, so
        // exec reach is part of that consent rather than a second prompt. The
        // capability exists so it can be named, audited, and revoked on its own
        // afterwards, not to add a step here.
        let mut grants = vec![list_grant, read_grant, write_grant];
        if self.shared.execute_commands {
            grants.push(Grant::from_consent(
                GrantId::new(),
                request.subject,
                Capability::ExecuteCommands,
                Scope::Root { root_id },
                consent,
            )?);
        }
        Ok(PreparedRegistration {
            conversation_id: request.conversation_id,
            root_id,
            root: RegisteredRoot {
                owner: request.subject,
                display_name,
                root: Arc::new(validated),
            },
            grants,
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

        let result = apply_root_attachment(&mut next, fingerprint, self.shared.execute_commands)
            .map_err(error_response);
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
            // The approval itself is gone, so there is no position left to
            // remember. Picking this folder again is a first arrival.
            next.settled.retain(|(_, root)| *root != request.root_id);
        }
        let result = Ok(RevokeRootResult { revoked });
        complete_revoke(&mut next, operation_id, fingerprint, result.clone())
            .map_err(error_response)?;
        self.commit_state(&mut state, next)
            .map_err(error_response)?;
        result
    }

    /// Withdraw one capability grant, live or dormant.
    ///
    /// Deriving the boundary from statements means the rows `authorize()`
    /// consults are exactly the rows the consent surface shows, so a
    /// statement-level "revoke" is a plain removal of that row — with one
    /// dependency kept honest: exec reach is only ever additional on top of
    /// read, so revoking a folder's `ReadFiles` takes its `ExecuteCommands`
    /// with it, while revoking exec alone leaves the folder readable.
    fn revoke_grant(
        &self,
        request: RevokeGrantRequest,
    ) -> Result<RevokeGrantResult, ErrorResponse> {
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        let revoked = remove_grant_statement(&mut next.grants, request.subject, request.grant_id)
            || next.unavailable.iter_mut().any(|root| {
                remove_grant_statement(&mut root.grants, request.subject, request.grant_id)
            });
        if revoked {
            self.commit_state(&mut state, next)
                .map_err(error_response)?;
        }
        Ok(RevokeGrantResult { revoked })
    }

    /// Forget every durable host row owned by one conversation.
    ///
    /// Conversation ids are never remapped. After a chat is deleted, grants
    /// and attachments for that subject are leftover authority with no product
    /// subject left to exercise them, so the trusted desktop removes them in
    /// one shot. Roots this conversation registered are revoked only when no
    /// other conversation still holds an attachment — otherwise detach this
    /// conversation and leave the registration for survivors.
    fn purge_conversation_subject(
        &self,
        request: PurgeConversationSubjectRequest,
    ) -> Result<PurgeConversationSubjectResult, ErrorResponse> {
        let subject = match GrantSubject::conversation(request.conversation_id) {
            Ok(subject) => subject,
            Err(_) => {
                return Err(error_response(BrokerError::SubjectConversationMismatch));
            }
        };
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        let mut changed = false;

        let before = next.grants.len();
        next.grants.retain(|grant| grant.subject() != subject);
        changed |= next.grants.len() != before;
        // The conversation subject is gone with the chat, so its settled
        // positions go too. A project subject the chat happened to use is not
        // this subject and keeps its own.
        next.settled.retain(|(settled, _)| *settled != subject);

        let before = next.attachments.len();
        next.attachments
            .retain(|attachment| attachment.conversation_id() != request.conversation_id);
        changed |= next.attachments.len() != before;

        for root in &mut next.unavailable {
            let before = root.grants.len();
            root.grants.retain(|grant| grant.subject() != subject);
            changed |= root.grants.len() != before;

            let before = root.attachments.len();
            root.attachments
                .retain(|attachment| attachment.conversation_id() != request.conversation_id);
            changed |= root.attachments.len() != before;
        }

        // Drop registrations this conversation owns once nothing else is still
        // attached to them. Shared roots keep their registration for survivors.
        let owned_root_ids: Vec<RootId> = next
            .roots
            .iter()
            .filter(|(_, root)| root.owner == subject)
            .map(|(root_id, _)| *root_id)
            .collect();
        for root_id in owned_root_ids {
            let still_attached = next
                .attachments
                .iter()
                .any(|attachment| attachment.root_id() == root_id)
                || next
                    .unavailable
                    .iter()
                    .any(|root| root.id == root_id && !root.attachments.is_empty());
            if still_attached {
                continue;
            }
            if next.roots.remove(&root_id).is_some() {
                changed = true;
                next.grants
                    .retain(|grant| !scope_targets_root(grant.scope(), root_id));
                next.attachments
                    .retain(|attachment| attachment.root_id() != root_id);
                next.settled.retain(|(_, settled)| *settled != root_id);
            }
        }

        let owned_unavailable: Vec<RootId> = next
            .unavailable
            .iter()
            .filter(|root| root.owner == subject && root.attachments.is_empty())
            .map(|root| root.id)
            .collect();
        if !owned_unavailable.is_empty() {
            changed = true;
            next.unavailable
                .retain(|root| !owned_unavailable.contains(&root.id));
            for root_id in owned_unavailable {
                next.grants
                    .retain(|grant| !scope_targets_root(grant.scope(), root_id));
            }
        }

        if changed {
            self.commit_state(&mut state, next)
                .map_err(error_response)?;
        }
        Ok(PurgeConversationSubjectResult { changed })
    }

    /// Widen one attached root by one capability, with fresh consent.
    ///
    /// The mirror of [`Broker::revoke_grant`] at the same statement
    /// granularity, for the folders panel's "allow write to this folder"
    /// affordance. The boundary is deliberately narrow: the root must be live
    /// (a set-aside root's directory cannot be confirmed) and already attached
    /// to the requesting conversation, so this can deepen reach over a folder
    /// the user is looking at but can never introduce a folder or attachment.
    /// `Grant::from_consent` rejects capability/scope vocabulary that does not
    /// describe an operation class, so only the per-root capabilities — read,
    /// write, exec — can be minted here.
    fn grant_root_capability(
        &self,
        request: GrantRootCapabilityRequest,
    ) -> Result<GrantRootCapabilityResult, ErrorResponse> {
        validate_subject_conversation(request.subject, request.conversation_id)
            .map_err(error_response)?;
        // A widening records a consent interaction the desktop actually held.
        // The picker methods describe choosing a folder, not answering a
        // capability question, and `CarriedForward` describes a migration.
        if !matches!(request.consent_method, ConsentMethod::PermissionDialog) {
            return Err(error_response(BrokerError::InvalidConsentMethod));
        }
        let mut state = self.lock_state().map_err(error_response)?;
        let mut next = state.clone();
        if !next.roots.contains_key(&request.root_id) {
            return Err(error_response(BrokerError::UnknownRoot));
        }
        if !has_root_attachment(&next, request.conversation_id, request.root_id) {
            return Err(error_response(BrokerError::Denied));
        }
        if request.capability == Capability::ExecuteCommands && !self.shared.execute_commands {
            return Err(error_response(BrokerError::Denied));
        }
        let already_granted = next.grants.iter().any(|grant| {
            grant.subject() == request.subject
                && grant.capability() == request.capability
                && matches!(*grant.scope(), Scope::Root { root_id } if root_id == request.root_id)
        });
        if already_granted {
            return Ok(GrantRootCapabilityResult { granted: false });
        }
        next.grants.push(
            Grant::from_consent(
                GrantId::new(),
                request.subject,
                request.capability,
                Scope::Root {
                    root_id: request.root_id,
                },
                ConsentRecord::new(request.consent_method, Utc::now()),
            )
            .map_err(BrokerError::from)
            .map_err(error_response)?,
        );
        self.commit_state(&mut state, next)
            .map_err(error_response)?;
        Ok(GrantRootCapabilityResult { granted: true })
    }

    /// Record one computer-use consent decision.
    ///
    /// The decision comes from the desktop's per-app approval card, so only
    /// [`ConsentMethod::PermissionDialog`] is accepted — the same discipline
    /// as [`Controller::grant_root_capability`]. The blocklist outranks
    /// consent: a blocked bundle can never hold a grant, so no dispatch path
    /// has to trust the grant table to be clean.
    fn cu_grant_app(&self, request: CuGrantAppRequest) -> Result<CuGrantAppResult, BrokerError> {
        if !matches!(request.consent, ConsentMethod::PermissionDialog) {
            return Err(BrokerError::InvalidConsentMethod);
        }
        if let Some(bundle_id) = &request.bundle_id {
            validate_bundle_id(bundle_id)?;
            if is_blocked_control_bundle(bundle_id) {
                return Err(BrokerError::BlockedApp);
            }
        }
        let scope = cu_grant_scope(request.capability, request.bundle_id.as_deref())?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let same_tuple = |grant: &Grant| {
            grant.subject() == request.subject
                && grant.capability() == request.capability
                && *grant.scope() == scope
        };
        // A standing grant already covers this tuple. Once is weaker: reuse
        // the standing row rather than stacking a one-shot beside it.
        if let Some(existing) = next
            .grants
            .iter()
            .find(|grant| same_tuple(grant) && !grant.is_single_use())
        {
            return Ok(CuGrantAppResult {
                granted: false,
                grant_id: existing.id(),
            });
        }
        if !request.single_use {
            // Standing consent replaces any leftover one-shot at this tuple.
            next.grants
                .retain(|grant| !(same_tuple(grant) && grant.is_single_use()));
        } else if let Some(existing) = next
            .grants
            .iter()
            .find(|grant| same_tuple(grant) && grant.is_single_use())
        {
            return Ok(CuGrantAppResult {
                granted: false,
                grant_id: existing.id(),
            });
        }
        let grant = Grant::from_consent(
            GrantId::new(),
            request.subject,
            request.capability,
            scope,
            ConsentRecord::new(request.consent, Utc::now()),
        )?;
        let grant = if request.single_use {
            grant.into_single_use()
        } else {
            grant
        };
        let grant_id = grant.id();
        next.grants.push(grant);
        self.commit_state(&mut state, next)?;
        Ok(CuGrantAppResult {
            granted: true,
            grant_id,
        })
    }

    /// Withdraw every grant exactly matching one (subject, capability, scope)
    /// computer-use tuple. Idempotent.
    fn cu_revoke_app(&self, request: CuRevokeAppRequest) -> Result<bool, BrokerError> {
        let scope = cu_grant_scope(request.capability, request.bundle_id.as_deref())?;
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let before = next.grants.len();
        next.grants.retain(|grant| {
            !(grant.subject() == request.subject
                && grant.capability() == request.capability
                && *grant.scope() == scope)
        });
        let revoked = next.grants.len() != before;
        if revoked {
            self.commit_state(&mut state, next)?;
        }
        Ok(revoked)
    }

    /// Redeem one staged capture: return the PNG bytes once, then discard
    /// both the record and the staged file. The single-use property is the
    /// point — a screenshot's pixels should not be re-readable by whatever
    /// later gains the identity.
    fn cu_resolve_handoff(
        &self,
        request: CuResolveHandoffRequest,
    ) -> Result<CuResolveHandoffResult, BrokerError> {
        let staged = {
            let mut state = self.lock_state()?;
            let staged = state.handoffs.remove(&request.handoff_id);
            if staged.is_some() {
                state.handoff_order.retain(|id| *id != request.handoff_id);
            }
            staged
        };
        let Some(staged) = staged else {
            return Err(BrokerError::DestinationMissing);
        };
        let path = self
            .shared
            .cu_staging
            .as_ref()
            .map(|dir| dir.join(handoff_file_name(request.handoff_id)));
        let bytes = match path.as_deref() {
            Some(path) => {
                let bytes = std::fs::read(path).map_err(BrokerError::Io)?;
                // Read-then-delete: a host failure deleting leaves an orphaned
                // file, but the record is already gone, so the bytes still
                // cannot be redeemed twice through the broker.
                let _ = std::fs::remove_file(path);
                bytes
            }
            None => return Err(BrokerError::StatePoisoned),
        };
        if bytes.len() > MAX_HANDOFF_BYTES {
            return Err(BrokerError::FileTooLarge {
                maximum: MAX_HANDOFF_BYTES,
            });
        }
        Ok(CuResolveHandoffResult {
            handoff_id: request.handoff_id,
            width: staged.width,
            height: staged.height,
            media_type: staged.media_type,
            bytes: bytes.len(),
            content_base64: BASE64.encode(bytes),
        })
    }

    /// Perform one held consequential control action after the user's
    /// explicit confirmation.
    ///
    /// The intent/completion pair is recorded here rather than by
    /// [`ControlAudit::from_request`]: the request names only a confirmation
    /// identity, while the held entry carries the actor and target the record
    /// must show. As with the operation surface, a control action that cannot
    /// record its intent does not run.
    fn cu_confirm_control_action(
        &self,
        request_id: RequestId,
        request: CuConfirmControlActionRequest,
    ) -> Result<ControlMeta, ErrorResponse> {
        let pending = (|| -> Result<_, BrokerError> {
            let mut state = self.lock_state()?;
            let pending = state.pending_confirmations.remove(&request.confirmation_id);
            if pending.is_some() {
                state
                    .confirmation_order
                    .retain(|id| *id != request.confirmation_id);
            }
            Ok(pending)
        })()
        .map_err(error_response)?;
        let Some(pending) = pending else {
            return Err(error_response(BrokerError::UnknownConfirmation));
        };
        // A held key press has no target element; an owned empty target keeps
        // the act-time element re-check inert (it only runs when an
        // `element_id` is present).
        let empty_target = ElementTarget::default();
        let (bundle_id, target, op) = match &pending.action {
            PendingActionKind::Click {
                bundle_id, target, ..
            } => (bundle_id, target, ControlOp::Click),
            PendingActionKind::TypeText {
                bundle_id, target, ..
            } => (bundle_id, target, ControlOp::TypeText),
            PendingActionKind::KeyPress { bundle_id, .. } => {
                (bundle_id, &empty_target, ControlOp::KeyPress)
            }
        };
        let audit_operation = match op {
            ControlOp::Click => AuditOperation::CuClick,
            ControlOp::TypeText => AuditOperation::CuTypeText,
            ControlOp::KeyPress => AuditOperation::CuKeyPress,
        };
        let audit = OperationAudit {
            actor: AuditActor::Operation {
                context: pending.context,
            },
            operation: audit_operation,
            capability: Capability::ControlApp,
            target: cu_element_audit_target(bundle_id, pending.expected_label.as_deref()),
            mutates: true,
        };
        self.shared
            .record_intent(&audit.intent(request_id))
            .map_err(|error| error_response(BrokerError::Audit(error)))?;
        let result = self
            .perform_confirmed_action(&pending, bundle_id, target, op)
            .map_err(error_response);
        if let Ok((_, grant_id)) = &result {
            let _ = self.shared.consume_single_use_grant(Some(*grant_id));
        }
        self.shared
            .record_completion(&audit.completion(request_id, result.as_ref().err()));
        result.map(|(meta, _)| meta)
    }

    /// Re-authorize, re-describe, and finally dispatch a confirmed action.
    /// TOCTOU discipline: everything the original request was gated on is
    /// re-checked against live state at act time.
    fn perform_confirmed_action(
        &self,
        pending: &PendingControlAction,
        bundle_id: &str,
        target: &ElementTarget,
        op: ControlOp,
    ) -> Result<(ControlMeta, GrantId), BrokerError> {
        if is_blocked_control_bundle(bundle_id) {
            return Err(BrokerError::BlockedApp);
        }
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, pending.context, Capability::ControlApp, bundle_id)?
        };
        // The confirmation is honored only while the element still
        // reports the label the user approved. A UI that shifted under
        // the prompt — or a hijacked page that swapped the button —
        // refuses as stale, and the agent must restart the flow.
        if target.element_id.is_some() {
            let description = self
                .shared
                .computer_use
                .describe_element(bundle_id, target)
                .map_err(BrokerError::ComputerUse)?;
            let live_label = description.label.as_deref().map(truncate_label);
            if live_label != pending.expected_label {
                return Err(BrokerError::StaleTarget);
            }
            // The stronger drift signal: the element's content fingerprint. A
            // swapped element that kept the same (possibly truncated) label has
            // a different fingerprint, so this catches a same-label swap the
            // label check alone would miss. Only compared when the confirmation
            // recorded one.
            if let Some(expected) = &pending.expected_fingerprint {
                if description.fingerprint.as_ref() != Some(expected) {
                    return Err(BrokerError::StaleTarget);
                }
            }
            // Defense in depth: the held label classified as consequential
            // once; if it somehow classifies benign now, the confirmation was
            // answered for a different question.
            if classify(
                op,
                description.role.as_deref(),
                description.label.as_deref(),
            ) == Consequence::Benign
            {
                return Err(BrokerError::StaleTarget);
            }
        }
        match &pending.action {
            PendingActionKind::Click {
                bundle_id,
                target,
                button,
                click_count,
            } => self
                .shared
                .computer_use
                .click(bundle_id, target, button.as_deref(), *click_count),
            PendingActionKind::TypeText {
                bundle_id,
                text,
                target,
            } => self.shared.computer_use.type_text(bundle_id, text, target),
            PendingActionKind::KeyPress {
                bundle_id,
                key,
                modifiers,
            } => self
                .shared
                .computer_use
                .key_press(bundle_id, key, modifiers.as_deref()),
        }
        .map_err(BrokerError::ComputerUse)
        .map(|meta| (meta, grant_id))
    }

    fn hello(&self) -> HelloResult {
        hello(self.shared.computer_use.is_available())
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
        // A confirmation hold is not terminal: the one-shot must cover the
        // confirm that follows. Any other outcome spends it.
        if !matches!(result, Ok(OperationResult::CuNeedsConfirmation(_))) {
            let _ = self.shared.consume_single_use_grant(grant_id);
        }
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
                    let (result, authorized_by) =
                        list_roots(&state, envelope.context, self.shared.execute_commands)?;
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
                OperationRequest::CuListWindows { bundle_id } => self
                    .cu_list_windows(envelope.context, bundle_id)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuCaptureScreen { target } => self
                    .cu_capture_screen(envelope.context, target)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuReadAppContent {
                    bundle_id,
                    max_depth,
                    max_nodes,
                } => self
                    .cu_read_app_content(envelope.context, bundle_id, max_depth, max_nodes)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuClick {
                    bundle_id,
                    target,
                    button,
                    click_count,
                } => self
                    .cu_click(envelope.context, bundle_id, target, button, click_count)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuTypeText {
                    bundle_id,
                    text,
                    target,
                } => self
                    .cu_type_text(envelope.context, bundle_id, text, target)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuKeyPress {
                    bundle_id,
                    key,
                    modifiers,
                } => self
                    .cu_key_press(envelope.context, bundle_id, key, modifiers)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuScroll {
                    bundle_id,
                    target,
                    dx,
                    dy,
                } => self
                    .cu_scroll(envelope.context, bundle_id, target, dx, dy)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuFocusWindow {
                    bundle_id,
                    window_id,
                } => self
                    .cu_focus_window(envelope.context, bundle_id, window_id)
                    .map(|(result, authorized_by)| {
                        grant_id = authorized_by;
                        result
                    }),
                OperationRequest::CuWait { seconds } => Ok(cu_wait(seconds)),
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
            Some(OperationResult::CuListWindows { windows }) => (Some(windows.len()), None),
            // The capture's bytes never cross this channel (the desktop
            // redeems them through the handoff), so only the read content
            // size is worth recording.
            Some(OperationResult::CuReadAppContent(tree)) => {
                (None, Some(tree.tree.to_string().len()))
            }
            Some(_) => (None, None),
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
            outcome: operation_audit_outcome(result),
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

    /// Every computer-use handler below follows the same spine: validate and
    /// bound the request, hard-refuse a blocked bundle, authorize against the
    /// live grants (deny-by-default, consent-card signal on a miss), and only
    /// then dispatch to the backend. The backend performs; it never decides.
    fn cu_list_windows(
        &self,
        context: ExecutionContext,
        bundle_id: Option<String>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        if let Some(bundle_id) = &bundle_id {
            validate_bundle_id(bundle_id)?;
            require_unblocked(bundle_id)?;
        }
        let grant_id = {
            let state = self.lock_state()?;
            match bundle_id.as_deref() {
                Some(bundle_id) => Some(authorize_computer_use(
                    &state,
                    context,
                    Capability::ReadAppContent,
                    bundle_id,
                )?),
                // A screen-wide window listing is display-scoped: a
                // whole-display capture grant covers it, and so does any
                // read-or-better app grant (its holder may already see one
                // app's content; the listing adds only other apps' titles,
                // which the blocklist still protects).
                None => authorize_cu_screen(&state, context).ok().or_else(|| {
                    state
                        .grants
                        .iter()
                        .find(|grant| {
                            context.grant_subject_matches(grant.subject())
                                && matches!(grant.scope(), Scope::App { .. })
                                && cu_read_granted(grant.capability())
                        })
                        .map(Grant::id)
                }),
            }
            .ok_or(BrokerError::Denied)?
        };
        let windows = self
            .shared
            .computer_use
            .list_windows(bundle_id.as_deref())
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuListWindows { windows }, Some(grant_id)))
    }

    fn cu_capture_screen(
        &self,
        context: ExecutionContext,
        target: CaptureTargetWire,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        let staging = self
            .shared
            .cu_staging
            .clone()
            .ok_or(BrokerError::StatePoisoned)?;
        let (backend_target, grant_id) = match &target {
            CaptureTargetWire::App { bundle_id } => {
                validate_bundle_id(bundle_id)?;
                require_unblocked(bundle_id)?;
                let grant_id = {
                    let state = self.lock_state()?;
                    authorize_computer_use(&state, context, Capability::CaptureScreen, bundle_id)?
                };
                (
                    CaptureTarget::App {
                        bundle_id: bundle_id.clone(),
                    },
                    grant_id,
                )
            }
            CaptureTargetWire::Display { display_id } => {
                let grant_id = {
                    let state = self.lock_state()?;
                    authorize_cu_screen(&state, context)?
                };
                (
                    CaptureTarget::Display {
                        display_id: *display_id,
                    },
                    grant_id,
                )
            }
        };
        let handoff_id = Uuid::new_v4();
        let out_path = staging.join(handoff_file_name(handoff_id));
        // An app capture is annotated: re-read the live tree, extract the
        // numbered interactive marks, and let the helper draw them over the
        // PNG. Best-effort — capture needs Screen Recording while the tree
        // needs Accessibility, so a missing AX grant degrades to an
        // unannotated capture rather than failing one permission with the
        // other's absence.
        let marks = match &backend_target {
            CaptureTarget::App { bundle_id } => self
                .shared
                .computer_use
                .read_ax_tree(bundle_id, Some(MAX_CU_AX_DEPTH), Some(MAX_CU_AX_NODES))
                .map(|tree| extract_marks(&tree.tree, MAX_CAPTURE_MARKS))
                .unwrap_or_default(),
            CaptureTarget::Display { .. } => Vec::new(),
        };
        let meta = self
            .shared
            .computer_use
            .capture_with_marks(&backend_target, &out_path, &marks)
            .map_err(BrokerError::ComputerUse)?;
        {
            let mut state = self.lock_state()?;
            state.handoffs.insert(
                handoff_id,
                StagedCapture {
                    width: meta.width,
                    height: meta.height,
                    media_type: meta.media_type.clone(),
                },
            );
            state.handoff_order.push_back(handoff_id);
            evict_handoffs(&mut state, &staging);
        }
        Ok((
            OperationResult::CuCaptureScreen(CuCaptureScreenResult {
                handoff_id,
                width: meta.width,
                height: meta.height,
                media_type: meta.media_type,
                marks,
            }),
            Some(grant_id),
        ))
    }

    fn cu_read_app_content(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        max_depth: Option<u32>,
        max_nodes: Option<u32>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ReadAppContent, &bundle_id)?
        };
        let tree = self
            .shared
            .computer_use
            .read_ax_tree(
                &bundle_id,
                Some(max_depth.map_or(MAX_CU_AX_DEPTH, |depth| depth.min(MAX_CU_AX_DEPTH))),
                Some(max_nodes.map_or(MAX_CU_AX_NODES, |nodes| nodes.min(MAX_CU_AX_NODES))),
            )
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuReadAppContent(tree), Some(grant_id)))
    }

    fn cu_click(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        target: ElementTargetWire,
        button: Option<String>,
        click_count: Option<u32>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        if let Some(button) = &button {
            if !matches!(button.as_str(), "left" | "right") {
                return Err(BrokerError::InvalidCuRequest);
            }
        }
        let target = ElementTarget::from(target);
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ControlApp, &bundle_id)?
        };
        if let Some(held) = self.consequential_gate(
            context,
            ControlOp::Click,
            &bundle_id,
            &target,
            PendingActionKind::Click {
                bundle_id: bundle_id.clone(),
                target: target.clone(),
                button: button.clone(),
                click_count,
            },
        )? {
            return Ok((held, Some(grant_id)));
        }
        let meta = self
            .shared
            .computer_use
            .click(&bundle_id, &target, button.as_deref(), click_count)
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuClick(meta), Some(grant_id)))
    }

    fn cu_type_text(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        text: String,
        target: ElementTargetWire,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        if text.len() > MAX_CU_TYPE_TEXT_BYTES {
            return Err(BrokerError::FileTooLarge {
                maximum: MAX_CU_TYPE_TEXT_BYTES,
            });
        }
        let target = ElementTarget::from(target);
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ControlApp, &bundle_id)?
        };
        if let Some(held) = self.consequential_gate(
            context,
            ControlOp::TypeText,
            &bundle_id,
            &target,
            PendingActionKind::TypeText {
                bundle_id: bundle_id.clone(),
                text: text.clone(),
                target: target.clone(),
            },
        )? {
            return Ok((held, Some(grant_id)));
        }
        let meta = self
            .shared
            .computer_use
            .type_text(&bundle_id, &text, &target)
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuTypeText(meta), Some(grant_id)))
    }

    /// The consequential gate shared by click and type. Returns `Ok(Some(..))`
    /// with the hold-for-confirmation result when the target's live label
    /// classifies as consequential — nothing has been acted on in that case —
    /// and `Ok(None)` when the op may proceed.
    ///
    /// Only an element-addressed target is classifiable: a raw coordinate
    /// point has no label to read, which is exactly why coordinates are the
    /// documented last resort.
    fn consequential_gate(
        &self,
        context: ExecutionContext,
        op: ControlOp,
        bundle_id: &str,
        target: &ElementTarget,
        action: PendingActionKind,
    ) -> Result<Option<OperationResult>, BrokerError> {
        if target.element_id.is_none() {
            return Ok(None);
        }
        let description = self
            .shared
            .computer_use
            .describe_element(bundle_id, target)
            .map_err(BrokerError::ComputerUse)?;
        let Consequence::Consequential { reason } = classify(
            op,
            description.role.as_deref(),
            description.label.as_deref(),
        ) else {
            return Ok(None);
        };
        let target_label = description.label.as_deref().map(truncate_label);
        self.hold_for_confirmation(
            context,
            bundle_id,
            action,
            target_label,
            description.fingerprint.clone(),
            reason,
        )
    }

    /// Hold a consequential action for explicit user confirmation, returning
    /// the single-use confirmation. The broker owns the action's parameters —
    /// the agent cannot substitute a different target or text at confirm time.
    /// Returns `Ok(None)` to proceed without a hold.
    fn hold_for_confirmation(
        &self,
        context: ExecutionContext,
        bundle_id: &str,
        action: PendingActionKind,
        target_label: Option<String>,
        expected_fingerprint: Option<String>,
        reason: String,
    ) -> Result<Option<OperationResult>, BrokerError> {
        let confirmation_id = Uuid::new_v4();
        {
            let mut state = self.lock_state()?;
            state.pending_confirmations.insert(
                confirmation_id,
                PendingControlAction {
                    context,
                    action,
                    expected_label: target_label.clone(),
                    expected_fingerprint: expected_fingerprint.clone(),
                },
            );
            // Enroll in insertion order so the cap below has something to
            // evict; without this the queue stays empty and the map grows
            // unboundedly.
            state.confirmation_order.push_back(confirmation_id);
            while state.confirmation_order.len() > MAX_PENDING_CONFIRMATIONS {
                if let Some(evicted) = state.confirmation_order.pop_front() {
                    state.pending_confirmations.remove(&evicted);
                }
            }
        }
        Ok(Some(OperationResult::CuNeedsConfirmation(
            CuNeedsConfirmationResult {
                confirmation_id,
                bundle_id: bundle_id.to_owned(),
                target_label,
                reason,
            },
        )))
    }

    fn cu_key_press(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        key: String,
        modifiers: Option<Vec<String>>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        if key.is_empty() || key.len() > MAX_CU_KEY_BYTES {
            return Err(BrokerError::InvalidCuRequest);
        }
        if let Some(modifiers) = &modifiers {
            if modifiers.len() > MAX_CU_MODIFIERS
                || modifiers
                    .iter()
                    .any(|modifier| modifier.is_empty() || modifier.len() > MAX_CU_KEY_BYTES)
            {
                return Err(BrokerError::InvalidCuRequest);
            }
        }
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ControlApp, &bundle_id)?
        };
        // A key press has no element label the consequential gate could read,
        // but chords and bare Return are the commit paths (send / delete /
        // quit). Confirm those before acting.
        if key_press_needs_confirmation(&key, modifiers.as_ref().is_some_and(|m| !m.is_empty())) {
            let label = match &modifiers {
                Some(modifiers) if !modifiers.is_empty() => {
                    format!("{}+{}", modifiers.join("+"), key)
                }
                _ => key.clone(),
            };
            if let Some(held) = self.hold_for_confirmation(
                context,
                &bundle_id,
                PendingActionKind::KeyPress {
                    bundle_id: bundle_id.clone(),
                    key: key.clone(),
                    modifiers: modifiers.clone(),
                },
                Some(truncate_label(&label)),
                // A key press has no element, so no fingerprint to bind.
                None,
                format!(
                    "This presses \u{201c}{}\u{201d}, a keyboard shortcut that can commit an action (send, delete, or quit) with no undo.",
                    truncate_label(&label)
                ),
            )? {
                return Ok((held, Some(grant_id)));
            }
        }
        let meta = self
            .shared
            .computer_use
            .key_press(&bundle_id, &key, modifiers.as_deref())
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuKeyPress(meta), Some(grant_id)))
    }

    fn cu_scroll(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        target: ElementTargetWire,
        dx: Option<f64>,
        dy: Option<f64>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        if [dx, dy]
            .into_iter()
            .flatten()
            .any(|delta| !delta.is_finite())
        {
            return Err(BrokerError::InvalidCuRequest);
        }
        let target = ElementTarget::from(target);
        // Scroll synthesizes a wheel event and warps the cursor to the target —
        // an input mutation, so it needs the control grant, not merely the
        // read grant.
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ControlApp, &bundle_id)?
        };
        let meta = self
            .shared
            .computer_use
            .scroll(&bundle_id, &target, dx, dy)
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuScroll(meta), Some(grant_id)))
    }

    fn cu_focus_window(
        &self,
        context: ExecutionContext,
        bundle_id: String,
        window_id: Option<u32>,
    ) -> Result<(OperationResult, Option<GrantId>), BrokerError> {
        validate_bundle_id(&bundle_id)?;
        require_unblocked(&bundle_id)?;
        // Focus activates and raises another app's window — a visible host
        // mutation, so it needs the control grant, not merely the read grant.
        let grant_id = {
            let state = self.lock_state()?;
            authorize_computer_use(&state, context, Capability::ControlApp, &bundle_id)?
        };
        let meta = self
            .shared
            .computer_use
            .focus_window(&bundle_id, window_id)
            .map_err(BrokerError::ComputerUse)?;
        Ok((OperationResult::CuFocusWindow(meta), Some(grant_id)))
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

fn hello(computer_use_available: bool) -> HelloResult {
    let mut operations = vec![
        "list_roots".to_owned(),
        "list_directory".to_owned(),
        "read_file".to_owned(),
        "read_file_binary".to_owned(),
        "write_file".to_owned(),
    ];
    // The computer-use ops are advertised only when a backend can actually
    // perform them (macOS with a resolved helper); an unsupported build keeps
    // them off the handshake so no client offers a tool that always fails.
    if computer_use_available {
        operations.extend(
            [
                "cu_list_windows",
                "cu_capture_screen",
                "cu_read_app_content",
                "cu_click",
                "cu_type_text",
                "cu_key_press",
                "cu_scroll",
                "cu_focus_window",
                "cu_wait",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    HelloResult {
        protocol_version: PROTOCOL_VERSION,
        operations,
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

/// The default computer-use backend: the bundled helper when one resolves on
/// this host, otherwise the always-refusing backend so the ops stay
/// unadvertised and harmless.
fn default_computer_use_backend() -> Arc<dyn ComputerUseBackend> {
    match HelperBackend::resolve() {
        Some(helper) => Arc::new(helper),
        None => Arc::new(UnsupportedBackend),
    }
}

/// The durable broker's staging directory, owner-only, cleared of files left
/// by a previous run (their handoff records died with that process, so the
/// bytes are unredeemable either way).
fn open_cu_staging(data_dir: &Path) -> Option<PathBuf> {
    prepare_cu_staging(data_dir.join(CU_STAGING_DIR_NAME))
}

/// An ephemeral broker's staging directory: unique per instance under the
/// system temp dir, still owner-only.
fn ephemeral_cu_staging() -> Option<PathBuf> {
    let directory = std::env::temp_dir().join(format!(
        "openwave-cu-staging-{}-{}",
        std::process::id(),
        Uuid::new_v4().as_simple()
    ));
    prepare_cu_staging(directory)
}

fn prepare_cu_staging(directory: PathBuf) -> Option<PathBuf> {
    if std::fs::create_dir_all(&directory).is_err() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).is_err() {
            return None;
        }
    }
    // Drop unredeemable leftovers from a previous owner of this directory.
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Some(directory)
}

fn handoff_file_name(handoff_id: Uuid) -> String {
    format!("{}.png", handoff_id.as_simple())
}

/// Bounded bundle id check shared by every app-scoped op and by grant
/// creation: non-empty and short enough to be a real bundle id, so the
/// blocklist and scope comparisons never see pathological inputs.
fn validate_bundle_id(bundle_id: &str) -> Result<(), BrokerError> {
    if bundle_id.trim().is_empty() || bundle_id.len() > 256 {
        return Err(BrokerError::InvalidCuRequest);
    }
    Ok(())
}

/// The blocklist outranks consent: refuse before any grant lookup, so a
/// blocked bundle cannot be acted on even if a grant somehow exists.
fn require_unblocked(bundle_id: &str) -> Result<(), BrokerError> {
    if is_blocked_control_bundle(bundle_id) {
        return Err(BrokerError::BlockedApp);
    }
    Ok(())
}

/// The scope a computer-use grant request names: an app scope for a bundle
/// id, or the whole-display scope (capture only). Any other combination is
/// not a meaningful grant and is refused.
fn cu_grant_scope(capability: Capability, bundle_id: Option<&str>) -> Result<Scope, BrokerError> {
    use Capability::{CaptureScreen, ControlApp, ReadAppContent};
    match (capability, bundle_id) {
        (CaptureScreen | ReadAppContent | ControlApp, Some(bundle_id)) => {
            validate_bundle_id(bundle_id)?;
            Ok(Scope::App {
                bundle_id: bundle_id.to_owned(),
            })
        }
        (CaptureScreen, None) => Ok(Scope::Screen),
        (ReadAppContent | ControlApp, None) => Err(BrokerError::InvalidCuRequest),
        _ => Err(BrokerError::InvalidCuRequest),
    }
}

/// Whether a held capability can read app content or better.
fn cu_read_granted(capability: Capability) -> bool {
    matches!(
        capability,
        Capability::ReadAppContent | Capability::CaptureScreen | Capability::ControlApp
    )
}

/// Authorize one app-scoped computer-use op: the live grants must contain a
/// grant for this subject whose capability implies the requested one at the
/// same app scope. `ControlApp` covers the read ops; a read grant never
/// covers control.
fn authorize_computer_use(
    state: &State,
    context: ExecutionContext,
    capability: Capability,
    bundle_id: &str,
) -> Result<GrantId, BrokerError> {
    state
        .grants
        .iter()
        .find(|grant| {
            context.grant_subject_matches(grant.subject())
                && matches!(
                    grant.scope(),
                    Scope::App { bundle_id: granted } if granted == bundle_id
                )
                && Capability::implies(grant.capability(), capability)
        })
        .map(Grant::id)
        .ok_or(BrokerError::Denied)
}

/// Authorize a whole-display capture: an exact `CaptureScreen`@`Screen`
/// grant. App-scoped grants do not widen to the display.
fn authorize_cu_screen(state: &State, context: ExecutionContext) -> Result<GrantId, BrokerError> {
    state
        .grants
        .iter()
        .find(|grant| {
            context.grant_subject_matches(grant.subject())
                && matches!(grant.scope(), Scope::Screen)
                && grant.capability() == Capability::CaptureScreen
        })
        .map(Grant::id)
        .ok_or(BrokerError::Denied)
}

/// The wait a request is allowed to ask for: an absent, negative, or
/// non-finite value waits zero; anything above the cap clamps to it.
fn bounded_wait_seconds(seconds: Option<f64>) -> f64 {
    seconds
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .unwrap_or(0.0)
        .min(MAX_CU_WAIT_SECONDS)
}

/// Broker-side wait: clamp to the bound and sleep. Never reaches the helper.
fn cu_wait(seconds: Option<f64>) -> OperationResult {
    let seconds = bounded_wait_seconds(seconds);
    if seconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    }
    OperationResult::CuWait { seconds }
}

/// The capability a window listing needs: app-scoped read when filtered to
/// one app, whole-display capture otherwise.
fn cu_list_windows_capability(bundle_id: Option<&str>) -> Capability {
    match bundle_id {
        Some(_) => Capability::ReadAppContent,
        None => Capability::CaptureScreen,
    }
}

/// De-sensitized audit target for a computer-use scope: the bundle id, or the
/// whole display when no app is named.
fn cu_scope_audit_target(bundle_id: Option<&str>) -> AuditTarget {
    match bundle_id {
        Some(bundle_id) => AuditTarget::app(bundle_id),
        None => AuditTarget::Screen,
    }
}

/// Evict the oldest staged captures beyond the retention bound, deleting
/// their staging files so the bytes do not linger unredeemable on disk.
fn evict_handoffs(state: &mut State, staging: &Path) {
    while state.handoff_order.len() > MAX_PENDING_HANDOFFS {
        if let Some(evicted) = state.handoff_order.pop_front() {
            state.handoffs.remove(&evicted);
            let _ = std::fs::remove_file(staging.join(handoff_file_name(evicted)));
        }
    }
}

/// One subject's computer-use grants, newest first, for the management UI.
/// Folder grants are excluded — they already surface through
/// [`ControlRequest::ListGrantStatements`].
fn list_cu_app_grants(state: &State, subject: GrantSubject) -> Vec<GrantStatementSummary> {
    let mut grants = state
        .grants
        .iter()
        .filter(|grant| {
            !grant.is_single_use()
                && grant.subject() == subject
                && matches!(grant.scope(), Scope::App { .. } | Scope::Screen)
        })
        .map(|grant| GrantStatementSummary {
            grant_id: grant.id(),
            subject: grant.subject(),
            capability: grant.capability(),
            scope: grant.scope().clone(),
            root_display_name: None,
            consent_method: grant.consent().method(),
            granted_at: grant.consent().granted_at(),
        })
        .collect::<Vec<_>>();
    grants.sort_by(|left, right| {
        right
            .granted_at
            .cmp(&left.granted_at)
            .then_with(|| left.grant_id.to_string().cmp(&right.grant_id.to_string()))
    });
    grants
}

impl AuditTarget {
    /// One app touched by a computer-use op, by its (sanitized, bounded)
    /// bundle id.
    fn app(bundle_id: &str) -> Self {
        Self::App {
            bundle_id: AuditLabel::from_host_text(bundle_id),
            element_label: None,
        }
    }
}

/// Audit target for one confirmed control action: the app plus the bounded
/// label the user approved, never raw screen text.
fn cu_element_audit_target(bundle_id: &str, element_label: Option<&str>) -> AuditTarget {
    AuditTarget::App {
        bundle_id: AuditLabel::from_host_text(bundle_id),
        element_label: element_label.map(AuditLabel::from_host_text),
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
        BrokerError::BlockedApp => (
            ErrorCode::Denied,
            "this application cannot be captured, read, or controlled",
            false,
        ),
        BrokerError::UnknownConfirmation => (
            ErrorCode::NotFound,
            "the confirmation is unknown or was already used",
            false,
        ),
        BrokerError::StaleTarget => (
            ErrorCode::StaleElement,
            "the target element changed after confirmation; retry the action",
            true,
        ),
        BrokerError::InvalidCuRequest => (
            ErrorCode::InvalidRequest,
            "computer-use request parameters are invalid",
            false,
        ),
        BrokerError::ComputerUse(error) => match error.kind {
            BackendErrorKind::PermissionDenied => (
                ErrorCode::OsPermissionDenied,
                "the OS screen-recording or accessibility permission is not granted",
                true,
            ),
            BackendErrorKind::NotFound => (
                ErrorCode::NotFound,
                "the target app, window, or display was not found",
                false,
            ),
            BackendErrorKind::InvalidRequest => (
                ErrorCode::InvalidRequest,
                "the computer-use request was rejected as malformed",
                false,
            ),
            BackendErrorKind::StaleElement => (
                ErrorCode::StaleElement,
                "the target element moved or changed; re-read the accessibility tree",
                true,
            ),
            BackendErrorKind::Yielded => (
                ErrorCode::Denied,
                "a system security surface owns the foreground",
                false,
            ),
            BackendErrorKind::OperationFailed => (
                ErrorCode::Internal,
                "the computer-use operation failed on the host",
                true,
            ),
            BackendErrorKind::Unsupported => (
                ErrorCode::Internal,
                "computer use is not available on this build",
                false,
            ),
        },
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

/// Completion outcome for an operator result. A hold is not an executed
/// action: [`AuditOutcome::Held`] is distinct from [`AuditOutcome::Allowed`].
fn operation_audit_outcome(result: &Result<OperationResult, ErrorResponse>) -> AuditOutcome {
    match result {
        Ok(OperationResult::CuNeedsConfirmation(_)) => AuditOutcome::Held,
        _ => audit_outcome(result.as_ref().err()),
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

/// How many completed attachment and write records the broker keeps.
///
/// A record earns its place for exactly two readers: a client retrying the
/// same operation identity with the same content, and a client asking for that
/// operation's receipt after a crash or a dropped connection. Both are
/// recovery paths that run within one attempt of the request — the desktop
/// reconciler and the CLI look the receipt up immediately, or on the next
/// launch — so nothing consults a record after thousands of later mutations
/// have gone through. Kept unbounded, the same records are the one part of the
/// state file that grows with use rather than with what the user has approved,
/// until it crosses [`state_file::MAX_STATE_FILE_BYTES`] and the broker can
/// neither save consent nor start.
///
/// The bound is a count rather than an age because nothing in a record says
/// when it was written, and inventing a clock for it would put the retention
/// of consent receipts at the mercy of the host's wall clock. Two thousand
/// records is far more than any recovery window needs and roughly two megabytes
/// at the largest record shape, which leaves the 16 MiB ceiling to the state it
/// exists for: approved folders, their grants, and their attachments.
const MAX_RETAINED_MUTATION_RECEIPTS: usize = 2048;

/// Records that may be dropped once they are old enough.
///
/// Registration and revocation records stay: the loader reads them against
/// each other — a registration receipt for a folder that is gone has to find
/// the revocation that removed it — so dropping one could make the state file
/// unloadable. They also accrue at the pace of a person choosing folders,
/// which is not the growth this bound exists for. Attachment and write records
/// are each validated on their own terms and are the two that a working
/// session produces continuously.
///
/// Only completed records qualify. A pending one is still owed an outcome, so
/// it is never enrolled and never evicted — which means a write whose dispatch
/// was abandoned leaves a record behind for good. That leak is one record per
/// abandoned in-flight write and is not what this bound is for.
fn is_prunable_receipt(record: &MutationRecord) -> bool {
    matches!(
        record,
        MutationRecord::Attachment {
            outcome: MutationOutcome::Complete(_),
            ..
        } | MutationRecord::Write {
            outcome: MutationOutcome::Complete(_),
            ..
        }
    )
}

/// Drop the oldest completed receipts until the retained set is within bounds.
fn prune_mutation_receipts(
    mutations: &mut HashMap<OperationId, MutationRecord>,
    order: &mut VecDeque<OperationId>,
) {
    while order.len() > MAX_RETAINED_MUTATION_RECEIPTS {
        let Some(operation_id) = order.pop_front() else {
            break;
        };
        mutations.remove(&operation_id);
    }
}

/// Enrol a just-completed record in the bounded retention set.
///
/// Only ever called once per operation identity, from the `Pending` to
/// `Complete` transition, so an identity cannot appear twice in the queue.
fn retain_mutation_receipt(state: &mut State, operation_id: OperationId) {
    state.receipt_order.push_back(operation_id);
    prune_mutation_receipts(&mut state.mutations, &mut state.receipt_order);
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
            retain_mutation_receipt(state, operation_id);
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
            retain_mutation_receipt(state, operation_id);
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

    let temporary = format!(".tidebreak-write-{operation_id}.tmp");
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
    execute_commands: bool,
) -> Result<RootAttachmentMutationResult, BrokerError> {
    validate_subject_conversation(request.subject, request.conversation_id)?;
    let changed = match request.mutation {
        RootAttachmentMutationKind::Attach => {
            state
                .roots
                .get(&request.root_id)
                .ok_or(BrokerError::UnknownRoot)?;
            let method = request
                .consent_method
                .ok_or(BrokerError::InvalidConsentMethod)?;
            // `CarriedForward` describes a migration, not an interaction, so a
            // control caller must not be able to stamp it on a live grant.
            if matches!(method, ConsentMethod::CarriedForward) {
                return Err(BrokerError::InvalidConsentMethod);
            }
            // An attachment that already stands is not a consent interaction:
            // re-running it — a retry, a reconciler pass, a second dialog for a
            // folder this chat already has — must leave authority exactly as it
            // is. Minting here is what let a redundant attach undo a
            // revocation.
            //
            // A genuinely new attachment still only mints for a subject that
            // has never held this folder. This check is keyed on the
            // conversation and the grants it guards are keyed on the subject,
            // and for a project chat those are different entities — a sibling
            // chat's first attach passes here while pointing at a position its
            // neighbour already narrowed. `ensure_default_subject_grants` is
            // where the subject's own history is consulted.
            if has_root_attachment(state, request.conversation_id, request.root_id) {
                false
            } else {
                let consent = ConsentRecord::new(method, Utc::now());
                ensure_default_subject_grants(
                    state,
                    request.subject,
                    request.root_id,
                    consent,
                    execute_commands,
                )?;
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
            // Disconnecting a folder is not an exercise of access to it, so it
            // must not require any. Requiring read here meant that narrowing a
            // folder to nothing also took away the only way to get rid of it:
            // the folder stayed attached, allowed nothing, and refused to go.
            // Detach only ever removes this conversation's own attachment rows,
            // which is why nothing is being protected by asking.
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

/// Remove one grant (and what depends on it) from a grant table.
///
/// `false` when no grant with this identity belongs to `subject` — a
/// mismatched subject is indistinguishable from an absent grant, so the
/// caller cannot probe another subject's rows. When the removed grant is a
/// folder's `ReadFiles`, that folder's `ExecuteCommands` grants for the same
/// subject go with it: exec reach is only ever additional on top of read.
fn remove_grant_statement(
    grants: &mut Vec<Grant>,
    subject: GrantSubject,
    grant_id: GrantId,
) -> bool {
    let Some(index) = grants
        .iter()
        .position(|grant| grant.id() == grant_id && grant.subject() == subject)
    else {
        return false;
    };
    let removed = grants.remove(index);
    if removed.capability() == Capability::ReadFiles {
        if let Scope::Root { root_id } | Scope::PathSubtree { root_id, .. } = *removed.scope() {
            let still_readable = grants.iter().any(|grant| {
                grant.subject() == subject
                    && grant.capability() == Capability::ReadFiles
                    && matches!(
                        *grant.scope(),
                        Scope::Root { root_id: granted } | Scope::PathSubtree { root_id: granted, .. }
                            if granted == root_id
                    )
            });
            if !still_readable {
                grants.retain(|grant| {
                    !(grant.subject() == subject
                        && grant.capability() == Capability::ExecuteCommands
                        && matches!(*grant.scope(), Scope::Root { root_id: granted } if granted == root_id))
                });
            }
        }
    }
    true
}

/// Does this subject already hold any standing authority over this folder?
///
/// Any statement scoped to the root counts, whatever the capability. The
/// question this answers is not "can it read" but "has the user already
/// settled what this subject may do here" — and the answer has to be
/// conservative, because a withdrawn grant leaves no trace: `revoke_grant`
/// removes the row, so "revoked write" and "never had write" are the same
/// state. Treating any surviving statement as a settled position is the only
/// reading that cannot silently restore what the user took away.
fn subject_has_any_root_grant(state: &State, subject: GrantSubject, root_id: RootId) -> bool {
    state
        .grants
        .iter()
        .any(|grant| grant.subject() == subject && scope_targets_root(grant.scope(), root_id))
}

/// Give a subject the folder's default access, but only if it has never had it.
///
/// Registration mints read, write and exec together because choosing a folder
/// in the picker is how the user says the agent may work in it. The same set
/// is right the first time a folder reaches a subject through an attachment or
/// a re-pick, so a chat that connects a folder can use it the way the product
/// says it can.
///
/// It is not right afterwards, and "afterwards" cannot be read off the grant
/// table: revoking deletes rows, so a subject narrowed to nothing looks exactly
/// like a subject that never had anything. Neither can it be read off the
/// conversation, because grants are keyed on the subject and attachments are
/// keyed on the conversation — for a project subject those are different
/// entities, and a sibling chat connecting the folder used to re-mint the
/// access its neighbour had just revoked. `State::settled` is the record kept
/// on the grant's own key, and it is what makes this once-only: after the first
/// mint, widening is a consent decision belonging to
/// [`Controller::grant_root_capability`] and its permission dialog.
///
/// A surviving grant still counts on its own, so an install that predates the
/// record is not re-minted for folders it can currently reach.
fn ensure_default_subject_grants(
    state: &mut State,
    subject: GrantSubject,
    root_id: RootId,
    consent: ConsentRecord,
    execute_commands: bool,
) -> Result<(), BrokerError> {
    let settled = state.settled.contains(&(subject, root_id))
        || subject_has_any_root_grant(state, subject, root_id);
    state.settled.insert((subject, root_id));
    if settled {
        return Ok(());
    }
    ensure_subject_grants(state, subject, root_id, consent, execute_commands)
}

fn ensure_subject_grants(
    state: &mut State,
    subject: GrantSubject,
    root_id: RootId,
    consent: ConsentRecord,
    execute_commands: bool,
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
            consent.clone(),
        )?);
    }
    if execute_commands
        && !state.grants.iter().any(|grant| {
            grant.subject() == subject
                && grant.capability() == Capability::ExecuteCommands
                && matches!(grant.scope(), Scope::Root { root_id: granted } if *granted == root_id)
        })
    {
        state.grants.push(Grant::from_consent(
            GrantId::new(),
            subject,
            Capability::ExecuteCommands,
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
        error_code: Some(unavailable_error_code(root.reason)),
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

/// The pinned directory of one live registration, for the app-folder trio.
///
/// Trusted-host surface: no conversation attachment and no grant lookup
/// apply — the server-side app grant is the consent gate
/// (`docs/folder-bindings.md`) — but only a *live* registration answers. A
/// set-aside or forgotten root is denied, and I/O through the pinned
/// descriptor means a renamed-and-replaced directory cannot answer either.
fn app_folder_root(state: &State, root_id: RootId) -> Result<Dir, BrokerError> {
    state
        .roots
        .get(&root_id)
        .ok_or(BrokerError::Denied)?
        .root
        .directory()
        .try_clone()
        .map_err(BrokerError::from)
}

/// One bounded app-folder write, atomic and digest-bound like the agent
/// write operation, minus its approval and idempotency machinery: there is
/// no chat to park an approval on, and the server-side dispatch does not
/// replay — a same-content retry reconciles via the destination check.
fn write_app_folder_file(
    root: &Dir,
    request: AppFolderWriteRequest,
) -> Result<ControlResult, BrokerError> {
    let probe = WriteFileRequest {
        operation_id: OperationId::from_uuid(Uuid::new_v4())
            .expect("a random v4 uuid is never nil"),
        root_id: request.root_id,
        path: request.path.clone(),
        mode: request.mode,
        approval: None,
        content_base64: request.content_base64.clone(),
        bytes: request.bytes,
        sha256: request.sha256,
    };
    let content = decode_write_content(&probe)?;
    match atomic_write_connected_file(
        root,
        &request.path,
        &content,
        request.mode,
        probe.operation_id,
    ) {
        Ok(replaced) => Ok(ControlResult::WriteAppFolderFile {
            bytes: request.bytes,
            replaced,
        }),
        Err(_error) if destination_matches(root, &request.path, request.bytes, request.sha256) => {
            Ok(ControlResult::WriteAppFolderFile {
                bytes: request.bytes,
                replaced: matches!(request.mode, WriteFileMode::Replace),
            })
        }
        Err(error) => Err(error),
    }
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

/// Every grant the broker holds, live and dormant, newest consent first.
///
/// Unavailable roots keep their grants in the listing: the approval stands
/// until something withdraws it, and hiding a statement because its volume is
/// unmounted would make the consent surface understate what the user agreed
/// to. Their display name is recovered from the approved path's basename, the
/// same identity a live registration would carry.
fn list_grant_statements(state: &State) -> Result<Vec<GrantStatementSummary>, ErrorResponse> {
    // Registration mints a bounded number of grants per root, so the grant
    // table scales with the root table; the cap exists for the same reason as
    // the root listing's.
    const MAX_LIST_GRANTS: usize = MAX_LIST_ROOTS * 8;
    let scope_display_name = |scope: &Scope, dormant: Option<&UnavailableRoot>| {
        let root_id = match scope {
            // Computer-use scopes (an app, the whole display) name no folder
            // root; their display identity comes from the bundle id, not a
            // registered root.
            Scope::Subject | Scope::App { .. } | Scope::Screen => return None,
            Scope::Root { root_id } | Scope::PathSubtree { root_id, .. } => *root_id,
        };
        if let Some(root) = state.roots.get(&root_id) {
            return Some(root.display_name.clone());
        }
        dormant
            .filter(|root| root.id == root_id)
            .and_then(|root| root.path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    };
    let live = state
        .grants
        .iter()
        .filter(|grant| !grant.is_single_use())
        .map(|grant| (grant, None::<&UnavailableRoot>));
    let dormant = state
        .unavailable
        .iter()
        .flat_map(|root| root.grants.iter().map(move |grant| (grant, Some(root))));
    let mut statements = live
        .chain(dormant)
        .map(|(grant, dormant)| GrantStatementSummary {
            grant_id: grant.id(),
            subject: grant.subject(),
            capability: grant.capability(),
            scope: grant.scope().clone(),
            root_display_name: scope_display_name(grant.scope(), dormant),
            consent_method: grant.consent().method(),
            granted_at: grant.consent().granted_at(),
        })
        .collect::<Vec<_>>();
    if statements.len() > MAX_LIST_GRANTS {
        return Err(error_response(BrokerError::RootListTooLarge));
    }
    statements.sort_by(|left, right| {
        right
            .granted_at
            .cmp(&left.granted_at)
            .then_with(|| left.grant_id.to_string().cmp(&right.grant_id.to_string()))
    });
    Ok(statements)
}

/// Every set-aside root, by its safe identity.
///
/// The management-surface companion to [`list_approved_roots`], which omits
/// these roots because they cannot be attached. Display names are recovered
/// from the approved path's basename — the same identity a live registration
/// would carry — and the owner and riding attachments are included so the
/// trusted desktop can tell an outage from a detach and address a deliberate
/// [`ControlRequest::RevokeRoot`] at the registering subject.
fn list_unavailable_roots(state: &State) -> Result<Vec<UnavailableRootSummary>, ErrorResponse> {
    if state.unavailable.len() > MAX_LIST_ROOTS {
        return Err(error_response(BrokerError::RootListTooLarge));
    }
    let mut roots = state
        .unavailable
        .iter()
        .map(|root| UnavailableRootSummary {
            root_id: root.id,
            display_name: root_display_name(&root.path),
            reason: root.reason,
            owner: root.owner,
            attached_conversations: root
                .attachments
                .iter()
                .map(|attachment| attachment.conversation_id())
                .collect(),
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
    execute_commands: bool,
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
            capabilities: root_capabilities(state, context, root_id, execute_commands),
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
        if !seen.insert(root_id) || authorize_exec_reach(state, request.context, &root_id).is_err()
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

/// Whether commands may run against one root's contents in this conversation.
///
/// Exec reach sits on top of read rather than beside it. A command with the
/// folder in front of it can read every byte the read capability covers, so
/// holding exec without read is not a state the broker will act on: revoking
/// read has to stop the shell too, or the narrower revocation would be a lie.
fn authorize_exec_reach(
    state: &State,
    context: ExecutionContext,
    root_id: &RootId,
) -> Result<GrantId, BrokerError> {
    authorize(
        state,
        context,
        Capability::ReadFiles,
        Resource::Root(root_id),
    )?;
    authorize(
        state,
        context,
        Capability::ExecuteCommands,
        Resource::Root(root_id),
    )
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
    execute_commands: bool,
) -> Vec<Capability> {
    let mut capabilities = [Capability::ReadFiles, Capability::WriteFiles]
        .into_iter()
        .filter(|capability| {
            authorize(state, context, *capability, Resource::Root(root_id)).is_ok()
        })
        .collect::<Vec<_>>();
    if execute_commands && authorize_exec_reach(state, context, root_id).is_ok() {
        capabilities.push(Capability::ExecuteCommands);
    }
    capabilities
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
        Scope::Subject | Scope::App { .. } | Scope::Screen => false,
    }
}

#[cfg(test)]
mod tests;

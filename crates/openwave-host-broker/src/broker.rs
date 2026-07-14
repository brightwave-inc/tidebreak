//! Owning broker for consent mutations and capability-checked filesystem reads.
//!
//! The controller and operator share one registry. Operations take an authorized
//! clone of a pinned root handle under a short lock, perform bounded-result I/O,
//! then reauthorize before releasing bytes. Revocation therefore completes
//! without waiting on host I/O, prevents new operations, and fences results from
//! operations that were already in flight.

use std::{
    collections::HashMap,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use cap_fs_ext::OpenOptionsSyncExt;
use cap_std::fs::{Dir, OpenOptions};
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    protocol::{
        ControlEnvelope, ControlRequest, ControlResponseEnvelope, ControlResult, DirectoryEntry,
        EntryKind, ErrorCode, ErrorResponse, HelloResult, OperationEnvelope, OperationRequest,
        OperationResponseEnvelope, OperationResult, PathRequest, ReadFileResult,
        RegisterRootRequest, RegisterRootResult, Response, ResponseEnvelope, RevokeRootRequest,
        RevokeRootResult, RootSummary, PROTOCOL_VERSION,
    },
    Capability, ConsentMethod, ConsentRecord, ExecutionContext, Grant, GrantError, GrantId,
    GrantSubject, OperationId, RelativePath, RootAttachment, RootId, RootPolicy, RootPolicyError,
    Scope, SubjectKind, ValidatedRoot,
};

const MAX_READ_FILE_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_ENTRIES: usize = 4_096;

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
    #[error("conversation does not match the conversation-scoped grant subject")]
    SubjectConversationMismatch,
    #[error("folder-picker registration used an invalid consent method")]
    InvalidConsentMethod,
    #[error("broker state lock was poisoned")]
    StatePoisoned,
    #[error("path is not a regular file")]
    NotRegularFile,
    #[error("file is too large (maximum {MAX_READ_FILE_BYTES} bytes)")]
    FileTooLarge,
    #[error("file is not valid UTF-8")]
    NotUtf8,
    #[error("directory listing exceeds broker limits")]
    DirectoryTooLarge,
    #[error(transparent)]
    RootPolicy(#[from] RootPolicyError),
    #[error(transparent)]
    InvalidGrant(#[from] GrantError),
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
}

#[derive(Default)]
struct State {
    roots: HashMap<RootId, RegisteredRoot>,
    grants: Vec<Grant>,
    attachments: Vec<RootAttachment>,
    mutations: HashMap<OperationId, MutationRecord>,
}

struct RegisteredRoot {
    owner: GrantSubject,
    display_name: String,
    root: ValidatedRoot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MutationRecord {
    Register {
        request: RegisterFingerprint,
        outcome: MutationOutcome<RegisterRootResult>,
    },
    Revoke {
        request: RevokeFingerprint,
        outcome: MutationOutcome<RevokeRootResult>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MutationOutcome<T> {
    InFlight,
    Complete(Result<T, ErrorResponse>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisterFingerprint {
    subject: GrantSubject,
    conversation_id: Uuid,
    path: PathBuf,
    consent_method: ConsentMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RevokeFingerprint {
    subject: GrantSubject,
    root_id: RootId,
}

struct PreparedRegistration {
    root_id: RootId,
    root: RegisteredRoot,
    grants: [Grant; 2],
    attachment: RootAttachment,
    result: RegisterRootResult,
}

impl Broker {
    /// Create an empty broker using the reviewed host-root policy.
    pub fn new(policy: RootPolicy) -> Self {
        Self {
            shared: Arc::new(Shared {
                policy,
                state: Mutex::new(State::default()),
            }),
        }
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
        let result = self.execute(envelope);
        response_envelope(request_id, result)
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
            match claim_register(&mut state, operation_id, &fingerprint).map_err(error_response)? {
                Claim::Start => {}
                Claim::Complete(result) => return result,
            }
        }

        let prepared = self.prepare_registration(request).map_err(error_response);
        let outcome = prepared
            .as_ref()
            .map(|item| item.result.clone())
            .map_err(Clone::clone);
        let mut state = self.lock_state().map_err(error_response)?;
        if let Ok(prepared) = prepared {
            state.roots.insert(prepared.root_id, prepared.root);
            state.grants.extend(prepared.grants);
            state.attachments.push(prepared.attachment);
        }
        complete_register(&mut state, operation_id, &fingerprint, outcome.clone())
            .map_err(error_response)?;
        outcome
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
        let display_name = validated
            .canonical_path()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Connected folder".to_owned());
        let root_id = RootId::new();
        let result = RegisterRootResult {
            root: RootSummary {
                root_id,
                display_name: display_name.clone(),
            },
        };
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
            root_id,
            root: RegisteredRoot {
                owner: request.subject,
                display_name,
                root: validated,
            },
            grants: [list_grant, read_grant],
            attachment: RootAttachment::new(request.conversation_id, root_id)?,
            result,
        })
    }

    fn revoke_root(&self, request: RevokeRootRequest) -> Result<RevokeRootResult, ErrorResponse> {
        let operation_id = request.operation_id;
        let fingerprint = RevokeFingerprint {
            subject: request.subject,
            root_id: request.root_id,
        };
        let mut state = self.lock_state().map_err(error_response)?;
        match claim_revoke(&mut state, operation_id, fingerprint).map_err(error_response)? {
            Claim::Complete(result) => return result,
            Claim::Start => {}
        }
        let owned = state
            .roots
            .get(&request.root_id)
            .is_some_and(|root| root.owner == request.subject);
        if owned {
            state.roots.remove(&request.root_id);
            state
                .grants
                .retain(|grant| !scope_targets_root(grant.scope(), request.root_id));
            state
                .attachments
                .retain(|attachment| attachment.root_id() != request.root_id);
        }
        let result = Ok(RevokeRootResult { revoked: owned });
        complete_revoke(&mut state, operation_id, fingerprint, result.clone())
            .map_err(error_response)?;
        result
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, BrokerError> {
        self.shared
            .state
            .lock()
            .map_err(|_| BrokerError::StatePoisoned)
    }
}

impl Operator {
    /// Authorize and perform one agent operation, returning a correlated safe
    /// response rather than exposing raw host errors.
    pub fn handle(&self, envelope: OperationEnvelope) -> OperationResponseEnvelope {
        let request_id = envelope.request_id;
        let result = self.execute(envelope).map_err(error_response);
        response_envelope(request_id, result)
    }

    fn execute(&self, envelope: OperationEnvelope) -> Result<OperationResult, BrokerError> {
        require_version(envelope.protocol_version)?;
        match envelope.request {
            OperationRequest::ListRoots => {
                let state = self.lock_state()?;
                list_roots(&state, envelope.context)
            }
            OperationRequest::ListDirectory(PathRequest { root_id, path }) => {
                let directory = self.authorized_root(envelope.context, root_id, &path)?;
                let result = list_directory(&directory, &path)?;
                self.reauthorize(envelope.context, root_id, &path)?;
                Ok(result)
            }
            OperationRequest::ReadFile(PathRequest { root_id, path }) => {
                let directory = self.authorized_root(envelope.context, root_id, &path)?;
                let result = read_file(&directory, &path)?;
                self.reauthorize(envelope.context, root_id, &path)?;
                Ok(result)
            }
        }
    }

    fn authorized_root(
        &self,
        context: ExecutionContext,
        root_id: RootId,
        path: &RelativePath,
    ) -> Result<Dir, BrokerError> {
        let state = self.lock_state()?;
        authorize(
            &state,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root_id,
                relative: path,
            },
        )?;
        state
            .roots
            .get(&root_id)
            .ok_or(BrokerError::Denied)?
            .root
            .directory()
            .try_clone()
            .map_err(Into::into)
    }

    fn reauthorize(
        &self,
        context: ExecutionContext,
        root_id: RootId,
        path: &RelativePath,
    ) -> Result<(), BrokerError> {
        let state = self.lock_state()?;
        authorize(
            &state,
            context,
            Capability::ReadFiles,
            Resource::Path {
                root_id: &root_id,
                relative: path,
            },
        )?;
        state.roots.get(&root_id).ok_or(BrokerError::Denied)?;
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, State>, BrokerError> {
        self.shared
            .state
            .lock()
            .map_err(|_| BrokerError::StatePoisoned)
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
        BrokerError::SubjectConversationMismatch
        | BrokerError::InvalidConsentMethod
        | BrokerError::InvalidGrant(_) => (
            ErrorCode::InvalidRequest,
            "host operation request is invalid",
            false,
        ),
        BrokerError::RootPolicy(_) => (
            ErrorCode::InvalidRoot,
            "selected folder is not an allowed connected root",
            false,
        ),
        BrokerError::FileTooLarge | BrokerError::DirectoryTooLarge => (
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
    };
    ErrorResponse {
        code,
        message: message.to_owned(),
        retryable,
    }
}

fn transient_io_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

enum Claim<T> {
    Start,
    Complete(Result<T, ErrorResponse>),
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
                    outcome: MutationOutcome::InFlight,
                },
            );
            Ok(Claim::Start)
        }
        Some(MutationRecord::Register {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if existing == request => Ok(Claim::Complete(result.clone())),
        Some(MutationRecord::Register {
            request: existing,
            outcome: MutationOutcome::InFlight,
        }) if existing == request => Err(BrokerError::OperationInProgress),
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
        }) if existing == request && matches!(outcome, MutationOutcome::InFlight) => {
            *outcome = MutationOutcome::Complete(result);
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
                    outcome: MutationOutcome::InFlight,
                },
            );
            Ok(Claim::Start)
        }
        Some(MutationRecord::Revoke {
            request: existing,
            outcome: MutationOutcome::Complete(result),
        }) if *existing == request => Ok(Claim::Complete(result.clone())),
        Some(MutationRecord::Revoke {
            request: existing,
            outcome: MutationOutcome::InFlight,
        }) if *existing == request => Err(BrokerError::OperationInProgress),
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
        }) if *existing == request && matches!(outcome, MutationOutcome::InFlight) => {
            *outcome = MutationOutcome::Complete(result);
            Ok(())
        }
        _ => Err(BrokerError::OperationIdConflict),
    }
}

fn list_roots(state: &State, context: ExecutionContext) -> Result<OperationResult, BrokerError> {
    authorize(state, context, Capability::ListRoots, Resource::Subject)?;
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
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.root_id.to_string().cmp(&right.root_id.to_string()))
    });
    Ok(OperationResult::ListRoots { roots })
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

    fn setup() -> (tempfile::TempDir, Broker, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join("Documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), "hello from broker").unwrap();
        std::fs::create_dir(root.join("reports")).unwrap();
        let policy = RootPolicy::for_test(
            home,
            vec![temp.path().join("sensitive")],
            vec![temp.path().to_path_buf()],
            Vec::new(),
        );
        (temp, Broker::new(policy), root)
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
        let directory = operator
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
}

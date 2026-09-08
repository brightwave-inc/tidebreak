//! Desktop implementation of the server-owned native computer-use boundary.
//!
//! The HTTP channel authenticates a code session and passes its exact
//! `{owner, workspace, session}` scope here. Each operation runs through the
//! shared computer-use executor in [`crate::client_execution::computer_use`]:
//! the same broker authority, per-app consent cards, blocklist, Stop latch,
//! and exclusive acting-dispatch ownership the chat executor uses — a code
//! session and a chat can never both synthesize input at once.
//!
//! On top of that shared execution this adapter owns the session lifecycle
//! the decision requires:
//!
//! * Scope binding — the desktop holds no code-session store, so the first
//!   scope the token-validated route presents for a session id becomes that
//!   session's authoritative `{owner, workspace}` binding. The route derives
//!   the triple from the registry token and the server's live session row
//!   before any call reaches this adapter, which makes that first triple
//!   trusted; any later mismatch on the same session id is rejected forever.
//! * One operation at a time per session — a per-session gate serializes
//!   calls, so exclusive desktop input ownership is whole-session, and an
//!   exact duplicate request always lands after its original.
//! * Request-id recovery — recent terminal results are stored keyed by
//!   request id, behind a larger tombstone set covering every request the
//!   session ever ran: an exact duplicate recovers the stored result, an
//!   evicted exact duplicate answers unknown-outcome rather than replaying,
//!   a reused id with different arguments conflicts, and a session that
//!   exhausts the tombstone budget refuses new requests instead of
//!   forgetting old ones.
//! * Interrupt — `cancel_session` cancels queued work, and when an
//!   operation is actually in flight it latches the executor's Stop —
//!   which cancels pending synthesized input and drains the acting
//!   dispatch — before returning. The latch holds until a trusted resume;
//!   nothing auto-resumes.
//! * Revocation — an ended session is tombstoned forever and its broker
//!   grants are purged; a stale or reissued token can never resurrect
//!   native authority for that session id.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use tauri::{AppHandle, Manager};
use tidebreak_core::computer_session::{
    ComputerUseCall, ComputerUseImage, ComputerUseOutcome, ComputerUseResult,
};
use tidebreak_core::{CallId, OwnerId, SessionId, WorkspaceId};
use tidebreak_server::{NativeRuntime, NativeRuntimeError, NativeRuntimeScope};
use uuid::Uuid;

use crate::client_execution::computer_use::{
    cancel_session_native_input, execute_session_native_operation, revoke_session_native_input,
    stop_all_native_input, SessionNativeOutput, SessionNativeResolution,
};
use crate::host_access::HostAccess;

/// Terminal results remembered per session for request-id recovery. Old
/// entries fall back to the tombstone set; recovery of full payloads is a
/// crash-window affordance, not an archive.
const MAX_STORED_RESULTS: usize = 16;

/// Every request id a session ever executed, with its call fingerprint.
/// Never evicted: when the budget is exhausted the session refuses new
/// requests, because forgetting an old id would let it replay.
const MAX_TRACKED_REQUESTS: usize = 4096;

/// Ceiling on one decoded capture image crossing the native channel.
const NATIVE_IMAGE_MAX_BYTES: usize = 1024 * 1024; // 1 MiB

/// Ceiling on one serialized result frame crossing the native channel.
const NATIVE_FRAME_MAX_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

pub(crate) struct DesktopNativeRuntime {
    app: AppHandle,
    available: bool,
    sessions: Mutex<HashMap<SessionId, Arc<SessionNativeState>>>,
}

/// The `{owner, workspace}` half of the first token-validated scope seen for
/// a session id. Immutable once bound.
#[derive(Clone, PartialEq, Eq)]
struct ScopeBinding {
    owner: OwnerId,
    workspace: WorkspaceId,
}

/// Per-session channel state. Lives from the first operation (or the
/// revocation that tombstones it) until process exit.
struct SessionNativeState {
    /// Serializes the session's operations: whole-session input ownership,
    /// and the ordering that makes duplicate detection exact.
    gate: tokio::sync::Mutex<()>,
    /// Bumped by every interrupt; queued and in-flight operations observe it.
    cancel: tokio::sync::watch::Sender<u64>,
    /// Set once, never cleared: the session ended.
    revoked: AtomicBool,
    binding: Mutex<Option<ScopeBinding>>,
    stored: Mutex<StoredResults>,
}

#[derive(Default)]
struct StoredResults {
    /// Recent full results, bounded by [`MAX_STORED_RESULTS`].
    results: HashMap<Uuid, ComputerUseResult>,
    order: VecDeque<Uuid>,
    /// Every request id ever executed → its call fingerprint. Bounded by
    /// [`MAX_TRACKED_REQUESTS`] and never evicted.
    seen: HashMap<Uuid, u64>,
}

impl SessionNativeState {
    fn new() -> Self {
        Self {
            gate: tokio::sync::Mutex::new(()),
            cancel: tokio::sync::watch::channel(0).0,
            revoked: AtomicBool::new(false),
            binding: Mutex::new(None),
            stored: Mutex::new(StoredResults::default()),
        }
    }

    /// Bind or verify the token-validated scope for this session id.
    fn bind_scope(&self, scope: &NativeRuntimeScope) -> Result<(), NativeRuntimeError> {
        let presented = ScopeBinding {
            owner: scope.owner.clone(),
            workspace: scope.workspace,
        };
        let mut binding = lock(&self.binding);
        match binding.as_ref() {
            Some(bound) if *bound == presented => Ok(()),
            Some(_) => Err(NativeRuntimeError::NotAuthorized(
                "native scope does not match this session's established owner and workspace"
                    .to_owned(),
            )),
            None => {
                *binding = Some(presented);
                Ok(())
            }
        }
    }

    /// Record a terminal result. The request id enters the never-evicted
    /// tombstone set; the full result enters the bounded recovery cache.
    fn store(&self, call: &ComputerUseCall, result: ComputerUseResult) {
        let mut stored = lock(&self.stored);
        stored.seen.insert(call.request_id, call_fingerprint(call));
        if !stored.results.contains_key(&call.request_id) {
            stored.order.push_back(call.request_id);
            while stored.order.len() > MAX_STORED_RESULTS {
                if let Some(evicted) = stored.order.pop_front() {
                    stored.results.remove(&evicted);
                }
            }
        }
        stored.results.insert(call.request_id, result);
    }

    /// The stored answer for this exact call: the recovered result, an
    /// unknown-outcome refusal for an evicted exact repeat, a conflict on an
    /// id reuse with different arguments, or nothing.
    fn recall(
        &self,
        call: &ComputerUseCall,
    ) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        let stored = lock(&self.stored);
        match stored.seen.get(&call.request_id) {
            None => Ok(None),
            Some(fingerprint) if *fingerprint != call_fingerprint(call) => {
                Err(NativeRuntimeError::RequestConflict)
            }
            Some(_) => match stored.results.get(&call.request_id) {
                Some(result) => Ok(Some(result.clone())),
                // Executed, but the full result aged out of the recovery
                // cache. Replaying could double an action; the caller must
                // inspect instead.
                None => Err(NativeRuntimeError::UnknownOutcome),
            },
        }
    }

    fn subscribe_at_generation(
        &self,
        expected: u64,
    ) -> Result<tokio::sync::watch::Receiver<u64>, NativeRuntimeError> {
        let receiver = self.cancel.subscribe();
        if *receiver.borrow() != expected {
            return Err(NativeRuntimeError::UnknownOutcome);
        }
        Ok(receiver)
    }

    fn reserve_request(&self, call: &ComputerUseCall) -> Result<(), NativeRuntimeError> {
        self.admit_new_request()?;
        self.store(call, unknown_before_dispatch(call));
        Ok(())
    }

    /// Whether a brand-new request may still be admitted.
    fn admit_new_request(&self) -> Result<(), NativeRuntimeError> {
        if lock(&self.stored).seen.len() >= MAX_TRACKED_REQUESTS {
            return Err(NativeRuntimeError::Failed(format!(
                "this session has exhausted its {MAX_TRACKED_REQUESTS}-request native budget; \
                 start a new session to continue using the computer"
            )));
        }
        Ok(())
    }
}

/// Order-insensitive fingerprint of a call's name and arguments, so the
/// tombstone set can distinguish an exact duplicate from an id reuse without
/// retaining every argument payload.
fn call_fingerprint(call: &ComputerUseCall) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    call.name.hash(&mut hasher);
    hash_value(&call.arguments, &mut hasher);
    hasher.finish()
}

fn hash_value(value: &serde_json::Value, hasher: &mut impl Hasher) {
    match value {
        serde_json::Value::Null => 0u8.hash(hasher),
        serde_json::Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        serde_json::Value::Number(n) => {
            2u8.hash(hasher);
            n.to_string().hash(hasher);
        }
        serde_json::Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        serde_json::Value::Array(items) => {
            4u8.hash(hasher);
            items.len().hash(hasher);
            for item in items {
                hash_value(item, hasher);
            }
        }
        serde_json::Value::Object(map) => {
            5u8.hash(hasher);
            map.len().hash(hasher);
            // BTreeMap iteration order makes the digest key-order stable.
            let sorted: std::collections::BTreeMap<_, _> = map.iter().collect();
            for (key, item) in sorted {
                key.hash(hasher);
                hash_value(item, hasher);
            }
        }
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Probe once at startup. Permissions are requested by trusted native UX;
/// missing platform support or an unusable helper is a capability absence.
fn native_runtime_available(app: &AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;
        if objc2_foundation::NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion
            < 14
        {
            return false;
        }
        let Some(helper) =
            crate::broker::computer_use_helper_path(app.path().resource_dir().ok().as_deref())
        else {
            return false;
        };
        if !std::fs::metadata(&helper)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            return false;
        }
        std::process::Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(helper)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

impl DesktopNativeRuntime {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
            available: native_runtime_available(&app),
            app,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn session(&self, session_id: SessionId) -> Arc<SessionNativeState> {
        lock(&self.sessions)
            .entry(session_id)
            .or_insert_with(|| Arc::new(SessionNativeState::new()))
            .clone()
    }

    /// Validate a token-derived scope against this adapter's live state: the
    /// exact `{owner, workspace}` binding for the session id (first
    /// token-validated triple wins, mismatches rejected forever) and the
    /// revocation tombstone. Shared by every entry point, and by the
    /// computer-runtime wrapper that multiplexes further adapters over the
    /// same session authority.
    pub(crate) async fn validate_scope(
        &self,
        scope: &NativeRuntimeScope,
    ) -> Result<(), NativeRuntimeError> {
        let session = self.session(scope.session);
        session.bind_scope(scope)?;
        if session.revoked.load(Ordering::SeqCst) {
            return Err(NativeRuntimeError::SessionEnded);
        }
        Ok(())
    }

    /// Cancel every session's pending native input at process exit. When an
    /// operation is mid-dispatch this latches the executor's Stop and waits
    /// for the acting dispatch to drain, so the app never exits while
    /// synthesized input may still be in flight.
    pub(crate) async fn shutdown(&self) {
        let sessions: Vec<Arc<SessionNativeState>> =
            lock(&self.sessions).values().cloned().collect();
        let state = self.app.state::<HostAccess>();
        if let Err(error) = stop_all_native_input(&self.app, state.inner()) {
            eprintln!("tidebreak-desktop: could not stop native input on shutdown: {error}");
        }
        for session in sessions {
            session.cancel.send_modify(|generation| *generation += 1);
        }
        state.computer_use.drain_acting().await;
    }
}

#[async_trait]
impl NativeRuntime for DesktopNativeRuntime {
    /// Native computer use exists where the host broker and its macOS helper
    /// do. Other platforms answer false so the server never mints a channel
    /// or advertises native tools there.
    fn is_available(&self) -> bool {
        self.available
    }

    async fn execute(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<ComputerUseResult, NativeRuntimeError> {
        if !self.is_available() {
            return Err(NativeRuntimeError::Unsupported(
                "native computer use on this platform".to_owned(),
            ));
        }
        self.validate_scope(scope).await?;
        let session = self.session(scope.session);
        // Remember the interrupt generation from before we queued: an
        // interrupt that lands while this call waits its turn cancels it.
        let queued_at_generation = *session.cancel.borrow();
        let _turn = session.gate.lock().await;
        if session.revoked.load(Ordering::SeqCst) {
            return Err(NativeRuntimeError::SessionEnded);
        }
        if *session.cancel.borrow() != queued_at_generation {
            return Err(NativeRuntimeError::Failed(
                "the operation was cancelled by an interrupt before it ran".to_owned(),
            ));
        }
        if session.recall(call)?.is_some() {
            // The route answers duplicates from `result_for_call`.
            return Err(NativeRuntimeError::Recovered);
        }
        // Reserve the identity before polling the executor. A transport or
        // result-mapping failure may follow real input and must never replay.
        session.reserve_request(call)?;
        let cancelled = session.subscribe_at_generation(queued_at_generation)?;

        let state = self.app.state::<HostAccess>();
        let operation = execute_session_native_operation(
            &self.app,
            state.inner(),
            scope.session,
            CallId::from(call.request_id),
            &call.name,
            call.arguments.clone(),
        );
        let acting = crate::client_execution::computer_use::acts_on_host(&call.name);
        let outcome = await_session_operation(
            &state.computer_use,
            scope.session,
            acting,
            cancelled,
            operation,
        )
        .await;
        match outcome {
            Ok(output) => {
                let output = output.map_err(NativeRuntimeError::Failed)?;
                let result = map_output(call, output)?;
                session.store(call, result.clone());
                if result.outcome == ComputerUseOutcome::Unknown {
                    return Err(NativeRuntimeError::UnknownOutcome);
                }
                Ok(result)
            }
            Err(()) => {
                // Interrupted input may have partly acted before the helper
                // stopped. Retain that uncertainty after draining it.
                if acting {
                    let unknown = unknown_after_interrupt(call);
                    session.store(call, unknown);
                    Err(NativeRuntimeError::UnknownOutcome)
                } else {
                    Err(NativeRuntimeError::Failed(
                        "the operation was cancelled by an interrupt".to_owned(),
                    ))
                }
            }
        }
    }

    async fn result_for_call(
        &self,
        scope: &NativeRuntimeScope,
        call: &ComputerUseCall,
    ) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        self.validate_scope(scope).await?;
        self.session(scope.session).recall(call)
    }

    async fn cancel_session(&self, scope: &NativeRuntimeScope) -> Result<(), String> {
        self.validate_scope(scope)
            .await
            .map_err(|_| "native scope is not valid for this session".to_owned())?;
        let session = self.session(scope.session);
        let state = self.app.state::<HostAccess>();
        // Signal the actual owner synchronously before waking its operation.
        // Even on a signal-file failure the helper's fail-closed removal and
        // the session stop take effect before any future can be dropped.
        let cancelled = cancel_session_native_input(&self.app, state.inner(), scope.session);
        session.cancel.send_modify(|generation| *generation += 1);
        if !matches!(cancelled, Ok(false)) {
            state.computer_use.drain_acting().await;
        }
        cancelled?;
        Ok(())
    }

    fn revoke_session(&self, scope: &NativeRuntimeScope) {
        let session = self.session(scope.session);
        // A revoke for a session id another triple bound is still honored:
        // ending access is the fail-closed direction. The binding itself
        // never changes.
        session.revoked.store(true, Ordering::SeqCst);
        let state = self.app.state::<HostAccess>();
        let cancelled = revoke_session_native_input(&self.app, state.inner(), scope.session);
        if let Err(error) = &cancelled {
            eprintln!("tidebreak-desktop: could not stop revoked native session: {error}");
        }
        session.cancel.send_modify(|generation| *generation += 1);
        // The synchronous stop precedes both the wake-up and asynchronous
        // grant cleanup. No helper input can outlive revocation unnoticed.
        let app = self.app.clone();
        let session_id = scope.session;
        tauri::async_runtime::spawn(async move {
            let state = app.state::<HostAccess>();
            if !matches!(cancelled, Ok(false)) {
                state.computer_use.drain_acting().await;
            }
            if let Err(error) = state.purge_session_native_subject(session_id.0).await {
                eprintln!(
                    "tidebreak-desktop: could not purge native grants for an ended session: {error}"
                );
            }
        });
    }
}

async fn await_session_operation<T>(
    computer_use: &crate::client_execution::computer_use::ComputerUseState,
    session: SessionId,
    acting: bool,
    mut cancelled: tokio::sync::watch::Receiver<u64>,
    operation: impl std::future::Future<Output = T>,
) -> Result<T, ()> {
    tokio::pin!(operation);
    tokio::select! {
        biased;
        _ = async {
            tokio::select! {
                _ = cancelled.changed() => {},
                _ = computer_use.wait_for_halt(), if acting => {},
            }
        } => {
            // Cancellation signals the helper before waking us. Retain its
            // dispatch until the broker receives the helper's completion;
            // dropping a waiter must not release input that is still draining.
            if computer_use.owns_dispatch(session) {
                let _ = operation.await;
            }
            Err(())
        },
        output = &mut operation => Ok(output),
    }
}

/// Map the executor's outcome to the wire result, enforcing the channel's
/// transport budgets: each decoded image at most 1 MiB, the whole serialized
/// frame at most 2 MiB. There is no scaling here, so the width and height in
/// a capture's metadata always describe the exact delivered pixels; an
/// over-budget capture fails with sizing guidance instead.
///
/// Known refusals return `Rejected`. Transport failures, generic acting
/// failures, and interrupted dispatches return `Unknown`: the helper may have
/// changed the target before its reply or verification failed. The model must
/// inspect the target before acting again.
fn map_output(
    call: &ComputerUseCall,
    output: SessionNativeOutput,
) -> Result<ComputerUseResult, NativeRuntimeError> {
    use base64::Engine as _;
    if let Some(oversized) = output
        .images
        .iter()
        .find(|image| image.bytes.len() > NATIVE_IMAGE_MAX_BYTES)
    {
        return Ok(oversized_result(
            call,
            format!(
                "the capture is {} bytes, over the {NATIVE_IMAGE_MAX_BYTES}-byte image budget; \
                 request a smaller capture (scope it to one app with app_id, or capture a \
                 single window)",
                oversized.bytes.len()
            ),
        ));
    }
    let images: Vec<ComputerUseImage> = output
        .images
        .into_iter()
        .map(|image| ComputerUseImage {
            mime_type: image.media_type,
            base64: base64::engine::general_purpose::STANDARD.encode(image.bytes),
        })
        .collect();
    let result = match output.resolution {
        SessionNativeResolution::Completed { result } => ComputerUseResult {
            request_id: call.request_id,
            outcome: ComputerUseOutcome::Completed,
            text: format!("{} completed.", call.name),
            data: result,
            error_code: None,
            images,
        },
        SessionNativeResolution::Failed {
            mut result,
            error_code,
        } => {
            let mut message = result
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("The computer-use operation was not performed.")
                .to_owned();
            let outcome = if output.acts_on_host
                && matches!(
                    error_code.as_str(),
                    "computer_unavailable" | "operation_failed"
                ) {
                // The helper may have changed the target before transport or
                // post-action verification failed. A failure is not a refusal.
                message = "The host could not confirm the computer-use operation. Inspect the target before acting again; do not repeat the action automatically.".into();
                if let Some(data) = result.as_object_mut() {
                    data.insert("message".into(), serde_json::Value::String(message.clone()));
                }
                ComputerUseOutcome::Unknown
            } else {
                ComputerUseOutcome::Rejected
            };
            ComputerUseResult {
                request_id: call.request_id,
                outcome,
                text: message,
                data: result,
                error_code: Some(error_code),
                images,
            }
        }
    };
    let frame_len = serde_json::to_vec(&result).map(|frame| frame.len()).ok();
    match frame_len {
        Some(len) if len <= NATIVE_FRAME_MAX_BYTES => Ok(result),
        Some(len) => Ok(oversized_result(
            call,
            format!(
                "the result frame is {len} bytes, over the {NATIVE_FRAME_MAX_BYTES}-byte limit; \
                 narrow the request (shallower tree, fewer nodes, or a smaller capture)"
            ),
        )),
        None => Err(NativeRuntimeError::Failed(
            "the native result could not be serialized".to_owned(),
        )),
    }
}

/// A bounded refusal that replaces an over-budget result. The action itself
/// completed or failed on the host; only its payload was too large to carry,
/// so the caller is told how to re-request rather than left guessing.
fn oversized_result(call: &ComputerUseCall, message: String) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Rejected,
        text: message,
        data: serde_json::json!({}),
        error_code: Some("result_too_large".to_owned()),
        images: Vec::new(),
    }
}

fn unknown_before_dispatch(call: &ComputerUseCall) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Unknown,
        text:
            "The operation may have acted, but its final outcome is unavailable. Capture or read \
               the app before acting again."
                .to_owned(),
        data: serde_json::json!({}),
        error_code: Some("native_outcome_unavailable".to_owned()),
        images: Vec::new(),
    }
}

fn unknown_after_interrupt(call: &ComputerUseCall) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Unknown,
        text: "The operation was interrupted while it may have been acting. Capture or read the \
               app to see its current state before acting again."
            .to_owned(),
        data: serde_json::json!({}),
        error_code: Some("interrupted".to_owned()),
        images: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: serde_json::Value) -> ComputerUseCall {
        ComputerUseCall {
            request_id: Uuid::new_v4(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn completed(request_id: Uuid) -> ComputerUseResult {
        ComputerUseResult {
            request_id,
            outcome: ComputerUseOutcome::Completed,
            text: "done".into(),
            data: serde_json::json!({}),
            error_code: None,
            images: vec![],
        }
    }

    fn scope(owner: &str, workspace: WorkspaceId, session: SessionId) -> NativeRuntimeScope {
        NativeRuntimeScope {
            owner: OwnerId::new(owner).unwrap(),
            workspace,
            session,
        }
    }

    #[test]
    fn the_first_scope_binds_and_mismatches_are_rejected_forever() {
        let session = SessionNativeState::new();
        let workspace = WorkspaceId::new();
        let id = SessionId::new();
        session.bind_scope(&scope("local", workspace, id)).unwrap();
        // Same triple keeps working.
        session.bind_scope(&scope("local", workspace, id)).unwrap();
        // A different owner or workspace on the same session id never binds.
        assert!(matches!(
            session.bind_scope(&scope("intruder", workspace, id)),
            Err(NativeRuntimeError::NotAuthorized(_))
        ));
        assert!(matches!(
            session.bind_scope(&scope("local", WorkspaceId::new(), id)),
            Err(NativeRuntimeError::NotAuthorized(_))
        ));
        // And is still rejected after the mismatch attempts.
        session.bind_scope(&scope("local", workspace, id)).unwrap();
    }

    #[test]
    fn recall_distinguishes_recovery_conflict_and_eviction() {
        let session = SessionNativeState::new();
        let first = call("computer_wait", serde_json::json!({"seconds": 1.0}));
        session.store(&first, completed(first.request_id));
        assert!(matches!(session.recall(&first), Ok(Some(_))));
        let reused = ComputerUseCall {
            arguments: serde_json::json!({"seconds": 2.0}),
            ..first.clone()
        };
        assert!(matches!(
            session.recall(&reused),
            Err(NativeRuntimeError::RequestConflict)
        ));
        // Push the first result out of the bounded recovery cache.
        for _ in 0..(MAX_STORED_RESULTS + 1) {
            let one = call("computer_wait", serde_json::json!({"seconds": 1.0}));
            session.store(&one, completed(one.request_id));
        }
        // An evicted exact repeat must not replay: it answers unknown.
        assert!(matches!(
            session.recall(&first),
            Err(NativeRuntimeError::UnknownOutcome)
        ));
        // An evicted id reused with different arguments still conflicts.
        assert!(matches!(
            session.recall(&reused),
            Err(NativeRuntimeError::RequestConflict)
        ));
        let fresh = call("computer_wait", serde_json::json!({"seconds": 1.0}));
        assert!(matches!(session.recall(&fresh), Ok(None)));
    }

    #[test]
    fn an_exhausted_request_budget_refuses_new_requests() {
        let session = SessionNativeState::new();
        {
            let mut stored = lock(&session.stored);
            for _ in 0..MAX_TRACKED_REQUESTS {
                stored.seen.insert(Uuid::new_v4(), 0);
            }
        }
        assert!(matches!(
            session.admit_new_request(),
            Err(NativeRuntimeError::Failed(_))
        ));
    }

    #[test]
    fn fingerprints_are_argument_order_insensitive_but_value_sensitive() {
        let a = call(
            "computer_click",
            serde_json::json!({"app_id": "com.example", "mark": 3}),
        );
        let b = ComputerUseCall {
            arguments: serde_json::json!({"mark": 3, "app_id": "com.example"}),
            ..a.clone()
        };
        let c = ComputerUseCall {
            arguments: serde_json::json!({"app_id": "com.example", "mark": 4}),
            ..a.clone()
        };
        assert_eq!(call_fingerprint(&a), call_fingerprint(&b));
        assert_ne!(call_fingerprint(&a), call_fingerprint(&c));
    }

    #[test]
    fn oversized_images_and_frames_are_replaced_with_sizing_guidance() {
        use crate::client_execution::computer_use::SessionCaptureImage;
        let one = call("computer_capture_screen", serde_json::json!({}));
        let output = SessionNativeOutput {
            resolution: SessionNativeResolution::Completed {
                result: serde_json::json!({"status": "ok"}),
            },
            images: vec![SessionCaptureImage {
                media_type: "image/png".into(),
                bytes: vec![0u8; NATIVE_IMAGE_MAX_BYTES + 1],
            }],
            acts_on_host: false,
        };
        let result = map_output(&one, output).unwrap();
        assert_eq!(result.outcome, ComputerUseOutcome::Rejected);
        assert_eq!(result.error_code.as_deref(), Some("result_too_large"));
        assert!(result.images.is_empty());
        assert!(result.text.contains("app_id"));

        // A frame that only exceeds the total limit (several images that fit
        // individually) is also refused with guidance.
        let output = SessionNativeOutput {
            resolution: SessionNativeResolution::Completed {
                result: serde_json::json!({"status": "ok"}),
            },
            images: (0..3)
                .map(|_| SessionCaptureImage {
                    media_type: "image/png".into(),
                    bytes: vec![0u8; NATIVE_IMAGE_MAX_BYTES],
                })
                .collect(),
            acts_on_host: false,
        };
        let result = map_output(&one, output).unwrap();
        assert_eq!(result.error_code.as_deref(), Some("result_too_large"));
    }

    #[test]
    fn a_failure_after_reservation_keeps_the_request_non_replayable() {
        let session = SessionNativeState::new();
        let call = call("computer_click", serde_json::json!({"app_id":"fixture"}));
        session.reserve_request(&call).unwrap();
        // The executor fails before it can replace the reserved receipt.
        let error: Result<(), NativeRuntimeError> =
            Err(NativeRuntimeError::Failed("lost reply".into()));
        assert!(error.is_err());
        let recovered = session.recall(&call).unwrap().unwrap();
        assert_eq!(recovered.outcome, ComputerUseOutcome::Unknown);
        assert_eq!(
            recovered.error_code.as_deref(),
            Some("native_outcome_unavailable")
        );
        let changed = ComputerUseCall {
            arguments: serde_json::json!({"app_id":"different"}),
            ..call.clone()
        };
        assert!(matches!(
            session.recall(&changed),
            Err(NativeRuntimeError::RequestConflict)
        ));
    }

    #[test]
    fn an_interrupt_before_subscription_is_not_lost() {
        let session = SessionNativeState::new();
        let queued_generation = *session.cancel.borrow();
        session.cancel.send_modify(|generation| *generation += 1);
        assert!(matches!(
            session.subscribe_at_generation(queued_generation),
            Err(NativeRuntimeError::UnknownOutcome)
        ));
    }

    #[tokio::test]
    async fn cancelled_owner_keeps_its_future_until_helper_completion() {
        use crate::client_execution::computer_use::ComputerUseState;
        use tokio::sync::oneshot;
        let computer_use = Arc::new(ComputerUseState::default());
        let session = SessionId::new();
        let (watch, receiver) = tokio::sync::watch::channel(0);
        let (entered, entered_rx) = oneshot::channel();
        let (helper_complete, helper_complete_rx) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        struct DropCheck {
            dropped: Arc<AtomicBool>,
            cancelled: Arc<AtomicBool>,
        }
        impl Drop for DropCheck {
            fn drop(&mut self) {
                assert!(
                    self.cancelled.load(Ordering::SeqCst),
                    "helper cancellation precedes future drop"
                );
                self.dropped.store(true, Ordering::SeqCst);
            }
        }
        let task_cu = computer_use.clone();
        let drop_check = DropCheck {
            dropped: dropped.clone(),
            cancelled: cancelled.clone(),
        };
        let task = tokio::spawn(async move {
            let dispatch = task_cu.test_dispatch_acting(session, async move {
                let _guard = drop_check;
                entered.send(()).unwrap();
                helper_complete_rx.await.unwrap();
            });
            await_session_operation(&task_cu, session, true, receiver, dispatch).await
        });
        entered_rx.await.unwrap();
        computer_use
            .cancel_session(session, || -> Result<(), ()> {
                cancelled.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        watch.send_modify(|generation| *generation += 1);
        tokio::task::yield_now().await;
        assert!(computer_use.owns_dispatch(session));
        assert!(!dropped.load(Ordering::SeqCst));
        assert!(!task.is_finished());
        helper_complete.send(()).unwrap();
        let _ = task.await.unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(!computer_use.owns_dispatch(session));
    }

    #[test]
    fn uncertain_actions_are_unknown_while_observation_failures_and_denials_are_rejected() {
        let one = call(
            "computer_click",
            serde_json::json!({"app_id": "com.example"}),
        );
        for (acts_on_host, error_code, expected) in [
            (true, "computer_unavailable", ComputerUseOutcome::Unknown),
            (true, "operation_failed", ComputerUseOutcome::Unknown),
            (false, "computer_unavailable", ComputerUseOutcome::Rejected),
            (false, "operation_failed", ComputerUseOutcome::Rejected),
            (true, "requires_foreground", ComputerUseOutcome::Rejected),
            (true, "control_yielded", ComputerUseOutcome::Rejected),
        ] {
            let result = map_output(
                &one,
                SessionNativeOutput {
                    resolution: SessionNativeResolution::Failed {
                        result: serde_json::json!({"message": "fixture failure"}),
                        error_code: error_code.into(),
                    },
                    images: vec![],
                    acts_on_host,
                },
            )
            .unwrap();
            assert_eq!(
                result.outcome, expected,
                "{error_code}, acts={acts_on_host}"
            );
            if expected == ComputerUseOutcome::Unknown {
                assert!(result
                    .text
                    .contains("Inspect the target before acting again"));
                assert_eq!(result.data["message"], result.text);
            }
        }
    }

    #[test]
    fn an_interrupted_acting_call_records_an_unknown_outcome() {
        let one = call(
            "computer_click",
            serde_json::json!({"app_id": "com.example"}),
        );
        let unknown = unknown_after_interrupt(&one);
        assert_eq!(unknown.outcome, ComputerUseOutcome::Unknown);
        assert_eq!(unknown.error_code.as_deref(), Some("interrupted"));
        assert!(unknown.text.contains("before acting again"));
    }
}

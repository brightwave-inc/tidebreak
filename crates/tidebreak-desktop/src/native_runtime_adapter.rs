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
//! * One operation at a time per session — a per-session gate serializes
//!   calls, so exclusive desktop input ownership is whole-session, and an
//!   exact duplicate request always lands after its original.
//! * Request-id recovery — every terminal result is stored (bounded) keyed
//!   by request id; the exact call recovers it, a reused id with different
//!   arguments conflicts.
//! * Interrupt — `cancel_session` aborts the queued and in-flight work; an
//!   acting operation whose dispatch had already begun is recorded with an
//!   unknown outcome that must be inspected before the next action.
//! * Revocation — an ended session is tombstoned forever and its broker
//!   grants are purged; a stale or reissued token can never resurrect
//!   native authority for that session id.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use tauri::{AppHandle, Manager};
use tidebreak_core::computer_session::{
    ComputerUseCall, ComputerUseImage, ComputerUseOutcome, ComputerUseResult,
};
use tidebreak_core::{CallId, SessionId};
use tidebreak_server::{NativeRuntime, NativeRuntimeError, NativeRuntimeScope};
use uuid::Uuid;

use crate::client_execution::computer_use::{
    execute_session_native_operation, SessionNativeOutput, SessionNativeResolution,
};
use crate::host_access::HostAccess;

/// Terminal results remembered per session for request-id recovery. Old
/// entries are evicted in insertion order; recovery is a crash-window
/// affordance, not an archive.
const MAX_STORED_RESULTS: usize = 16;

pub(crate) struct DesktopNativeRuntime {
    app: AppHandle,
    sessions: Mutex<HashMap<SessionId, Arc<SessionNativeState>>>,
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
    stored: Mutex<StoredResults>,
}

#[derive(Default)]
struct StoredResults {
    by_request: HashMap<Uuid, (ComputerUseCall, ComputerUseResult)>,
    order: VecDeque<Uuid>,
}

impl SessionNativeState {
    fn new() -> Self {
        Self {
            gate: tokio::sync::Mutex::new(()),
            cancel: tokio::sync::watch::channel(0).0,
            revoked: AtomicBool::new(false),
            stored: Mutex::new(StoredResults::default()),
        }
    }

    fn store(&self, call: &ComputerUseCall, result: ComputerUseResult) {
        let mut stored = lock(&self.stored);
        if !stored.by_request.contains_key(&call.request_id) {
            stored.order.push_back(call.request_id);
            while stored.order.len() > MAX_STORED_RESULTS {
                if let Some(evicted) = stored.order.pop_front() {
                    stored.by_request.remove(&evicted);
                }
            }
        }
        stored
            .by_request
            .insert(call.request_id, (call.clone(), result));
    }

    /// The stored answer for this exact call: recovered result, conflict on
    /// an id reuse with different arguments, or nothing.
    fn recall(
        &self,
        call: &ComputerUseCall,
    ) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        match lock(&self.stored).by_request.get(&call.request_id) {
            Some((prior, result)) if prior == call => Ok(Some(result.clone())),
            Some(_) => Err(NativeRuntimeError::RequestConflict),
            None => Ok(None),
        }
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl DesktopNativeRuntime {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self {
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
}

#[async_trait]
impl NativeRuntime for DesktopNativeRuntime {
    /// Native computer use exists where the host broker and its macOS helper
    /// do. Other platforms answer false so the server never mints a channel
    /// or advertises native tools there.
    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
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
        let session = self.session(scope.session);
        if session.revoked.load(Ordering::SeqCst) {
            return Err(NativeRuntimeError::SessionEnded);
        }
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
        if let Some(_stored) = session.recall(call)? {
            // The route answers duplicates from `result_for_call`.
            return Err(NativeRuntimeError::Recovered);
        }

        let state = self.app.state::<HostAccess>();
        let operation = execute_session_native_operation(
            &self.app,
            state.inner(),
            scope.session,
            CallId::from(call.request_id),
            &call.name,
            call.arguments.clone(),
        );
        let mut cancelled = session.cancel.subscribe();
        let acting = tidebreak_core::is_computer_use_control_tool(&call.name);
        tokio::select! {
            output = operation => {
                let output = output.map_err(NativeRuntimeError::Failed)?;
                let result = map_output(call, output)?;
                session.store(call, result.clone());
                if result.outcome == ComputerUseOutcome::Unknown {
                    return Err(NativeRuntimeError::UnknownOutcome);
                }
                Ok(result)
            }
            _ = cancelled.changed() => {
                // The in-flight future is dropped, but an acting dispatch may
                // already have synthesized input — record that honestly as an
                // unknown outcome the caller must inspect before acting again.
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
        let session = self.session(scope.session);
        if session.revoked.load(Ordering::SeqCst) {
            return Err(NativeRuntimeError::SessionEnded);
        }
        session.recall(call)
    }

    async fn cancel_session(&self, scope: &NativeRuntimeScope) -> Result<(), String> {
        let session = self.session(scope.session);
        session.cancel.send_modify(|generation| *generation += 1);
        Ok(())
    }

    fn revoke_session(&self, scope: &NativeRuntimeScope) {
        let session = self.session(scope.session);
        session.revoked.store(true, Ordering::SeqCst);
        session.cancel.send_modify(|generation| *generation += 1);
        // Withdraw every broker grant the session's conversation subject
        // accumulated, so nothing outlives the session. Best-effort and
        // async: the tombstone above is what closes the channel.
        let app = self.app.clone();
        let session_id = scope.session;
        tauri::async_runtime::spawn(async move {
            let state = app.state::<HostAccess>();
            if let Err(error) = state.purge_session_native_subject(session_id.0).await {
                eprintln!(
                    "tidebreak-desktop: could not purge native grants for an ended session: {error}"
                );
            }
        });
    }
}

/// Map the executor's outcome to the wire result.
///
/// Rejections and failures share `Rejected` — the host did not (or will not)
/// perform the operation, and the text says why and what to do. The two
/// honest unknowns — an acting operation that failed at the transport layer,
/// and one interrupted mid-dispatch — become `Unknown` so the model inspects
/// the target before acting again.
fn map_output(
    call: &ComputerUseCall,
    output: SessionNativeOutput,
) -> Result<ComputerUseResult, NativeRuntimeError> {
    use base64::Engine as _;
    let images = output
        .images
        .into_iter()
        .map(|image| ComputerUseImage {
            mime_type: image.media_type,
            base64: base64::engine::general_purpose::STANDARD.encode(image.bytes),
        })
        .collect();
    match output.resolution {
        SessionNativeResolution::Completed { result } => Ok(ComputerUseResult {
            request_id: call.request_id,
            outcome: ComputerUseOutcome::Completed,
            text: format!("{} completed.", call.name),
            data: result,
            error_code: None,
            images,
        }),
        SessionNativeResolution::Failed { result, error_code } => {
            let message = result
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("The computer-use operation was not performed.")
                .to_owned();
            let outcome = if output.acts_on_host && error_code == "computer_unavailable" {
                // The broker transport failed after an acting dispatch may
                // have left this process; whether input landed is unknowable.
                ComputerUseOutcome::Unknown
            } else {
                ComputerUseOutcome::Rejected
            };
            Ok(ComputerUseResult {
                request_id: call.request_id,
                outcome,
                text: message,
                data: result,
                error_code: Some(error_code),
                images,
            })
        }
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

    #[test]
    fn recall_distinguishes_recovery_from_conflict() {
        let session = SessionNativeState::new();
        let first = call("computer_wait", serde_json::json!({"seconds": 1.0}));
        let result = ComputerUseResult {
            request_id: first.request_id,
            outcome: ComputerUseOutcome::Completed,
            text: "done".into(),
            data: serde_json::json!({}),
            error_code: None,
            images: vec![],
        };
        session.store(&first, result.clone());
        assert!(matches!(session.recall(&first), Ok(Some(_))));
        let reused = ComputerUseCall {
            arguments: serde_json::json!({"seconds": 2.0}),
            ..first.clone()
        };
        assert!(matches!(
            session.recall(&reused),
            Err(NativeRuntimeError::RequestConflict)
        ));
        let fresh = call("computer_wait", serde_json::json!({"seconds": 1.0}));
        assert!(matches!(session.recall(&fresh), Ok(None)));
    }

    #[test]
    fn stored_results_stay_bounded_in_insertion_order() {
        let session = SessionNativeState::new();
        let mut calls = Vec::new();
        for index in 0..(MAX_STORED_RESULTS + 4) {
            let one = call("computer_wait", serde_json::json!({"seconds": 1.0}));
            let result = ComputerUseResult {
                request_id: one.request_id,
                outcome: ComputerUseOutcome::Completed,
                text: format!("done {index}"),
                data: serde_json::json!({}),
                error_code: None,
                images: vec![],
            };
            session.store(&one, result);
            calls.push(one);
        }
        assert!(matches!(session.recall(&calls[0]), Ok(None)));
        assert!(matches!(session.recall(calls.last().unwrap()), Ok(Some(_))));
        assert_eq!(lock(&session.stored).by_request.len(), MAX_STORED_RESULTS);
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

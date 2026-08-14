//! The run-scoped reverse-RPC capability host: deny-by-default authorization,
//! execute-once dispatch over the [`OperationStore`] seam, and control-lane
//! cancellation.
//!
//! The structural decision that carries the weight — the same one the spike
//! proved — is the split between run-scoped state and connection-scoped state.
//! A [`CapabilityHost`] holds the grant set, the responder, and the operation
//! log; it outlives any single connection. A connection is transient plumbing
//! that forwards request frames in and response frames out. When a connection
//! drops and the supervisor reconnects, it reattaches to the *same*
//! `CapabilityHost`, so the operation log is exactly where idempotent replay is
//! served from.
//!
//! Each `OperationId` runs its capability at most once. The execution is a
//! spawned task that records its outcome in the store and publishes it on a
//! `watch` channel; the connection's per-request forwarder merely awaits that
//! outcome. Because the two are decoupled, a dropped connection cannot abort an
//! execution — it keeps running and records for a later re-issue — while a
//! control-lane cancel *can*, because the execution selects on a cancel signal.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::{oneshot, watch};

use crate::{
    ids::OperationId,
    oplog::{ClaimOutcome, OperationState, OperationStore},
    protocol::{require_version, ErrorCode, ErrorResponse, Response},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ReverseEnvelope, ReverseRequest, ReverseResult,
    },
};

/// Where a dispatched operation stands for the waiters attached to it.
#[derive(Clone)]
enum Outcome {
    Pending,
    Settled(Response<ReverseResult>),
}

/// Process-local record of an in-flight execution, keyed by `OperationId`.
struct Inflight {
    /// Cloned by every attaching connection; the execution task drives it.
    outcome: watch::Receiver<Outcome>,
    /// Settlement ownership and the cancellation signal move together. A
    /// terminal host transition may claim cancellation before it can wake the
    /// execution task; the task must observe that ownership before recording a
    /// responder completion.
    settlement: Mutex<InflightSettlement>,
}

struct InflightSettlement {
    owner: SettlementOwner,
    /// Taken by whichever side first claims settlement. Cancellation sends it;
    /// execution drops it after claiming the responder completion.
    cancel: Option<oneshot::Sender<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettlementOwner {
    Pending,
    Cancellation,
    Settling,
}

struct HostState {
    /// Once closed, no fresh reverse operation may be admitted for execution.
    closed: bool,
    inflight: HashMap<OperationId, Arc<Inflight>>,
}

struct Shared {
    grants: GrantSet,
    responder: Arc<dyn CapabilityResponder>,
    store: Arc<dyn OperationStore>,
    /// Admission and the in-flight set share one lock: closing the host either
    /// wins before a dispatch (which is refused) or after it (which means the
    /// operation is in this map and will be cancelled).
    state: Mutex<HostState>,
    /// Retained in-flight cardinality. Unlike a one-shot notification, `watch`
    /// remembers the final zero transition if it lands before a waiter polls.
    inflight_count: watch::Sender<usize>,
}

/// The run-scoped reverse-RPC server. Cloneable; every clone shares one log.
#[derive(Clone)]
pub struct CapabilityHost {
    shared: Arc<Shared>,
}

impl CapabilityHost {
    /// Build a host for one run: `grants` authorizes deny-by-default, `responder`
    /// executes granted capabilities, and `store` records operation identities.
    #[must_use]
    pub fn new(
        grants: GrantSet,
        responder: Arc<dyn CapabilityResponder>,
        store: Arc<dyn OperationStore>,
    ) -> Self {
        let (inflight_count, _) = watch::channel(0);
        Self {
            shared: Arc::new(Shared {
                grants,
                responder,
                store,
                state: Mutex::new(HostState {
                    closed: false,
                    inflight: HashMap::new(),
                }),
                inflight_count,
            }),
        }
    }

    /// The grant set, for the attach handshake's capability advertisement.
    #[must_use]
    pub fn grants(&self) -> &GrantSet {
        &self.shared.grants
    }

    /// Resolve one reverse request to the outcome its response will be read
    /// from. This is the whole idempotency rule in one place.
    ///
    /// Version and capability failures fail closed, pre-settled, and never
    /// execute. An unknown identity is claimed and executed once; a terminal
    /// identity replays its recorded outcome; a concurrent duplicate attaches to
    /// the live execution; a reused identity with a different request conflicts.
    #[must_use]
    pub fn dispatch(&self, envelope: ReverseEnvelope) -> ReverseWaiter {
        if require_version(envelope.protocol_version).is_err() {
            return ReverseWaiter::settled(Response::Error(ErrorResponse::new(
                ErrorCode::ProtocolVersion,
                "sandbox protocol version mismatch",
                false,
            )));
        }
        if !self.shared.grants.allows(envelope.request.capability()) {
            return ReverseWaiter::settled(Response::Error(ErrorResponse::denied()));
        }
        // An inbound request is untrusted input. Refuse an over-bound one before
        // claiming or executing it — never forward it to the responder.
        if !envelope.request.within_bounds() {
            return ReverseWaiter::settled(Response::Error(ErrorResponse::new(
                ErrorCode::TooLarge,
                "reverse request exceeds its per-capability bound",
                false,
            )));
        }

        let id = envelope.operation_id;
        let fingerprint = envelope.request.clone();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("capability host state lock");
        if state.closed {
            return ReverseWaiter::settled(closed());
        }
        match self.shared.store.claim(id, &fingerprint) {
            ClaimOutcome::Fresh => {
                let (outcome_tx, outcome_rx) = watch::channel(Outcome::Pending);
                let (cancel_tx, cancel_rx) = oneshot::channel();
                let entry = Arc::new(Inflight {
                    outcome: outcome_rx.clone(),
                    settlement: Mutex::new(InflightSettlement {
                        owner: SettlementOwner::Pending,
                        cancel: Some(cancel_tx),
                    }),
                });
                state.inflight.insert(id, Arc::clone(&entry));
                self.shared
                    .inflight_count
                    .send_replace(state.inflight.len());
                drop(state);
                let shared = Arc::clone(&self.shared);
                tokio::spawn(execute(
                    shared,
                    id,
                    envelope.request,
                    cancel_rx,
                    outcome_tx,
                    entry,
                ));
                ReverseWaiter::pending(outcome_rx)
            }
            ClaimOutcome::Replay(OperationState::Recorded(result)) => {
                ReverseWaiter::settled(Response::Ok(result))
            }
            ClaimOutcome::Replay(OperationState::Failed(error)) => {
                ReverseWaiter::settled(Response::Error(error))
            }
            ClaimOutcome::Replay(OperationState::Claimed) => match state.inflight.get(&id) {
                Some(entry) => ReverseWaiter::pending(entry.outcome.clone()),
                // In-memory: a `Claimed` entry always has a live in-flight
                // execution, so this arm is unreachable. A durable store reaches
                // it after a crash and must fail closed, which is what we do.
                None => ReverseWaiter::settled(ambiguous()),
            },
            // The operation ran once and completed; retention evicted its body.
            // It is done, so it is never re-executed — but the outcome is known,
            // so this is a distinct refusal, not the after-crash ambiguity.
            ClaimOutcome::Replay(OperationState::Evicted) => {
                ReverseWaiter::settled(Response::Error(ErrorResponse::new(
                    ErrorCode::OperationEvicted,
                    "operation already completed; its recorded result was evicted",
                    false,
                )))
            }
            // The durable store's after-crash refusal: never re-execute a call
            // whose external effect cannot be proven unexecuted.
            ClaimOutcome::ClaimedElsewhere => ReverseWaiter::settled(ambiguous()),
            ClaimOutcome::Conflict => ReverseWaiter::settled(Response::Error(ErrorResponse::new(
                ErrorCode::OperationIdConflict,
                "operation identity was reused for a different request",
                false,
            ))),
        }
    }

    /// Fire the control-lane cancel for an in-flight operation, if any.
    ///
    /// This is invoked from the reserved control lane, never the request lane,
    /// so it is not queued behind request backpressure.
    pub fn cancel(&self, operation_id: OperationId) {
        let sender = self
            .shared
            .state
            .lock()
            .expect("capability host state lock")
            .inflight
            .get(&operation_id)
            .and_then(|entry| claim_cancellation(entry));
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    /// Cancel every operation currently executing for this run.
    ///
    /// New dispatches are governed separately by the connection lifecycle; a
    /// driver uses this after it has stopped accepting container events, then
    /// waits for [`Self::wait_idle`] before committing a terminal run state.
    pub fn cancel_all(&self) {
        let senders = {
            let state = self
                .shared
                .state
                .lock()
                .expect("capability host state lock");
            claim_cancellations(state.inflight.values())
        };
        signal_cancellations(senders);
    }

    /// Permanently close fresh reverse-operation admission and cancel every
    /// operation admitted before the close boundary.
    ///
    /// Closing is synchronized with [`Self::dispatch`]. After this returns, a
    /// racing dispatch either already appears in the captured in-flight set or
    /// observes the closed state and receives a terminal cancellation error;
    /// no fresh responder execution can begin outside that boundary.
    pub fn close(&self) {
        signal_cancellations(self.begin_close());
    }

    /// Wait until every detached capability execution has quiesced.
    pub async fn wait_idle(&self) {
        wait_for_idle(self.shared.inflight_count.subscribe()).await;
    }

    /// Acknowledge that the sandbox consumed a terminal response and will never
    /// re-issue its operation identity. Retention may now discard the replay
    /// body while the durable store keeps a commit marker.
    pub fn acknowledge(&self, operation_id: OperationId) {
        self.shared.store.evict(operation_id);
    }

    /// The capabilities this run may request, for the attach advertisement.
    #[must_use]
    pub fn granted_capabilities(&self) -> Vec<Capability> {
        self.shared.grants.granted()
    }

    /// Close admission and atomically claim cancellation settlement for every
    /// operation that has not already claimed its responder completion.
    /// Signals are returned so they can be fired after releasing the host lock.
    fn begin_close(&self) -> Vec<oneshot::Sender<()>> {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("capability host state lock");
        state.closed = true;
        claim_cancellations(state.inflight.values())
    }
}

fn claim_cancellations<'a>(
    entries: impl Iterator<Item = &'a Arc<Inflight>>,
) -> Vec<oneshot::Sender<()>> {
    entries
        .filter_map(|entry| claim_cancellation(entry))
        .collect()
}

fn claim_cancellation(entry: &Inflight) -> Option<oneshot::Sender<()>> {
    let mut settlement = entry.settlement.lock().expect("in-flight settlement lock");
    if settlement.owner == SettlementOwner::Pending {
        settlement.owner = SettlementOwner::Cancellation;
        settlement.cancel.take()
    } else {
        None
    }
}

fn signal_cancellations(senders: Vec<oneshot::Sender<()>>) {
    for sender in senders {
        let _ = sender.send(());
    }
}

fn cancellation_won(entry: &Inflight) -> bool {
    entry
        .settlement
        .lock()
        .expect("in-flight settlement lock")
        .owner
        == SettlementOwner::Cancellation
}

fn claim_responder_settlement(shared: &Shared, id: OperationId, entry: &Arc<Inflight>) -> bool {
    // Match close's lock order so marking the host terminal and assigning every
    // pending entry to cancellation is one boundary. If response settlement
    // claims first, close sees `Settling` and wait_idle observes the entry until
    // its durable record is complete.
    let state = shared.state.lock().expect("capability host state lock");
    if !state
        .inflight
        .get(&id)
        .is_some_and(|current| Arc::ptr_eq(current, entry))
    {
        return false;
    }
    let mut settlement = entry.settlement.lock().expect("in-flight settlement lock");
    match settlement.owner {
        SettlementOwner::Pending => {
            settlement.owner = SettlementOwner::Settling;
            settlement.cancel.take();
            true
        }
        SettlementOwner::Cancellation => false,
        SettlementOwner::Settling => false,
    }
}

async fn wait_for_idle(mut inflight_count: watch::Receiver<usize>) {
    loop {
        let count = *inflight_count.borrow_and_update();
        if count == 0 {
            return;
        }
        // The sender lives in `Shared`, but handle closure defensively rather
        // than parking forever if that ownership changes later.
        if inflight_count.changed().await.is_err() {
            return;
        }
    }
}

/// A handle to one reverse operation's eventual outcome, decoupled from the
/// connection that requested it.
pub struct ReverseWaiter {
    outcome: watch::Receiver<Outcome>,
}

impl ReverseWaiter {
    fn settled(response: Response<ReverseResult>) -> Self {
        let (tx, rx) = watch::channel(Outcome::Settled(response));
        // `watch` retains the last value for existing receivers, so dropping the
        // sender here is fine.
        drop(tx);
        Self { outcome: rx }
    }

    fn pending(outcome: watch::Receiver<Outcome>) -> Self {
        Self { outcome }
    }

    /// Await the operation's settled outcome, tolerating a value already settled
    /// before this waiter attached (a pre-settled error, or a replay).
    pub async fn wait(mut self) -> Response<ReverseResult> {
        loop {
            if let Outcome::Settled(response) = &*self.outcome.borrow() {
                return response.clone();
            }
            if self.outcome.changed().await.is_err() {
                return Response::Error(ErrorResponse::new(
                    ErrorCode::Internal,
                    "operation ended without recording an outcome",
                    true,
                ));
            }
        }
    }
}

fn ambiguous() -> Response<ReverseResult> {
    Response::Error(ErrorResponse::new(
        ErrorCode::OperationAmbiguous,
        "operation is claimed with no provable outcome and will not be replayed",
        false,
    ))
}

fn closed() -> Response<ReverseResult> {
    Response::Error(ErrorResponse::new(
        ErrorCode::Cancelled,
        "reverse request was refused because the run is terminal",
        false,
    ))
}

/// Run one operation's capability exactly once, honoring a cancel signal, and
/// record its terminal outcome durably before publishing it.
async fn execute(
    shared: Arc<Shared>,
    id: OperationId,
    request: ReverseRequest,
    mut cancel: oneshot::Receiver<()>,
    outcome: watch::Sender<Outcome>,
    entry: Arc<Inflight>,
) {
    // A close can signal cancellation after dispatch admits the operation but
    // before this spawned task receives its first poll. Check that retained
    // signal before constructing the responder future, then keep cancellation
    // first in the biased select so a signal racing this boundary wins without
    // polling the responder.
    let settled = if cancellation_won(&entry) {
        cancelled()
    } else {
        match cancel.try_recv() {
            Ok(()) | Err(oneshot::error::TryRecvError::Closed) => cancelled(),
            Err(oneshot::error::TryRecvError::Empty) => tokio::select! {
                biased;
                _ = &mut cancel => cancelled(),
                response = shared.responder.respond(request) => response,
            },
        }
    };

    // Enforce the result bound before recording: a responder that returns an
    // over-bound completion has its result rejected, not persisted.
    let responder_settled = match settled {
        Response::Ok(result) if !result.within_bounds() => Response::Error(ErrorResponse::new(
            ErrorCode::TooLarge,
            "reverse result exceeds its per-capability bound",
            false,
        )),
        settled => settled,
    };

    // A responder becoming ready is not itself the settlement point. Claim
    // ownership while synchronized with close before making any durable write;
    // a close that won the boundary is always recorded as cancellation even if
    // its wakeup signal has not been delivered yet.
    let settled = if claim_responder_settlement(&shared, id, &entry) {
        responder_settled
    } else {
        cancelled()
    };

    // Record durably first, so a re-issue racing this completion observes the
    // terminal state in the store rather than a `Claimed` with a vanishing
    // in-flight entry.
    match &settled {
        Response::Ok(result) => {
            let _ = shared.store.record(id, result.clone());
        }
        Response::Error(error) => {
            let _ = shared.store.fail(id, error.clone());
        }
    }
    {
        let mut state = shared.state.lock().expect("capability host state lock");
        state.inflight.remove(&id);
        shared.inflight_count.send_replace(state.inflight.len());
    };
    // A dropped receiver only means no connection is currently waiting; the
    // recorded outcome still stands for a later re-issue.
    let _ = outcome.send(Outcome::Settled(settled));
}

fn cancelled() -> Response<ReverseResult> {
    Response::Error(ErrorResponse::new(
        ErrorCode::Cancelled,
        "reverse request was cancelled",
        false,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{
        ids::{OperationId, RequestId, RunId},
        oplog::{
            ClaimOutcome, InMemoryOperationStore, OperationFingerprint, OperationState,
            OperationStore, StoreError,
        },
        protocol::{MAX_MODEL_COMPLETION_BYTES, PROTOCOL_VERSION},
        reverse::{
            Capability, CapabilityResponder, GrantSet, ModelInferenceParams, ModelInferenceResult,
            ReverseEnvelope, ReverseRequest, ReverseResult, RunProvenance,
        },
    };

    use super::*;

    /// A store that reports every claim as `ClaimedElsewhere` — the after-crash
    /// state a durable store surfaces for a `Claimed` entry it did not itself
    /// dispatch. In-memory stores can never reach it, so it is faked here.
    struct CrashedStore;

    impl OperationStore for CrashedStore {
        fn claim(&self, _id: OperationId, _fingerprint: &OperationFingerprint) -> ClaimOutcome {
            ClaimOutcome::ClaimedElsewhere
        }
        fn record(&self, _id: OperationId, _result: ReverseResult) -> Result<(), StoreError> {
            Ok(())
        }
        fn fail(&self, _id: OperationId, _error: ErrorResponse) -> Result<(), StoreError> {
            Ok(())
        }
        fn state(&self, _id: OperationId) -> Option<OperationState> {
            None
        }
        fn evict(&self, _id: OperationId) {}
        fn len(&self) -> usize {
            0
        }
        fn retained_body_count(&self) -> usize {
            0
        }
    }

    struct CountingResponder {
        executions: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CapabilityResponder for CountingResponder {
        async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
                completion: "should never run".to_owned(),
            }))
        }
    }

    struct PendingResponder {
        executions: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl CapabilityResponder for PendingResponder {
        async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct GatedReadyResponder {
        executions: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl CapabilityResponder for GatedReadyResponder {
        async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
                completion: "ready after close".to_owned(),
            }))
        }
    }

    /// A responder that runs and returns an over-bound completion — the result
    /// bound must reject it after execution, on the record path.
    struct OverBoundResponder {
        executions: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CapabilityResponder for OverBoundResponder {
        async fn respond(&self, _request: ReverseRequest) -> Response<ReverseResult> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
                completion: "x".repeat(MAX_MODEL_COMPLETION_BYTES + 1),
            }))
        }
    }

    #[tokio::test]
    async fn over_bound_completion_is_refused_and_never_recorded() {
        let executions = Arc::new(AtomicUsize::new(0));
        let provenance = RunProvenance {
            run_id: RunId::new(),
            provider: "test".to_owned(),
        };
        let store = InMemoryOperationStore::new();
        let host = CapabilityHost::new(
            GrantSet::new(provenance, [Capability::ModelInference]),
            Arc::new(OverBoundResponder {
                executions: Arc::clone(&executions),
            }),
            Arc::new(store.clone()),
        );

        // The request is within bounds, so it clears the pre-execution gate and
        // reaches the responder; only the completion is over-bound.
        let operation_id = OperationId::new();
        let response = host
            .dispatch(ReverseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(),
                operation_id,
                request: ReverseRequest::ModelInference(ModelInferenceParams {
                    prompt: "within bounds".to_owned(),
                }),
            })
            .wait()
            .await;

        match response {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::TooLarge),
            Response::Ok(_) => panic!("an over-bound completion must be refused"),
        }
        // The witness that this is the result path, not the request bound: the
        // responder actually ran and produced the over-bound completion.
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "the responder executed before its over-bound result was rejected"
        );
        // The refusal is recorded as a terminal failure, never as a Recorded
        // success — the over-bound completion is not persisted.
        match store.state(operation_id) {
            Some(OperationState::Failed(error)) => assert_eq!(error.code, ErrorCode::TooLarge),
            other => panic!("an over-bound completion must be Failed, not Recorded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn claimed_after_crash_refuses_conservatively_and_never_executes() {
        let executions = Arc::new(AtomicUsize::new(0));
        let provenance = RunProvenance {
            run_id: RunId::new(),
            provider: "test".to_owned(),
        };
        let host = CapabilityHost::new(
            GrantSet::new(provenance, [Capability::ModelInference]),
            Arc::new(CountingResponder {
                executions: Arc::clone(&executions),
            }),
            Arc::new(CrashedStore),
        );

        let response = host
            .dispatch(ReverseEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(),
                operation_id: OperationId::new(),
                request: ReverseRequest::ModelInference(ModelInferenceParams {
                    prompt: "resume after crash".to_owned(),
                }),
            })
            .wait()
            .await;

        match response {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::OperationAmbiguous),
            Response::Ok(_) => panic!("a claimed-after-crash call must not be replayed"),
        }
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "a conservatively-refused call must never execute"
        );
    }

    #[tokio::test]
    async fn close_before_first_execution_poll_never_invokes_ready_responder() {
        let executions = Arc::new(AtomicUsize::new(0));
        let store = InMemoryOperationStore::new();
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id: RunId::new(),
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(CountingResponder {
                executions: Arc::clone(&executions),
            }),
            Arc::new(store.clone()),
        );
        let operation_id = OperationId::new();
        let waiter = host.dispatch(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id,
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "must not execute".to_owned(),
            }),
        });

        // This test uses Tokio's current-thread runtime. With no await between
        // dispatch and close, the operation is admitted and its cancellation is
        // signalled before the spawned execution can receive its first poll.
        host.close();

        let response = waiter.wait().await;
        match response {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::Cancelled),
            Response::Ok(_) => panic!("close-before-first-poll must cancel the operation"),
        }
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "an already-cancelled operation must not poll its ready responder"
        );
        match store.state(operation_id) {
            Some(OperationState::Failed(error)) => {
                assert_eq!(error.code, ErrorCode::Cancelled)
            }
            other => panic!("the pre-poll cancellation must be recorded, got {other:?}"),
        }
        host.wait_idle().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_claims_cancellation_before_signalling_a_ready_responder() {
        let executions = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let store = InMemoryOperationStore::new();
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id: RunId::new(),
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(GatedReadyResponder {
                executions: Arc::clone(&executions),
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
            Arc::new(store.clone()),
        );
        let operation_id = OperationId::new();
        let waiter = host.dispatch(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id,
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "race close against a ready response".to_owned(),
            }),
        });
        started.notified().await;

        // Split close at its production signal boundary. Cancellation owns the
        // pending entry while the signal remains intentionally undelivered;
        // releasing the responder now reproduces the ready-response race that
        // previously recorded Ok after the host had already become terminal.
        let cancellation_signals = host.begin_close();
        assert_eq!(cancellation_signals.len(), 1);
        release.notify_one();

        let response = tokio::time::timeout(std::time::Duration::from_secs(1), waiter.wait())
            .await
            .expect("settlement ownership resolves without the delayed signal");
        match response {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::Cancelled),
            Response::Ok(_) => panic!("close must win settlement before its signal is delivered"),
        }
        match store.state(operation_id) {
            Some(OperationState::Failed(error)) => {
                assert_eq!(error.code, ErrorCode::Cancelled)
            }
            other => panic!("the close winner must be durably recorded, got {other:?}"),
        }

        // Deliver the retained wakeup after settlement to prove it is no longer
        // the authority that decides between cancellation and success.
        signal_cancellations(cancellation_signals);
        tokio::time::timeout(std::time::Duration::from_secs(1), host.wait_idle())
            .await
            .expect("the close winner quiesces normally");
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_admission_cancels_inflight_and_refuses_fresh_execution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let store = InMemoryOperationStore::new();
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id: RunId::new(),
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(PendingResponder {
                executions: executions.clone(),
                started: started.clone(),
            }),
            Arc::new(store.clone()),
        );
        let request = |operation_id| ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id,
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "one step".to_owned(),
            }),
        };

        let first_id = OperationId::new();
        let first = host.dispatch(request(first_id));
        started.notified().await;

        host.close();
        let late_id = OperationId::new();
        let late = host.dispatch(request(late_id)).wait().await;
        match late {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::Cancelled),
            Response::Ok(_) => panic!("a fresh request after close must be refused"),
        }
        assert!(
            store.state(late_id).is_none(),
            "a request refused at the admission boundary must not be claimed"
        );

        let cancelled = first.wait().await;
        match cancelled {
            Response::Error(error) => assert_eq!(error.code, ErrorCode::Cancelled),
            Response::Ok(_) => panic!("the in-flight request must be cancelled"),
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), host.wait_idle())
            .await
            .expect("the closed host quiesces");
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "the post-close request must never reach the responder"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_state_retains_the_final_transition_before_wait_poll() {
        let executions = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id: RunId::new(),
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(PendingResponder {
                executions,
                started: started.clone(),
            }),
            Arc::new(InMemoryOperationStore::new()),
        );
        let waiter = host.dispatch(ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(),
            operation_id: OperationId::new(),
            request: ReverseRequest::ModelInference(ModelInferenceParams {
                prompt: "one step".to_owned(),
            }),
        });
        started.notified().await;

        // Mark the non-idle generation observed, then let the final transition
        // happen before `wait_for_idle` is first polled. A retained state change
        // returns immediately; a transient wakeup would be lost here.
        let mut idle = host.shared.inflight_count.subscribe();
        assert_eq!(*idle.borrow_and_update(), 1);
        host.close();
        let _ = waiter.wait().await;
        tokio::time::timeout(std::time::Duration::from_secs(1), wait_for_idle(idle))
            .await
            .expect("the final zero transition is retained for a late poll");
    }
}

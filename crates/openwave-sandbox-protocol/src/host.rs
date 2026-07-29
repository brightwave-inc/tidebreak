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
    /// Taken and fired by the first control-lane cancel for this operation.
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

struct Shared {
    grants: GrantSet,
    responder: Arc<dyn CapabilityResponder>,
    store: Arc<dyn OperationStore>,
    inflight: Mutex<HashMap<OperationId, Arc<Inflight>>>,
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
        Self {
            shared: Arc::new(Shared {
                grants,
                responder,
                store,
                inflight: Mutex::new(HashMap::new()),
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

        let id = envelope.operation_id;
        let fingerprint = envelope.request.clone();
        let mut inflight = self.shared.inflight.lock().expect("inflight lock");
        match self.shared.store.claim(id, &fingerprint) {
            ClaimOutcome::Fresh => {
                let (outcome_tx, outcome_rx) = watch::channel(Outcome::Pending);
                let (cancel_tx, cancel_rx) = oneshot::channel();
                inflight.insert(
                    id,
                    Arc::new(Inflight {
                        outcome: outcome_rx.clone(),
                        cancel: Mutex::new(Some(cancel_tx)),
                    }),
                );
                drop(inflight);
                let shared = Arc::clone(&self.shared);
                tokio::spawn(execute(shared, id, envelope.request, cancel_rx, outcome_tx));
                ReverseWaiter::pending(outcome_rx)
            }
            ClaimOutcome::Replay(OperationState::Recorded(result)) => {
                ReverseWaiter::settled(Response::Ok(result))
            }
            ClaimOutcome::Replay(OperationState::Failed(error)) => {
                ReverseWaiter::settled(Response::Error(error))
            }
            ClaimOutcome::Replay(OperationState::Claimed) => match inflight.get(&id) {
                Some(entry) => ReverseWaiter::pending(entry.outcome.clone()),
                // In-memory: a `Claimed` entry always has a live in-flight
                // execution, so this arm is unreachable. A durable store reaches
                // it after a crash and must fail closed, which is what we do.
                None => ReverseWaiter::settled(ambiguous()),
            },
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
        let entry = self
            .shared
            .inflight
            .lock()
            .expect("inflight lock")
            .get(&operation_id)
            .map(Arc::clone);
        if let Some(entry) = entry {
            if let Some(sender) = entry.cancel.lock().expect("cancel lock").take() {
                let _ = sender.send(());
            }
        }
    }

    /// The capabilities this run may request, for the attach advertisement.
    #[must_use]
    pub fn granted_capabilities(&self) -> Vec<Capability> {
        self.shared.grants.granted()
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

/// Run one operation's capability exactly once, honoring a cancel signal, and
/// record its terminal outcome durably before publishing it.
async fn execute(
    shared: Arc<Shared>,
    id: OperationId,
    request: ReverseRequest,
    cancel: oneshot::Receiver<()>,
    outcome: watch::Sender<Outcome>,
) {
    let settled = tokio::select! {
        response = shared.responder.respond(request) => response,
        _ = cancel => Response::Error(ErrorResponse::new(
            ErrorCode::Cancelled,
            "reverse request was cancelled",
            false,
        )),
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
    shared.inflight.lock().expect("inflight lock").remove(&id);
    // A dropped receiver only means no connection is currently waiting; the
    // recorded outcome still stands for a later re-issue.
    let _ = outcome.send(Outcome::Settled(settled));
}

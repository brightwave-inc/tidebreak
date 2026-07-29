//! The host side of the reverse channel: the run-scoped capability server.
//!
//! The key structural decision is the split between run-scoped state and
//! connection-scoped state. [`CapabilityHost`] holds the grant set, the model
//! proxy, and the operation log; it outlives any single connection. A
//! connection is just transient plumbing that reads request frames and writes
//! response frames. When a connection drops and the supervisor reconnects, it
//! reattaches to the *same* `CapabilityHost`, so the operation log — the durable
//! per-run identity map — is exactly where idempotent replay is served from.
//!
//! Each `OperationId` runs its capability at most once. The execution is a
//! spawned task that writes its outcome into a `watch` channel; the connection's
//! per-request task merely awaits that outcome and forwards it. Because the two
//! are decoupled, a dropped connection cannot abort an execution (it keeps
//! running and records its outcome for a later re-issue), while an explicit
//! cancel *can* (it fires a signal the execution selects on).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, BufReader},
    sync::{oneshot, watch, Mutex as AsyncMutex},
    task::JoinSet,
};

use crate::{
    model::ModelProvider,
    protocol::{
        CancelFrame, Capability, ErrorCode, ErrorResponse, Frame, OperationId, Response,
        ReverseEnvelope, ReverseRequest, ReverseResponseEnvelope, ReverseResult, PROTOCOL_VERSION,
    },
    transport::{read_frame, write_frame, FrameError},
};

/// Where an operation stands. `Pending` until its single execution settles.
#[derive(Debug, Clone)]
enum Outcome {
    Pending,
    Settled(Response<ReverseResult>),
}

/// One durable operation-log entry, keyed by `OperationId`.
struct OperationEntry {
    /// The request this identity was first claimed with. A later re-issue whose
    /// request differs is a conflict, exactly as the broker treats a reused
    /// operation identity with a different fingerprint.
    fingerprint: ReverseRequest,
    /// Cloned by every attaching connection; the execution task drives it.
    outcome: watch::Receiver<Outcome>,
    /// Taken and fired by the first cancel for this operation.
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

struct HostShared {
    grants: Vec<Capability>,
    model: Arc<dyn ModelProvider>,
    log: Mutex<HashMap<OperationId, Arc<OperationEntry>>>,
}

/// The run-scoped reverse-RPC server. Cloneable; every clone shares one log.
#[derive(Clone)]
pub struct CapabilityHost {
    shared: Arc<HostShared>,
}

impl CapabilityHost {
    /// Build a host granting exactly `grants` and proxying `model`.
    ///
    /// Deny-by-default: a capability absent from `grants` is refused.
    #[must_use]
    pub fn new(model: Arc<dyn ModelProvider>, grants: Vec<Capability>) -> Self {
        Self {
            shared: Arc::new(HostShared {
                grants,
                model,
                log: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// How many distinct operations this run has ever admitted. Test-facing;
    /// lets a test assert that a replay added no new execution.
    #[must_use]
    pub fn operation_count(&self) -> usize {
        self.shared.log.lock().expect("log lock").len()
    }

    /// Resolve a request to the receiver its response will be read from.
    ///
    /// This is the whole idempotency rule in one place: an unknown identity is
    /// claimed and executed once; a known identity attaches to the existing
    /// outcome without re-executing; a known identity with a different request
    /// is a conflict. Version and capability checks fail closed, pre-settled.
    fn dispatch(&self, envelope: ReverseEnvelope) -> watch::Receiver<Outcome> {
        if envelope.protocol_version != PROTOCOL_VERSION {
            return settled_error(
                ErrorCode::ProtocolVersion,
                "reverse-rpc protocol version mismatch",
                false,
            );
        }
        if !self.shared.grants.contains(&envelope.request.capability()) {
            return settled_error(
                ErrorCode::Denied,
                "capability is not granted to this run",
                false,
            );
        }

        let mut log = self.shared.log.lock().expect("log lock");
        if let Some(entry) = log.get(&envelope.operation_id) {
            if entry.fingerprint == envelope.request {
                return entry.outcome.clone();
            }
            return settled_error(
                ErrorCode::OperationIdConflict,
                "operation identity was reused for a different request",
                false,
            );
        }

        let (outcome_tx, outcome_rx) = watch::channel(Outcome::Pending);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        log.insert(
            envelope.operation_id,
            Arc::new(OperationEntry {
                fingerprint: envelope.request.clone(),
                outcome: outcome_rx.clone(),
                cancel: Mutex::new(Some(cancel_tx)),
            }),
        );
        let model = Arc::clone(&self.shared.model);
        tokio::spawn(execute(model, envelope.request, cancel_rx, outcome_tx));
        outcome_rx
    }

    /// Fire the cancel signal for an operation if it is still in flight.
    fn cancel(&self, cancel: CancelFrame) {
        let entry = {
            let log = self.shared.log.lock().expect("log lock");
            log.get(&cancel.operation_id).map(Arc::clone)
        };
        if let Some(entry) = entry {
            if let Some(sender) = entry.cancel.lock().expect("cancel lock").take() {
                let _ = sender.send(());
            }
        }
    }
}

/// Run one operation's capability exactly once, honoring a cancel signal.
async fn execute(
    model: Arc<dyn ModelProvider>,
    request: ReverseRequest,
    cancel: oneshot::Receiver<()>,
    outcome: watch::Sender<Outcome>,
) {
    let settled = match request {
        ReverseRequest::ModelInference(params) => {
            tokio::select! {
                completion = model.complete(params) => {
                    Response::Ok(ReverseResult::ModelInference(completion))
                }
                _ = cancel => {
                    Response::Error(ErrorResponse::new(
                        ErrorCode::Cancelled,
                        "reverse request was cancelled",
                        false,
                    ))
                }
            }
        }
    };
    // A dropped receiver only means no connection is currently waiting; the
    // recorded outcome still stands for a later re-issue, so ignore the error.
    let _ = outcome.send(Outcome::Settled(settled));
}

/// Serve one connection against `host` until the peer closes it.
///
/// Returns when the read half reaches EOF or errors — i.e. the connection
/// dropped. The per-request response forwarders live in a [`JoinSet`] scoped to
/// this connection, so aborting this future (a simulated disconnect) tears them
/// down and releases the write half. The *execution* tasks are detached and so
/// keep running past the disconnect, recording their outcome for a later
/// re-issue — the asymmetry the disconnect semantics require.
pub async fn serve_connection<S>(host: CapabilityHost, stream: S) -> Result<(), FrameError>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let writer = Arc::new(AsyncMutex::new(write_half));
    let mut forwarders: JoinSet<()> = JoinSet::new();

    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            // Closed or malformed both end this connection; the supervisor
            // reconnects and re-issues, which is safe by the idempotency rule.
            Err(_) => break,
        };
        match frame {
            Frame::Request(envelope) => {
                let request_id = envelope.request_id;
                let operation_id = envelope.operation_id;
                let mut outcome = host.dispatch(envelope);
                let writer = Arc::clone(&writer);
                forwarders.spawn(async move {
                    let response = wait_settled(&mut outcome).await;
                    let frame = Frame::Response(ReverseResponseEnvelope {
                        protocol_version: PROTOCOL_VERSION,
                        request_id,
                        operation_id,
                        response,
                    });
                    let mut guard = writer.lock().await;
                    // A write failure means the connection dropped mid-response;
                    // the outcome is already recorded for re-issue, so drop it.
                    let _ = write_frame(&mut *guard, &frame).await;
                });
            }
            Frame::Cancel(cancel) => host.cancel(cancel),
            // The host is the responder on this channel; it never receives
            // responses. Ignore rather than trusting peer-shaped input.
            Frame::Response(_) => {}
        }
        while forwarders.try_join_next().is_some() {}
    }
    Ok(())
}

/// Await an operation's settled outcome, tolerating a value that was already
/// settled before this waiter attached (a pre-settled error, or a replay).
async fn wait_settled(outcome: &mut watch::Receiver<Outcome>) -> Response<ReverseResult> {
    loop {
        if let Outcome::Settled(response) = &*outcome.borrow() {
            return response.clone();
        }
        if outcome.changed().await.is_err() {
            return Response::Error(ErrorResponse::new(
                ErrorCode::Internal,
                "operation ended without recording an outcome",
                true,
            ));
        }
    }
}

/// Build a receiver that is already settled on an error, for fail-closed paths.
fn settled_error(code: ErrorCode, message: &str, retryable: bool) -> watch::Receiver<Outcome> {
    let (tx, rx) = watch::channel(Outcome::Settled(Response::Error(ErrorResponse::new(
        code, message, retryable,
    ))));
    // Keep the sender alive for the lifetime of the value by leaking it into the
    // receiver's channel: dropping it here is fine because `watch` retains the
    // last sent value for existing receivers.
    drop(tx);
    rx
}

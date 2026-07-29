//! On-the-wire conformance for the sandbox-agent transport.
//!
//! Every scenario runs the host-side client and the sandbox-side server against
//! each other over a real TCP loopback connection — not the in-process channels
//! the [reference backend](openwave_sandbox_protocol::reference) uses — so the
//! version handshake, the newline framing, the reverse-RPC correlation, the
//! resumable event stream, and the reserved control lane are all exercised as
//! bytes on a socket. This closes the interop gap the protocol crate deferred to
//! the concrete-transport step: the reference suite pins semantics, this suite
//! pins that the same semantics survive a real socket.
//!
//! The four contract areas required of the wire transport, and where:
//!
//! - version-mismatch refusal on the wire — [`version_mismatch_is_refused_on_the_wire`];
//! - a model-inference reverse call round-trip (with exactly-once replay) —
//!   [`model_inference_round_trips_and_replays_exactly_once`];
//! - event-stream delivery and resume-by-sequence — [`event_stream_delivers_then_resumes_by_sequence`];
//! - control-lane cancel preempting a saturated request lane —
//!   [`control_lane_cancel_preempts_a_saturated_request_lane`].
//!
//! Each test body runs under a wall-clock [`timeout`](tokio::time::timeout), so a
//! transport regression — including a spin-loop that never observes its
//! condition — fails fast rather than hanging the suite.

use std::{
    future::Future,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    time::timeout,
};

use openwave_sandbox_protocol::{
    ids::{EventCursor, OperationId, RunId, Sequence},
    oplog::{InMemoryOperationStore, OperationStore},
    protocol::{AttachRequest, ErrorCode, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceParams, ModelInferenceResult,
        ReverseRequest, ReverseResult, RunProvenance,
    },
    serve_connection, CapabilityHost, ConnectError, HostConnection, ReverseOutcome, SandboxRun,
    WireClient,
};

/// Run one scenario under a wall-clock bound so nothing can hang the suite.
async fn bounded<F: Future<Output = ()>>(scenario: F) {
    timeout(Duration::from_secs(15), scenario)
        .await
        .expect("scenario completed within its time bound");
}

/// A responder that answers instantly and counts how many times it ran — the
/// exactly-once witness for idempotent replay over the wire.
struct EchoResponder {
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl CapabilityResponder for EchoResponder {
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let prompt = match request {
            ReverseRequest::ModelInference(params) => params.prompt,
            _ => unreachable!("only model inference is exercised"),
        };
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion: format!("echo:{prompt}"),
        }))
    }
}

/// A responder that blocks until the test releases a gate, so a cancel can be
/// observed while the request is genuinely in flight on the host.
struct GateResponder {
    gate: Arc<Semaphore>,
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl CapabilityResponder for GateResponder {
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult> {
        self.started.fetch_add(1, Ordering::SeqCst);
        let permit = self.gate.acquire().await.expect("gate open");
        permit.forget();
        self.finished.fetch_add(1, Ordering::SeqCst);
        let prompt = match request {
            ReverseRequest::ModelInference(params) => params.prompt,
            _ => unreachable!("only model inference is exercised"),
        };
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion: format!("done:{prompt}"),
        }))
    }
}

fn infer(prompt: &str) -> ReverseRequest {
    ReverseRequest::ModelInference(ModelInferenceParams {
        prompt: prompt.to_owned(),
    })
}

fn completion(result: &ReverseResult) -> String {
    match result {
        ReverseResult::ModelInference(result) => result.completion.clone(),
        _ => unreachable!("only model inference is exercised"),
    }
}

fn host_with(
    grants: Vec<Capability>,
    responder: Arc<dyn CapabilityResponder>,
) -> (CapabilityHost, InMemoryOperationStore) {
    let store = InMemoryOperationStore::new();
    let provenance = RunProvenance {
        run_id: RunId::new(),
        provider: "wire-conformance".to_owned(),
    };
    let host = CapabilityHost::new(
        GrantSet::new(provenance, grants),
        responder,
        Arc::new(store.clone()),
    );
    (host, store)
}

fn attach(resume_from: EventCursor) -> AttachRequest {
    AttachRequest {
        protocol_version: PROTOCOL_VERSION,
        run_id: RunId::new(),
        resume_from,
    }
}

/// Accept host connections and serve them against `run` on a loopback port.
async fn spawn_sandbox(run: SandboxRun) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let run = run.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, run).await;
            });
        }
    });
    addr
}

/// Dial the sandbox at `addr` and complete the attach handshake.
async fn connect(
    addr: SocketAddr,
    host: CapabilityHost,
    resume_from: EventCursor,
) -> Result<HostConnection, ConnectError> {
    let stream = TcpStream::connect(addr).await.expect("dial loopback");
    WireClient::connect(stream, attach(resume_from), host).await
}

/// A version-skewed peer is refused on the wire, and no session is established.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_mismatch_is_refused_on_the_wire() {
    bounded(async {
        // The sandbox speaks a version the host does not.
        let run =
            SandboxRun::with_config(PROTOCOL_VERSION + 1, 16, 4, [Capability::ModelInference]);
        let addr = spawn_sandbox(run).await;
        let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);

        match connect(addr, host, EventCursor::START).await {
            Err(ConnectError::VersionRefused(refused)) => {
                // The refusal frame carried the sandbox's own version over the socket.
                assert_eq!(refused.protocol_version, PROTOCOL_VERSION + 1);
                assert_eq!(refused.code, ErrorCode::ProtocolVersion);
            }
            Err(other) => panic!("expected a version-mismatch refusal, got {other:?}"),
            Ok(_) => panic!("a version mismatch must refuse the connection on the wire"),
        }
    })
    .await;
}

/// A model-inference reverse call round-trips over the socket, and re-issuing the
/// same operation identity replays the recorded answer without re-executing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_inference_round_trips_and_replays_exactly_once() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference]);
        let addr = spawn_sandbox(run.clone()).await;
        let (responder, executions) = echo();
        let (host, store) = host_with(vec![Capability::ModelInference], responder);

        // Keep the connection alive for the duration of the calls.
        let _conn = connect(addr, host, EventCursor::START)
            .await
            .expect("attach accepted");

        let operation = OperationId::new();
        match run.call(operation, infer("forecast")).await {
            ReverseOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), "echo:forecast");
            }
            other => panic!("expected a settled success, got {other:?}"),
        }
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        // Re-issue the same operation identity: the host replays the recorded
        // outcome from its operation log and does not execute the model again.
        match run.call(operation, infer("forecast")).await {
            ReverseOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), "echo:forecast");
            }
            other => panic!("expected a recorded replay, got {other:?}"),
        }
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "a replay must not execute the model again"
        );
        assert_eq!(store.len(), 1, "one distinct operation was recorded");
    })
    .await;
}

/// The event stream is delivered in order over the socket, and a fresh
/// connection resuming from a committed cursor receives only newer events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_stream_delivers_then_resumes_by_sequence() {
    bounded(async {
        let run = SandboxRun::new([]);
        // Emit before any host attaches: the events buffer for replay.
        for letter in ["a", "b", "c", "d", "e"] {
            run.emit_progress(letter).await.expect("emit");
        }
        let addr = spawn_sandbox(run.clone()).await;

        // First attach from the start replays all five, in order.
        let (host, _store) = host_with(Vec::new(), echo().0);
        let mut first = connect(addr, host, EventCursor::START)
            .await
            .expect("attach accepted");
        let mut sequences = Vec::new();
        for _ in 0..5 {
            let event = first.next_event().await.expect("stream open");
            sequences.push(event.sequence.get());
        }
        assert_eq!(sequences, vec![1, 2, 3, 4, 5], "monotonic and gapless");
        drop(first);

        // A second connection resuming from a committed cursor gets only what is
        // strictly newer than it.
        let (host, _store) = host_with(Vec::new(), echo().0);
        let mut resumed = connect(addr, host, EventCursor::committed(Sequence::new(3)))
            .await
            .expect("reattach accepted");
        let mut resumed_sequences = Vec::new();
        for _ in 0..2 {
            let event = resumed.next_event().await.expect("stream open");
            resumed_sequences.push(event.sequence.get());
        }
        assert_eq!(
            resumed_sequences,
            vec![4, 5],
            "resume delivers only events past the committed cursor"
        );
    })
    .await;
}

/// A cancel on the reserved control lane lands even while the request lane is
/// saturated, aborting the in-flight execution; the saturated calls never reach
/// the host, and the cancelled effect never finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_lane_cancel_preempts_a_saturated_request_lane() {
    bounded(async {
        // A request lane with exactly two in-flight permits.
        let run = SandboxRun::with_config(PROTOCOL_VERSION, 16, 2, [Capability::ModelInference]);
        let addr = spawn_sandbox(run.clone()).await;

        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let responder = Arc::new(GateResponder {
            gate: Arc::clone(&gate),
            started: Arc::clone(&started),
            finished: Arc::clone(&finished),
        });
        let (host, _store) = host_with(vec![Capability::ModelInference], responder);
        let _conn = connect(addr, host, EventCursor::START)
            .await
            .expect("attach accepted");

        // Saturate the request lane: two gated calls hold both permits and block
        // in the host's responder.
        let target = OperationId::new();
        let call_a = {
            let run = run.clone();
            tokio::spawn(async move { run.call(target, infer("a")).await })
        };
        let call_b = {
            let run = run.clone();
            tokio::spawn(async move { run.call(OperationId::new(), infer("b")).await })
        };
        while started.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // A third call cannot even acquire a permit, so it never reaches the host.
        let call_c = {
            let run = run.clone();
            tokio::spawn(async move { run.call(OperationId::new(), infer("c")).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            started.load(Ordering::SeqCst),
            2,
            "a third request is blocked behind request backpressure"
        );

        // The cancel rides the reserved control lane, acquires no request permit,
        // and lands despite the saturated request lane.
        run.cancel(target);
        match call_a.await.expect("join") {
            ReverseOutcome::Settled(Response::Error(error)) => {
                assert_eq!(error.code, ErrorCode::Cancelled);
            }
            other => panic!("expected the cancel to land, got {other:?}"),
        }
        assert_eq!(
            finished.load(Ordering::SeqCst),
            0,
            "the cancelled effect never finished"
        );

        // Cleanup: the still-blocked calls are abandoned with the test.
        call_b.abort();
        call_c.abort();
    })
    .await;
}

fn echo() -> (Arc<dyn CapabilityResponder>, Arc<AtomicUsize>) {
    let executions = Arc::new(AtomicUsize::new(0));
    let responder = Arc::new(EchoResponder {
        executions: Arc::clone(&executions),
    });
    (responder, executions)
}

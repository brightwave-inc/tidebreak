//! On-the-wire conformance for the sandbox-agent transport.
//!
//! Every scenario runs the host-side client and the sandbox-side server against
//! each other over a real TCP loopback connection — not the in-process channels
//! the [reference backend](tidebreak_sandbox_protocol::reference) uses — so the
//! version handshake, the newline framing, the reverse-RPC correlation, the
//! resumable event stream, and the reserved control lane are all exercised as
//! bytes on a socket. This closes the interop gap the protocol crate deferred to
//! the concrete-transport step: the reference suite pins semantics, this suite
//! pins that the same semantics survive a real socket.
//!
//! The contract areas required of the wire transport, and where:
//!
//! - version-mismatch refusal on the wire — [`version_mismatch_is_refused_on_the_wire`];
//! - authenticated attach: the correct transport secret is accepted and serves a
//!   capability ([`correct_secret_attaches_and_serves_a_capability`]), a wrong or
//!   absent secret is refused and serves nothing
//!   ([`wrong_or_absent_secret_is_refused_and_serves_nothing`]), and a refused
//!   second connection cannot hijack the live one
//!   ([`a_refused_second_connection_cannot_hijack_the_live_one`]);
//! - a model-inference reverse call round-trip with response acknowledgement —
//!   [`model_inference_round_trips_then_acknowledges_retention`];
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

use tidebreak_sandbox_protocol::{
    ids::{EventCursor, OperationId, RunId, Sequence},
    oplog::{InMemoryOperationStore, OperationState, OperationStore},
    protocol::{AttachRequest, ErrorCode, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceParams, ModelInferenceResult,
        ReverseRequest, ReverseResult, RunProvenance,
    },
    serve_connection, CapabilityHost, ConnectError, HostConnection, ReverseOutcome, SandboxRun,
    TransportSecret, WireClient,
};

/// The per-run transport secret both sides of these tests share. A run expects
/// it; the [`attach`] helper presents it; the authentication tests present a
/// wrong or empty token instead.
const SECRET: &str = "wire-conformance-transport-secret";

/// The expected secret a run authenticates attaches against.
fn expected_secret() -> Option<TransportSecret> {
    Some(TransportSecret::new(SECRET))
}

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
    attach_with(resume_from, SECRET)
}

/// An attach presenting an explicit transport secret, for the authentication
/// tests that dial with a wrong or empty token.
fn attach_with(resume_from: EventCursor, secret: &str) -> AttachRequest {
    AttachRequest {
        protocol_version: PROTOCOL_VERSION,
        run_id: RunId::new(),
        resume_from,
        transport_secret: TransportSecret::new(secret),
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

/// A version-skewed peer is refused on the wire, and no session is established —
/// even though it presents the *correct* transport secret. This pins the ordering
/// invariant: authentication never bypasses the version gate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_mismatch_is_refused_on_the_wire() {
    bounded(async {
        // The sandbox speaks a version the host does not, but shares the secret.
        let run = SandboxRun::with_config(
            PROTOCOL_VERSION + 1,
            16,
            4,
            [Capability::ModelInference],
            expected_secret(),
        );
        let addr = spawn_sandbox(run).await;
        let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);

        // `connect` presents the correct secret via `attach`; the refusal is still
        // a version refusal, so a matching secret does not slip past the skew.
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

/// The correct transport secret is accepted and the connection serves a
/// capability: an authenticated attach round-trips a model-inference reverse call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correct_secret_attaches_and_serves_a_capability() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference], expected_secret());
        let addr = spawn_sandbox(run.clone()).await;
        let (responder, executions) = echo();
        let (host, _store) = host_with(vec![Capability::ModelInference], responder);

        // Keep the authenticated connection open for the call.
        let _conn = connect(addr, host, EventCursor::START)
            .await
            .expect("the correct secret is accepted");
        match run.call(OperationId::new(), infer("hi")).await {
            ReverseOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), "echo:hi");
            }
            other => panic!("an authenticated attach must serve a capability, got {other:?}"),
        }
        assert_eq!(executions.load(Ordering::SeqCst), 1);
    })
    .await;
}

/// A wrong secret, and an absent (empty) one, are each refused as
/// `Unauthenticated` after the version gate — and because the attach never
/// completes, no capability is ever served over that connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_or_absent_secret_is_refused_and_serves_nothing() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference], expected_secret());
        let addr = spawn_sandbox(run).await;

        for bad in ["a-totally-wrong-secret", ""] {
            let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);
            let stream = TcpStream::connect(addr).await.expect("dial loopback");
            let kind = if bad.is_empty() {
                "an absent"
            } else {
                "a wrong"
            };
            match WireClient::connect(stream, attach_with(EventCursor::START, bad), host).await {
                Err(ConnectError::Unauthenticated(refused)) => {
                    // Versions matched — the refusal is authentication, not skew.
                    assert_eq!(refused.protocol_version, PROTOCOL_VERSION);
                    assert_eq!(refused.code, ErrorCode::Unauthenticated);
                }
                Err(other) => {
                    panic!("{kind} secret must be refused as Unauthenticated, got {other:?}");
                }
                Ok(_) => panic!("{kind} secret must be refused, but a session was established"),
            }
            // The `Err` is the whole point: there is no `HostConnection`, so the
            // attacker holds nothing it could serve or drive a reverse call over.
        }
    })
    .await;
}

/// The hijack scenario: a first, authenticated connection is live and is the
/// run's reverse-RPC peer; a second connection presenting a wrong (then absent)
/// secret is refused and must NOT displace the live one. The sandbox installs the
/// newest *accepted* connection via `send_replace`, so a refused attach that never
/// reaches that install cannot steal the channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_second_connection_cannot_hijack_the_live_one() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference], expected_secret());
        let addr = spawn_sandbox(run.clone()).await;

        // Host 1 attaches with the correct secret and becomes the live peer.
        let (responder, executions) = echo();
        let (host1, _store) = host_with(vec![Capability::ModelInference], responder);
        let _conn1 = connect(addr, host1, EventCursor::START)
            .await
            .expect("the authenticated attach is accepted");
        match run.call(OperationId::new(), infer("one")).await {
            ReverseOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), "echo:one");
            }
            other => panic!("the authenticated peer must answer, got {other:?}"),
        }
        assert_eq!(executions.load(Ordering::SeqCst), 1);

        // An attacker dials the same port with a wrong secret, then with none.
        for bad in ["stolen-guess", ""] {
            let (attacker, _s) = host_with(vec![Capability::ModelInference], echo().0);
            let stream = TcpStream::connect(addr).await.expect("dial loopback");
            match WireClient::connect(stream, attach_with(EventCursor::START, bad), attacker).await
            {
                Err(ConnectError::Unauthenticated(_)) => {}
                Err(other) => panic!("a {bad:?} secret must be refused, got {other:?}"),
                Ok(_) => panic!("a {bad:?} secret must not establish a session"),
            }

            // The live peer is unchanged: a fresh reverse call still routes to
            // host 1, whose responder count advances — the attacker never became
            // the run's connection, so it could not answer, read events, or drive
            // the agent.
            match run.call(OperationId::new(), infer("again")).await {
                ReverseOutcome::Settled(Response::Ok(result)) => {
                    assert_eq!(completion(&result), "echo:again");
                }
                other => {
                    panic!("the live connection must survive the refused hijack, got {other:?}")
                }
            }
        }
        assert_eq!(
            executions.load(Ordering::SeqCst),
            3,
            "every call was served by the authenticated peer, never the attacker"
        );
    })
    .await;
}

/// A model-inference reverse call round-trips over the socket, then the
/// sandbox's acknowledgement releases its replay body from the operation log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_inference_round_trips_then_acknowledges_retention() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference], expected_secret());
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

        // The acknowledgement rides behind the response on the reserved control
        // lane. Wait for it to be processed, then prove the full replay body was
        // reduced to an in-memory commit marker.
        for _ in 0..100 {
            if matches!(store.state(operation), Some(OperationState::Evicted)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "acknowledgement must not execute the model again"
        );
        assert!(
            matches!(store.state(operation), Some(OperationState::Evicted)),
            "the acknowledged replay body was reduced to a commit marker"
        );
        assert_eq!(store.len(), 1, "the audit/identity marker remains");
        assert_eq!(
            store.retained_body_count(),
            0,
            "acknowledgement releases the replay body"
        );
    })
    .await;
}

/// The event stream is delivered in order over the socket, and a fresh
/// connection resuming from a committed cursor receives only newer events.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_stream_delivers_then_resumes_by_sequence() {
    bounded(async {
        let run = SandboxRun::new([], expected_secret());
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
        let run = SandboxRun::with_config(
            PROTOCOL_VERSION,
            16,
            2,
            [Capability::ModelInference],
            expected_secret(),
        );
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

/// A resume that replays more buffered events than the outbound request-lane
/// queue depth still delivers every event: the writer drains the queue while the
/// replay fills it, rather than the replay wedging on a full channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_replays_more_events_than_the_request_lane_depth() {
    bounded(async {
        let run = SandboxRun::new([], expected_secret());
        // Buffer far more events than the outbound `data` queue depth
        // (MAX_INFLIGHT_REQUESTS = 16), the earlier deadlock's trigger.
        const COUNT: u64 = 40;
        for index in 0..COUNT {
            run.emit_progress(format!("event-{index}"))
                .await
                .expect("emit");
        }
        let addr = spawn_sandbox(run).await;

        let (host, _store) = host_with(Vec::new(), echo().0);
        let mut conn = connect(addr, host, EventCursor::START)
            .await
            .expect("attach accepted");
        let mut sequences = Vec::new();
        for _ in 0..COUNT {
            let event = conn.next_event().await.expect("stream open");
            sequences.push(event.sequence.get());
        }
        assert_eq!(
            sequences,
            (1..=COUNT).collect::<Vec<_>>(),
            "every buffered event replays past the queue depth, in order"
        );
    })
    .await;
}

/// Run-scoped state outlives the connection over the real wire: a reverse call
/// whose connection drops fails to the sandbox, its host-side execution keeps
/// running and records, and re-issuing the same operation identity on a fresh
/// connection replays the recorded outcome with the model still run exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reverse_call_survives_a_disconnect_and_reissue_replays_exactly_once() {
    bounded(async {
        let run = SandboxRun::new([Capability::ModelInference], expected_secret());
        let addr = spawn_sandbox(run.clone()).await;

        // A gated host model, shared run-scoped across both connections.
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let responder = Arc::new(GateResponder {
            gate: Arc::clone(&gate),
            started: Arc::clone(&started),
            finished: Arc::clone(&finished),
        });
        let (host, store) = host_with(vec![Capability::ModelInference], responder);

        let operation = OperationId::new();

        // Connection 1: issue a call, let the host start executing, then drop the
        // connection with the call still in flight.
        let conn1 = connect(addr, host.clone(), EventCursor::START)
            .await
            .expect("attach accepted");
        let inflight = {
            let run = run.clone();
            tokio::spawn(async move { run.call(operation, infer("job")).await })
        };
        while started.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(conn1);
        match inflight.await.expect("join") {
            ReverseOutcome::Disconnected => {}
            other => panic!("a dropped connection must fail the in-flight call, got {other:?}"),
        }

        // The detached host-side execution outlives the connection; release it so
        // it runs to completion and records.
        gate.add_permits(1);

        // Connection 2 over a fresh stream, same run-scoped host: re-issue the same
        // operation identity. Whether the record has landed or the execution is
        // still live, the host answers once — never a second execution.
        let _conn2 = connect(addr, host.clone(), EventCursor::START)
            .await
            .expect("reattach accepted");
        match run.call(operation, infer("job")).await {
            ReverseOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), "done:job");
            }
            other => panic!("expected the recorded outcome to replay, got {other:?}"),
        }
        assert_eq!(
            finished.load(Ordering::SeqCst),
            1,
            "the model executed exactly once across the disconnect and re-issue"
        );
        assert_eq!(store.len(), 1, "one operation-log record");
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

//! The protocol conformance suite.
//!
//! This is the CI artifact for the protocol per the design's Testing section:
//! one suite, shipped against the [in-process reference backend](crate::reference),
//! and — once the local container backend lands (delivery-sequence step 7.1) —
//! run against it too, by re-pointing these scenarios at that backend behind the
//! same seam. Each scenario is a standalone async function that constructs its
//! own fixture and panics on a contract violation, so `tests/conformance.rs` can
//! surface them as individual CI checks.
//!
//! The four contract areas the suite must cover, and where:
//!
//! - **version-mismatch refusal** — [`version_mismatch_is_refused`];
//! - **deny-by-default** — [`deny_by_default_refuses_ungranted_capability`];
//! - **the event-stream resumable-cursor contract** —
//!   [`event_stream_resumes_from_committed_cursor`] and
//!   [`event_buffer_overflow_checkpoints_and_resumes`];
//! - **reverse-RPC correlation / cancellation / disconnect-reissue** —
//!   [`reverse_rpc_correlates_concurrent_calls`],
//!   [`reverse_rpc_cancel_aborts_in_flight`],
//!   [`reverse_rpc_disconnect_fails_inflight_then_reissue_replays`], and
//!   [`reverse_rpc_reissue_with_a_different_request_conflicts`].
//!
//! Two further scenarios pin the provision/address/destroy decomposition
//! ([`self_hosted_and_managed_share_one_attach_path`]) and artifact collection
//! ([`artifact_collection_roundtrips_and_is_bounded`]).

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use tokio::sync::Semaphore;

use crate::{
    ids::{EventCursor, OperationId, RunId, Sequence},
    oplog::{InMemoryOperationStore, OperationStore},
    protocol::{
        AttachRequest, ErrorCode, Response, MAX_BUFFERED_EVENTS, MAX_EVENT_PAYLOAD_BYTES,
        MAX_MODEL_PROMPT_BYTES, PROTOCOL_VERSION,
    },
    provisioning::{ProvisionRequest, SandboxBackend, TransportSecret},
    reference::{ReferenceBackend, ReferenceSandbox, ReverseCallOutcome, Session},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceParams, ModelInferenceResult,
        ReverseRequest, ReverseResult, RunProvenance,
    },
    CapabilityHost,
};

/// A responder that answers instantly and counts how many times it ran — the
/// exactly-once witness for idempotent replay.
struct EchoResponder {
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl CapabilityResponder for EchoResponder {
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let ReverseRequest::ModelInference(params) = request;
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion: format!("echo:{}", params.prompt),
        }))
    }
}

/// A responder that blocks until the test releases a gate, so cancellation and
/// disconnect can be observed mid-flight.
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
        let ReverseRequest::ModelInference(params) = request;
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion: format!("done:{}", params.prompt),
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
    }
}

fn host_with(
    grants: Vec<Capability>,
    responder: Arc<dyn CapabilityResponder>,
) -> (CapabilityHost, InMemoryOperationStore) {
    let store = InMemoryOperationStore::new();
    let provenance = RunProvenance {
        run_id: RunId::new(),
        provider: "reference".to_owned(),
    };
    let host = CapabilityHost::new(
        GrantSet::new(provenance, grants),
        responder,
        Arc::new(store.clone()),
    );
    (host, store)
}

fn secret() -> TransportSecret {
    TransportSecret::new("per-run-transport-secret")
}

fn provision_request() -> ProvisionRequest {
    ProvisionRequest {
        run_id: RunId::new(),
        tag: crate::ids::SandboxTag::new(),
        lifetime_cap_secs: None,
        task: Some("the delegated task".to_owned()),
    }
}

fn attach_request() -> AttachRequest {
    AttachRequest {
        protocol_version: PROTOCOL_VERSION,
        run_id: RunId::new(),
        resume_from: EventCursor::START,
    }
}

/// Attach a session to a self-hosted reference sandbox at `base_url`.
async fn attach(
    backend: &ReferenceBackend,
    host: CapabilityHost,
    resume_from: EventCursor,
) -> Session {
    let handle = backend
        .provision(provision_request())
        .await
        .expect("provision");
    let address = backend.address(&handle).await.expect("address");
    backend
        .connect(
            &address,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id: RunId::new(),
                resume_from,
            },
            host,
        )
        .expect("connect")
}

/// A version-skewed peer is refused, never attached.
pub async fn version_mismatch_is_refused() {
    let sandbox = ReferenceSandbox::with_config(PROTOCOL_VERSION + 1, 8, 4);
    let backend = ReferenceBackend::self_hosted("inproc://skewed", secret(), sandbox);
    let handle = backend
        .provision(provision_request())
        .await
        .expect("provision");
    let address = backend.address(&handle).await.expect("address");

    let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);
    match backend.connect(&address, attach_request(), host) {
        Err(crate::reference::ConnectError::VersionRefused(refused)) => {
            // The skew answer is an on-wire refusal carrying the sandbox's own
            // version, and no session was established.
            assert_eq!(refused.protocol_version, PROTOCOL_VERSION + 1);
            assert_eq!(refused.code, ErrorCode::ProtocolVersion);
        }
        Err(other) => panic!("expected a version-mismatch refusal, got {other:?}"),
        Ok(_) => panic!("a version mismatch must refuse the connection"),
    }
}

/// An ungranted capability is refused and never executes.
pub async fn deny_by_default_refuses_ungranted_capability() {
    let (responder, executions) = echo();
    // Grant nothing.
    let (host, store) = host_with(Vec::new(), responder);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://deny", secret(), sandbox);
    let session = attach(&backend, host, EventCursor::START).await;
    assert!(
        session.accepted().granted_capabilities.is_empty(),
        "an ungranted run advertises no capabilities"
    );

    let outcome = control
        .issue_reverse(OperationId::new(), infer("nope"))
        .await;
    match outcome {
        ReverseCallOutcome::Settled(Response::Error(error)) => {
            assert_eq!(error.code, ErrorCode::Denied);
        }
        other => panic!("expected a deny-by-default refusal, got {other:?}"),
    }
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a denied request must never execute"
    );
    assert!(store.is_empty(), "a denied request records no operation");
}

/// The event stream is monotonic and gapless, and a resume returns only events
/// strictly newer than the committed cursor.
pub async fn event_stream_resumes_from_committed_cursor() {
    let (host, _store) = host_with(Vec::new(), echo().0);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    for letter in ["a", "b", "c", "d", "e"] {
        control.emit_progress(letter).expect("emit");
    }
    let backend = ReferenceBackend::self_hosted("inproc://events", secret(), sandbox);
    let session = attach(&backend, host, EventCursor::START).await;

    let all = session.events_since(EventCursor::START);
    let sequences: Vec<u64> = all
        .events
        .iter()
        .map(|event| event.sequence.get())
        .collect();
    assert_eq!(
        sequences,
        vec![1, 2, 3, 4, 5],
        "monotonic and gapless from one"
    );

    // Resume from a committed cursor: only newer events.
    let resumed = session.events_since(EventCursor::committed(Sequence::new(2)));
    let resumed_sequences: Vec<u64> = resumed
        .events
        .iter()
        .map(|event| event.sequence.get())
        .collect();
    assert_eq!(resumed_sequences, vec![3, 4, 5]);

    // A re-delivery after committing further discards what is at or below the
    // cursor and never skips.
    let redelivered = session.events_since(EventCursor::committed(Sequence::new(4)));
    let redelivered_sequences: Vec<u64> = redelivered
        .events
        .iter()
        .map(|event| event.sequence.get())
        .collect();
    assert_eq!(redelivered_sequences, vec![5]);
}

/// The un-acknowledged buffer overflow checkpoints and stops producing, and a
/// drain that advances the cursor lets production resume.
pub async fn event_buffer_overflow_checkpoints_and_resumes() {
    let (host, _store) = host_with(Vec::new(), echo().0);
    let sandbox = ReferenceSandbox::with_config(PROTOCOL_VERSION, 2, 4);
    let control = sandbox.control();
    control.emit_progress("one").expect("first fits");
    control.emit_progress("two").expect("second fits");
    assert_eq!(
        control.emit_progress("three"),
        Err(crate::reference::EmitError::Overflow),
        "a full buffer checkpoints rather than dropping"
    );

    let backend = ReferenceBackend::self_hosted("inproc://overflow", secret(), sandbox);
    let session = attach(&backend, host, EventCursor::START).await;
    let drained = session.events_since(EventCursor::START);
    assert_eq!(drained.events.len(), 2);

    // Draining advanced the cursor; production resumes.
    control
        .emit_progress("three")
        .expect("production resumes after the drain");
}

/// Overlapping reverse calls each receive their own response over one session.
pub async fn reverse_rpc_correlates_concurrent_calls() {
    let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://correlate", secret(), sandbox);
    let _session = attach(&backend, host, EventCursor::START).await;

    let mut tasks = Vec::new();
    for index in 0..5u32 {
        let control = control.clone();
        tasks.push(tokio::spawn(async move {
            let outcome = control
                .issue_reverse(OperationId::new(), infer(&format!("q{index}")))
                .await;
            (index, outcome)
        }));
    }
    for task in tasks {
        let (index, outcome) = task.await.expect("join");
        match outcome {
            ReverseCallOutcome::Settled(Response::Ok(result)) => {
                assert_eq!(completion(&result), format!("echo:q{index}"));
            }
            other => panic!("expected a correlated answer, got {other:?}"),
        }
    }
}

/// A control-lane cancel aborts the in-flight execution; the effect never
/// finishes and a re-issue returns the recorded `Cancelled`.
pub async fn reverse_rpc_cancel_aborts_in_flight() {
    let (responder, started, finished, gate) = gated();
    let (host, _store) = host_with(vec![Capability::ModelInference], responder);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://cancel", secret(), sandbox);
    let _session = attach(&backend, host, EventCursor::START).await;

    let operation = OperationId::new();
    let issue = {
        let control = control.clone();
        tokio::spawn(async move { control.issue_reverse(operation, infer("slow")).await })
    };
    while started.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    control.cancel_reverse(operation);
    match issue.await.expect("join") {
        ReverseCallOutcome::Settled(Response::Error(error)) => {
            assert_eq!(error.code, ErrorCode::Cancelled);
        }
        other => panic!("expected a cancelled response, got {other:?}"),
    }

    // Even once a permit is offered, the aborted execution never finished.
    gate.add_permits(1);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 0);
}

/// A disconnect fails the in-flight call while the host's execution keeps
/// running; re-issuing the same identity on a fresh session replays the recorded
/// answer and the effect ran exactly once.
pub async fn reverse_rpc_disconnect_fails_inflight_then_reissue_replays() {
    let (responder, started, finished, gate) = gated();
    let (host, store) = host_with(vec![Capability::ModelInference], responder);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://disconnect", secret(), sandbox);
    let handle = backend
        .provision(provision_request())
        .await
        .expect("provision");
    let address = backend.address(&handle).await.expect("address");

    // The run-scoped host outlives any single connection.
    let session1 = backend
        .connect(&address, attach_request(), host.clone())
        .expect("connect");
    let operation = OperationId::new();
    let issue = {
        let control = control.clone();
        tokio::spawn(async move { control.issue_reverse(operation, infer("job")).await })
    };
    while started.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Drop the connection with the request still executing.
    drop(session1);
    match issue.await.expect("join") {
        ReverseCallOutcome::Disconnected => {}
        other => panic!("expected a disconnect, got {other:?}"),
    }

    // The detached execution survives; let it finish and record.
    gate.add_permits(1);

    // Reconnect on the same address and run-scoped host, then re-issue.
    let _session2 = backend
        .connect(&address, attach_request(), host.clone())
        .expect("reconnect");

    let replayed = control.issue_reverse(operation, infer("job")).await;
    match replayed {
        ReverseCallOutcome::Settled(Response::Ok(result)) => {
            assert_eq!(completion(&result), "done:job");
        }
        other => panic!("expected a recorded replay, got {other:?}"),
    }
    assert_eq!(
        finished.load(Ordering::SeqCst),
        1,
        "the effect ran exactly once"
    );
    assert_eq!(store.len(), 1);
}

/// Re-issuing an operation identity with a different request is refused.
pub async fn reverse_rpc_reissue_with_a_different_request_conflicts() {
    let (host, _store) = host_with(vec![Capability::ModelInference], echo().0);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://conflict", secret(), sandbox);
    let _session = attach(&backend, host, EventCursor::START).await;

    let operation = OperationId::new();
    control.issue_reverse(operation, infer("first")).await;
    let conflict = control.issue_reverse(operation, infer("second")).await;
    match conflict {
        ReverseCallOutcome::Settled(Response::Error(error)) => {
            assert_eq!(error.code, ErrorCode::OperationIdConflict);
        }
        other => panic!("expected an operation-id conflict, got {other:?}"),
    }
}

/// The self-hosted backend (no provisioning) and the managed backend drive one
/// attach path with no special case.
pub async fn self_hosted_and_managed_share_one_attach_path() {
    // Managed: provision stands up a fresh sandbox.
    let managed = ReferenceBackend::managed(secret());
    drive_one_run(&managed, None).await;

    // Self-hosted: provision is a no-op over a pre-supplied endpoint.
    let sandbox = ReferenceSandbox::new();
    let self_hosted = ReferenceBackend::self_hosted("inproc://self-hosted", secret(), sandbox);
    drive_one_run(&self_hosted, Some("inproc://self-hosted")).await;
}

async fn drive_one_run(backend: &ReferenceBackend, endpoint: Option<&str>) {
    let (host, store) = host_with(vec![Capability::ModelInference], echo().0);
    let handle = backend
        .provision(provision_request())
        .await
        .expect("provision");
    let address = backend.address(&handle).await.expect("address");
    match endpoint {
        // A self-hosted backend's provision is a no-op: address must resolve to
        // the exact user-supplied endpoint, not a freshly minted one.
        Some(endpoint) => assert_eq!(
            address.base_url, endpoint,
            "self-hosted address routes to the user-supplied endpoint"
        ),
        // A managed backend mints its own reachable endpoint.
        None => assert!(
            address.base_url.starts_with("inproc://"),
            "managed backend provisions its own endpoint"
        ),
    }
    let control = backend
        .control(&handle)
        .expect("control resolves for both modes");
    let session = backend
        .connect(
            &address,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id: RunId::new(),
                resume_from: EventCursor::START,
            },
            host,
        )
        .expect("connect travels one path for both modes");

    let outcome = control
        .issue_reverse(OperationId::new(), infer("hello"))
        .await;
    assert!(
        matches!(outcome, ReverseCallOutcome::Settled(Response::Ok(_))),
        "a reverse call succeeds on both backends"
    );
    assert_eq!(store.len(), 1);
    drop(session);
    backend
        .destroy(&handle)
        .await
        .expect("destroy is idempotent for both modes");
}

/// Artifact collection round-trips a manifest and bounded content, and refuses a
/// missing name.
pub async fn artifact_collection_roundtrips_and_is_bounded() {
    let (host, _store) = host_with(Vec::new(), echo().0);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    control.put_artifact("report.md", b"hello".to_vec());
    control.put_artifact("data.bin", vec![1u8, 2, 3, 4]);
    let backend = ReferenceBackend::self_hosted("inproc://artifacts", secret(), sandbox);
    let session = attach(&backend, host, EventCursor::START).await;

    let manifest = session.collect_artifacts().expect("manifest within bounds");
    assert!(manifest.within_bounds());
    let names: Vec<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, vec!["data.bin", "report.md"]);

    let content = session.fetch_artifact("report.md").expect("fetch");
    assert_eq!(content.bytes, 5);
    use sha2::{Digest, Sha256};
    let expected: [u8; 32] = Sha256::digest(b"hello").into();
    assert_eq!(content.digest(), expected);

    let missing = session
        .fetch_artifact("nope")
        .expect_err("missing is refused");
    assert_eq!(missing.code, ErrorCode::NotFound);
}

/// An over-bound inbound reverse request is refused before it executes.
pub async fn over_bound_request_is_refused() {
    let (responder, executions) = echo();
    let (host, store) = host_with(vec![Capability::ModelInference], responder);
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://bound-req", secret(), sandbox);
    let _session = attach(&backend, host, EventCursor::START).await;

    let oversize = "x".repeat(MAX_MODEL_PROMPT_BYTES + 1);
    let outcome = control
        .issue_reverse(OperationId::new(), infer(&oversize))
        .await;
    match outcome {
        ReverseCallOutcome::Settled(Response::Error(error)) => {
            assert_eq!(error.code, ErrorCode::TooLarge);
        }
        other => panic!("expected an over-bound refusal, got {other:?}"),
    }
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "an over-bound request must never execute"
    );
    assert!(
        store.is_empty(),
        "an over-bound request records no operation"
    );
}

/// An over-bound event is refused rather than emitted.
pub async fn over_bound_event_is_refused() {
    let sandbox = ReferenceSandbox::new();
    let control = sandbox.control();
    let oversize = "y".repeat(MAX_EVENT_PAYLOAD_BYTES + 1);
    assert_eq!(
        control.emit_progress(oversize),
        Err(crate::reference::EmitError::TooLarge),
        "an over-bound event must be refused, not emitted"
    );
    // A within-bound event still emits, so the refusal is specific to the bound.
    control.emit_progress("ok").expect("a bounded event emits");
}

/// A control-lane cancel lands even while the request lane is saturated: the
/// reserved lane is not subject to request backpressure.
pub async fn control_lane_cancel_preempts_saturated_request_lane() {
    // The gate is never released: these calls stay in flight to keep the
    // request lane saturated, and are abandoned when the test ends.
    let (responder, started, finished, _gate) = gated();
    let (host, _store) = host_with(vec![Capability::ModelInference], responder);
    // A request lane with exactly two in-flight permits.
    let sandbox = ReferenceSandbox::with_config(PROTOCOL_VERSION, MAX_BUFFERED_EVENTS, 2);
    let control = sandbox.control();
    let backend = ReferenceBackend::self_hosted("inproc://saturated", secret(), sandbox);
    let _session = attach(&backend, host, EventCursor::START).await;

    // Saturate the request lane: two gated calls hold both permits.
    let target = OperationId::new();
    let call_a = {
        let control = control.clone();
        tokio::spawn(async move { control.issue_reverse(target, infer("a")).await })
    };
    let call_b = {
        let control = control.clone();
        tokio::spawn(async move { control.issue_reverse(OperationId::new(), infer("b")).await })
    };
    while started.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // A third call cannot even register: no permit is free while the lane is
    // saturated. It never reaches the responder.
    let call_c = {
        let control = control.clone();
        tokio::spawn(async move { control.issue_reverse(OperationId::new(), infer("c")).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        started.load(Ordering::SeqCst),
        2,
        "a third request is blocked behind request backpressure"
    );

    // The cancel travels the reserved control lane, acquires no request permit,
    // and lands despite the saturated request lane.
    control.cancel_reverse(target);
    match call_a.await.expect("join") {
        ReverseCallOutcome::Settled(Response::Error(error)) => {
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
}

/// Run every conformance scenario in sequence.
pub async fn run_all() {
    version_mismatch_is_refused().await;
    deny_by_default_refuses_ungranted_capability().await;
    over_bound_request_is_refused().await;
    over_bound_event_is_refused().await;
    event_stream_resumes_from_committed_cursor().await;
    event_buffer_overflow_checkpoints_and_resumes().await;
    reverse_rpc_correlates_concurrent_calls().await;
    reverse_rpc_cancel_aborts_in_flight().await;
    control_lane_cancel_preempts_saturated_request_lane().await;
    reverse_rpc_disconnect_fails_inflight_then_reissue_replays().await;
    reverse_rpc_reissue_with_a_different_request_conflicts().await;
    self_hosted_and_managed_share_one_attach_path().await;
    artifact_collection_roundtrips_and_is_bounded().await;
}

fn echo() -> (Arc<dyn CapabilityResponder>, Arc<AtomicUsize>) {
    let executions = Arc::new(AtomicUsize::new(0));
    let responder = Arc::new(EchoResponder {
        executions: Arc::clone(&executions),
    });
    (responder, executions)
}

fn gated() -> (
    Arc<dyn CapabilityResponder>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<Semaphore>,
) {
    let gate = Arc::new(Semaphore::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let responder = Arc::new(GateResponder {
        gate: Arc::clone(&gate),
        started: Arc::clone(&started),
        finished: Arc::clone(&finished),
    });
    (responder, started, finished, gate)
}

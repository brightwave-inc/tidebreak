//! Scenario tests for the reverse-RPC spike.
//!
//! Each test drives the real host and supervisor across a `tokio::io::duplex`
//! pipe and asserts one of the properties the go/no-go depends on: idempotent
//! replay across a reconnect, correlation over one connection, cancellation of
//! an in-flight request, backpressure against a slow host, and a disconnect
//! failing an in-flight request while leaving it safe to re-issue.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{sync::Semaphore, task::JoinHandle, time::timeout};

use openwave_reverse_rpc_spike::{
    model::Completion, serve_connection, Capability, CapabilityHost, ClientError,
    ModelInferenceParams, ModelInferenceResult, ModelProvider, OperationId, ReverseClient,
    ReverseRequest, ReverseResult,
};

/// A model that completes instantly and counts how many times it actually ran.
/// The counter is the exactly-once witness for idempotent replay.
struct EchoModel {
    executions: Arc<AtomicUsize>,
}

impl EchoModel {
    fn new() -> Self {
        Self {
            executions: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn executions(&self) -> usize {
        self.executions.load(Ordering::SeqCst)
    }
}

impl ModelProvider for EchoModel {
    fn complete(&self, params: ModelInferenceParams) -> Completion<'_> {
        let executions = Arc::clone(&self.executions);
        Box::pin(async move {
            executions.fetch_add(1, Ordering::SeqCst);
            ModelInferenceResult {
                completion: format!("echo:{}", params.prompt),
            }
        })
    }
}

/// A model whose completions block until the test releases a gate permit.
/// `started` counts entries; `finished` counts completions that ran to the end,
/// so a cancelled or disconnected completion is visible as started-but-unfinished.
struct GatedModel {
    gate: Arc<Semaphore>,
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
}

impl GatedModel {
    fn new() -> Self {
        Self {
            gate: Arc::new(Semaphore::new(0)),
            started: Arc::new(AtomicUsize::new(0)),
            finished: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Let one blocked completion run to the end.
    fn release_one(&self) {
        self.gate.add_permits(1);
    }
}

impl ModelProvider for GatedModel {
    fn complete(&self, params: ModelInferenceParams) -> Completion<'_> {
        let gate = Arc::clone(&self.gate);
        let started = Arc::clone(&self.started);
        let finished = Arc::clone(&self.finished);
        Box::pin(async move {
            started.fetch_add(1, Ordering::SeqCst);
            let permit = gate.acquire().await.expect("gate open");
            permit.forget();
            finished.fetch_add(1, Ordering::SeqCst);
            ModelInferenceResult {
                completion: format!("done:{}", params.prompt),
            }
        })
    }
}

fn infer(prompt: &str) -> ReverseRequest {
    ReverseRequest::ModelInference(ModelInferenceParams {
        prompt: prompt.to_owned(),
    })
}

fn completion(result: ReverseResult) -> String {
    match result {
        ReverseResult::ModelInference(result) => result.completion,
        #[allow(unreachable_patterns)]
        _ => panic!("unexpected reverse result variant"),
    }
}

/// Attach a fresh connection to `host`, returning the client and the server
/// task. Aborting the server task models the connection dropping.
fn connect(host: &CapabilityHost, max_in_flight: usize) -> (ReverseClient, JoinHandle<()>) {
    let (host_side, client_side) = tokio::io::duplex(64 * 1024);
    let host = host.clone();
    let server = tokio::spawn(async move {
        let _ = serve_connection(host, host_side).await;
    });
    let client = ReverseClient::connect(client_side, max_in_flight);
    (client, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idempotent_replay_across_reconnect() {
    let model = Arc::new(EchoModel::new());
    let host = CapabilityHost::new(
        Arc::clone(&model) as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    let operation = OperationId::new();

    let (client1, server1) = connect(&host, 4);
    let first = client1
        .call(operation, infer("forecast"))
        .await
        .expect("issue")
        .wait()
        .await
        .expect("first response");
    assert_eq!(completion(first), "echo:forecast");
    assert_eq!(model.executions(), 1);

    // The connection drops — as if the response was already delivered but the
    // host slept, or it was lost — and the supervisor reconnects.
    server1.abort();
    drop(client1);

    let (client2, _server2) = connect(&host, 4);
    let replayed = client2
        .call(operation, infer("forecast"))
        .await
        .expect("re-issue")
        .wait()
        .await
        .expect("replayed response");

    // Same recorded answer, and the model ran exactly once across both attempts.
    assert_eq!(completion(replayed), "echo:forecast");
    assert_eq!(
        model.executions(),
        1,
        "replay must not execute a second time"
    );
    assert_eq!(host.operation_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reissue_with_a_different_request_is_a_conflict() {
    let model = Arc::new(EchoModel::new());
    let host = CapabilityHost::new(
        model as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    let operation = OperationId::new();

    let (client, _server) = connect(&host, 4);
    client
        .call(operation, infer("first"))
        .await
        .expect("issue")
        .wait()
        .await
        .expect("response");

    let conflict = client
        .call(operation, infer("second"))
        .await
        .expect("issue")
        .wait()
        .await;
    match conflict {
        Err(ClientError::Rejected(error)) => {
            assert_eq!(
                error.code,
                openwave_reverse_rpc_spike::ErrorCode::OperationIdConflict
            );
        }
        other => panic!("expected an operation-id conflict, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_calls_correlate_to_their_own_responses() {
    let model = Arc::new(EchoModel::new());
    let host = CapabilityHost::new(
        model as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    let (client, _server) = connect(&host, 8);

    // Issue several overlapping calls, then await them. Each must get exactly
    // its own answer back over the one multiplexed connection.
    let mut calls = Vec::new();
    for index in 0..5 {
        let call = client
            .call(OperationId::new(), infer(&format!("q{index}")))
            .await
            .expect("issue");
        calls.push((index, call));
    }
    for (index, call) in calls {
        let result = call.wait().await.expect("response");
        assert_eq!(completion(result), format!("echo:q{index}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_aborts_an_in_flight_request() {
    let model = Arc::new(GatedModel::new());
    let host = CapabilityHost::new(
        Arc::clone(&model) as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    let (client, _server) = connect(&host, 4);

    let call = client
        .call(OperationId::new(), infer("slow"))
        .await
        .expect("issue");
    // Let the host reach the model and block on the closed gate.
    while model.started.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    call.cancel();
    match call.wait().await {
        Err(ClientError::Rejected(error)) => {
            assert_eq!(error.code, openwave_reverse_rpc_spike::ErrorCode::Cancelled);
        }
        other => panic!("expected a cancelled response, got {other:?}"),
    }

    // Even if a permit arrives now, the aborted completion never finished its
    // effect — cancellation actually interrupted the in-flight work.
    model.release_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(model.started.load(Ordering::SeqCst), 1);
    assert_eq!(model.finished.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backpressure_blocks_new_calls_until_the_host_drains() {
    let model = Arc::new(GatedModel::new());
    let host = CapabilityHost::new(
        Arc::clone(&model) as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    // Two in-flight permits; the host is paused (gate closed), so it drains
    // nothing until the test releases it.
    let (client, _server) = connect(&host, 2);

    let call1 = client
        .call(OperationId::new(), infer("a"))
        .await
        .expect("first");
    let call2 = client
        .call(OperationId::new(), infer("b"))
        .await
        .expect("second");

    // A third call cannot make progress: no permit is free while the host holds
    // both in flight. That bound is the sandbox's protection against unbounded
    // buffering behind a slow host.
    let blocked = timeout(
        Duration::from_millis(100),
        client.call(OperationId::new(), infer("c")),
    )
    .await;
    assert!(
        blocked.is_err(),
        "a third call must block under backpressure"
    );

    // Let the host drain the two in-flight calls, freeing their permits.
    model.release_one();
    model.release_one();
    assert_eq!(completion(call1.wait().await.expect("a")), "done:a");
    assert_eq!(completion(call2.wait().await.expect("b")), "done:b");

    // Now the third call proceeds.
    model.release_one();
    let third = timeout(
        Duration::from_secs(1),
        client.call(OperationId::new(), infer("c")),
    )
    .await
    .expect("permit freed")
    .expect("third issue");
    assert_eq!(completion(third.wait().await.expect("c")), "done:c");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_fails_inflight_and_reissue_returns_the_recorded_response() {
    let model = Arc::new(GatedModel::new());
    let host = CapabilityHost::new(
        Arc::clone(&model) as Arc<dyn ModelProvider>,
        vec![Capability::ModelInference],
    );
    let operation = OperationId::new();

    let (client1, server1) = connect(&host, 4);
    let call = client1.call(operation, infer("job")).await.expect("issue");
    while model.started.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Drop the connection with the request still executing.
    server1.abort();
    match call.wait().await {
        Err(ClientError::Disconnected) => {}
        other => panic!("expected a disconnect failure, got {other:?}"),
    }

    // The detached execution survives the disconnect; let it finish and record.
    model.release_one();

    // Reconnect and re-issue the same operation identity: the recorded outcome
    // comes back, and the model still ran exactly once.
    let (client2, _server2) = connect(&host, 4);
    let replayed = client2
        .call(operation, infer("job"))
        .await
        .expect("re-issue")
        .wait()
        .await
        .expect("recorded response");
    assert_eq!(completion(replayed), "done:job");
    assert_eq!(model.finished.load(Ordering::SeqCst), 1);
    assert_eq!(host.operation_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ungranted_capability_is_denied_by_default() {
    let model = Arc::new(EchoModel::new());
    // No capabilities granted.
    let host = CapabilityHost::new(Arc::clone(&model) as Arc<dyn ModelProvider>, Vec::new());
    let (client, _server) = connect(&host, 4);

    let denied = client
        .call(OperationId::new(), infer("nope"))
        .await
        .expect("issue")
        .wait()
        .await;
    match denied {
        Err(ClientError::Rejected(error)) => {
            assert_eq!(error.code, openwave_reverse_rpc_spike::ErrorCode::Denied);
        }
        other => panic!("expected a deny-by-default rejection, got {other:?}"),
    }
    assert_eq!(model.executions(), 0, "a denied request must never execute");
}

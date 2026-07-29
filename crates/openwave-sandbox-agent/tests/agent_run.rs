//! End-to-end: the in-container agent, driven over a real socket against a mock
//! host model.
//!
//! This runs the agent server in-process — the sandbox-side transport server and
//! the agent loop — with the host side dialing in over a TCP loopback and
//! answering model steps from a scripted mock model. It exercises the whole
//! sandbox-resident path the image packages: attach handshake, model inference
//! dialed back over reverse RPC, a local tool call, the event stream, and a
//! submitted result — and asserts exactly-once against the host's operation-log
//! seam (each model step runs once, one record per operation).
//!
//! The whole scenario runs under a wall-clock [`timeout`](tokio::time::timeout),
//! so a transport regression fails the test fast instead of hanging the suite.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use tokio::{net::TcpStream, time::Duration};

use openwave_sandbox_agent::run_agent;
use openwave_sandbox_protocol::{
    events::EventPayload,
    ids::RunId,
    oplog::{InMemoryOperationStore, OperationStore},
    protocol::{AttachRequest, Response, PROTOCOL_VERSION},
    reverse::{
        Capability, CapabilityResponder, GrantSet, ModelInferenceResult, ReverseRequest,
        ReverseResult, RunProvenance,
    },
    serve_connection, CapabilityHost, EventCursor, SandboxRun, TransportSecret, WireClient,
};

/// The per-run transport secret the sandbox expects and the host presents.
const SECRET: &str = "agent-run-transport-secret";

/// A mock host model that scripts the loop: it asks for the word-count tool on
/// the first step, then — once the transcript shows the tool's result — returns a
/// final answer. It counts executions so a test can assert exactly-once.
struct ScriptedModel {
    executions: Arc<AtomicUsize>,
}

const FINAL_ANSWER: &str = "counted the words in the sample";

#[async_trait::async_trait]
impl CapabilityResponder for ScriptedModel {
    async fn respond(&self, request: ReverseRequest) -> Response<ReverseResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let prompt = match request {
            ReverseRequest::ModelInference(params) => params.prompt,
            _ => unreachable!("only model inference is exercised"),
        };
        let completion = if prompt.contains("Tool word_count result") {
            FINAL_ANSWER.to_owned()
        } else {
            // Drive a real local tool call over the loop's directive protocol.
            "use-tool:word_count:{\"text\":\"alpha beta gamma delta\"}".to_owned()
        };
        Response::Ok(ReverseResult::ModelInference(ModelInferenceResult {
            completion,
        }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_infer_tool_and_submit_result_runs_exactly_once() {
    // Bound the whole scenario so a transport deadlock fails fast rather than
    // hanging the test runner.
    tokio::time::timeout(Duration::from_secs(20), scenario())
        .await
        .expect("agent scenario completed within its time bound");
}

async fn scenario() {
    // Sandbox side: the run and its transport server on a loopback port.
    let run = SandboxRun::new(
        [Capability::ModelInference],
        Some(TransportSecret::new(SECRET)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    {
        let run = run.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let run = run.clone();
                tokio::spawn(async move {
                    let _ = serve_connection(stream, run).await;
                });
            }
        });
    }

    // Host side: the operation-log-backed capability host, wired to the mock
    // model, that answers reverse calls over the connection.
    let executions = Arc::new(AtomicUsize::new(0));
    let store = InMemoryOperationStore::new();
    let host = CapabilityHost::new(
        GrantSet::new(
            RunProvenance {
                run_id: RunId::new(),
                provider: "agent-run-test".to_owned(),
            },
            [Capability::ModelInference],
        ),
        Arc::new(ScriptedModel {
            executions: Arc::clone(&executions),
        }),
        Arc::new(store.clone()),
    );

    let stream = TcpStream::connect(addr).await.expect("dial");
    let mut conn = WireClient::connect(
        stream,
        AttachRequest {
            protocol_version: PROTOCOL_VERSION,
            run_id: RunId::new(),
            resume_from: EventCursor::START,
            transport_secret: TransportSecret::new(SECRET),
        },
        host,
    )
    .await
    .expect("attach accepted");

    // Drive the agent loop; it dials its model steps back over the connection.
    let agent = tokio::spawn(async move { run_agent(run, "count the words in the sample").await });

    // Drain the event stream until the terminal result arrives.
    let mut progress = Vec::new();
    let result = loop {
        let event = conn
            .next_event()
            .await
            .expect("stream stays open until result");
        match event.payload {
            EventPayload::Progress(text) => progress.push(text),
            EventPayload::Result(text) => break text,
            _ => {}
        }
    };

    assert_eq!(result, FINAL_ANSWER, "the run submitted its final answer");
    // The loop actually ran the local tool: word_count("alpha beta gamma delta")
    // is 4, surfaced on a progress event.
    assert!(
        progress.iter().any(|line| line.contains("word_count -> 4")),
        "the sandbox-resident tool ran locally: {progress:?}"
    );

    let answer = agent
        .await
        .expect("agent task joins")
        .expect("agent run succeeds");
    assert_eq!(answer, FINAL_ANSWER);

    // Exactly-once seam: two model steps, two distinct operations, each recorded
    // once and executed once.
    assert_eq!(
        executions.load(Ordering::SeqCst),
        2,
        "each model step executed exactly once"
    );
    assert_eq!(store.len(), 2, "one operation-log record per model step");
}

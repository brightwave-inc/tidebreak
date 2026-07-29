//! Tests for the sandbox-resident container driver.
//!
//! The non-Docker tests drive the **real** in-container agent loop
//! (`openwave_sandbox_agent::run_agent` plus its `word_count` tool and the
//! sandbox transport server) over a real loopback TCP socket, with only Docker
//! (the [`SandboxBackend`]) and the host model (the [`ProviderResolver`]) mocked.
//! That exercises the whole stack — provision, attach, reverse-RPC model
//! inference answered by the host proxy through the durable op-log, event drain,
//! fenced result commit, teardown — without a container runtime. The Docker
//! end-to-end test at the bottom re-points the same driver at a real container
//! and is skipped cleanly when no daemon is present.
//!
//! Every socket test is wrapped in a timeout so a regression fails loudly rather
//! than hanging CI, and every test uses the multi-thread runtime the durable
//! operation store's `block_in_place` bridge requires.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use openwave_core::{
    AdmitSandboxAgentRunOutcome, AgentRun, AgentRunExecutionLocation, AgentRunId, AgentRunStatus,
    CallId, Chat, ChatId, ChatRequest, DbStore, ModelProvider, ProviderEvent, ProviderId, Result,
    StopReason, Store,
};
use openwave_sandbox_agent::run_agent;
use openwave_sandbox_protocol::{
    ids::{OperationId, RunId},
    protocol::{Response, PROTOCOL_VERSION},
    reverse::{
        Capability, GrantSet, ModelInferenceParams, ReverseEnvelope, ReverseRequest, ReverseResult,
        RunProvenance,
    },
    serve_connection, BackendError, CapabilityHost, ProvisionRequest, SandboxAddress,
    SandboxBackend, SandboxHandle, SandboxRun, TransportSecret,
};
use tokio::net::TcpListener;
use uuid::Uuid;

use super::{
    HostModelProxy, SandboxContainerRunConfig, SandboxContainerRunOutcome, SandboxContainerRunner,
};
use crate::durable_oplog::DurableOperationStore;
use crate::resolver::ProviderResolver;

// --- Mock host model (the resolver the driver proxies inference through) ------

/// A provider that scripts one directive step then a final answer, counting how
/// many completions it is asked for. Drives the real in-container loop: the first
/// completion tells the sandbox to run `word_count`, the second is the final
/// result the sandbox submits.
struct ScriptedProvider {
    completions: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl ScriptedProvider {
    fn new(completions: Vec<String>) -> Self {
        Self {
            completions: Mutex::new(completions),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted-host-model")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let text = self
            .completions
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| "the final answer".to_owned());
        Ok(stream::iter(vec![
            ProviderEvent::TextDelta { text },
            ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            },
        ])
        .boxed())
    }
}

/// A resolver that always hands back the same scripted provider, so completion
/// counts are observable and the op-log's exactly-once holds across a re-issue.
struct FixedResolver(Arc<ScriptedProvider>);

#[async_trait]
impl ProviderResolver for FixedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

// --- Mock Docker backend ------------------------------------------------------

/// A [`SandboxBackend`] that stands in for Docker: `provision` and `destroy`
/// only record, and `address` returns a fixed loopback address the test's own
/// sandbox listener is bound to (or an unreachable one, to exercise failure).
struct MockBackend {
    address: Mutex<Option<String>>,
    provisions: AtomicUsize,
    destroys: AtomicUsize,
}

impl MockBackend {
    fn reachable(base_url: String) -> Arc<Self> {
        Arc::new(Self {
            address: Mutex::new(Some(base_url)),
            provisions: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl SandboxBackend for MockBackend {
    async fn provision(
        &self,
        request: ProvisionRequest,
    ) -> std::result::Result<SandboxHandle, BackendError> {
        self.provisions.fetch_add(1, Ordering::SeqCst);
        Ok(SandboxHandle {
            reference: format!("mock-{}", request.run_id),
            tag: request.tag,
        })
    }

    async fn address(
        &self,
        _handle: &SandboxHandle,
    ) -> std::result::Result<SandboxAddress, BackendError> {
        let Some(base_url) = self.address.lock().unwrap().clone() else {
            return Err(BackendError::Unaddressable("no address".to_owned()));
        };
        Ok(SandboxAddress {
            base_url,
            transport_secret: TransportSecret::new("test-secret"),
        })
    }

    async fn destroy(&self, _handle: &SandboxHandle) -> std::result::Result<(), BackendError> {
        self.destroys.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// --- Store / admission fixture ------------------------------------------------

async fn store() -> (tempfile::TempDir, Arc<dyn Store>, Chat) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap(),
    );
    let chat = Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("container".into()),
        model: Some("host-model".into()),
        reasoning_effort: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    store.create_chat(&chat).await.unwrap();
    (dir, store, chat)
}

/// Admit one container-located sandbox child under the chat's running turn,
/// mirroring how a foreground turn admits a sandbox child — but at the container
/// execution location.
async fn admit_container_run(store: &Arc<dyn Store>, chat_id: ChatId, task: &str) -> AgentRunId {
    let turn_id = openwave_core::TurnId::new();
    store
        .accept_turn(turn_id, chat_id, "host-model", "delegate a container run")
        .await
        .unwrap();
    let lease = Uuid::new_v4();
    let now = chrono::Utc::now();
    let turn = store
        .claim_turn_run(lease, now, now + chrono::Duration::hours(1))
        .await
        .unwrap()
        .turn
        .expect("test turn should claim");
    let call = CallId::new();
    match store
        .admit_sandbox_container_agent_run(
            turn.id,
            call,
            task,
            lease,
            turn.steer_revision,
            AgentRun::MAX_CONCURRENCY_LIMIT,
            chrono::Utc::now(),
        )
        .await
        .unwrap()
        .expect("container admission should resolve")
    {
        AdmitSandboxAgentRunOutcome::Accepted { child, .. }
        | AdmitSandboxAgentRunOutcome::Existing { child, .. } => child.id,
        outcome => panic!("unexpected container admission: {outcome:?}"),
    }
}

/// Bind a loopback listener and serve the real in-container agent behind it: the
/// transport server against a fresh [`SandboxRun`], plus the agent loop that
/// dials model completions back over the reverse channel. Returns the bound
/// `http://` base URL.
async fn spawn_sandbox_agent(task: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let run = SandboxRun::new([Capability::ModelInference]);
    let agent_run = run.clone();
    let task = task.to_owned();
    tokio::spawn(async move {
        let _ = run_agent(agent_run, task).await;
    });
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let run = run.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, run).await;
            });
        }
    });
    base_url
}

fn fast_config() -> SandboxContainerRunConfig {
    SandboxContainerRunConfig {
        lease: Duration::from_secs(30),
        dial_timeout: Duration::from_secs(2),
        max_tokens: 256,
        reattach_attempts: 2,
        reattach_backoff: Duration::from_millis(10),
    }
}

// --- Tests --------------------------------------------------------------------

/// The whole stack over loopback: admit a container run, drive it with the real
/// in-container agent loop answering `word_count` and dialing the host for model
/// inference, and assert the host committed the result exactly once, proxied
/// each model step through the resolver, and tore the container down.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drives_a_container_run_end_to_end_over_loopback() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "count some words").await;

        let base_url = spawn_sandbox_agent("count some words").await;
        let backend = MockBackend::reachable(base_url);
        // Step 1: run word_count on three words. Step 2: the final answer.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:word_count:{\"text\":\"one two three\"}".to_owned(),
            "the text has three words".to_owned(),
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner =
            SandboxContainerRunner::new(store.clone(), backend.clone(), resolver, fast_config());
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

        // The host committed the container's final result exactly once, through
        // the fenced result path.
        let committed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(committed.status, AgentRunStatus::Completed);

        // Both model steps were proxied through the host resolver (no model
        // credential in the container), and the op-log recorded each exactly once.
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "each model step should proxy through the host exactly once"
        );

        // The container was provisioned once and torn down.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// A model-inference call re-issued after a reconnect is answered from the
/// durable op-log, not executed a second time: the host proxy runs once and the
/// re-issue replays its recorded completion. This is the reverse-RPC exactly-once
/// guarantee the reattachment path depends on, at the seam this slice adds
/// (the host proxy over the durable operation store).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_model_proxy_answers_a_reissued_inference_from_the_op_log() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, _chat) = store().await;
        let run_id = RunId::new();
        let provider = Arc::new(ScriptedProvider::new(vec!["the-answer".to_owned()]));
        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id,
                    provider: "test".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(HostModelProxy {
                resolver: Arc::new(FixedResolver(provider.clone())),
                model: "host-model".to_owned(),
                max_tokens: 256,
            }),
            Arc::new(DurableOperationStore::new(store.clone(), run_id)),
        );

        let operation_id = OperationId::new();
        let request = ReverseRequest::ModelInference(ModelInferenceParams {
            prompt: "one step".to_owned(),
        });
        let envelope = || ReverseEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: openwave_sandbox_protocol::ids::RequestId::new(),
            operation_id,
            request: request.clone(),
        };

        let first = host.dispatch(envelope()).wait().await;
        // A re-issue with the same operation identity (a reconnect) — a fresh
        // RequestId, the same OperationId.
        let replay = host.dispatch(envelope()).wait().await;

        let expect = Response::Ok(ReverseResult::ModelInference(
            openwave_sandbox_protocol::reverse::ModelInferenceResult {
                completion: "the-answer".to_owned(),
            },
        ));
        assert_eq!(first, expect);
        assert_eq!(
            replay, expect,
            "the re-issue must replay the recorded answer"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "a re-issued inference must not spend a second model call"
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// A container that never becomes reachable is failed terminally after the
/// reattach budget, and its teardown obligation is still driven to completion —
/// a sandbox-resident run has exactly one attempt and is never re-executed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fails_terminally_and_tears_down_when_the_container_is_unreachable() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "unreachable").await;

        // A loopback port with nothing listening: every dial fails, so the driver
        // exhausts its reattach budget and fails the run.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead.local_addr().unwrap();
        drop(dead);
        let backend = MockBackend::reachable(format!("http://{dead_addr}"));
        let resolver = Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![]))));

        let runner =
            SandboxContainerRunner::new(store.clone(), backend.clone(), resolver, fast_config());
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));

        let failed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
        // The teardown obligation was driven even though the run failed.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Admission routes a container run to the container execution location: the
/// in-process scheduler leaves it, and only the container claim picks it up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_routes_to_the_container_location_not_the_in_process_scheduler() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "routed").await;

        let admitted = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(
            admitted.execution_location,
            AgentRunExecutionLocation::Container
        );
        assert_eq!(admitted.status, AgentRunStatus::Queued);
        // One attempt only: the run tier's retry machinery does not apply.
        assert_eq!(admitted.max_attempts, 1);

        // The in-process scheduler must not claim a container run.
        assert!(
            store
                .claim_agent_run(Uuid::new_v4(), chrono::Duration::minutes(5), 4, 4)
                .await
                .unwrap()
                .is_none(),
            "the in-process scheduler must leave a container run for the driver"
        );

        // The container claim transitions it to running under a lease.
        let lease = Uuid::new_v4();
        let claimed = store
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
            .expect("the container claim should pick up the queued run");
        assert_eq!(claimed.id, run_id);
        assert_eq!(claimed.status, AgentRunStatus::Running);
        assert_eq!(claimed.lease_token, Some(lease));

        // Re-claiming with the same token recovers the same live claim, never a
        // second attempt.
        let reclaimed = store
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
            .expect("reusing the token recovers the live claim");
        assert_eq!(reclaimed.id, run_id);
        assert_eq!(reclaimed.attempt_count, claimed.attempt_count);
    })
    .await
    .expect("test completed within its time bound");
}

// --- Docker end-to-end (gated on a container runtime + the agent image) -------

/// The full stack on a real Docker container: build the `openwave-sandbox-agent`
/// image, admit a container run, and drive it end to end — provision a container
/// from the image, attach over its published loopback port, answer its
/// `word_count`-then-final model steps from a mock host model, and assert the
/// result committed exactly once and the container was torn down.
///
/// Skipped cleanly when no container runtime or daemon is present (there is none
/// in the unit-test sandbox); CI runners have Docker. Building the image is heavy
/// (it compiles the agent crate's slice of the workspace inside Docker), so this
/// is the one place that pays for it, guarded behind daemon detection and a long
/// timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Docker daemon and builds the agent image; run explicitly or in the Docker CI lane"]
async fn docker_end_to_end_drives_a_real_container() {
    use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend, RUN_TAG_LABEL};

    let backend_probe = DockerSandboxBackend::with_defaults();
    if !backend_probe.is_available() {
        eprintln!("skipping: no container runtime on PATH");
        return;
    }

    // Build the agent image from the workspace root (the Dockerfile's build
    // context is the whole workspace, so Cargo.lock is visible to `--locked`).
    let image = "openwave-sandbox-agent:it";
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let build = tokio::time::timeout(
        Duration::from_secs(1800),
        tokio::process::Command::new("docker")
            .current_dir(&workspace_root)
            .args([
                "build",
                "-f",
                "crates/openwave-sandbox-agent/Dockerfile",
                "-t",
                image,
                ".",
            ])
            .output(),
    )
    .await;
    match build {
        Ok(Ok(output)) if output.status.success() => {}
        Ok(Ok(output)) => {
            panic!(
                "building the agent image failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(Err(error)) => {
            eprintln!("skipping: could not invoke docker build: {error}");
            return;
        }
        Err(_) => panic!("building the agent image timed out"),
    }

    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "count these four words now").await;

    let backend = Arc::new(DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    }));
    let provider = Arc::new(ScriptedProvider::new(vec![
        "use-tool:word_count:{\"text\":\"count these four words\"}".to_owned(),
        "the count is four".to_owned(),
    ]));
    let resolver = Arc::new(FixedResolver(provider.clone()));

    let runner = SandboxContainerRunner::new(
        store.clone(),
        backend,
        resolver,
        SandboxContainerRunConfig {
            dial_timeout: Duration::from_secs(30),
            ..SandboxContainerRunConfig::default()
        },
    );
    let outcome = tokio::time::timeout(Duration::from_secs(120), runner.drive(run_id))
        .await
        .expect("driving a real container completes within its bound")
        .expect("driving succeeds")
        .expect("the container run is claimable");
    assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

    let committed = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(committed.status, AgentRunStatus::Completed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

    // The container was torn down: no container carrying this run's tag remains.
    let listed = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label={RUN_TAG_LABEL}"),
            "--filter",
            &format!("label=openwave.run-id={run_id}"),
            "--format",
            "{{.ID}}",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&listed.stdout).trim().is_empty(),
        "the container should have been torn down"
    );
}

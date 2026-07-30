//! Tests for the sandbox-resident container driver.
//!
//! The non-Docker tests drive the **real** in-container agent loop
//! (`openwave_sandbox_agent::run_agent` plus its sandbox-resident tool registry
//! and the sandbox transport server) over a real loopback TCP socket, with only Docker
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

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use openwave_core::{
    AdmitSandboxAgentRunOutcome, AgentConfig, AgentRun, AgentRunExecutionLocation, AgentRunId,
    AgentRunStatus, CallId, Chat, ChatId, ChatRequest, DbStore, ModelProvider, ProviderEvent,
    ProviderId, Result, StopReason, Store,
};
use openwave_sandbox_agent::run_agent;
use openwave_sandbox_protocol::{
    ids::{OperationId, RunId, SandboxTag},
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
/// completion tells the sandbox to run a filesystem tool, the second is the
/// final result the sandbox submits.
struct ScriptedProvider {
    completions: Mutex<Vec<String>>,
    calls: AtomicUsize,
    /// Every prompt the sandbox asked the host to complete. The sandbox's
    /// transcript opens with `Task: <task>`, so this is where a test reads back
    /// which task the container actually received.
    prompts: Mutex<Vec<String>>,
    /// How long each completion stalls, so a test can hold a drive open across
    /// several lease periods.
    delay: Duration,
}

impl ScriptedProvider {
    fn new(completions: Vec<String>) -> Self {
        Self {
            completions: Mutex::new(completions),
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
        }
    }

    /// The same provider, but stalling `delay` before answering each completion,
    /// so a drive spans several lease periods.
    fn slow(completions: Vec<String>, delay: Duration) -> Self {
        Self {
            delay,
            ..Self::new(completions)
        }
    }

    fn first_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted-host-model")
    }

    async fn stream(&self, request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        for message in &request.messages {
            for block in &message.content {
                if let openwave_core::ContentBlock::Text { text } = block {
                    self.prompts.lock().unwrap().push(text.clone());
                }
            }
        }
        let text = self
            .completions
            .lock()
            .unwrap()
            .get(index)
            .cloned()
            .unwrap_or_else(|| "the final answer".to_owned());
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
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

/// A [`SandboxBackend`] that stands in for Docker.
///
/// `provision` starts the real in-container agent on a loopback listener with
/// no task — exactly as the image starts. The task only ever arrives in the
/// run-init frame the driver sends after attach, so task delivery is genuinely
/// testable: a driver that failed to send init leaves the agent parked and the
/// test times out, and the prompt the agent asks the host to complete proves
/// which task it actually received.
struct MockBackend {
    /// When set, `address` resolves here instead of the provisioned sandbox —
    /// used to point the driver at an unreachable port, or at a sandbox the
    /// test started itself for the reconcile path.
    address_override: Option<String>,
    /// The loopback address of the sandbox started at provision.
    address: Mutex<Option<String>>,
    provisions: AtomicUsize,
    destroys: AtomicUsize,
    /// While set, `destroy` refuses to confirm — the unconfirmed-teardown case.
    failing_destroys: std::sync::atomic::AtomicBool,
    /// Every live-tag set `reclaim_orphans` was asked to preserve.
    reclaim_live_sets:
        Mutex<Vec<std::collections::HashSet<openwave_sandbox_protocol::ids::SandboxTag>>>,
}

impl MockBackend {
    /// A backend that starts the real agent on provision, carrying whatever task
    /// the driver delivered.
    fn spawning() -> Arc<Self> {
        Arc::new(Self {
            address_override: None,
            address: Mutex::new(None),
            provisions: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            failing_destroys: std::sync::atomic::AtomicBool::new(false),
            reclaim_live_sets: Mutex::new(Vec::new()),
        })
    }

    /// A backend whose containers are never reachable at `base_url`.
    fn unreachable(base_url: String) -> Arc<Self> {
        Arc::new(Self {
            address_override: Some(base_url),
            address: Mutex::new(None),
            provisions: AtomicUsize::new(0),
            destroys: AtomicUsize::new(0),
            failing_destroys: std::sync::atomic::AtomicBool::new(false),
            reclaim_live_sets: Mutex::new(Vec::new()),
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
        if self.address_override.is_none() {
            // Start the sandbox exactly as the image does: with no task at all.
            // The agent waits for the run-init frame, so a driver that never
            // delivers one leaves the loop parked and the test times out — the
            // failure this models.
            *self.address.lock().unwrap() = Some(spawn_sandbox_agent().await);
        }
        Ok(SandboxHandle {
            reference: format!("mock-{}", request.run_id),
            tag: request.tag,
        })
    }

    async fn address(
        &self,
        _handle: &SandboxHandle,
    ) -> std::result::Result<SandboxAddress, BackendError> {
        let base_url = match &self.address_override {
            Some(base_url) => base_url.clone(),
            None => self
                .address
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| BackendError::Unaddressable("not provisioned".to_owned()))?,
        };
        Ok(SandboxAddress {
            base_url,
            transport_secret: TransportSecret::new("test-secret"),
        })
    }

    async fn destroy(&self, _handle: &SandboxHandle) -> std::result::Result<(), BackendError> {
        if self.failing_destroys.load(Ordering::SeqCst) {
            return Err(BackendError::Teardown("destroy refused by test".to_owned()));
        }
        self.destroys.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn reclaim_orphans(
        &self,
        live_tags: &std::collections::HashSet<openwave_sandbox_protocol::ids::SandboxTag>,
    ) -> std::result::Result<Vec<SandboxHandle>, BackendError> {
        self.reclaim_live_sets
            .lock()
            .unwrap()
            .push(live_tags.clone());
        Ok(Vec::new())
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
        permission_mode: None,
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
async fn spawn_sandbox_agent() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    // The supervisor expects the same per-run secret the MockBackend hands the
    // driver from `address()`, so the driver's authenticated attach is accepted.
    let run = SandboxRun::new(
        [Capability::ModelInference],
        Some(TransportSecret::new("test-secret")),
    );
    let agent_run = run.clone();
    // The in-container tool surface is rooted at a workspace directory; give the
    // loop a private temp one that lives as long as the run.
    let workspace = tempfile::tempdir().unwrap();
    let workspace_path = workspace.path().to_path_buf();
    tokio::spawn(async move {
        let _workspace = workspace;
        // As in the image's entrypoint: the loop starts only once the host
        // delivers the run init.
        let init = agent_run.init().await;
        let _ = run_agent(agent_run, init.task, workspace_path).await;
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
        heartbeat: Duration::from_secs(5),
        dial_timeout: Duration::from_secs(2),
        reattach_attempts: 2,
        reattach_backoff: Duration::from_millis(10),
        provision_window: Duration::from_secs(120),
        max_concurrent_containers: 4,
        max_inference_operations: 24,
    }
}

// --- Tests --------------------------------------------------------------------

/// The whole stack over loopback: admit a container run, drive it with the real
/// in-container agent loop running a sandbox filesystem tool and dialing the host
/// for model inference, and assert the host committed the result exactly once,
/// proxied each model step through the resolver, delivered the run's ACTUAL task,
/// and tore the container down.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drives_a_container_run_end_to_end_over_loopback() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "count the words in this delegated sentence";
        let run_id = admit_container_run(&store, chat.id, task).await;

        // The backend starts the sandbox on whatever task the driver delivered,
        // exactly as Docker starts the container from its environment.
        let backend = MockBackend::spawning();
        // Step 1: write a workspace file. Step 2: the final answer.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"delegated\"}".to_owned(),
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

        // The run's ACTUAL delegated task reached the container over the
        // run-init frame and is what the container asked the host to reason
        // about.
        assert!(
            provider.first_prompt().contains(task),
            "the container must work on the delegated task, got prompt: {}",
            provider.first_prompt()
        );

        // The container was provisioned once and torn down.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 1);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// An agent loop that ends WITHOUT submitting a result — it exhausts its step
/// budget — still terminalizes the run and tears the container down.
///
/// This is the container-leak case a reachable-but-resultless container creates:
/// the supervisor keeps serving after the agent loop returns, so a driver that
/// only watched for a result would wait on the open socket forever, never tear
/// down, and leave the run to be reaped. The agent's terminal failure event is
/// what closes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminalizes_and_tears_down_when_the_agent_loop_ends_without_a_result() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "never finishes").await;

        let backend = MockBackend::spawning();
        // Every completion is another tool directive, so the loop never submits a
        // final answer and exhausts MAX_STEPS.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a\"}".to_owned();
            16
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

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
        assert_eq!(
            failed.last_error_code.as_deref(),
            Some("sandbox_agent_failed"),
            "a loop that ended without a result is an agent failure, not a transport one"
        );
        // The container did not leak: teardown ran even though no result arrived.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The host stops answering model inference at the run's spend budget, no matter
/// how many reverse calls the sandbox issues.
///
/// The in-container step limit is enforced by untrusted code, so it bounds
/// nothing; this is the host-side cap #920 requires before anything routes to
/// the container location. The refusal is non-retryable, so the sandbox's failed
/// model step terminalizes the run and the container is torn down — and the
/// provider must have been asked for exactly the budgeted number of completions,
/// not one more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stops_proxying_inference_at_the_runs_spend_budget() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "spends forever").await;

        let backend = MockBackend::spawning();
        // Every completion is another tool directive: left alone, the loop would
        // take all of its 8 in-container steps. The host's budget of 2 must cut
        // it off first.
        let provider = Arc::new(ScriptedProvider::new(vec![
            "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a\"}".to_owned();
            16
        ]));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            resolver,
            SandboxContainerRunConfig {
                max_inference_operations: 2,
                ..fast_config()
            },
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Failed(run_id));

        let failed = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(failed.status, AgentRunStatus::Failed);
        assert_eq!(
            failed.last_error_code.as_deref(),
            Some("sandbox_agent_failed")
        );

        // The user's credentials were spent exactly the budgeted number of
        // times; the third request was refused before reaching the provider.
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "the host must not answer inference past the run's budget"
        );
        // The refusal closed the run rather than leaking the container.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The driver keeps the run's lease live while the container works, so a run that
/// outlives one lease period is not reaped mid-flight.
///
/// The lease here is deliberately short and the heartbeat shorter: the drive is
/// held open past several lease periods, and the run must still be `running` with
/// a lease extended beyond its original expiry rather than terminalized.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_the_lease_so_a_long_run_is_not_reaped() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "takes a while").await;

        let backend = MockBackend::spawning();
        // A provider that stalls each completion keeps the container working, so
        // the drive stays open across several lease periods.
        let provider = Arc::new(ScriptedProvider::slow(
            vec![
                "use-tool:write_file:{\"path\":\"note.txt\",\"content\":\"a b\"}".to_owned(),
                "done".to_owned(),
            ],
            Duration::from_millis(700),
        ));
        let resolver = Arc::new(FixedResolver(provider.clone()));

        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            resolver,
            SandboxContainerRunConfig {
                lease: Duration::from_secs(2),
                heartbeat: Duration::from_millis(100),
                ..fast_config()
            },
        );

        // Observe the lease while the drive runs: it must be extended past the
        // original 2s expiry rather than left to lapse.
        let observer = {
            let store = store.clone();
            tokio::spawn(async move {
                let mut seen: Vec<chrono::DateTime<chrono::Utc>> = Vec::new();
                for _ in 0..30 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(Some(run)) = store.get_agent_run(run_id).await {
                        if let Some(expiry) = run.lease_expires_at {
                            seen.push(expiry);
                        }
                        if run.status != AgentRunStatus::Running {
                            break;
                        }
                    }
                }
                seen
            })
        };

        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        // The run completed normally rather than being reaped out from under the
        // still-working container.
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));

        let seen = observer.await.unwrap();
        assert!(
            seen.windows(2).any(|pair| pair[1] > pair[0]),
            "the lease must be extended while the container works, saw: {seen:?}"
        );
    })
    .await
    .expect("test completed within its time bound");
}

/// A container run is exempt from the in-process lease reaper: its lease
/// expiring does not terminalize it, because no in-process worker holds it and
/// the container may still be working and spending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_in_process_reaper_leaves_an_expired_container_lease_alone() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "reaper bait").await;

        // Claim with a lease that expires immediately, then let the in-process
        // scheduler scan. Its lease reaper would otherwise fail this run (a
        // container run has max_attempts = 1, so attempt_count >= max_attempts
        // the moment it is claimed).
        let lease = Uuid::new_v4();
        store
            .claim_container_agent_run(run_id, lease, chrono::Duration::milliseconds(1), 4)
            .await
            .unwrap()
            .expect("the container claim should pick up the queued run");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let _ = store
            .claim_agent_run(Uuid::new_v4(), chrono::Duration::minutes(5), 4, 4)
            .await
            .unwrap();

        let after = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(
            after.status,
            AgentRunStatus::Running,
            "the in-process reaper must not terminalize a container run on lease expiry"
        );
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
                config: AgentConfig {
                    model: "host-model".to_owned(),
                    ..AgentConfig::default()
                },
                spent: AtomicU32::new(0),
                budget: 24,
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
        let backend = MockBackend::unreachable(format!("http://{dead_addr}"));
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
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5), 4)
            .await
            .unwrap()
            .expect("the container claim should pick up the queued run");
        assert_eq!(claimed.id, run_id);
        assert_eq!(claimed.status, AgentRunStatus::Running);
        assert_eq!(claimed.lease_token, Some(lease));

        // Re-claiming with the same token recovers the same live claim, never a
        // second attempt.
        let reclaimed = store
            .claim_container_agent_run(run_id, lease, chrono::Duration::minutes(5), 4)
            .await
            .unwrap()
            .expect("reusing the token recovers the live claim");
        assert_eq!(reclaimed.id, run_id);
        assert_eq!(reclaimed.attempt_count, claimed.attempt_count);
    })
    .await
    .expect("test completed within its time bound");
}

// --- Durable provisioning records (issue #920) --------------------------------

/// The commit predicate: a handle commit that arrives after the window lapsed
/// finds the intent already disowned and must not resurrect it — the driver
/// that holds the late container destroys it instead of running on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lapsed_intent_refuses_a_late_handle_commit() {
    let (_dir, store, _chat) = store().await;
    let run_uuid = Uuid::new_v4();
    let tag = SandboxTag::new();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .begin_sandbox_provision(run_uuid, &tag.to_string(), expired)
            .await
            .unwrap(),
        openwave_core::BeginSandboxProvisionOutcome::Started
    ));

    let lapsed = store
        .lapse_sandbox_provisions(chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].run_id, run_uuid);

    // The create returns late: its commit must lose.
    assert!(!store
        .commit_sandbox_provision_handle(run_uuid, "late-container")
        .await
        .unwrap());
    // The disowned intent owes a teardown, and its tag is no longer live.
    assert_eq!(store.list_sandbox_teardowns().await.unwrap().len(), 1);
    assert!(store.live_sandbox_tags().await.unwrap().is_empty());
}

/// The sweep converges a crash between provision and handle commit: the lapsed
/// intent is disowned, the tag sweep is asked to preserve only live tags, and
/// the handle-less obligation completes once the backend proves nothing outside
/// them remains. An unlapsed intent stays live throughout, so the sweep can
/// never race a slow in-flight create.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_reclaims_a_lapsed_intent_and_preserves_live_ones() {
    let (_dir, store, _chat) = store().await;
    let dead_tag = SandboxTag::new();
    let live_tag = SandboxTag::new();
    let expired = chrono::Utc::now() - chrono::Duration::seconds(1);
    let open = chrono::Utc::now() + chrono::Duration::seconds(600);
    store
        .begin_sandbox_provision(Uuid::new_v4(), &dead_tag.to_string(), expired)
        .await
        .unwrap();
    store
        .begin_sandbox_provision(Uuid::new_v4(), &live_tag.to_string(), open)
        .await
        .unwrap();

    let backend = MockBackend::spawning();
    let runner = SandboxContainerRunner::new(
        store.clone(),
        backend.clone(),
        Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
        fast_config(),
    );
    runner.sweep().await.expect("the sweep succeeds");

    let live_sets = backend.reclaim_live_sets.lock().unwrap().clone();
    assert_eq!(live_sets.len(), 1);
    assert!(
        live_sets[0].contains(&live_tag),
        "an unlapsed intent's tag must stay live through the tag sweep"
    );
    assert!(
        !live_sets[0].contains(&dead_tag),
        "a lapsed intent's tag must be reclaimable"
    );
    // The obligation completed under the backend's nothing-remains guarantee.
    assert!(store.list_sandbox_teardowns().await.unwrap().is_empty());
    assert_eq!(
        store.live_sandbox_tags().await.unwrap(),
        vec![live_tag.to_string()]
    );
}

/// An unconfirmed teardown outlives the driver: the obligation is persisted at
/// the end of the drive, and the next sweep's directed destroy completes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unconfirmed_teardown_is_redriven_by_the_sweep() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "clean up after me").await;
        let backend = MockBackend::spawning();
        backend.failing_destroys.store(true, Ordering::SeqCst);
        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        // The destroy never confirmed, so the obligation survived the drive.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 0);
        assert_eq!(store.list_sandbox_teardowns().await.unwrap().len(), 1);

        backend.failing_destroys.store(false, Ordering::SeqCst);
        runner.sweep().await.expect("the sweep succeeds");
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
        assert!(store.list_sandbox_teardowns().await.unwrap().is_empty());
    })
    .await
    .expect("test completed within its time bound");
}

/// A committed handle from a prior interrupted attempt is reconciled: the
/// driver attaches to the container that already exists instead of provisioning
/// a second one for the same single-attempt run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_committed_handle_is_reconciled_not_reprovisioned() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "resume where the interrupted attempt left off";
        let run_id = admit_container_run(&store, chat.id, task).await;
        let run_uuid = *run_id.as_uuid();

        // The sandbox already exists — a prior attempt provisioned it and
        // committed its handle before losing its own commit — so the backend
        // can only address it, never create it.
        let base_url = spawn_sandbox_agent().await;
        let backend = MockBackend::unreachable(base_url);
        let tag = SandboxTag::new();
        store
            .begin_sandbox_provision(
                run_uuid,
                &tag.to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
            )
            .await
            .unwrap();
        assert!(store
            .commit_sandbox_provision_handle(run_uuid, "prior-container")
            .await
            .unwrap());

        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcome = runner
            .drive(run_id)
            .await
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Completed(run_id));
        // Reconciled, not re-provisioned — and its teardown still completed.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// The recovery pass that replaces the lease reaper for container runs: a run
/// abandoned `running` under an expired lease is reclaimed under a fresh lease
/// on the SAME attempt, its committed container reattached and driven to the
/// result the reaper would have thrown away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_drivers_container_run_is_recovered_to_completion() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let task = "finish what the dead driver started";
        let run_id = admit_container_run(&store, chat.id, task).await;
        let run_uuid = *run_id.as_uuid();

        // The dead driver: claimed the run, provisioned a container, committed
        // its handle — then vanished, leaving the lease to expire.
        let dead_token = Uuid::new_v4();
        store
            .claim_container_agent_run(run_id, dead_token, chrono::Duration::milliseconds(50), 4)
            .await
            .unwrap()
            .expect("the dead driver's claim succeeds");
        let base_url = spawn_sandbox_agent().await;
        let backend = MockBackend::unreachable(base_url);
        let tag = SandboxTag::new();
        store
            .begin_sandbox_provision(
                run_uuid,
                &tag.to_string(),
                chrono::Utc::now() + chrono::Duration::seconds(600),
            )
            .await
            .unwrap();
        assert!(store
            .commit_sandbox_provision_handle(run_uuid, "abandoned-container")
            .await
            .unwrap());
        tokio::time::sleep(Duration::from_millis(150)).await;

        let provider = Arc::new(ScriptedProvider::new(vec![]));
        let runner = SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            fast_config(),
        );
        let outcomes = runner.recover().await.expect("recovery succeeds");
        assert_eq!(
            outcomes,
            vec![SandboxContainerRunOutcome::Completed(run_id)]
        );

        let recovered = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(recovered.status, AgentRunStatus::Completed);
        assert_eq!(
            recovered.attempt_count, 1,
            "recovery re-drives the single attempt, never a second one"
        );
        // Reconciled the abandoned container: no new provision, torn down once.
        assert_eq!(backend.provisions.load(Ordering::SeqCst), 0);
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// A container run failed terminally through the fenced store path leaves its
/// provisioning record owing a teardown in the same transaction — the link the
/// deadline scan uses so an expired run's live container is swept, not leaked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_container_failure_enqueues_its_teardown() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "fails terminally").await;
    let run_uuid = *run_id.as_uuid();
    let token = Uuid::new_v4();
    store
        .claim_container_agent_run(run_id, token, chrono::Duration::seconds(30), 4)
        .await
        .unwrap()
        .expect("the claim succeeds");
    let tag = SandboxTag::new();
    store
        .begin_sandbox_provision(
            run_uuid,
            &tag.to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
        )
        .await
        .unwrap();
    assert!(store
        .commit_sandbox_provision_handle(run_uuid, "doomed-container")
        .await
        .unwrap());

    store
        .fail_agent_run(
            run_id,
            token,
            "sandbox_agent_failed",
            "the loop ended without a result",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the terminal failure commits");

    let teardowns = store.list_sandbox_teardowns().await.unwrap();
    assert_eq!(teardowns.len(), 1);
    assert_eq!(teardowns[0].run_id, run_uuid);
    assert_eq!(teardowns[0].handle.as_deref(), Some("doomed-container"));
}

/// Container runs bypass the in-process scheduler's caps, so their own bound is
/// enforced at the claim: a second claim past the cap is refused and the run
/// stays queued, becoming claimable once a slot frees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_container_claim_refuses_past_the_concurrency_cap() {
    let (_dir, store, chat) = store().await;
    // The cap is global across chats, so give each run its own chat (a chat
    // runs one turn at a time, and the admit helper claims the chat's turn).
    let other = Chat {
        id: ChatId::new(),
        title: Some("second container chat".into()),
        ..chat.clone()
    };
    store.create_chat(&other).await.unwrap();
    let first = admit_container_run(&store, chat.id, "occupies the only slot").await;
    let second = admit_container_run(&store, other.id, "waits for the slot").await;

    let first_token = Uuid::new_v4();
    store
        .claim_container_agent_run(first, first_token, chrono::Duration::seconds(30), 1)
        .await
        .unwrap()
        .expect("the first claim takes the only slot");
    assert!(
        store
            .claim_container_agent_run(second, Uuid::new_v4(), chrono::Duration::seconds(30), 1)
            .await
            .unwrap()
            .is_none(),
        "a claim past the cap must be refused, not queued into a second container"
    );
    // The refused run is still queued, not damaged: it claims once a slot frees.
    store
        .fail_agent_run(
            first,
            first_token,
            "sandbox_agent_failed",
            "released its slot",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the first run fails terminally");
    store
        .claim_container_agent_run(second, Uuid::new_v4(), chrono::Duration::seconds(30), 1)
        .await
        .unwrap()
        .expect("the freed slot admits the queued run");
}

/// A well-formed result that arrives after the run is already terminal fails
/// the fenced commit predicate and is retained as evidence instead — first
/// writer wins, and nothing is ever committed from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_result_is_retained_as_evidence_not_committed() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "finishes too late").await;
    let run_uuid = *run_id.as_uuid();
    let token = Uuid::new_v4();
    store
        .claim_container_agent_run(run_id, token, chrono::Duration::seconds(30), 4)
        .await
        .unwrap()
        .expect("the claim succeeds");
    store
        .begin_sandbox_provision(
            run_uuid,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
        )
        .await
        .unwrap();

    // The run goes terminal (the deadline scan, a cancellation) while the
    // container still works; its result then arrives and must not commit.
    store
        .fail_agent_run(
            run_id,
            token,
            "deadline_exceeded",
            "went terminal before the result arrived",
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap()
        .expect("the terminal failure commits");
    assert!(store
        .submit_agent_run_result(run_id, token, "the late answer")
        .await
        .unwrap()
        .is_none());

    assert!(store
        .record_late_container_result_evidence(run_uuid, "the late answer")
        .await
        .unwrap());
    // A redelivery is a no-op, not an overwrite.
    assert!(!store
        .record_late_container_result_evidence(run_uuid, "a different answer")
        .await
        .unwrap());
    let record = store
        .get_sandbox_provision(run_uuid)
        .await
        .unwrap()
        .expect("the provisioning record exists");
    assert_eq!(
        record.late_result_evidence.as_deref(),
        Some("the late answer")
    );
    // The run's authoritative outcome is still the failure.
    let run = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(run.status, AgentRunStatus::Failed);
}

/// Cancelling a run mid-drive: the heartbeat refusal is read as cancellation,
/// the driver commits the terminal cancellation it still owns, and the teardown
/// that follows is what actually stops the container.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_cancellation_is_acknowledged_and_torn_down() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (_dir, store, chat) = store().await;
        let run_id = admit_container_run(&store, chat.id, "cancelled mid-flight").await;

        let backend = MockBackend::spawning();
        // Slow completions hold the drive open across several heartbeats, so
        // the cancellation lands while the container is genuinely working.
        let provider = Arc::new(ScriptedProvider::slow(vec![], Duration::from_secs(5)));
        let runner = Arc::new(SandboxContainerRunner::new(
            store.clone(),
            backend.clone(),
            Arc::new(FixedResolver(provider)),
            SandboxContainerRunConfig {
                lease: Duration::from_secs(2),
                heartbeat: Duration::from_millis(100),
                ..fast_config()
            },
        ));
        let drive = tokio::spawn({
            let runner = runner.clone();
            async move { runner.drive(run_id).await }
        });
        // Let the drive claim and attach, then cancel out from under it.
        tokio::time::sleep(Duration::from_millis(500)).await;
        store
            .request_agent_run_cancellation(run_id)
            .await
            .unwrap()
            .expect("the cancellation request lands");

        let outcome = drive
            .await
            .unwrap()
            .expect("driving succeeds")
            .expect("the container run is claimable");
        assert_eq!(outcome, SandboxContainerRunOutcome::Cancelled(run_id));
        let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
        // The container did not outlive the cancellation.
        assert_eq!(backend.destroys.load(Ordering::SeqCst), 1);
    })
    .await
    .expect("test completed within its time bound");
}

/// Cancelling an unattached container child — its driver is gone, its lease
/// expired — terminalizes immediately and leaves the container's teardown
/// obligation enqueued in the same transaction, for the sweep to destroy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_an_unattached_container_child_enqueues_its_teardown() {
    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "orphaned by its driver").await;
    let run_uuid = *run_id.as_uuid();
    store
        .claim_container_agent_run(run_id, Uuid::new_v4(), chrono::Duration::milliseconds(1), 4)
        .await
        .unwrap()
        .expect("the dead driver's claim succeeds");
    store
        .begin_sandbox_provision(
            run_uuid,
            &SandboxTag::new().to_string(),
            chrono::Utc::now() + chrono::Duration::seconds(600),
        )
        .await
        .unwrap();
    assert!(store
        .commit_sandbox_provision_handle(run_uuid, "unattended-container")
        .await
        .unwrap());
    tokio::time::sleep(Duration::from_millis(50)).await;

    store
        .request_agent_run_cancellation(run_id)
        .await
        .unwrap()
        .expect("the cancellation request lands");

    let cancelled = store.get_agent_run(run_id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, AgentRunStatus::Cancelled);
    let teardowns = store.list_sandbox_teardowns().await.unwrap();
    assert_eq!(teardowns.len(), 1);
    assert_eq!(teardowns[0].handle.as_deref(), Some("unattended-container"));
}

// --- Docker end-to-end (gated on a container runtime + the agent image) -------

/// Build the sandbox-agent image for the Docker-gated tests, returning its tag,
/// or `None` when no container runtime is present (there is none in the
/// unit-test sandbox; CI runners have Docker). A present daemon that fails the
/// build is a defect, not an environment to skip, so that panics.
///
/// Both Docker tests build the same tag; the CI lane serializes them so the
/// second build is a cache hit.
async fn build_agent_image() -> Option<&'static str> {
    use crate::sandbox_docker::DockerSandboxBackend;

    let backend_probe = DockerSandboxBackend::with_defaults();
    if !backend_probe.is_available() {
        eprintln!("skipping: no container runtime on PATH");
        return None;
    }

    // Build from the workspace root (the Dockerfile's build context is the
    // whole workspace, so Cargo.lock is visible to `--locked`).
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
        Ok(Ok(output)) if output.status.success() => Some(image),
        Ok(Ok(output)) => {
            panic!(
                "building the agent image failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(Err(error)) => {
            eprintln!("skipping: could not invoke docker build: {error}");
            None
        }
        Err(_) => panic!("building the agent image timed out"),
    }
}

/// The full stack on a real Docker container: build the `openwave-sandbox-agent`
/// image, admit a container run, and drive it end to end — provision a container
/// from the image, attach over its published loopback port, answer its
/// `exec`-then-final model steps from a mock host model (so the container really
/// runs a shell command in its own boundary), and assert the result committed
/// exactly once and the container was torn down.
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

    let Some(image) = build_agent_image().await else {
        return;
    };

    let (_dir, store, chat) = store().await;
    let run_id = admit_container_run(&store, chat.id, "count these four words now").await;

    let backend = Arc::new(DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    }));
    let provider = Arc::new(ScriptedProvider::new(vec![
        // Runs a real shell command inside the real container (in-container
        // execution is the containment).
        "use-tool:exec:{\"command\":\"echo count these four words\"}".to_owned(),
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

/// Dial the container's published loopback port, retrying while it starts up.
async fn dial_container(authority: &str) -> tokio::net::TcpStream {
    for _ in 0..60 {
        if let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect(authority),
        )
        .await
        {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("could not dial the container at {authority}");
}

/// The packaged agent image conforms at the transport boundary.
///
/// This is the conformance slice #822 deferred until the container backend
/// existed: the in-process suite proves the reference implementation, and this
/// proves the supervisor actually shipped in the image, over a real published
/// port. Three scenarios are the ones a third-party host would hit first, and
/// the only ones the real (well-behaved) agent can exhibit:
///
/// 1. a version skew is answered with the sandbox's own version, then refused;
/// 2. a wrong transport secret is refused before any capability is served;
/// 3. the event stream resumes from a committed cursor across a reconnect,
///    redelivering an unacknowledged terminal event with its original sequence.
///
/// Scenarios that script sandbox-side misbehavior (ungranted capabilities,
/// over-bound frames, lane saturation) stay against the in-process reference —
/// the shipped agent does not misbehave on demand.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Docker daemon and builds the agent image; run explicitly or in the Docker CI lane"]
async fn docker_container_conforms_at_the_transport_boundary() {
    use crate::sandbox_docker::{DockerConfig, DockerSandboxBackend};
    use openwave_sandbox_protocol::{
        events::EventPayload, ids::EventCursor, protocol::AttachRequest, ConnectError, SandboxTag,
        WireClient,
    };

    let Some(image) = build_agent_image().await else {
        return;
    };

    let (_dir, store, _chat) = store().await;
    let backend = DockerSandboxBackend::new(DockerConfig {
        image: image.to_owned(),
        ..DockerConfig::default()
    });
    let run_id = RunId::new();
    let handle = backend
        .provision(ProvisionRequest {
            run_id,
            tag: SandboxTag::new(),
            lifetime_cap_secs: None,
        })
        .await
        .expect("provisioning a conformance container succeeds");

    // A failed assertion below leaks the container in a local run; the CI
    // runner is ephemeral, and local reruns reclaim it through the tag sweep.
    tokio::time::timeout(Duration::from_secs(120), async {
        let address = backend.address(&handle).await.expect("container address");
        let authority = address
            .base_url
            .trim_start_matches("http://")
            .to_owned();

        let host = CapabilityHost::new(
            GrantSet::new(
                RunProvenance {
                    run_id,
                    provider: "local-container".to_owned(),
                },
                [Capability::ModelInference],
            ),
            Arc::new(HostModelProxy {
                // No scripted directives: every completion defaults to a final
                // answer, so the agent emits progress then a terminal result.
                resolver: Arc::new(FixedResolver(Arc::new(ScriptedProvider::new(vec![])))),
                config: AgentConfig::default(),
                spent: AtomicU32::new(0),
                budget: 24,
            }),
            Arc::new(DurableOperationStore::new(store.clone(), run_id)),
        );

        // 1. Version skew: answered with the sandbox's own version, refused,
        //    and the connection is not established.
        let Err(refusal) = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION + 1,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        else {
            panic!("a version skew must be refused");
        };
        match refusal {
            ConnectError::VersionRefused(refused) => {
                assert_eq!(
                    refused.protocol_version, PROTOCOL_VERSION,
                    "the refusal must carry the sandbox's own version so the peer learns the mismatch"
                );
            }
            other => panic!("expected a version refusal, got: {other}"),
        }

        // 2. Wrong transport secret: refused before anything is served.
        let Err(refusal) = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: TransportSecret::new("not-the-minted-secret"),
            },
            host.clone(),
        )
        .await
        else {
            panic!("a wrong secret must be refused");
        };
        assert!(
            matches!(refusal, ConnectError::Unauthenticated(_)),
            "expected an authentication refusal, got: {refusal}"
        );

        // 3. Attach for real, take the stream to its terminal event, but leave
        //    that terminal event unacknowledged.
        let mut conn = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: EventCursor::START,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        .expect("an authenticated attach is accepted");
        // The packaged agent starts nothing until the run init arrives.
        conn.send_init(openwave_sandbox_protocol::init::RunInit {
            run_id,
            provenance: RunProvenance {
                run_id,
                provider: "local-container".to_owned(),
            },
            task: "answer briefly".to_owned(),
            deadline_unix_secs: 4_102_444_800,
            admission: openwave_sandbox_protocol::init::AdmissionMode::AttachedOnly,
            policy: openwave_sandbox_protocol::init::PolicySnapshot {
                egress_allowlist: Vec::new(),
                granted_capabilities: vec![Capability::ModelInference],
            },
            scoped_token: None,
        })
        .await;
        let first = conn.next_event().await.expect("the agent's first event");
        let mut committed = EventCursor::committed(first.sequence);
        conn.acknowledge(committed).await;
        let terminal = loop {
            let event = conn.next_event().await.expect("the stream reaches a terminal event");
            if matches!(
                event.payload,
                EventPayload::Result(_) | EventPayload::Failed(_)
            ) {
                break event;
            }
            committed = EventCursor::committed(event.sequence);
            conn.acknowledge(committed).await;
        };
        assert!(
            matches!(terminal.payload, EventPayload::Result(_)),
            "a directive-free completion must end the run with a result"
        );
        drop(conn);

        // Reattach from the last committed cursor: the sandbox must redeliver
        // the unacknowledged terminal event, same sequence, same payload.
        let mut conn = WireClient::connect(
            dial_container(&authority).await,
            AttachRequest {
                protocol_version: PROTOCOL_VERSION,
                run_id,
                resume_from: committed,
                transport_secret: address.transport_secret.clone(),
            },
            host.clone(),
        )
        .await
        .expect("a reattach after a disconnect is accepted");
        let redelivered = conn
            .next_event()
            .await
            .expect("the unacknowledged event is redelivered");
        assert_eq!(redelivered.sequence, terminal.sequence);
        assert_eq!(redelivered.payload, terminal.payload);
    })
    .await
    .expect("conformance checks completed within their bound");

    backend
        .destroy(&handle)
        .await
        .expect("tearing the conformance container down succeeds");
}

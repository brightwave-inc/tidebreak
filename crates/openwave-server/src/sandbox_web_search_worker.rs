//! Durable execution of the one sandbox-safe web-search checkpoint.
//!
//! A model loop still has no advertised web-search tool. Only a durably
//! accepted checkpoint can arrive here, where its exact executor lease is the
//! authority for the bounded outbound operation.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openwave_core::{
    AgentError, ClaimSandboxToolCallOutcome, Result, SandboxToolCall, Store, ToolCallResolution,
};
use openwave_web_search::{
    SearchDomain, WebSearchProvider, WebSearchRequest, WebSearchResponse, MAX_OUTPUT_BYTES,
};
use serde::Deserialize;
use tokio::sync::Notify;

use crate::web_search;

const WEB_SEARCH_TOOL: &str = "web_search";
const DEFAULT_MAX_RESULTS: usize = 5;
const CANDIDATE_BATCH_SIZE: u64 = 16;
const EGRESS_SAFETY_MARGIN: Duration = Duration::from_millis(250);

#[async_trait]
pub(crate) trait SandboxWebSearch: Send + Sync {
    /// Resolve host policy and credentials without sending a request. The
    /// worker revalidates its durable lease after this await and immediately
    /// before it calls `WebSearchProvider::search`.
    async fn resolve(
        &self,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError>;
}

/// Deliberately non-diagnostic host search failures. Provider and secret
/// details must never become checkpoint receipts or model context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxWebSearchError {
    Failed,
}

#[derive(Clone)]
struct HostSandboxWebSearch {
    store: Arc<dyn Store>,
    secrets: Arc<dyn openwave_core::SecretProvider>,
}

#[async_trait]
impl SandboxWebSearch for HostSandboxWebSearch {
    async fn resolve(
        &self,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError> {
        web_search::resolve_provider(&*self.store, &*self.secrets)
            .await
            .map_err(|_| SandboxWebSearchError::Failed)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SandboxWebSearchWorkerConfig {
    lease: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
    max_concurrency: usize,
}

impl Default for SandboxWebSearchWorkerConfig {
    fn default() -> Self {
        // The configured HTTP request is capped at 60 seconds. The executor
        // lease leaves scheduling room, while the database caps it at the run
        // deadline and the final local timeout stays below the live expiry.
        Self {
            lease: Duration::from_secs(75),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxWebSearchWorkerOutcome {
    Idle,
    Resolved(openwave_core::CallId),
    LeaseLost(openwave_core::CallId),
}

#[derive(Clone)]
pub(crate) struct SandboxWebSearchWorker {
    store: Arc<dyn Store>,
    search: Arc<dyn SandboxWebSearch>,
    wake: Arc<Notify>,
    config: SandboxWebSearchWorkerConfig,
}

impl SandboxWebSearchWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn openwave_core::SecretProvider>,
        wake: Arc<Notify>,
        config: SandboxWebSearchWorkerConfig,
    ) -> Self {
        Self::with_search(
            store.clone(),
            Arc::new(HostSandboxWebSearch { store, secrets }),
            wake,
            config,
        )
    }

    fn with_search(
        store: Arc<dyn Store>,
        search: Arc<dyn SandboxWebSearch>,
        wake: Arc<Notify>,
        config: SandboxWebSearchWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(config.max_concurrency > 0);
        Self {
            store,
            search,
            wake,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut lanes = tokio::task::JoinSet::new();
        for _ in 0..self.config.max_concurrency {
            lanes.spawn(self.clone().run_lane());
        }
        while let Some(result) = lanes.join_next().await {
            if let Err(error) = result {
                eprintln!("openwave: sandbox web-search worker lane stopped: {error}");
                tokio::time::sleep(self.config.failure_delay).await;
            }
            lanes.spawn(self.clone().run_lane());
        }
    }

    async fn run_lane(self) {
        let mut idle_delay = self.config.idle_min;
        loop {
            match self.run_once().await {
                Ok(SandboxWebSearchWorkerOutcome::Idle) => {
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => idle_delay = self.config.idle_min,
                Err(error) => {
                    eprintln!("openwave: sandbox web-search worker iteration failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    /// Claim and resolve one exact persisted sandbox tool checkpoint.
    pub(crate) async fn run_once(&self) -> Result<SandboxWebSearchWorkerOutcome> {
        for candidate in self
            .store
            .list_sandbox_tool_call_candidates_named(WEB_SEARCH_TOOL, CANDIDATE_BATCH_SIZE)
            .await?
        {
            let lease_token = uuid::Uuid::new_v4();
            let call = match self
                .store
                .claim_sandbox_tool_call_named(
                    candidate.id,
                    WEB_SEARCH_TOOL,
                    lease_token,
                    chrono_duration(self.config.lease)?,
                )
                .await?
            {
                ClaimSandboxToolCallOutcome::Claimed(call)
                | ClaimSandboxToolCallOutcome::Existing(call) => call,
                ClaimSandboxToolCallOutcome::Unavailable => continue,
            };
            self.wake.notify_one();
            return self.process(call, lease_token).await;
        }
        Ok(SandboxWebSearchWorkerOutcome::Idle)
    }

    async fn process(
        &self,
        call: SandboxToolCall,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxWebSearchWorkerOutcome> {
        let resolution = match parse_web_search_request(&call) {
            Err(resolution) => resolution,
            Ok(request) => match self.search.resolve().await {
                Ok(None) => failed_resolution(
                    "web_search_disabled",
                    "Web search is not configured for this host.",
                ),
                Err(SandboxWebSearchError::Failed) => {
                    failed_resolution("web_search_failed", "Web search could not complete.")
                }
                Ok(Some(provider)) => {
                    // This is the final database-clock cancellation/deadline/lease
                    // proof after settings and credentials resolve, immediately
                    // before the provider can observe an outbound request.
                    let Some(expires_at) = self
                        .store
                        .heartbeat_sandbox_tool_call(
                            call.id,
                            lease_token,
                            chrono_duration(
                                self.config.lease.saturating_add(Duration::from_millis(1)),
                            )?,
                        )
                        .await?
                    else {
                        return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id));
                    };
                    match remaining_execution_time(expires_at) {
                        None => return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id)),
                        Some(timeout) => {
                            match tokio::time::timeout(timeout, provider.search(request)).await {
                                Ok(Ok(response)) => serialize_response(response),
                                Ok(Err(_)) => failed_resolution(
                                    "web_search_failed",
                                    "Web search could not complete.",
                                ),
                                Err(_) => failed_resolution(
                                    "web_search_timed_out",
                                    "Web search did not complete before its sandbox lease expired.",
                                ),
                            }
                        }
                    }
                }
            },
        };
        match self
            .store
            .resolve_sandbox_tool_call(call.id, lease_token, &resolution)
            .await?
        {
            openwave_core::ResolveSandboxToolCallOutcome::Resolved
            | openwave_core::ResolveSandboxToolCallOutcome::Existing => {
                self.wake.notify_one();
                Ok(SandboxWebSearchWorkerOutcome::Resolved(call.id))
            }
            openwave_core::ResolveSandboxToolCallOutcome::NotFound
            | openwave_core::ResolveSandboxToolCallOutcome::AlreadyTerminal
            | openwave_core::ResolveSandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSearchArguments {
    query: String,
    #[serde(default = "default_max_results")]
    max_results: usize,
    #[serde(default)]
    domains: Vec<String>,
    #[serde(default)]
    start_published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    end_published_at: Option<DateTime<Utc>>,
}

const fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

fn parse_web_search_request(
    call: &SandboxToolCall,
) -> std::result::Result<WebSearchRequest, ToolCallResolution> {
    if call.name != WEB_SEARCH_TOOL {
        return Err(failed_resolution(
            "unsupported_sandbox_tool",
            "This sandbox tool is not available.",
        ));
    }
    let arguments: WebSearchArguments =
        serde_json::from_value(call.arguments.clone()).map_err(|_| {
            failed_resolution(
                "invalid_web_search_arguments",
                "Web search arguments are invalid.",
            )
        })?;
    let domains = arguments
        .domains
        .into_iter()
        .map(SearchDomain::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            failed_resolution(
                "invalid_web_search_arguments",
                "Web search arguments are invalid.",
            )
        })?;
    WebSearchRequest::new(arguments.query, arguments.max_results)
        .and_then(|request| request.with_domains(domains))
        .and_then(|request| {
            request.with_published_between(arguments.start_published_at, arguments.end_published_at)
        })
        .map_err(|_| {
            failed_resolution(
                "invalid_web_search_arguments",
                "Web search arguments are invalid.",
            )
        })
}

fn remaining_execution_time(remaining: chrono::Duration) -> Option<Duration> {
    let remaining = remaining.to_std().ok()?;
    remaining
        .checked_sub(EGRESS_SAFETY_MARGIN)
        .filter(|value| !value.is_zero())
}

fn serialize_response(response: WebSearchResponse) -> ToolCallResolution {
    match serde_json::to_string(&response) {
        Ok(result) if result.len() <= MAX_OUTPUT_BYTES => ToolCallResolution::Completed { result },
        // A bad injected adapter must not wedge the checkpoint or exceed its
        // durable receipt budget.
        Ok(_) | Err(_) => failed_resolution(
            "web_search_output_invalid",
            "Web search returned an invalid response.",
        ),
    }
}

fn failed_resolution(code: &str, result: &str) -> ToolCallResolution {
    ToolCallResolution::Failed {
        result: result.into(),
        error_code: code.into(),
        error_detail: None,
    }
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid sandbox web-search duration: {error}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use openwave_core::{
        AgentRunStatus, CallId, Chat, ChatId, ClaimSandboxToolCallOutcome, DbStore,
        ParkSandboxToolCallOutcome, RequestAgentRunCancellationOutcome, SandboxToolCallRequest,
        Store,
    };
    use openwave_web_search::{WebSearchProviderKind, WebSearchResult};

    use super::*;

    struct FakeSearch {
        resolution: std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError>,
    }

    struct FakeProvider {
        requests: Mutex<Vec<WebSearchRequest>>,
        response: WebSearchResponse,
    }

    #[async_trait]
    impl WebSearchProvider for FakeProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        async fn search(
            &self,
            request: WebSearchRequest,
        ) -> std::result::Result<WebSearchResponse, openwave_web_search::WebSearchError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.response.clone())
        }
    }

    #[async_trait]
    impl SandboxWebSearch for FakeSearch {
        async fn resolve(
            &self,
        ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError>
        {
            self.resolution.clone()
        }
    }

    async fn test_store() -> (Arc<DbStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("test.db").display()
            ))
            .await
            .unwrap(),
        );
        (store, dir)
    }

    async fn checkpoint(
        store: &Arc<DbStore>,
        name: &str,
        arguments: serde_json::Value,
    ) -> SandboxToolCallRequest {
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("sandbox web search".into()),
            model: Some("model".into()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = openwave_core::TurnId::new();
        store
            .accept_turn(
                turn_id,
                chat.id,
                "sandbox-test-model",
                "sandbox search test",
            )
            .await
            .unwrap();
        let turn_lease = uuid::Uuid::new_v4();
        let now = Utc::now();
        let turn = store
            .claim_turn_run(turn_lease, now, now + chrono::Duration::hours(1))
            .await
            .unwrap()
            .turn
            .unwrap();
        let call = CallId::new();
        let run = match store
            .admit_sandbox_agent_run(
                turn.id,
                call,
                "search",
                turn_lease,
                turn.steer_revision,
                1,
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap()
        {
            openwave_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
            outcome => panic!("unexpected sandbox admission: {outcome:?}"),
        };
        let worker_lease = uuid::Uuid::new_v4();
        assert_eq!(
            store
                .claim_agent_run(worker_lease, chrono::Duration::minutes(5), 1, 1)
                .await
                .unwrap()
                .unwrap()
                .id,
            run.id
        );
        let request = SandboxToolCallRequest {
            id: CallId::new(),
            agent_run_id: run.id,
            chat_id: chat.id,
            provider_id: "provider-call".into(),
            name: name.into(),
            arguments,
        };
        assert!(matches!(
            store
                .park_agent_run_for_sandbox_tool_call(run.id, worker_lease, &request)
                .await
                .unwrap(),
            ParkSandboxToolCallOutcome::Parked { .. }
        ));
        request
    }

    fn worker(store: Arc<DbStore>, search: Arc<dyn SandboxWebSearch>) -> SandboxWebSearchWorker {
        SandboxWebSearchWorker::with_search(
            store,
            search,
            Arc::new(Notify::new()),
            SandboxWebSearchWorkerConfig::default(),
        )
    }

    #[tokio::test]
    async fn claimed_web_search_resolves_a_bounded_receipt_and_resumes_its_sandbox() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"durable search"}),
        )
        .await;
        let provider = Arc::new(FakeProvider {
            requests: Mutex::new(Vec::new()),
            response: WebSearchResponse::new(WebSearchProviderKind::Exa, Vec::new()),
        });
        let fake = Arc::new(FakeSearch {
            resolution: Ok(Some(provider.clone())),
        });
        assert_eq!(
            worker(store.clone(), fake.clone())
                .run_once()
                .await
                .unwrap(),
            SandboxWebSearchWorkerOutcome::Resolved(call.id)
        );
        assert_eq!(
            provider.requests.lock().unwrap()[0].max_results,
            DEFAULT_MAX_RESULTS
        );
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status,
            openwave_core::SandboxToolCallStatus::Completed
        );
        assert!(receipt.result.len() <= MAX_OUTPUT_BYTES);
        assert_eq!(
            store
                .get_agent_run(call.agent_run_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            AgentRunStatus::RetryWait
        );
    }

    #[tokio::test]
    async fn malformed_checkpoints_resolve_without_search() {
        let (store, _dir) = test_store().await;
        let fake = Arc::new(FakeSearch {
            resolution: Ok(None),
        });
        let malformed = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"ok", "endpoint":"https://untrusted.example"}),
        )
        .await;
        assert_eq!(
            worker(store.clone(), fake.clone())
                .run_once()
                .await
                .unwrap(),
            SandboxWebSearchWorkerOutcome::Resolved(malformed.id)
        );
        // No provider exists, so malformed input cannot trigger egress.
        assert_eq!(
            store
                .get_sandbox_tool_call_receipt(malformed.id)
                .await
                .unwrap()
                .unwrap()
                .error_code
                .as_deref(),
            Some("invalid_web_search_arguments")
        );
    }

    #[tokio::test]
    async fn other_tool_names_stay_untouched_for_their_own_dispatcher() {
        let (store, _dir) = test_store().await;
        let unknown = checkpoint(&store, "untrusted_tool", serde_json::json!({"query":"ok"})).await;
        let fake = Arc::new(FakeSearch {
            resolution: Ok(None),
        });
        assert_eq!(
            worker(store.clone(), fake).run_once().await.unwrap(),
            SandboxWebSearchWorkerOutcome::Idle
        );
        assert!(store
            .get_sandbox_tool_call_receipt(unknown.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn oversized_provider_output_becomes_a_bounded_failure_receipt() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"oversized"}),
        )
        .await;
        let provider = Arc::new(FakeProvider {
            requests: Mutex::new(Vec::new()),
            response: WebSearchResponse {
                provider: WebSearchProviderKind::Exa,
                results: vec![WebSearchResult {
                    url: "https://example.com/oversized".into(),
                    title: "x".repeat(MAX_OUTPUT_BYTES),
                    snippet: "x".repeat(MAX_OUTPUT_BYTES),
                    content: None,
                    score: None,
                    published_at: None,
                    image_url: None,
                    metadata: BTreeMap::new(),
                }],
            },
        });
        let fake = Arc::new(FakeSearch {
            resolution: Ok(Some(provider)),
        });
        assert_eq!(
            worker(store.clone(), fake).run_once().await.unwrap(),
            SandboxWebSearchWorkerOutcome::Resolved(call.id)
        );
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.error_code.as_deref(),
            Some("web_search_output_invalid")
        );
        assert!(receipt.result.len() <= openwave_core::SandboxToolCall::MAX_RESULT_BYTES);
    }

    #[tokio::test]
    async fn stale_or_cancelled_claims_do_not_reach_search() {
        let (store, _dir) = test_store().await;
        let provider = Arc::new(FakeProvider {
            requests: Mutex::new(Vec::new()),
            response: WebSearchResponse::new(WebSearchProviderKind::Exa, Vec::new()),
        });
        let fake = Arc::new(FakeSearch {
            resolution: Ok(Some(provider.clone())),
        });
        let stale = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"stale"}),
        )
        .await;
        let lease = uuid::Uuid::new_v4();
        let claimed = match store
            .claim_sandbox_tool_call(stale.id, lease, chrono::Duration::minutes(1))
            .await
            .unwrap()
        {
            ClaimSandboxToolCallOutcome::Claimed(call) => call,
            outcome => panic!("unexpected claim: {outcome:?}"),
        };
        assert_eq!(
            worker(store.clone(), fake.clone())
                .process(claimed, uuid::Uuid::new_v4())
                .await
                .unwrap(),
            SandboxWebSearchWorkerOutcome::LeaseLost(stale.id)
        );

        let cancelled = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"cancelled"}),
        )
        .await;
        let cancelled_lease = uuid::Uuid::new_v4();
        let claimed = match store
            .claim_sandbox_tool_call(cancelled.id, cancelled_lease, chrono::Duration::minutes(1))
            .await
            .unwrap()
        {
            ClaimSandboxToolCallOutcome::Claimed(call) => call,
            outcome => panic!("unexpected claim: {outcome:?}"),
        };
        assert!(matches!(
            store
                .request_agent_run_cancellation(cancelled.agent_run_id)
                .await
                .unwrap(),
            Some(RequestAgentRunCancellationOutcome::Cancelled(_))
        ));
        assert_eq!(
            worker(store.clone(), fake.clone())
                .process(claimed, cancelled_lease)
                .await
                .unwrap(),
            SandboxWebSearchWorkerOutcome::LeaseLost(cancelled.id)
        );
        assert_eq!(provider.requests.lock().unwrap().len(), 0);
    }
}

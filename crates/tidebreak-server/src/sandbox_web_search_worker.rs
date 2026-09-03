//! Durable execution of the one sandbox-safe web-search checkpoint.
//!
//! Only a durably accepted sandbox checkpoint can arrive here; foreground
//! agents use the ordinary tool registry and approval path. The checkpoint's
//! exact executor lease is the authority for this bounded outbound operation.

use std::sync::Arc;
use std::time::Duration;

use crate::web_search::{
    request_from_tool_arguments, WebSearchError, WebSearchProvider, WebSearchRequest,
    WebSearchResponse, MAX_OUTPUT_BYTES,
};
use async_trait::async_trait;
#[cfg(test)]
use chrono::Utc;
use tidebreak_core::{
    AgentError, ClaimSandboxToolCallOutcome, Result, SandboxToolCall, SessionId, Store,
    ToolCallResolution,
};
use tokio::sync::Notify;

use crate::lane::{self, LaneOutcome, LanePacing, LaneStep};
use crate::retry::LaneBackoff;
use crate::state::SandboxAttemptGuard;
use crate::web_search;

const WEB_SEARCH_TOOL: &str = "web_search";
const CANDIDATE_BATCH_SIZE: u64 = 16;
const EGRESS_SAFETY_MARGIN: Duration = Duration::from_millis(250);

#[async_trait]
pub(crate) trait SandboxWebSearch: Send + Sync {
    /// Resolve host policy and credentials without sending a request. The
    /// worker revalidates its durable lease after this await and immediately
    /// before it calls `WebSearchProvider::search`.
    ///
    /// The chat decides which backend answers when the host searches through
    /// the conversation's own model provider, so a background call resolves
    /// against the chat it belongs to rather than against host settings alone.
    async fn resolve(
        &self,
        chat: SessionId,
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
    secrets: Arc<dyn tidebreak_core::SecretProvider>,
    providers: Arc<dyn crate::resolver::ProviderResolver>,
    default_model: String,
}

#[async_trait]
impl SandboxWebSearch for HostSandboxWebSearch {
    async fn resolve(
        &self,
        chat: SessionId,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError> {
        web_search::resolve_provider(
            &*self.store,
            &*self.secrets,
            Some(chat),
            Some(&self.providers),
            &self.default_model,
        )
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
    /// Ceiling on the lane's own backoff after consecutive iteration errors,
    /// so a store outage is not polled at a fixed rate forever.
    failure_delay_cap: Duration,
    /// Backoff before a classified-transient provider failure's single bounded
    /// retry becomes claimable. This work sits inside a foreground turn the
    /// user is waiting on, so the whole envelope stays within a few seconds.
    retry_delay: Duration,
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
            failure_delay_cap: Duration::from_secs(30),
            retry_delay: Duration::from_secs(2),
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxWebSearchWorkerOutcome {
    Idle,
    Resolved(tidebreak_core::CallId),
    /// A classified-transient provider failure parked the call in
    /// `retry_wait`; it becomes claimable again after its short backoff.
    RetryScheduled(tidebreak_core::CallId),
    LeaseLost(tidebreak_core::CallId),
}

impl LaneOutcome for SandboxWebSearchWorkerOutcome {
    fn lane_step(&self) -> LaneStep {
        match self {
            Self::Idle => LaneStep::Idle,
            _ => LaneStep::Worked,
        }
    }
}

/// The name this worker's lanes log under.
const LANE_NAME: &str = "sandbox web-search worker";

#[derive(Clone)]
pub(crate) struct SandboxWebSearchWorker {
    store: Arc<dyn Store>,
    search: Arc<dyn SandboxWebSearch>,
    wake: Arc<Notify>,
    attempts: Arc<SandboxAttemptGuard>,
    config: SandboxWebSearchWorkerConfig,
}

impl SandboxWebSearchWorker {
    pub(crate) fn with_attempts(
        store: Arc<dyn Store>,
        secrets: Arc<dyn tidebreak_core::SecretProvider>,
        providers: Arc<dyn crate::resolver::ProviderResolver>,
        default_model: String,
        wake: Arc<Notify>,
        attempts: Arc<SandboxAttemptGuard>,
        config: SandboxWebSearchWorkerConfig,
    ) -> Self {
        Self::with_search_and_attempts(
            store.clone(),
            Arc::new(HostSandboxWebSearch {
                store,
                secrets,
                providers,
                default_model,
            }),
            wake,
            attempts,
            config,
        )
    }

    #[cfg(test)]
    fn with_search(
        store: Arc<dyn Store>,
        search: Arc<dyn SandboxWebSearch>,
        wake: Arc<Notify>,
        config: SandboxWebSearchWorkerConfig,
    ) -> Self {
        Self::with_search_and_attempts(
            store,
            search,
            wake,
            Arc::new(SandboxAttemptGuard::default()),
            config,
        )
    }

    fn with_search_and_attempts(
        store: Arc<dyn Store>,
        search: Arc<dyn SandboxWebSearch>,
        wake: Arc<Notify>,
        attempts: Arc<SandboxAttemptGuard>,
        config: SandboxWebSearchWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(config.max_concurrency > 0);
        Self {
            store,
            search,
            wake,
            attempts,
            config,
        }
    }

    pub(crate) async fn run(self) {
        lane::supervise_lanes(
            LANE_NAME,
            self.config.max_concurrency,
            LaneBackoff::new(self.config.failure_delay, self.config.failure_delay_cap),
            move || self.clone().run_lane(),
        )
        .await;
    }

    async fn run_lane(self) {
        let this = &self;
        lane::run_lane(LANE_NAME, self.pacing(), &self.wake, move || {
            this.run_once()
        })
        .await;
    }

    fn pacing(&self) -> LanePacing {
        LanePacing::backoff(
            self.config.idle_min,
            self.config.idle_cap,
            self.config.failure_delay,
            self.config.failure_delay_cap,
        )
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
        let Some(active_attempt) =
            self.attempts
                .register_checkpoint(call.id, call.agent_run_id, lease_token)
        else {
            return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id));
        };
        let cancel = active_attempt.cancel_token();
        // Close cancel-before-register and prove this exact executor lease
        // before resolving credentials or beginning outbound work.
        let Some(_) = self
            .store
            .heartbeat_sandbox_tool_call(call.id, lease_token, chrono_duration(self.config.lease)?)
            .await?
        else {
            return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id));
        };
        let resolution = match parse_web_search_request(&call) {
            Err(resolution) => resolution,
            Ok(request) => match {
                let resolve = self.search.resolve(call.chat_id);
                tokio::pin!(resolve);
                tokio::select! {
                    resolved = &mut resolve => Some(resolved),
                    _ = cancel.cancelled() => None,
                }
            } {
                None => return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id)),
                Some(resolved) => match resolved {
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
                            Some(timeout) => match {
                                let search =
                                    tokio::time::timeout(timeout, provider.search(request));
                                tokio::pin!(search);
                                tokio::select! {
                                    result = &mut search => Some(result),
                                    _ = cancel.cancelled() => None,
                                }
                            } {
                                None => return Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id)),
                                Some(result) => match result {
                                    Ok(Ok(response)) => serialize_response(response),
                                    Ok(Err(error)) => {
                                        if transient_provider_failure(&error)
                                            && call.retry_at.is_none()
                                        {
                                            return self
                                                .schedule_retry(call.id, lease_token)
                                                .await;
                                        }
                                        failed_resolution(
                                            "web_search_failed",
                                            "Web search could not complete.",
                                        )
                                    }
                                    Err(_) => failed_resolution(
                                        "web_search_timed_out",
                                        "Web search did not complete before its sandbox lease expired.",
                                    ),
                                },
                            },
                        }
                    }
                },
            },
        };
        match self
            .store
            .resolve_sandbox_tool_call(call.id, lease_token, &resolution)
            .await?
        {
            tidebreak_core::ResolveSandboxToolCallOutcome::Resolved
            | tidebreak_core::ResolveSandboxToolCallOutcome::Existing => {
                self.wake.notify_one();
                Ok(SandboxWebSearchWorkerOutcome::Resolved(call.id))
            }
            tidebreak_core::ResolveSandboxToolCallOutcome::NotFound
            | tidebreak_core::ResolveSandboxToolCallOutcome::AlreadyTerminal
            | tidebreak_core::ResolveSandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxWebSearchWorkerOutcome::LeaseLost(call.id))
            }
        }
    }

    /// Park the call for its single bounded retry instead of writing a
    /// terminal failure receipt. Only a first-attempt classified-transient
    /// failure reaches here; the durable `retry_at` marker makes the second
    /// attempt terminal on any failure.
    async fn schedule_retry(
        &self,
        id: tidebreak_core::CallId,
        lease_token: uuid::Uuid,
    ) -> Result<SandboxWebSearchWorkerOutcome> {
        match self
            .store
            .retry_sandbox_tool_call(id, lease_token, chrono_duration(self.config.retry_delay)?)
            .await?
        {
            tidebreak_core::RetrySandboxToolCallOutcome::Scheduled => {
                self.wake.notify_one();
                Ok(SandboxWebSearchWorkerOutcome::RetryScheduled(id))
            }
            tidebreak_core::RetrySandboxToolCallOutcome::LeaseLost => {
                Ok(SandboxWebSearchWorkerOutcome::LeaseLost(id))
            }
        }
    }
}

/// Whether a provider failure is worth the single bounded retry: a transport
/// fault (including a timed-out request), a provider 5xx, or a rate limit.
/// Configuration and request failures recur on the next call and resolve
/// terminally at once.
fn transient_provider_failure(error: &WebSearchError) -> bool {
    match error {
        WebSearchError::Transport(_) | WebSearchError::RateLimited(_) => true,
        WebSearchError::HttpStatus { status, .. } => (500..=599).contains(status),
        _ => false,
    }
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
    request_from_tool_arguments(call.arguments.clone()).map_err(|_| {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    use crate::web_search::{WebSearchProviderKind, WebSearchResult};
    use tidebreak_core::{
        AgentRunStatus, CallId, Chat, ClaimSandboxToolCallOutcome, DbStore,
        ParkSandboxToolCallOutcome, RequestAgentRunCancellationOutcome, SandboxToolCallRequest,
        SessionId, Store,
    };

    use super::*;

    struct FakeSearch {
        resolution: std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError>,
    }

    struct FakeProvider {
        requests: Mutex<Vec<WebSearchRequest>>,
        response: WebSearchResponse,
    }

    struct FlakyProvider {
        failures: Mutex<Vec<WebSearchError>>,
        requests: std::sync::atomic::AtomicUsize,
        response: WebSearchResponse,
    }

    #[async_trait]
    impl WebSearchProvider for FlakyProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        async fn search(
            &self,
            _request: WebSearchRequest,
        ) -> std::result::Result<WebSearchResponse, WebSearchError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let mut failures = self.failures.lock().unwrap();
            if failures.is_empty() {
                Ok(self.response.clone())
            } else {
                Err(failures.remove(0))
            }
        }
    }

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingResolution {
        entered: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl SandboxWebSearch for BlockingResolution {
        async fn resolve(
            &self,
            _chat: SessionId,
        ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, SandboxWebSearchError>
        {
            let _drop = DropMarker(self.dropped.clone());
            self.entered.notify_one();
            futures::future::pending().await
        }
    }

    struct BlockingSearchProvider {
        entered: Arc<Notify>,
        dropped: Arc<AtomicBool>,
    }

    #[async_trait]
    impl WebSearchProvider for BlockingSearchProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        async fn search(
            &self,
            _request: WebSearchRequest,
        ) -> std::result::Result<WebSearchResponse, crate::web_search::WebSearchError> {
            let _drop = DropMarker(self.dropped.clone());
            self.entered.notify_one();
            futures::future::pending().await
        }
    }

    #[async_trait]
    impl WebSearchProvider for FakeProvider {
        fn kind(&self) -> WebSearchProviderKind {
            WebSearchProviderKind::Exa
        }

        async fn search(
            &self,
            request: WebSearchRequest,
        ) -> std::result::Result<WebSearchResponse, crate::web_search::WebSearchError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.response.clone())
        }
    }

    #[async_trait]
    impl SandboxWebSearch for FakeSearch {
        async fn resolve(
            &self,
            _chat: SessionId,
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
            id: SessionId::new(),
            project_id: None,
            title: Some("sandbox web search".into()),
            model: Some("model".into()),
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let turn_id = tidebreak_core::TurnId::new();
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
            .claim_turn(turn_lease, now, now + chrono::Duration::hours(1))
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
            tidebreak_core::AdmitSandboxAgentRunOutcome::Accepted { child, .. } => child,
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
                .park_agent_run_for_sandbox_tool_calls(
                    run.id,
                    worker_lease,
                    &[crate::tests::dispatchable(&request)]
                )
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
            crate::web_search::DEFAULT_MAX_RESULTS
        );
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status,
            tidebreak_core::SandboxToolCallStatus::Completed
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
    async fn transient_failure_retries_once_then_second_failure_terminalizes() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"flaky"}),
        )
        .await;
        let provider = Arc::new(FlakyProvider {
            failures: Mutex::new(vec![
                WebSearchError::HttpStatus {
                    provider: WebSearchProviderKind::Exa,
                    status: 503,
                },
                WebSearchError::Transport("connection reset".into()),
            ]),
            requests: std::sync::atomic::AtomicUsize::new(0),
            response: WebSearchResponse::new(WebSearchProviderKind::Exa, Vec::new()),
        });
        let fake = Arc::new(FakeSearch {
            resolution: Ok(Some(provider.clone())),
        });
        // A zero backoff keeps the test fast; the schedule itself is durable.
        let worker = SandboxWebSearchWorker::with_search(
            store.clone(),
            fake,
            Arc::new(Notify::new()),
            SandboxWebSearchWorkerConfig {
                retry_delay: Duration::ZERO,
                ..SandboxWebSearchWorkerConfig::default()
            },
        );
        // The first transient failure parks the call instead of writing a
        // terminal receipt, and the sandbox stays parked on its checkpoint.
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxWebSearchWorkerOutcome::RetryScheduled(call.id)
        );
        let parked = store.get_sandbox_tool_call(call.id).await.unwrap().unwrap();
        assert_eq!(
            parked.status,
            tidebreak_core::SandboxToolCallStatus::RetryWait
        );
        assert!(parked.retry_at.is_some());
        assert!(store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get_agent_run(call.agent_run_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            AgentRunStatus::Waiting
        );
        // The single retry is spent, so the second transient failure resolves
        // terminally and resumes the sandbox with the failure receipt.
        assert_eq!(
            worker.run_once().await.unwrap(),
            SandboxWebSearchWorkerOutcome::Resolved(call.id)
        );
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.status,
            tidebreak_core::SandboxToolCallStatus::Failed
        );
        assert_eq!(receipt.error_code.as_deref(), Some("web_search_failed"));
        assert_eq!(provider.requests.load(Ordering::SeqCst), 2);
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
    async fn non_transient_failure_never_spends_the_retry() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"quota"}),
        )
        .await;
        let provider = Arc::new(FlakyProvider {
            failures: Mutex::new(vec![WebSearchError::QuotaExhausted(
                WebSearchProviderKind::Exa,
            )]),
            requests: std::sync::atomic::AtomicUsize::new(0),
            response: WebSearchResponse::new(WebSearchProviderKind::Exa, Vec::new()),
        });
        let fake = Arc::new(FakeSearch {
            resolution: Ok(Some(provider.clone())),
        });
        assert_eq!(
            worker(store.clone(), fake).run_once().await.unwrap(),
            SandboxWebSearchWorkerOutcome::Resolved(call.id)
        );
        assert_eq!(
            store
                .get_sandbox_tool_call_receipt(call.id)
                .await
                .unwrap()
                .unwrap()
                .error_code
                .as_deref(),
            Some("web_search_failed")
        );
        assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
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
        assert!(receipt.result.len() <= tidebreak_core::SandboxToolCall::MAX_RESULT_BYTES);
    }

    #[tokio::test]
    async fn local_signal_drops_web_search_resolution_and_late_work_stays_fenced() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"cancel resolution"}),
        )
        .await;
        let lease = uuid::Uuid::new_v4();
        let claimed = match store
            .claim_sandbox_tool_call(call.id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
        {
            ClaimSandboxToolCallOutcome::Claimed(call) => call,
            outcome => panic!("unexpected claim: {outcome:?}"),
        };
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(SandboxAttemptGuard::default());
        let worker = SandboxWebSearchWorker::with_search_and_attempts(
            store.clone(),
            Arc::new(BlockingResolution {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }),
            Arc::new(Notify::new()),
            attempts.clone(),
            SandboxWebSearchWorkerConfig::default(),
        );
        let entered_wait = entered.notified();
        let execution = tokio::spawn(async move { worker.process(claimed, lease).await });
        entered_wait.await;
        assert!(matches!(
            store
                .request_agent_run_cancellation(call.agent_run_id)
                .await
                .unwrap(),
            Some(RequestAgentRunCancellationOutcome::Cancelled(_))
        ));
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert!(attempts.cancel_checkpoint(
            call.id,
            call.agent_run_id,
            receipt.executor_lease_token
        ));
        assert_eq!(
            execution.await.unwrap().unwrap(),
            SandboxWebSearchWorkerOutcome::LeaseLost(call.id)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(
            store
                .resolve_sandbox_tool_call(
                    call.id,
                    lease,
                    &ToolCallResolution::Completed {
                        result: "late".into()
                    }
                )
                .await
                .unwrap(),
            tidebreak_core::ResolveSandboxToolCallOutcome::LeaseLost
                | tidebreak_core::ResolveSandboxToolCallOutcome::AlreadyTerminal
        ));
        assert_eq!(
            store
                .get_sandbox_tool_call_receipt(call.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            tidebreak_core::SandboxToolCallStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn local_signal_drops_web_search_http_and_late_work_stays_fenced() {
        let (store, _dir) = test_store().await;
        let call = checkpoint(
            &store,
            WEB_SEARCH_TOOL,
            serde_json::json!({"query":"cancel HTTP"}),
        )
        .await;
        let lease = uuid::Uuid::new_v4();
        let claimed = match store
            .claim_sandbox_tool_call(call.id, lease, chrono::Duration::minutes(5))
            .await
            .unwrap()
        {
            ClaimSandboxToolCallOutcome::Claimed(call) => call,
            outcome => panic!("unexpected claim: {outcome:?}"),
        };
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(SandboxAttemptGuard::default());
        let worker = SandboxWebSearchWorker::with_search_and_attempts(
            store.clone(),
            Arc::new(FakeSearch {
                resolution: Ok(Some(Arc::new(BlockingSearchProvider {
                    entered: entered.clone(),
                    dropped: dropped.clone(),
                }))),
            }),
            Arc::new(Notify::new()),
            attempts.clone(),
            SandboxWebSearchWorkerConfig::default(),
        );
        let entered_wait = entered.notified();
        let execution = tokio::spawn(async move { worker.process(claimed, lease).await });
        entered_wait.await;
        assert!(matches!(
            store
                .request_agent_run_cancellation(call.agent_run_id)
                .await
                .unwrap(),
            Some(RequestAgentRunCancellationOutcome::Cancelled(_))
        ));
        let receipt = store
            .get_sandbox_tool_call_receipt(call.id)
            .await
            .unwrap()
            .unwrap();
        assert!(attempts.cancel_checkpoint(
            call.id,
            call.agent_run_id,
            receipt.executor_lease_token
        ));
        assert_eq!(
            execution.await.unwrap().unwrap(),
            SandboxWebSearchWorkerOutcome::LeaseLost(call.id)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            store
                .get_sandbox_tool_call_receipt(call.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            tidebreak_core::SandboxToolCallStatus::Cancelled
        );
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

//! Bounded process diagnostics for operators and performance investigations.
//!
//! The live surface stays deliberately small: request and named-operation
//! histograms, process uptime/resource counters, OpenMetrics text, and a ZIP
//! bundle containing those snapshots plus the profile's allowlisted log files.
//! It never reads the database, blobs, keychain, or arbitrary profile files.
//!
//! The event target is separate from the human log target. The logging module
//! writes these high-volume timing events only to the structured JSONL file,
//! where an agent can analyze them without flooding `tidebreak.log`.

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{MatchedPath, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use futures::StreamExt as _;
use serde::Serialize;
use tidebreak_core::{
    ChatRequest, ModelProvider, Profile, ProviderEvent, ProviderId, Result as AgentResult,
    StopReason,
};
use tracing::Instrument as _;
use zip::write::SimpleFileOptions;

use crate::error::ServerError;
use crate::resolver::ProviderResolver;
use crate::state::AppState;

/// Tracing target reserved for machine-oriented timing events.
pub const EVENT_TARGET: &str = tidebreak_core::DIAGNOSTICS_TRACING_TARGET;

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const BUNDLE_SCHEMA_VERSION: u32 = 1;
const BUNDLE_LOG_TAIL_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MODEL_METRIC_SERIES: usize = 128;
const MAX_METRIC_LABEL_BYTES: usize = 128;

/// Millisecond boundaries shared by the JSON snapshot and OpenMetrics export.
const DURATION_BUCKETS_MS: [u64; 16] = [
    1, 2, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 300_000,
];

const BUILD_VERSION: &str = match option_env!("TIDEBREAK_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Per-process measurements. One instance belongs to one assembled server.
pub struct Diagnostics {
    started_at: DateTime<Utc>,
    started: Instant,
    in_flight_http: Arc<AtomicU64>,
    in_flight_model_requests: AtomicU64,
    state: Mutex<DiagnosticState>,
}

#[derive(Default)]
struct DiagnosticState {
    http: BTreeMap<HttpMetricKey, Histogram>,
    model_requests: BTreeMap<ModelMetricKey, ModelMeasurements>,
    operations: BTreeMap<OperationMetricKey, Histogram>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpMetricKey {
    method: String,
    route: String,
    status_class: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OperationMetricKey {
    operation: String,
    outcome: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ModelMetricKey {
    provider: String,
    model: String,
    outcome: String,
}

#[derive(Clone, Debug, Default)]
struct ModelMeasurements {
    duration: Histogram,
    time_to_first_event: Histogram,
    input_tokens: u64,
    uncached_input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

#[derive(Clone, Debug, Default)]
struct Histogram {
    count: u64,
    sum_ms: u128,
    max_ms: u64,
    buckets: [u64; DURATION_BUCKETS_MS.len()],
}

impl Histogram {
    fn observe(&mut self, duration: Duration) {
        let elapsed_ms = duration_ms(duration);
        self.count = self.count.saturating_add(1);
        self.sum_ms = self.sum_ms.saturating_add(u128::from(elapsed_ms));
        self.max_ms = self.max_ms.max(elapsed_ms);
        for (index, boundary) in DURATION_BUCKETS_MS.iter().enumerate() {
            if elapsed_ms <= *boundary {
                self.buckets[index] = self.buckets[index].saturating_add(1);
            }
        }
    }

    fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            count: self.count,
            sum_ms: self.sum_ms.min(u128::from(u64::MAX)) as u64,
            max_ms: self.max_ms,
            buckets: DURATION_BUCKETS_MS
                .iter()
                .zip(self.buckets)
                .map(|(le_ms, count)| HistogramBucketSnapshot {
                    le_ms: *le_ms,
                    count,
                })
                .collect(),
        }
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new()
    }
}

impl Diagnostics {
    pub fn new() -> Self {
        Self {
            started_at: Utc::now(),
            started: Instant::now(),
            in_flight_http: Arc::new(AtomicU64::new(0)),
            in_flight_model_requests: AtomicU64::new(0),
            state: Mutex::new(DiagnosticState::default()),
        }
    }

    fn begin_http(&self) -> HttpInFlightGuard {
        self.in_flight_http.fetch_add(1, Ordering::Relaxed);
        HttpInFlightGuard {
            gauge: self.in_flight_http.clone(),
        }
    }

    fn observe_http(&self, method: &str, route: &str, status: StatusCode, duration: Duration) {
        let key = HttpMetricKey {
            method: method.to_owned(),
            route: route.to_owned(),
            status_class: status_class(status).to_owned(),
        };
        self.lock_state()
            .http
            .entry(key)
            .or_default()
            .observe(duration);
    }

    /// Record one bounded, host-defined operation. Callers must use stable
    /// names and outcomes rather than ids or provider-authored strings.
    pub fn observe_operation(
        &self,
        operation: &'static str,
        outcome: &'static str,
        duration: Duration,
    ) {
        let key = OperationMetricKey {
            operation: operation.to_owned(),
            outcome: outcome.to_owned(),
        };
        self.lock_state()
            .operations
            .entry(key)
            .or_default()
            .observe(duration);
    }

    fn observe_model_request(
        &self,
        provider: &str,
        model: &str,
        outcome: &str,
        duration: Duration,
        first_event_ms: Option<u64>,
        usage: ModelUsageSnapshot,
    ) {
        let mut state = self.lock_state();
        let mut key = ModelMetricKey {
            provider: bounded_metric_label(provider),
            model: bounded_metric_label(model),
            outcome: bounded_metric_label(outcome),
        };
        if !state.model_requests.contains_key(&key)
            && state.model_requests.len() >= MAX_MODEL_METRIC_SERIES
        {
            key.provider = "other".to_owned();
            key.model = "other".to_owned();
        }
        let measurements = state.model_requests.entry(key).or_default();
        measurements.duration.observe(duration);
        if let Some(first_event_ms) = first_event_ms {
            measurements
                .time_to_first_event
                .observe(Duration::from_millis(first_event_ms));
        }
        measurements.input_tokens = measurements.input_tokens.saturating_add(usage.input_tokens);
        measurements.uncached_input_tokens = measurements
            .uncached_input_tokens
            .saturating_add(usage.uncached_input_tokens);
        measurements.output_tokens = measurements
            .output_tokens
            .saturating_add(usage.output_tokens);
        measurements.cache_read_input_tokens = measurements
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        measurements.cache_creation_input_tokens = measurements
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
    }

    pub fn snapshot(&self, profile: Profile) -> DiagnosticSnapshot {
        let state = self.lock_state();
        DiagnosticSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            generated_at: Utc::now(),
            build: BuildSnapshot {
                version: BUILD_VERSION.to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                profile: profile_name(profile).to_owned(),
            },
            process: process_snapshot(self.started_at, self.started.elapsed()),
            runtime: runtime_snapshot(),
            http: HttpDiagnosticsSnapshot {
                in_flight: self.in_flight_http.load(Ordering::Relaxed),
                requests: state
                    .http
                    .iter()
                    .map(|(key, histogram)| HttpRequestSnapshot {
                        method: key.method.clone(),
                        route: key.route.clone(),
                        status_class: key.status_class.clone(),
                        duration: histogram.snapshot(),
                    })
                    .collect(),
            },
            model: ModelDiagnosticsSnapshot {
                in_flight: self.in_flight_model_requests.load(Ordering::Relaxed),
                requests: state
                    .model_requests
                    .iter()
                    .map(|(key, measurements)| ModelRequestSnapshot {
                        provider: key.provider.clone(),
                        model: key.model.clone(),
                        outcome: key.outcome.clone(),
                        duration: measurements.duration.snapshot(),
                        time_to_first_event: measurements.time_to_first_event.snapshot(),
                        usage: ModelUsageSnapshot {
                            input_tokens: measurements.input_tokens,
                            uncached_input_tokens: measurements.uncached_input_tokens,
                            output_tokens: measurements.output_tokens,
                            cache_read_input_tokens: measurements.cache_read_input_tokens,
                            cache_creation_input_tokens: measurements.cache_creation_input_tokens,
                        },
                    })
                    .collect(),
            },
            operations: state
                .operations
                .iter()
                .map(|(key, histogram)| OperationSnapshot {
                    operation: key.operation.clone(),
                    outcome: key.outcome.clone(),
                    duration: histogram.snapshot(),
                })
                .collect(),
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, DiagnosticState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Add model-request spans and timings without changing provider behavior.
pub struct DiagnosticProviderResolver {
    inner: Arc<dyn ProviderResolver>,
    diagnostics: Arc<Diagnostics>,
}

impl DiagnosticProviderResolver {
    pub fn new(inner: Arc<dyn ProviderResolver>, diagnostics: Arc<Diagnostics>) -> Self {
        Self { inner, diagnostics }
    }

    fn wrap(&self, provider: Arc<dyn ModelProvider>) -> Arc<dyn ModelProvider> {
        Arc::new(DiagnosticModelProvider {
            inner: provider,
            diagnostics: self.diagnostics.clone(),
        })
    }
}

#[async_trait]
impl ProviderResolver for DiagnosticProviderResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.wrap(self.inner.resolve().await)
    }

    async fn resolve_for(&self, owner: Option<&tidebreak_core::OwnerId>) -> Arc<dyn ModelProvider> {
        self.wrap(self.inner.resolve_for(owner).await)
    }

    fn enforces_model_registry(&self) -> bool {
        self.inner.enforces_model_registry()
    }
}

struct DiagnosticModelProvider {
    inner: Arc<dyn ModelProvider>,
    diagnostics: Arc<Diagnostics>,
}

#[async_trait]
impl ModelProvider for DiagnosticModelProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    async fn stream(&self, request: ChatRequest) -> AgentResult<BoxStream<'static, ProviderEvent>> {
        let provider = request
            .provider
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| self.inner.id().to_string());
        let otel_provider = otel_provider_name(&provider);
        let model = request.model.clone();
        let conversation = request.conversation.map(|id| id.to_string());
        let message_count = request.messages.len();
        let tool_count = request.tools.len();
        let image_count = request.images.len();
        let image_bytes = request.images.total_bytes();
        let max_tokens = request.max_tokens;
        let span_name = format!("chat {model}");
        let span = tracing::info_span!(
            target: EVENT_TARGET,
            "gen_ai.client.operation",
            otel.name = %span_name,
            otel.kind = "client",
            otel.status_code = tracing::field::Empty,
            gen_ai.operation.name = "chat",
            gen_ai.provider.name = %otel_provider,
            tidebreak.provider.id = %provider,
            gen_ai.request.model = %model,
            gen_ai.conversation.id = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            gen_ai.usage.cache_creation.input_tokens = tracing::field::Empty,
            gen_ai.request.max_tokens = tracing::field::Empty,
            tidebreak.request.message_count = message_count,
            tidebreak.request.tool_count = tool_count,
            tidebreak.request.image_count = image_count,
            tidebreak.request.image_bytes = image_bytes,
            tidebreak.outcome = tracing::field::Empty,
            tidebreak.finish_reason = tracing::field::Empty,
            tidebreak.duration_ms = tracing::field::Empty,
            tidebreak.time_to_first_event_ms = tracing::field::Empty,
            error.type = tracing::field::Empty,
        );
        if let Some(conversation) = conversation.as_deref() {
            span.record("gen_ai.conversation.id", conversation);
        }
        if let Some(max_tokens) = max_tokens {
            span.record("gen_ai.request.max_tokens", max_tokens);
        }
        let mut guard =
            ProviderRequestGuard::new(self.diagnostics.clone(), span.clone(), provider, model);
        let mut stream = match self.inner.stream(request).instrument(span.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                guard.finish(ProviderTerminal::error(error.kind()));
                return Err(error);
            }
        };
        Ok(futures::stream::poll_fn(move |cx| {
            let poll = span.in_scope(|| stream.as_mut().poll_next(cx));
            match poll {
                Poll::Ready(Some(event)) => {
                    guard.observe(&event);
                    Poll::Ready(Some(event))
                }
                Poll::Ready(None) => {
                    guard.finish_stream();
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .boxed())
    }
}

#[derive(Clone, Copy)]
struct ProviderTerminal {
    outcome: &'static str,
    finish_reason: Option<&'static str>,
    error_type: Option<&'static str>,
    is_error: bool,
}

impl ProviderTerminal {
    fn stop(reason: StopReason) -> Self {
        let reason = stop_reason_name(reason);
        Self {
            outcome: reason,
            finish_reason: Some(reason),
            error_type: None,
            is_error: false,
        }
    }

    fn error(kind: &str) -> Self {
        let kind = provider_error_kind(kind);
        Self {
            outcome: kind,
            finish_reason: None,
            error_type: Some(kind),
            is_error: true,
        }
    }

    const fn refusal() -> Self {
        Self {
            outcome: "refusal",
            finish_reason: Some("refusal"),
            error_type: None,
            is_error: false,
        }
    }

    const fn incomplete() -> Self {
        Self {
            outcome: "incomplete_stream",
            finish_reason: None,
            error_type: Some("incomplete_stream"),
            is_error: true,
        }
    }

    const fn cancelled() -> Self {
        Self {
            outcome: "cancelled",
            finish_reason: Some("cancelled"),
            error_type: None,
            is_error: false,
        }
    }
}

struct ProviderRequestGuard {
    diagnostics: Arc<Diagnostics>,
    span: tracing::Span,
    provider: String,
    model: String,
    started: Instant,
    first_event_ms: Option<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    terminal: Option<ProviderTerminal>,
    finished: bool,
}

impl ProviderRequestGuard {
    fn new(
        diagnostics: Arc<Diagnostics>,
        span: tracing::Span,
        provider: String,
        model: String,
    ) -> Self {
        diagnostics
            .in_flight_model_requests
            .fetch_add(1, Ordering::Relaxed);
        Self {
            diagnostics,
            span,
            provider,
            model,
            started: Instant::now(),
            first_event_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            terminal: None,
            finished: false,
        }
    }

    fn observe(&mut self, event: &ProviderEvent) {
        self.first_event_ms
            .get_or_insert_with(|| duration_ms(self.started.elapsed()));
        match event {
            ProviderEvent::Usage(usage) => {
                self.input_tokens = self
                    .input_tokens
                    .saturating_add(u64::from(usage.input_tokens));
                self.output_tokens = self
                    .output_tokens
                    .saturating_add(u64::from(usage.output_tokens));
                self.cache_read_input_tokens = self
                    .cache_read_input_tokens
                    .saturating_add(u64::from(usage.cache_read_input_tokens));
                self.cache_creation_input_tokens = self
                    .cache_creation_input_tokens
                    .saturating_add(u64::from(usage.cache_creation_input_tokens));
            }
            ProviderEvent::Stop { reason } => {
                self.terminal.get_or_insert(ProviderTerminal::stop(*reason));
            }
            ProviderEvent::Refusal { .. } => {
                self.terminal.get_or_insert(ProviderTerminal::refusal());
            }
            ProviderEvent::Failed { error } => {
                self.terminal
                    .get_or_insert_with(|| ProviderTerminal::error(&error.kind));
            }
            _ => {}
        }
    }

    fn finish_stream(&mut self) {
        self.finish(self.terminal.unwrap_or_else(ProviderTerminal::incomplete));
    }

    fn finish(&mut self, terminal: ProviderTerminal) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.diagnostics
            .in_flight_model_requests
            .fetch_sub(1, Ordering::Relaxed);
        let duration = self.started.elapsed();
        let duration_ms = duration_ms(duration);
        let usage = ModelUsageSnapshot {
            input_tokens: self
                .input_tokens
                .saturating_add(self.cache_read_input_tokens)
                .saturating_add(self.cache_creation_input_tokens),
            uncached_input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
        };
        self.diagnostics.observe_model_request(
            &self.provider,
            &self.model,
            terminal.outcome,
            duration,
            self.first_event_ms,
            usage,
        );
        self.diagnostics
            .observe_operation("model_request", terminal.outcome, duration);
        self.span.record("tidebreak.outcome", terminal.outcome);
        self.span.record("tidebreak.duration_ms", duration_ms);
        self.span
            .record("gen_ai.usage.input_tokens", usage.input_tokens);
        self.span
            .record("gen_ai.usage.output_tokens", self.output_tokens);
        self.span.record(
            "gen_ai.usage.cache_read.input_tokens",
            self.cache_read_input_tokens,
        );
        self.span.record(
            "gen_ai.usage.cache_creation.input_tokens",
            self.cache_creation_input_tokens,
        );
        if let Some(first_event_ms) = self.first_event_ms {
            self.span
                .record("tidebreak.time_to_first_event_ms", first_event_ms);
        }
        if let Some(finish_reason) = terminal.finish_reason {
            self.span.record("tidebreak.finish_reason", finish_reason);
        }
        if let Some(error_type) = terminal.error_type {
            self.span.record("error.type", error_type);
        }
        self.span.record(
            "otel.status_code",
            if terminal.is_error { "ERROR" } else { "OK" },
        );
        self.span.in_scope(|| {
            tracing::info!(
                target: EVENT_TARGET,
                event_name = "gen_ai.client.operation.completed",
                provider = %self.provider,
                model = %self.model,
                outcome = terminal.outcome,
                duration_ms,
                time_to_first_event_ms = self.first_event_ms.unwrap_or_default(),
                first_event_observed = self.first_event_ms.is_some(),
                input_tokens = usage.input_tokens,
                uncached_input_tokens = usage.uncached_input_tokens,
                output_tokens = self.output_tokens,
                cache_read_input_tokens = self.cache_read_input_tokens,
                cache_creation_input_tokens = self.cache_creation_input_tokens,
                "model request completed"
            );
        });
    }
}

impl Drop for ProviderRequestGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finish(self.terminal.unwrap_or_else(ProviderTerminal::cancelled));
    }
}

fn stop_reason_name(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::ToolUse => "tool_use",
        StopReason::StopSequence => "stop_sequence",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        _ => "other",
    }
}

fn provider_error_kind(kind: &str) -> &'static str {
    match kind {
        "authentication" => "authentication",
        "access_denied" => "access_denied",
        "rate_limited" => "rate_limited",
        "overloaded" => "overloaded",
        "invalid_request" => "invalid_request",
        "refusal" => "refusal",
        "prompt_too_long" => "prompt_too_long",
        "missing_credential" => "missing_credential",
        "invalid_target" => "invalid_target",
        _ => "provider",
    }
}

fn otel_provider_name(provider: &str) -> &str {
    match provider {
        "gemini" => "gcp.gemini",
        "xai" => "x_ai",
        _ => provider,
    }
}

struct HttpInFlightGuard {
    gauge: Arc<AtomicU64>,
}

impl Drop for HttpInFlightGuard {
    fn drop(&mut self) {
        self.gauge.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticSnapshot {
    schema_version: u32,
    generated_at: DateTime<Utc>,
    build: BuildSnapshot,
    process: ProcessSnapshot,
    runtime: Option<RuntimeSnapshot>,
    http: HttpDiagnosticsSnapshot,
    model: ModelDiagnosticsSnapshot,
    operations: Vec<OperationSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
struct BuildSnapshot {
    version: String,
    os: String,
    arch: String,
    profile: String,
}

#[derive(Clone, Debug, Serialize)]
struct ProcessSnapshot {
    pid: u32,
    started_at: DateTime<Utc>,
    uptime_ms: u64,
    cpu_user_ms: Option<u64>,
    cpu_system_ms: Option<u64>,
    max_resident_memory_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct RuntimeSnapshot {
    worker_threads: u64,
    alive_tasks: u64,
    global_queue_depth: u64,
}

#[derive(Clone, Debug, Serialize)]
struct HttpDiagnosticsSnapshot {
    in_flight: u64,
    requests: Vec<HttpRequestSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
struct ModelDiagnosticsSnapshot {
    in_flight: u64,
    requests: Vec<ModelRequestSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
struct ModelRequestSnapshot {
    provider: String,
    model: String,
    outcome: String,
    duration: HistogramSnapshot,
    time_to_first_event: HistogramSnapshot,
    usage: ModelUsageSnapshot,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct ModelUsageSnapshot {
    input_tokens: u64,
    uncached_input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
}

#[derive(Clone, Debug, Serialize)]
struct HttpRequestSnapshot {
    method: String,
    route: String,
    status_class: String,
    duration: HistogramSnapshot,
}

#[derive(Clone, Debug, Serialize)]
struct OperationSnapshot {
    operation: String,
    outcome: String,
    duration: HistogramSnapshot,
}

#[derive(Clone, Debug, Serialize)]
struct HistogramSnapshot {
    count: u64,
    sum_ms: u64,
    max_ms: u64,
    buckets: Vec<HistogramBucketSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
struct HistogramBucketSnapshot {
    le_ms: u64,
    count: u64,
}

/// Measure one matched request without recording its raw URI or query string.
pub async fn observe_http_request(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>")
        .to_owned();
    let operation_name = format!("{method} {route}");
    let span = tracing::info_span!(
        target: EVENT_TARGET,
        "http.server.request",
        otel.name = %operation_name,
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        http.request.method = %method,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
        tidebreak.duration_ms = tracing::field::Empty,
        error.type = tracing::field::Empty,
    );
    let started = Instant::now();
    let _in_flight = state.diagnostics.begin_http();
    let response = next.run(request).instrument(span.clone()).await;
    let duration = started.elapsed();
    let status = response.status();
    state
        .diagnostics
        .observe_http(&method, &route, status, duration);
    span.record("http.response.status_code", status.as_u16());
    span.record("tidebreak.duration_ms", duration_ms(duration));
    if status.is_server_error() {
        span.record("error.type", status.as_str());
        span.record("otel.status_code", "ERROR");
    }
    span.in_scope(|| {
        tracing::info!(
            target: EVENT_TARGET,
            event_name = "http.server.request.completed",
            http_status_code = status.as_u16(),
            duration_ms = duration_ms(duration),
            "request completed"
        );
    });
    response
}

/// `GET /diagnostics/snapshot` — one stable JSON view of live measurements.
pub async fn get_snapshot(State(state): State<AppState>) -> Response {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Json(state.diagnostics.snapshot(state.config.profile)),
    )
        .into_response()
}

/// `GET /diagnostics/metrics` — OpenMetrics text for local scraping or export.
pub async fn get_metrics(State(state): State<AppState>) -> Response {
    let snapshot = state.diagnostics.snapshot(state.config.profile);
    let body = render_openmetrics(&snapshot);
    (
        [
            (
                header::CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

/// `GET /diagnostics/export` — the snapshot, OpenMetrics, and allowlisted logs.
pub async fn get_export(State(state): State<AppState>) -> Result<Response, ServerError> {
    let snapshot = state.diagnostics.snapshot(state.config.profile);
    let metrics = render_openmetrics(&snapshot);
    let data_dir = state.config.data_dir.clone();
    let bytes = tokio::task::spawn_blocking(move || build_bundle(&data_dir, &snapshot, &metrics))
        .await
        .map_err(|_| ServerError::internal("diagnostic export worker stopped"))?
        .map_err(|error| {
            tracing::warn!(%error, "could not build diagnostic export");
            ServerError::internal("could not build diagnostic export")
        })?;
    let byte_len = bytes.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"tidebreak-diagnostics.zip\"",
        )
        .header(header::CONTENT_LENGTH, byte_len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| ServerError::internal("could not build diagnostic export response"))
}

fn render_openmetrics(snapshot: &DiagnosticSnapshot) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;

    out.push_str(
        "# HELP tidebreak_process_uptime_seconds Time since this Tidebreak process started.\n",
    );
    out.push_str("# TYPE tidebreak_process_uptime_seconds gauge\n");
    let _ = writeln!(
        out,
        "tidebreak_process_uptime_seconds {}",
        seconds(snapshot.process.uptime_ms)
    );
    out.push_str("# HELP tidebreak_http_server_requests_in_flight Requests that have entered Tidebreak and not returned.\n");
    out.push_str("# TYPE tidebreak_http_server_requests_in_flight gauge\n");
    let _ = writeln!(
        out,
        "tidebreak_http_server_requests_in_flight {}",
        snapshot.http.in_flight
    );
    out.push_str("# HELP tidebreak_gen_ai_client_requests_in_flight Model requests that have started and not reached a terminal stream outcome.\n");
    out.push_str("# TYPE tidebreak_gen_ai_client_requests_in_flight gauge\n");
    let _ = writeln!(
        out,
        "tidebreak_gen_ai_client_requests_in_flight {}",
        snapshot.model.in_flight
    );
    if let Some(runtime) = &snapshot.runtime {
        out.push_str("# HELP tidebreak_runtime_worker_threads Tokio runtime worker threads.\n");
        out.push_str("# TYPE tidebreak_runtime_worker_threads gauge\n");
        let _ = writeln!(
            out,
            "tidebreak_runtime_worker_threads {}",
            runtime.worker_threads
        );
        out.push_str("# HELP tidebreak_runtime_tasks_alive Tasks that have started and not completed on this Tokio runtime.\n");
        out.push_str("# TYPE tidebreak_runtime_tasks_alive gauge\n");
        let _ = writeln!(out, "tidebreak_runtime_tasks_alive {}", runtime.alive_tasks);
        out.push_str("# HELP tidebreak_runtime_global_queue_depth Tasks pending in the Tokio runtime global queue.\n");
        out.push_str("# TYPE tidebreak_runtime_global_queue_depth gauge\n");
        let _ = writeln!(
            out,
            "tidebreak_runtime_global_queue_depth {}",
            runtime.global_queue_depth
        );
    }
    out.push_str("# HELP tidebreak_http_server_request_duration_seconds End-to-end HTTP request duration by matched route.\n");
    out.push_str("# TYPE tidebreak_http_server_request_duration_seconds histogram\n");
    for request in &snapshot.http.requests {
        let labels = format!(
            "method=\"{}\",route=\"{}\",status_class=\"{}\"",
            metric_label(&request.method),
            metric_label(&request.route),
            metric_label(&request.status_class),
        );
        render_histogram(
            &mut out,
            "tidebreak_http_server_request_duration_seconds",
            &labels,
            &request.duration,
        );
    }
    out.push_str("# HELP tidebreak_gen_ai_client_operation_duration_seconds End-to-end model request duration.\n");
    out.push_str("# TYPE tidebreak_gen_ai_client_operation_duration_seconds histogram\n");
    for request in &snapshot.model.requests {
        let labels = format!(
            "operation=\"chat\",provider=\"{}\",model=\"{}\",outcome=\"{}\"",
            metric_label(&request.provider),
            metric_label(&request.model),
            metric_label(&request.outcome),
        );
        render_histogram(
            &mut out,
            "tidebreak_gen_ai_client_operation_duration_seconds",
            &labels,
            &request.duration,
        );
    }
    out.push_str("# HELP tidebreak_gen_ai_client_time_to_first_event_seconds Time from model request start to the first provider stream event.\n");
    out.push_str("# TYPE tidebreak_gen_ai_client_time_to_first_event_seconds histogram\n");
    for request in &snapshot.model.requests {
        if request.time_to_first_event.count == 0 {
            continue;
        }
        let labels = format!(
            "operation=\"chat\",provider=\"{}\",model=\"{}\",outcome=\"{}\"",
            metric_label(&request.provider),
            metric_label(&request.model),
            metric_label(&request.outcome),
        );
        render_histogram(
            &mut out,
            "tidebreak_gen_ai_client_time_to_first_event_seconds",
            &labels,
            &request.time_to_first_event,
        );
    }
    out.push_str(
        "# HELP tidebreak_gen_ai_client_tokens_total Model tokens reported by providers.\n",
    );
    out.push_str("# TYPE tidebreak_gen_ai_client_tokens_total counter\n");
    for request in &snapshot.model.requests {
        let labels = format!(
            "operation=\"chat\",provider=\"{}\",model=\"{}\",outcome=\"{}\"",
            metric_label(&request.provider),
            metric_label(&request.model),
            metric_label(&request.outcome),
        );
        for (token_type, count) in [
            ("input", request.usage.input_tokens),
            ("uncached_input", request.usage.uncached_input_tokens),
            ("output", request.usage.output_tokens),
            ("cache_read_input", request.usage.cache_read_input_tokens),
            (
                "cache_creation_input",
                request.usage.cache_creation_input_tokens,
            ),
        ] {
            let _ = writeln!(
                out,
                "tidebreak_gen_ai_client_tokens_total{{{labels},type=\"{token_type}\"}} {count}"
            );
        }
    }
    out.push_str(
        "# HELP tidebreak_operation_duration_seconds Duration of host-defined operations.\n",
    );
    out.push_str("# TYPE tidebreak_operation_duration_seconds histogram\n");
    for operation in &snapshot.operations {
        let labels = format!(
            "operation=\"{}\",outcome=\"{}\"",
            metric_label(&operation.operation),
            metric_label(&operation.outcome),
        );
        render_histogram(
            &mut out,
            "tidebreak_operation_duration_seconds",
            &labels,
            &operation.duration,
        );
    }
    if let Some(bytes) = snapshot.process.max_resident_memory_bytes {
        out.push_str("# HELP tidebreak_process_max_resident_memory_bytes Highest resident memory observed by the operating system.\n");
        out.push_str("# TYPE tidebreak_process_max_resident_memory_bytes gauge\n");
        let _ = writeln!(out, "tidebreak_process_max_resident_memory_bytes {bytes}");
    }
    for (mode, value) in [
        ("user", snapshot.process.cpu_user_ms),
        ("system", snapshot.process.cpu_system_ms),
    ] {
        if let Some(value) = value {
            let _ = writeln!(
                out,
                "tidebreak_process_cpu_seconds_total{{mode=\"{mode}\"}} {}",
                seconds(value)
            );
        }
    }
    if snapshot.process.cpu_user_ms.is_some() || snapshot.process.cpu_system_ms.is_some() {
        out.insert_str(
            out.find("tidebreak_process_cpu_seconds_total")
                .unwrap_or(out.len()),
            "# HELP tidebreak_process_cpu_seconds_total CPU time consumed by this process.\n# TYPE tidebreak_process_cpu_seconds_total counter\n",
        );
    }
    out.push_str("# EOF\n");
    out
}

fn render_histogram(out: &mut String, name: &str, labels: &str, histogram: &HistogramSnapshot) {
    use std::fmt::Write as _;

    for bucket in &histogram.buckets {
        let _ = writeln!(
            out,
            "{name}_bucket{{{labels},le=\"{}\"}} {}",
            seconds(bucket.le_ms),
            bucket.count
        );
    }
    let _ = writeln!(
        out,
        "{name}_bucket{{{labels},le=\"+Inf\"}} {}",
        histogram.count
    );
    let _ = writeln!(out, "{name}_sum{{{labels}}} {}", seconds(histogram.sum_ms));
    let _ = writeln!(out, "{name}_count{{{labels}}} {}", histogram.count);
}

fn seconds(milliseconds: u64) -> String {
    format!("{}.{:03}", milliseconds / 1_000, milliseconds % 1_000)
}

fn metric_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn bounded_metric_label(value: &str) -> String {
    let mut bounded = String::new();
    for character in value.chars() {
        let character = if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if bounded.len().saturating_add(character.len_utf8()) > MAX_METRIC_LABEL_BYTES {
            break;
        }
        bounded.push(character);
    }
    if bounded.is_empty() {
        "unknown".to_owned()
    } else {
        bounded
    }
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Desktop => "desktop",
        Profile::SelfHost => "self_host",
        _ => "unknown",
    }
}

fn process_snapshot(started_at: DateTime<Utc>, uptime: Duration) -> ProcessSnapshot {
    let (cpu_user_ms, cpu_system_ms, max_resident_memory_bytes) = process_usage();
    ProcessSnapshot {
        pid: std::process::id(),
        started_at,
        uptime_ms: duration_ms(uptime),
        cpu_user_ms,
        cpu_system_ms,
        max_resident_memory_bytes,
    }
}

fn runtime_snapshot() -> Option<RuntimeSnapshot> {
    let metrics = tokio::runtime::Handle::try_current().ok()?.metrics();
    Some(RuntimeSnapshot {
        worker_threads: usize_u64(metrics.num_workers()),
        alive_tasks: usize_u64(metrics.num_alive_tasks()),
        global_queue_depth: usize_u64(metrics.global_queue_depth()),
    })
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(unix)]
fn process_usage() -> (Option<u64>, Option<u64>, Option<u64>) {
    // SAFETY: `getrusage` initializes the provided `rusage` value for the
    // current process, and the pointer stays valid for the duration of the call.
    let usage = unsafe {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return (None, None, None);
        }
        usage.assume_init()
    };
    let user = timeval_ms(usage.ru_utime.tv_sec, usage.ru_utime.tv_usec);
    let system = timeval_ms(usage.ru_stime.tv_sec, usage.ru_stime.tv_usec);
    #[cfg(target_os = "macos")]
    let max_rss = u64::try_from(usage.ru_maxrss).ok();
    #[cfg(not(target_os = "macos"))]
    let max_rss = u64::try_from(usage.ru_maxrss)
        .ok()
        .map(|value| value.saturating_mul(1_024));
    (user, system, max_rss)
}

#[cfg(unix)]
fn timeval_ms(seconds: libc::time_t, micros: libc::suseconds_t) -> Option<u64> {
    let seconds = u64::try_from(seconds).ok()?;
    let micros = u64::try_from(micros).ok()?;
    seconds.checked_mul(1_000)?.checked_add(micros / 1_000)
}

#[cfg(not(unix))]
fn process_usage() -> (Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None)
}

#[derive(Serialize)]
struct BundleManifest {
    schema_version: u32,
    generated_at: DateTime<Utc>,
    build_version: String,
    profile: String,
    files: Vec<BundleFileManifest>,
    excludes: Vec<&'static str>,
}

#[derive(Serialize)]
struct BundleFileManifest {
    path: String,
    source_bytes: u64,
    included_bytes: usize,
    tail_truncated: bool,
}

struct BundleEntry {
    path: &'static str,
    bytes: Vec<u8>,
    source_bytes: u64,
    tail_truncated: bool,
}

fn build_bundle(
    data_dir: &Path,
    snapshot: &DiagnosticSnapshot,
    metrics: &str,
) -> Result<Vec<u8>, String> {
    let mut entries = Vec::new();
    let directory = Dir::open_ambient_dir(data_dir, ambient_authority()).ok();
    if let Some(directory) = directory.as_ref() {
        for (source, destination) in [
            ("logs/tidebreak.log", "logs/tidebreak.log"),
            ("logs/tidebreak.log.1", "logs/tidebreak.log.1"),
            ("logs/tidebreak.events.jsonl", "logs/tidebreak.events.jsonl"),
            (
                "logs/tidebreak.events.jsonl.1",
                "logs/tidebreak.events.jsonl.1",
            ),
            ("boot-failures.log", "logs/boot-failures.log"),
        ] {
            if let Some((bytes, source_bytes, tail_truncated)) =
                read_regular_tail(directory, source, BUNDLE_LOG_TAIL_BYTES)?
            {
                entries.push(BundleEntry {
                    path: destination,
                    bytes,
                    source_bytes,
                    tail_truncated,
                });
            }
        }
    }

    let file_manifest = entries
        .iter()
        .map(|entry| BundleFileManifest {
            path: entry.path.to_owned(),
            source_bytes: entry.source_bytes,
            included_bytes: entry.bytes.len(),
            tail_truncated: entry.tail_truncated,
        })
        .collect();
    let manifest = BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        generated_at: snapshot.generated_at,
        build_version: snapshot.build.version.clone(),
        profile: snapshot.build.profile.clone(),
        files: file_manifest,
        excludes: vec![
            "database contents",
            "conversation transcripts",
            "blobs and attachments",
            "credentials and keychain values",
            "arbitrary profile files",
        ],
    };
    let manifest = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not encode diagnostic manifest: {error}"))?;
    let snapshot = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("could not encode diagnostic snapshot: {error}"))?;

    let cursor = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    write_zip_file(
        &mut archive,
        "README.txt",
        BUNDLE_README.as_bytes(),
        options,
    )?;
    write_zip_file(&mut archive, "manifest.json", &manifest, options)?;
    write_zip_file(&mut archive, "snapshot.json", &snapshot, options)?;
    write_zip_file(&mut archive, "metrics.prom", metrics.as_bytes(), options)?;
    for entry in entries {
        write_zip_file(&mut archive, entry.path, &entry.bytes, options)?;
    }
    archive
        .finish()
        .map(Cursor::into_inner)
        .map_err(|error| format!("could not finish diagnostic archive: {error}"))
}

fn write_zip_file(
    archive: &mut zip::ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> Result<(), String> {
    archive
        .start_file(path, options)
        .map_err(|error| format!("could not add {path} to diagnostic archive: {error}"))?;
    archive
        .write_all(bytes)
        .map_err(|error| format!("could not write {path} to diagnostic archive: {error}"))
}

fn read_regular_tail(
    directory: &Dir,
    relative_path: &str,
    limit: u64,
) -> Result<Option<(Vec<u8>, u64, bool)>, String> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match directory.open_with(relative_path, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not open diagnostic log {relative_path}: {error}"
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect diagnostic log {relative_path}: {error}"))?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let source_bytes = metadata.len();
    let start = source_bytes.saturating_sub(limit);
    if start > 0 {
        file.seek(SeekFrom::Start(start))
            .map_err(|error| format!("could not seek diagnostic log {relative_path}: {error}"))?;
    }
    let read_len = source_bytes.saturating_sub(start).min(limit);
    let mut bytes = Vec::with_capacity(usize::try_from(read_len).unwrap_or(usize::MAX));
    file.take(read_len)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read diagnostic log {relative_path}: {error}"))?;
    Ok(Some((bytes, source_bytes, start > 0)))
}

const BUNDLE_README: &str = "Tidebreak diagnostic bundle\n\
\n\
This archive contains a process snapshot, OpenMetrics text, and bounded local log tails.\n\
The exporter does not read the Tidebreak database, conversations, blobs, attachments, or credential stores.\n\
Logs can contain local file paths, opaque record IDs, and provider diagnostics. Review them before sharing the archive.\n\
\n\
snapshot.json is the stable machine-readable snapshot.\n\
metrics.prom uses OpenMetrics text.\n\
logs/tidebreak.events.jsonl contains structured tracing events and span-close records.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_and_openmetrics_keep_bounded_route_labels() {
        let diagnostics = Diagnostics::new();
        diagnostics.observe_http(
            "GET",
            "/chats/{id}",
            StatusCode::OK,
            Duration::from_millis(37),
        );
        diagnostics.observe_operation(
            "foreground_turn_segment",
            "completed",
            Duration::from_millis(1_250),
        );
        diagnostics.observe_model_request(
            "anthropic",
            "claude-sonnet",
            "end_turn",
            Duration::from_millis(800),
            Some(75),
            ModelUsageSnapshot {
                input_tokens: 120,
                uncached_input_tokens: 40,
                output_tokens: 45,
                cache_read_input_tokens: 80,
                cache_creation_input_tokens: 0,
            },
        );
        let snapshot = diagnostics.snapshot(Profile::Desktop);
        assert_eq!(snapshot.http.requests.len(), 1);
        assert_eq!(snapshot.http.requests[0].route, "/chats/{id}");
        assert_eq!(snapshot.http.requests[0].duration.count, 1);
        assert_eq!(snapshot.operations[0].duration.max_ms, 1_250);
        assert_eq!(snapshot.model.requests[0].provider, "anthropic");
        assert_eq!(snapshot.model.requests[0].usage.input_tokens, 120);
        assert_eq!(snapshot.model.requests[0].usage.uncached_input_tokens, 40);
        assert_eq!(snapshot.model.requests[0].usage.output_tokens, 45);

        let metrics = render_openmetrics(&snapshot);
        assert!(metrics.contains("route=\"/chats/{id}\""));
        assert!(metrics.contains("operation=\"foreground_turn_segment\""));
        assert!(metrics.contains("provider=\"anthropic\",model=\"claude-sonnet\""));
        assert!(metrics.contains("type=\"uncached_input\"} 40"));
        assert!(metrics.contains("type=\"cache_read_input\"} 80"));
        assert!(metrics.ends_with("# EOF\n"));
    }

    #[test]
    fn otel_provider_names_use_semantic_convention_values() {
        assert_eq!(otel_provider_name("anthropic"), "anthropic");
        assert_eq!(otel_provider_name("gemini"), "gcp.gemini");
        assert_eq!(otel_provider_name("xai"), "x_ai");
        assert_eq!(otel_provider_name("model_gateway"), "model_gateway");
    }

    struct CompletedProvider;

    #[async_trait]
    impl ModelProvider for CompletedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("anthropic")
        }

        async fn stream(
            &self,
            _request: ChatRequest,
        ) -> AgentResult<BoxStream<'static, ProviderEvent>> {
            Ok(futures::stream::iter([
                ProviderEvent::Usage(tidebreak_core::Usage {
                    input_tokens: 10,
                    output_tokens: 3,
                    cache_read_input_tokens: 7,
                    cache_creation_input_tokens: 0,
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    #[tokio::test]
    async fn model_provider_wrapper_records_stream_usage_and_completion() {
        let diagnostics = Arc::new(Diagnostics::new());
        let provider = DiagnosticModelProvider {
            inner: Arc::new(CompletedProvider),
            diagnostics: diagnostics.clone(),
        };
        let stream = provider
            .stream(ChatRequest {
                provider: Some(ProviderId::new("anthropic")),
                model: "claude-sonnet".to_owned(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(diagnostics.snapshot(Profile::Desktop).model.in_flight, 1);

        let events = stream.collect::<Vec<_>>().await;

        assert_eq!(events.len(), 2);
        let snapshot = diagnostics.snapshot(Profile::Desktop);
        assert_eq!(snapshot.model.in_flight, 0);
        assert_eq!(snapshot.model.requests.len(), 1);
        assert_eq!(snapshot.model.requests[0].outcome, "end_turn");
        assert_eq!(snapshot.model.requests[0].usage.input_tokens, 17);
        assert_eq!(snapshot.model.requests[0].usage.uncached_input_tokens, 10);
        assert_eq!(snapshot.model.requests[0].usage.output_tokens, 3);
    }

    #[test]
    fn bundle_includes_only_allowlisted_logs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join("logs/tidebreak.log"), b"human log").unwrap();
        std::fs::write(
            dir.path().join("logs/tidebreak.events.jsonl"),
            b"{\"event\":\"timing\"}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("tidebreak.db"), b"private database").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"private secret").unwrap();

        let diagnostics = Diagnostics::new();
        let snapshot = diagnostics.snapshot(Profile::Desktop);
        let bytes = build_bundle(dir.path(), &snapshot, &render_openmetrics(&snapshot)).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert!(names.contains(&"logs/tidebreak.log".to_owned()));
        assert!(names.contains(&"logs/tidebreak.events.jsonl".to_owned()));
        assert!(names.contains(&"manifest.json".to_owned()));
        assert!(names.contains(&"metrics.prom".to_owned()));
        assert!(names.contains(&"snapshot.json".to_owned()));
        assert!(!names.iter().any(|name| name.contains("tidebreak.db")));
        assert!(!names.iter().any(|name| name.contains("secret")));
    }

    #[test]
    fn log_tail_reads_only_the_snapshotted_bound() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join("logs/tidebreak.log"), b"0123456789").unwrap();
        let directory = Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap();

        let (bytes, source_bytes, truncated) =
            read_regular_tail(&directory, "logs/tidebreak.log", 4)
                .unwrap()
                .unwrap();

        assert_eq!(bytes, b"6789");
        assert_eq!(source_bytes, 10);
        assert!(truncated);
    }

    #[cfg(unix)]
    #[test]
    fn log_tail_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("logs")).unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"private secret").unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("secret.txt"),
            dir.path().join("logs/tidebreak.log"),
        )
        .unwrap();
        let directory = Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap();

        let result = read_regular_tail(&directory, "logs/tidebreak.log", 1024);

        assert!(result.is_err());
    }
}

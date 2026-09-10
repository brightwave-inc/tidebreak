//! The poll client and the bounded event outbox.
//!
//! One rule separates this client from a naive HTTP loop: it never retries
//! silently past a refusal that retrying cannot fix. The endpoint names its
//! refusals — an unsupported poll schema, an event the stream will not
//! accept — and an agent that swallows those and keeps polling turns a
//! five-second bug into an invisible dead sandbox. Named refusals are fatal
//! and loud; everything else (network faults, server errors, a sidecar still
//! starting) is retryable, and the driver bounds how long retrying may last.

use std::collections::VecDeque;
use std::time::Duration;

use crate::wire::{
    EmbeddedEngineRegistration, PollRejection, SupervisorEvent, SupervisorInstructions,
    SupervisorPoll,
};

/// Serialized ceiling for one poll's event batch.
///
/// The endpoint bounds each event payload at 256 KiB and the sidecar bounds
/// the request body; staying well under both keeps a full batch plus the poll
/// envelope inside every ceiling at once.
pub const MAX_BATCH_BYTES: usize = 192 * 1024;

/// Refusal codes that retrying cannot fix.
///
/// Each one means this agent built a request the endpoint will never accept:
/// a schema it does not speak, an event kind the stream refuses, a payload
/// over the ceiling, or more deliverables than one poll may carry.
const FATAL_REJECTIONS: [&str; 4] = [
    "supervisor_poll_schema_unsupported",
    "sandbox_event_invalid",
    "sandbox_event_too_large",
    "sandbox_event_flood",
];

/// One poll's outcome, when it is not instructions.
#[derive(Debug, thiserror::Error)]
pub enum PollFailure {
    /// A transient fault: retry on the poll cadence.
    #[error("supervisor poll failed: {0}")]
    Retryable(String),
    /// A named refusal that retrying cannot fix: exit loudly.
    #[error("the control endpoint refused this agent ({code}): {description}")]
    Fatal {
        /// Machine-readable refusal code.
        code: String,
        /// The endpoint's own description.
        description: String,
    },
}

/// Events waiting for a poll, drained oldest-first under the byte ceiling.
///
/// Nothing is dropped: what one poll cannot carry rides the next, and a
/// batch whose poll failed retryably goes back to the front in order.
#[derive(Debug, Default)]
pub struct Outbox {
    events: VecDeque<SupervisorEvent>,
}

impl Outbox {
    /// Queues one event.
    pub fn push(&mut self, kind: &str, payload: serde_json::Value) {
        self.events.push_back(SupervisorEvent::new(kind, payload));
    }

    /// Whether anything is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Takes the next batch, keeping its serialized size under the ceiling.
    ///
    /// Always takes at least one event when any is queued: an event whose
    /// payload alone exceeds the ceiling goes out alone, so the endpoint's
    /// refusal is loud rather than the event silently wedging the queue.
    pub fn take_batch(&mut self) -> Vec<SupervisorEvent> {
        let mut batch = Vec::new();
        let mut bytes = 0_usize;
        while let Some(event) = self.events.front() {
            let event_bytes = serde_json::to_vec(event).map_or(0, |body| body.len());
            if !batch.is_empty() && bytes + event_bytes > MAX_BATCH_BYTES {
                break;
            }
            bytes += event_bytes;
            batch.push(self.events.pop_front().expect("front was just observed"));
        }
        batch
    }

    /// Returns a failed batch to the front, preserving order.
    pub fn requeue(&mut self, batch: Vec<SupervisorEvent>) {
        for event in batch.into_iter().rev() {
            self.events.push_front(event);
        }
    }
}

/// The poll client for one control endpoint.
#[derive(Debug)]
pub struct Control {
    client: reqwest::Client,
    poll_url: String,
    embedded_engine: Option<EmbeddedEngineRegistration>,
}

impl Control {
    /// Builds a client for the endpoint at `control_url`.
    #[must_use]
    pub fn new(control_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            client,
            poll_url: format!("{}/supervisor/poll", control_url.trim_end_matches('/')),
            embedded_engine: None,
        }
    }

    /// Require Gateway to confirm this identity on every poll.
    #[must_use]
    pub fn with_embedded_engine(mut self, identity: Option<EmbeddedEngineRegistration>) -> Self {
        self.embedded_engine = identity;
        self
    }

    /// Register before catalog queries, repository bootstrap, or engine launch.
    /// Ordinary custom workloads have no registration handshake.
    pub async fn register(&self) -> Result<(), PollFailure> {
        self.register_with_retry(
            crate::inputs::POLL_INTERVAL,
            crate::drive::MAX_CONSECUTIVE_POLL_FAILURES,
        )
        .await
    }

    async fn register_with_retry(
        &self,
        interval: Duration,
        attempts: u32,
    ) -> Result<(), PollFailure> {
        if self.embedded_engine.is_none() {
            return Ok(());
        }
        let poll = SupervisorPoll::new(true, None);
        for attempt in 0..attempts.max(1) {
            match self.poll(&poll).await {
                Ok(instructions) if instructions.stop => {
                    return Err(PollFailure::Fatal {
                        code: "managed_engine_stopped".into(),
                        description: instructions.stop_reason.unwrap_or_else(|| {
                            "the sandbox stopped before engine registration".into()
                        }),
                    })
                }
                Ok(_) => return Ok(()),
                Err(PollFailure::Retryable(_)) if attempt + 1 < attempts => {
                    tokio::time::sleep(interval).await
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("a registration attempt always returns or retries")
    }

    /// Posts one poll and classifies the reply.
    pub async fn poll(&self, poll: &SupervisorPoll) -> Result<SupervisorInstructions, PollFailure> {
        let mut poll = poll.clone();
        poll.embedded_engine.clone_from(&self.embedded_engine);
        let response = self
            .client
            .post(&self.poll_url)
            .json(&poll)
            .send()
            .await
            .map_err(|error| PollFailure::Retryable(error.to_string()))?;
        let status = response.status();
        if status.is_success() {
            let instructions =
                response
                    .json::<SupervisorInstructions>()
                    .await
                    .map_err(|error| {
                        if self.embedded_engine.is_some() {
                            PollFailure::Fatal {
                                code: "managed_engine_not_confirmed".into(),
                                description: format!("instructions were unreadable: {error}"),
                            }
                        } else {
                            PollFailure::Retryable(format!("instructions were unreadable: {error}"))
                        }
                    })?;
            if let Some(expected) = &self.embedded_engine {
                if instructions.embedded_engine.as_ref() != Some(expected) {
                    return Err(PollFailure::Fatal {
                        code: "managed_engine_not_confirmed".into(),
                        description: "Gateway did not confirm this session's installed engine; update Gateway before retrying".into(),
                    });
                }
            }
            return Ok(instructions);
        }
        let body = response.bytes().await.unwrap_or_default();
        // 413 is the transport's own "too large": the request can never
        // shrink by retrying, so it is as final as the named refusals.
        if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            return Err(PollFailure::Fatal {
                code: "payload_too_large".to_owned(),
                description: "the poll request body exceeds the endpoint's ceiling".to_owned(),
            });
        }
        if let Ok(rejection) = serde_json::from_slice::<PollRejection>(&body) {
            if FATAL_REJECTIONS.contains(&rejection.error.as_str())
                || (self.embedded_engine.is_some() && status.is_client_error())
            {
                return Err(PollFailure::Fatal {
                    code: rejection.error,
                    description: rejection.error_description,
                });
            }
            return Err(PollFailure::Retryable(format!(
                "{status}: {} ({})",
                rejection.error, rejection.error_description
            )));
        }
        Err(PollFailure::Retryable(format!(
            "{status}: {}",
            String::from_utf8_lossy(&body)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> EmbeddedEngineRegistration {
        EmbeddedEngineRegistration {
            engine_session_id: tidebreak_core::SessionId::new(),
            engine: tidebreak_core::HarnessKind::Codex,
            engine_version: "0.147.0".into(),
        }
    }

    #[tokio::test]
    async fn managed_registration_retries_the_exact_identity_then_checks_every_poll() {
        use axum::response::IntoResponse;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let recorded = seen.clone();
        let app = axum::Router::new().route(
            "/supervisor/poll",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let recorded = recorded.clone();
                async move {
                    let mut seen = recorded.lock().unwrap();
                    seen.push(body.clone());
                    if seen.len() == 1 {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    axum::Json(serde_json::json!({"embedded_engine":body["embedded_engine"]}))
                        .into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let expected = identity();
        let control = Control::new(&base).with_embedded_engine(Some(expected.clone()));
        control
            .register_with_retry(Duration::ZERO, 2)
            .await
            .unwrap();
        control
            .poll(&SupervisorPoll::new(false, Some(4)))
            .await
            .unwrap();
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0], requests[1]);
        for request in requests.iter() {
            assert_eq!(
                request["embedded_engine"],
                serde_json::to_value(&expected).unwrap()
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn managed_registration_refuses_missing_or_mismatched_acknowledgments() {
        let expected = identity();
        let exact = serde_json::to_value(&expected).unwrap();
        let mut acknowledgments = vec![serde_json::json!({})];
        for (field, value) in [
            ("engine", "claude_code".to_owned()),
            (
                "engine_session_id",
                tidebreak_core::SessionId::new().to_string(),
            ),
            ("engine_version", "0.148.0".to_owned()),
        ] {
            let mut wrong = exact.clone();
            wrong[field] = serde_json::json!(value);
            acknowledgments.push(serde_json::json!({"embedded_engine":wrong}));
        }
        for acknowledgment in acknowledgments {
            let app = axum::Router::new().route(
                "/supervisor/poll",
                axum::routing::post(move || {
                    let acknowledgment = acknowledgment.clone();
                    async move { axum::Json(acknowledgment) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let control = Control::new(&base).with_embedded_engine(Some(expected.clone()));
            assert!(matches!(
                control.register_with_retry(Duration::ZERO, 1).await,
                Err(PollFailure::Fatal { .. })
            ));
            // An ordinary custom workload does not require an engine acknowledgment.
            assert!(Control::new(&base)
                .poll(&SupervisorPoll::new(true, None))
                .await
                .is_ok());
            server.abort();
        }
        // Standalone registration does not contact a control endpoint.
        Control::new("http://127.0.0.1:1").register().await.unwrap();
    }

    #[test]
    fn batches_stay_under_the_byte_ceiling() {
        let mut outbox = Outbox::default();
        // Each event serializes to roughly 64 KiB; four queued means the
        // first batch carries under the ceiling and the rest wait.
        let payload = serde_json::json!({"body": "x".repeat(64 * 1024)});
        for _ in 0..4 {
            outbox.push("turn_started", payload.clone());
        }
        let batch = outbox.take_batch();
        let bytes: usize = batch
            .iter()
            .map(|event| serde_json::to_vec(event).unwrap().len())
            .sum();
        assert!(bytes <= MAX_BATCH_BYTES);
        assert!(batch.len() < 4);
        assert!(!outbox.is_empty());
    }

    /// An oversized event must go out alone and fail loudly there, not wedge
    /// the queue forever.
    #[test]
    fn an_oversized_event_still_leaves_the_queue() {
        let mut outbox = Outbox::default();
        outbox.push(
            "task_output",
            serde_json::json!({"body": "x".repeat(MAX_BATCH_BYTES + 1)}),
        );
        outbox.push("supervisor_stopped", serde_json::json!({}));
        let batch = outbox.take_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].kind, "task_output");
        assert_eq!(outbox.take_batch().len(), 1);
    }

    #[test]
    fn a_requeued_batch_keeps_its_order() {
        let mut outbox = Outbox::default();
        outbox.push("supervisor_started", serde_json::json!({}));
        outbox.push("turn_started", serde_json::json!({}));
        outbox.push("turn_completed", serde_json::json!({}));
        let batch = outbox.take_batch();
        assert_eq!(batch.len(), 3);
        outbox.requeue(batch);
        let batch = outbox.take_batch();
        let kinds: Vec<&str> = batch.iter().map(|event| event.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["supervisor_started", "turn_started", "turn_completed"]
        );
    }
}

//! `/code/llm/*` routes: the engine-facing inference relay.
//!
//! These routes authenticate with the per-session relay key minted by
//! [`crate::code::harness_llm::HarnessLlmRelay`] — never the per-launch app
//! token. They are registered outside `require_token` (see `crate::app`),
//! and the relay resolves the key from each request's own headers.

use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;

use crate::code::harness_llm::RelayEndpoint;
use crate::state::AppState;

/// Body cap for one relayed inference request. A long session's request
/// carries its whole context, so this is far above the default limit and
/// still bounded: the gateway enforces its own ceiling behind it.
pub(crate) const MAX_HARNESS_LLM_BODY_BYTES: usize = 64 * 1024 * 1024;

pub async fn harness_llm_anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(
        state,
        RelayEndpoint::AnthropicMessages,
        headers,
        query,
        body,
    )
    .await
}

pub async fn harness_llm_openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Body,
) -> Response {
    relay(state, RelayEndpoint::OpenAiResponses, headers, query, body).await
}

pub async fn harness_llm_openai_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    match state.code.as_ref().and_then(|code| code.harness_llm()) {
        Some(relay) => {
            relay
                .forward_models(RelayEndpoint::OpenAiModels, &headers)
                .await
        }
        // The desktop and static-token self-host profiles run engines on the
        // machine's own provider configuration; they mint no relay keys, so
        // nothing legitimate calls this.
        None => RelayEndpoint::OpenAiModels.error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "this machine has no harness inference relay",
        ),
    }
}

async fn relay(
    state: AppState,
    endpoint: RelayEndpoint,
    headers: HeaderMap,
    query: Option<String>,
    body: Body,
) -> Response {
    match state.code.as_ref().and_then(|code| code.harness_llm()) {
        Some(relay) => relay.forward(endpoint, &headers, query, body).await,
        // The desktop and static-token self-host profiles run engines on the
        // machine's own provider configuration; they mint no relay keys, so
        // nothing legitimate calls this.
        None => endpoint.error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "this machine has no harness inference relay",
        ),
    }
}

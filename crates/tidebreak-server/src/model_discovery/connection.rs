//! Test a provider's saved credential and endpoint with one cheap request.
//!
//! The test reads the smallest page of the provider's own model list, the
//! call Find models starts with, and says what the answer means: the key
//! works, the provider refused it, nothing answered, the provider is limiting
//! requests, or something answered that is not a model list. OpenRouter lists
//! its models without a key, so its test asks about the key itself instead.
//!
//! The same guarantees as discovery hold. The key never leaves this process,
//! the request follows no redirects, the whole test gives up after
//! [`CONNECTION_TEST_TIMEOUT`], and the result is Tidebreak's own sentence:
//! it never repeats the provider's answer or the key.

use std::time::Duration;

use serde_json::Value;
use tidebreak_core::{SecretProvider, Store};

use super::{discovery_base, fireworks_control_root, listing_url, ollama_native_root, Fetcher};
use crate::error::ServerError;
use crate::managed_policy::ManagedPolicy;
use crate::providers::{
    self, ProviderCredential, ProviderKind, ProviderTestOutcome, ProviderTestResult,
};

/// The whole budget for one connection test.
pub const CONNECTION_TEST_TIMEOUT: Duration = Duration::from_secs(8);

/// The largest successful answer read. Together and OpenAI list every model
/// in one answer, a few megabytes at most.
const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;

/// The largest refusal read. Only a few providers need the body to tell a
/// rejected key from another bad request, and it is never shown.
const MAX_REFUSAL_BYTES: usize = 16 * 1024;

/// Test `kind` with its saved credential and endpoint, and record the result
/// as its last test.
///
/// Refused, and not recorded, before any request when there is nothing to
/// test: the gateway, a managed profile, ChatGPT sign-in, a missing key or
/// endpoint, or an endpoint the key may not travel to.
pub async fn test_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    policy: &ManagedPolicy,
) -> std::result::Result<ProviderTestResult, ServerError> {
    if !kind.accepts_configured_models() {
        return Err(ServerError::bad_request_kind(
            "provider_test_unsupported",
            "The model gateway is checked when you sign in to it.",
        ));
    }
    if policy.managed {
        return Err(providers::managed_profile_refusal(
            "this profile is managed by a model gateway; provider API keys are locked",
        ));
    }
    let config = providers::read_config(store, kind).await?;
    let api_key = test_key(secrets, kind).await?;
    let base = discovery_base(kind, config.base_url.as_deref())?;
    if !providers::endpoint_is_allowed(kind, &base, api_key.is_some(), config.allow_loopback_http) {
        return Err(ServerError::bad_request_kind(
            "provider_endpoint_insecure",
            format!(
                "{} needs an HTTPS endpoint before Tidebreak sends it a key.",
                subject(kind).0
            ),
        ));
    }
    let fingerprint = providers::test_fingerprint(secrets, kind, &config).await;
    let result = test_at(kind, &base, api_key.as_deref()).await;
    // The test can take seconds. When the key, the endpoint, or the consent
    // changed meanwhile, its verdict describes neither, so it is not kept;
    // readers also check the fingerprint, which covers a change that lands
    // between this check and the write.
    let config = providers::read_config(store, kind).await?;
    if providers::test_fingerprint(secrets, kind, &config).await == fingerprint {
        providers::write_last_test(store, kind, &fingerprint, &result).await?;
    }
    Ok(result)
}

/// The key a test sends: the saved one, or its environment fallback. Ollama
/// and OpenAI-compatible endpoints may go without one.
async fn test_key(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> std::result::Result<Option<String>, ServerError> {
    let name = kind.display_name();
    match providers::read_credential(secrets, kind).await {
        Ok(Some(ProviderCredential::Oauth {})) => {
            return Err(ServerError::conflict_kind(
                "provider_test_unsupported",
                "ChatGPT sign-in is checked when you sign in. Tidebreak asks you to sign in again if OpenAI rejects it.",
            ));
        }
        Err(_) => {
            return Err(ServerError::conflict_kind(
                "provider_credential_unreadable",
                format!("The saved {name} credential could not be read. Save it again."),
            ));
        }
        Ok(_) => {}
    }
    let key = providers::resolve_api_key(secrets, kind).await;
    if key.is_none() && kind.requires_credential() {
        return Err(ServerError::conflict_kind(
            "provider_credential_missing",
            format!("Save an API key for {name} first."),
        ));
    }
    Ok(key)
}

/// Test against an explicit endpoint. Split from [`test_provider`] so tests
/// can point a fixed-endpoint provider at a local stand-in.
pub(crate) async fn test_at(
    kind: ProviderKind,
    base: &str,
    api_key: Option<&str>,
) -> ProviderTestResult {
    let answer = match tidebreak_router::http::bypass_proxy_for_loopback(
        reqwest::Client::builder()
            .timeout(CONNECTION_TEST_TIMEOUT)
            // A redirect could carry the key to another host.
            .redirect(reqwest::redirect::Policy::none()),
        base,
    )
    .build()
    {
        Ok(http) => {
            let fetcher = Fetcher {
                http,
                kind,
                api_key: api_key.map(str::to_owned),
            };
            tokio::time::timeout(CONNECTION_TEST_TIMEOUT, ask(&fetcher, base))
                .await
                .unwrap_or(Answer::TimedOut)
        }
        Err(_) => Answer::Unreachable,
    };
    judge(kind, base, api_key.is_some(), answer)
}

/// What came back from the one request.
#[derive(Debug)]
enum Answer {
    /// A successful status and its body.
    Listed(Vec<u8>),
    /// An unsuccessful status and the start of its body.
    Refused(u16, Vec<u8>),
    /// No connection could be made.
    Unreachable,
    /// No answer within the budget.
    TimedOut,
    /// A successful answer larger than any model list.
    TooLarge,
}

/// The one request a test makes for `kind`: the smallest page of its model
/// list, or, for OpenRouter, the key's own details.
fn probe_url(kind: ProviderKind, base: &str) -> Option<String> {
    match kind {
        ProviderKind::Anthropic => {
            listing_url(&format!("{base}/v1/models"), &[("limit", "1")]).ok()
        }
        ProviderKind::Gemini => {
            listing_url(&format!("{base}/v1beta/models"), &[("pageSize", "1")]).ok()
        }
        ProviderKind::Fireworks => listing_url(
            &format!(
                "{}/v1/accounts/fireworks/models",
                fireworks_control_root(base)
            ),
            &[("filter", "supports_serverless=true"), ("pageSize", "1")],
        )
        .ok(),
        // OpenRouter lists its models to anyone, so only the key endpoint
        // proves the key.
        ProviderKind::Openrouter => Some(format!("{base}/key")),
        ProviderKind::Ollama => Some(format!("{}/api/tags", ollama_native_root(base))),
        ProviderKind::Openai
        | ProviderKind::Xai
        | ProviderKind::Together
        | ProviderKind::OpenaiCompatible => Some(format!("{base}/models")),
        ProviderKind::ModelGateway => None,
    }
}

async fn ask(fetcher: &Fetcher, base: &str) -> Answer {
    let Some(url) = probe_url(fetcher.kind, base) else {
        return Answer::Unreachable;
    };
    let response = match fetcher.authorize(fetcher.http.get(url)).send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => return Answer::TimedOut,
        Err(_) => return Answer::Unreachable,
    };
    let status = response.status();
    if status.is_success() {
        return match read_up_to(response, MAX_ANSWER_BYTES).await {
            Ok(Some(body)) => Answer::Listed(body),
            Ok(None) => Answer::TooLarge,
            Err(error) if error.is_timeout() => Answer::TimedOut,
            Err(_) => Answer::Unreachable,
        };
    }
    // A refusal body helps only to classify it, so a partial one will do.
    let body = match read_up_to(response, MAX_REFUSAL_BYTES).await {
        Ok(Some(body)) => body,
        Ok(None) | Err(_) => Vec::new(),
    };
    Answer::Refused(status.as_u16(), body)
}

/// Read a body of at most `limit` bytes. `None` when it is longer.
async fn read_up_to(
    mut response: reqwest::Response,
    limit: usize,
) -> std::result::Result<Option<Vec<u8>>, reqwest::Error> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > limit {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// How a sentence names `kind`, at its start and inside it. "OpenAI-compatible"
/// is not a name a sentence can open with, so that kind is "the server".
fn subject(kind: ProviderKind) -> (&'static str, &'static str) {
    match kind {
        ProviderKind::OpenaiCompatible => ("The server", "the server"),
        _ => (kind.display_name(), kind.display_name()),
    }
}

/// Turn the answer into the result a person reads.
fn judge(kind: ProviderKind, base: &str, sent_key: bool, answer: Answer) -> ProviderTestResult {
    let (name, inner) = subject(kind);
    let (outcome, status, model_count, message) = match answer {
        Answer::Listed(body) => match listed_models(kind, &body) {
            Some(count) => (
                ProviderTestOutcome::Connected,
                None,
                count,
                connected_message(kind, sent_key, count),
            ),
            None => (
                ProviderTestOutcome::UnexpectedAnswer,
                None,
                None,
                format!("{name} answered, but not with a model list. Check the base URL."),
            ),
        },
        Answer::Refused(status, body) => {
            let (outcome, message) = refusal(kind, sent_key, status, &body);
            (outcome, Some(status), None, message)
        }
        Answer::Unreachable => (
            ProviderTestOutcome::Unreachable,
            None,
            None,
            if kind.has_configurable_transport() {
                format!("Tidebreak could not reach {inner} at {base}. Check that it is running.")
            } else {
                format!("Tidebreak could not reach {inner}. Check your network connection.")
            },
        ),
        Answer::TimedOut => (
            ProviderTestOutcome::Unreachable,
            None,
            None,
            format!(
                "{name} did not answer within {} seconds.",
                CONNECTION_TEST_TIMEOUT.as_secs()
            ),
        ),
        Answer::TooLarge => (
            ProviderTestOutcome::UnexpectedAnswer,
            None,
            None,
            format!("{name} answered with more than a model list. Check the base URL."),
        ),
    };
    ProviderTestResult {
        outcome,
        message,
        status,
        model_count,
        tested_at: chrono::Utc::now(),
    }
}

/// Whether a successful body is the answer this provider documents, and, for
/// a server on this computer, how many models it lists. `None` when the body
/// is something else.
///
/// Hosted providers are asked for one model, or list hundreds nobody picks
/// from here, so their count is left out.
fn listed_models(kind: ProviderKind, body: &[u8]) -> Option<Option<u32>> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let count = |items: &Value| {
        items
            .as_array()
            .map(|items| Some(u32::try_from(items.len()).unwrap_or(u32::MAX)))
    };
    // An account with nothing to list may leave the list out entirely.
    let paged = |key: &str| match value.get(key) {
        Some(items) => items.as_array().map(|_| None),
        None => value.as_object().map(|_| None),
    };
    match kind {
        ProviderKind::Anthropic => value.get("data")?.as_array().map(|_| None),
        ProviderKind::Gemini | ProviderKind::Fireworks => paged("models"),
        ProviderKind::Openrouter => value.get("data")?.as_object().map(|_| None),
        ProviderKind::Together => value.as_array().map(|_| None),
        ProviderKind::Openai | ProviderKind::Xai => value.get("data")?.as_array().map(|_| None),
        ProviderKind::Ollama => count(value.get("models")?),
        ProviderKind::OpenaiCompatible => count(value.get("data")?),
        ProviderKind::ModelGateway => None,
    }
}

fn connected_message(kind: ProviderKind, sent_key: bool, count: Option<u32>) -> String {
    let (name, inner) = subject(kind);
    let reached = if sent_key {
        format!("{name} accepted the saved key.")
    } else {
        format!("Tidebreak reached {inner}.")
    };
    let listed = match (kind, count) {
        (ProviderKind::Ollama, Some(0)) => {
            " No models are pulled yet. Pull one with `ollama pull`, then add it here.".to_owned()
        }
        (ProviderKind::Ollama, Some(1)) => " 1 model is pulled.".to_owned(),
        (ProviderKind::Ollama, Some(count)) => format!(" {count} models are pulled."),
        (_, Some(0)) => " It lists no models yet.".to_owned(),
        (_, Some(1)) => " It lists 1 model.".to_owned(),
        (_, Some(count)) => format!(" It lists {count} models."),
        (_, None) => String::new(),
    };
    format!("{reached}{listed}")
}

/// What an unsuccessful status means, and the sentence that says so.
fn refusal(
    kind: ProviderKind,
    sent_key: bool,
    status: u16,
    body: &[u8],
) -> (ProviderTestOutcome, String) {
    let (name, _) = subject(kind);
    match status {
        401 if !sent_key => (
            ProviderTestOutcome::KeyRejected,
            format!("{name} asked for an API key (HTTP 401). Save one, then test again."),
        ),
        401 => (
            ProviderTestOutcome::KeyRejected,
            format!(
                "{name} rejected the saved API key (HTTP 401). Save a valid key, then test again."
            ),
        ),
        400 if sent_key && names_a_rejected_key(body) => (
            ProviderTestOutcome::KeyRejected,
            format!(
                "{name} rejected the saved API key (HTTP 400). Save a valid key, then test again."
            ),
        ),
        // A bare 403 does not say whether the key, the account, or a policy
        // refused, so the key is not called invalid.
        403 => (
            ProviderTestOutcome::AccessDenied,
            format!(
                "{name} refused access (HTTP 403). The key may lack a permission, or the account may be out of credits or restricted."
            ),
        ),
        429 => (
            ProviderTestOutcome::RateLimited,
            format!("{name} is limiting requests (HTTP 429). Wait a minute, then test again."),
        ),
        300..=399 => (
            ProviderTestOutcome::UnexpectedAnswer,
            format!(
                "{name} redirected the request (HTTP {status}). Tidebreak does not follow redirects with a key. Check the base URL."
            ),
        ),
        404 => (
            ProviderTestOutcome::UnexpectedAnswer,
            format!("{name} has no model list at this address (HTTP 404). Check the base URL."),
        ),
        _ => (
            ProviderTestOutcome::UnexpectedAnswer,
            format!("{name} answered with HTTP {status} instead of a model list."),
        ),
    }
}

/// Whether a 400 body is a provider's way of saying the key is wrong.
///
/// Gemini answers an invalid key with `400 INVALID_ARGUMENT` and the reason
/// `API_KEY_INVALID`, and xAI with a 400 whose message names an incorrect
/// key. Read only to classify; the body is never shown.
fn names_a_rejected_key(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let error = value.get("error");
    let containers = [error, Some(&value)];
    let mut codes: Vec<&str> = error
        .and_then(|error| error.get("details"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|detail| detail.get("reason").and_then(Value::as_str))
        .collect();
    for container in containers.into_iter().flatten() {
        for field in ["code", "type"] {
            if let Some(code) = container.get(field).and_then(Value::as_str) {
                codes.push(code);
            }
        }
    }
    if codes.iter().any(|code| {
        matches!(
            *code,
            "API_KEY_INVALID" | "invalid_api_key" | "authentication_error"
        )
    }) {
        return true;
    }
    let names_a_bad_key = [
        error.and_then(|error| error.get("message")),
        error.filter(|error| error.is_string()),
        value.get("message"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .map(str::to_ascii_lowercase)
    .any(|message| {
        message.contains("incorrect api key")
            || message.contains("invalid api key")
            || message.contains("api key not valid")
    });
    names_a_bad_key
}

#[cfg(test)]
mod tests;

//! Classify an engine's turn failure into the vocabulary chat and code turns
//! share ([`TurnFailureCategory`]).
//!
//! Each engine says why a turn failed in its own way, and each classifier
//! reads what its engine states outright before it reads any prose:
//!
//! - Claude Code: the error type on the API-error assistant line, the
//!   `api_error_status` on the result, and the account state its
//!   `rate_limit_event` lines carry.
//! - Codex: `codexErrorInfo` on the failed turn, with the HTTP status some
//!   of its variants carry, and the reset times on `account/rateLimits`.
//! - opencode: the error's `name`, with `statusCode` on an `APIError`.
//! - Grok: the JSON-RPC error code, and the `http_status` its internal
//!   errors carry.
//!
//! Text is read only where an engine states nothing structured, and every
//! phrase matched here is one an engine was seen printing; the tests hold
//! the captured messages.

use serde_json::Value;
use tidebreak_core::{names_missing_model, HarnessKind, TurnFailure, TurnFailureCategory};

use TurnFailureCategory as C;

/// What an HTTP status from a model API means for an engine's turn.
///
/// `message` is what came back with the status. It matters only for a 404:
/// the model is what is missing only when the words say so, and otherwise
/// the address the engine called is wrong.
pub(crate) fn category_for_status(status: u64, message: &str) -> Option<TurnFailureCategory> {
    Some(match status {
        401 => C::EngineAuth,
        // Credits or billing (402), or the account's access (403).
        402 | 403 => C::ProviderAccess,
        404 if names_missing_model(message) => C::ModelUnavailable,
        404 => C::EndpointNotFound,
        408 => C::Transient,
        413 => C::ContextOverflow,
        400 | 422 => C::RequestRejected,
        429 => C::RateLimited,
        502 | 503 | 504 | 529 => C::Overloaded,
        500..=599 => C::Transient,
        _ => return None,
    })
}

/// Whether an engine's or provider's words say the conversation outgrew the
/// model's context window. The phrases are the ones providers send back
/// through the engines: "prompt is too long" (Anthropic), "maximum context
/// length" (OpenAI-compatible routers), and "maximum prompt length" (xAI).
fn context_overflow_text(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "prompt is too long",
        "maximum context length",
        "maximum prompt length",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// A failure with nothing structured behind it and no phrase recognized.
fn unknown(engine: HarnessKind) -> TurnFailure {
    TurnFailure::new(C::Unknown).with_engine(engine)
}

// --- Claude Code -----------------------------------------------------------

/// What Claude Code's `rate_limit_event` lines last said about the account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeRateLimit {
    /// The account is over a plan limit (`status: "rejected"`).
    pub rejected: bool,
    /// When that limit resets, in Unix seconds, when the engine said.
    pub resets_at: Option<i64>,
}

impl ClaudeRateLimit {
    /// Read one `rate_limit_event` line's `rate_limit_info`.
    pub(crate) fn from_event(value: &Value) -> Option<Self> {
        let info = value.get("rate_limit_info")?;
        Some(Self {
            rejected: info.get("status").and_then(Value::as_str) == Some("rejected"),
            resets_at: info.get("resetsAt").and_then(Value::as_i64),
        })
    }
}

/// Classify a failed Claude Code turn.
///
/// `api_error` is the error type on the turn's API-error assistant line (the
/// 2.1.259 SDK names `authentication_failed`, `oauth_org_not_allowed`,
/// `account_on_hold`, `billing_error`, `rate_limit`, `overloaded`,
/// `invalid_request`, `model_not_found`, `server_error`, `unknown`, and
/// `max_output_tokens`). `status` is the result's `api_error_status`.
pub(crate) fn claude_failure(
    api_error: Option<&str>,
    status: Option<u64>,
    rate_limit: ClaudeRateLimit,
    message: &str,
) -> TurnFailure {
    let engine = HarnessKind::ClaudeCode;
    let over_plan_limit = || {
        TurnFailure::new(C::UsageLimit)
            .with_engine(engine)
            .with_reset_timestamp(rate_limit.resets_at)
    };
    let by_status = status.and_then(|status| category_for_status(status, message));
    let category = match api_error {
        Some("authentication_failed" | "oauth_org_not_allowed") => C::EngineAuth,
        Some("account_on_hold") => C::ProviderAccess,
        // Claude Code files an exhausted plan or credit balance here and
        // tells its own reader "usage limit reached, check plan".
        Some("billing_error") => C::UsageLimit,
        // A subscription over its window is a 429 like any throttle; the
        // rate-limit line is what tells the two apart.
        Some("rate_limit") if rate_limit.rejected => return over_plan_limit(),
        Some("rate_limit") => C::RateLimited,
        Some("overloaded") => C::Overloaded,
        Some("model_not_found") => C::ModelUnavailable,
        Some("invalid_request") => match by_status {
            Some(category @ (C::EngineAuth | C::ProviderAccess)) => category,
            _ if context_overflow_text(message) => C::ContextOverflow,
            _ => C::RequestRejected,
        },
        // The engine files network loss and upstream 5xx alike here; the
        // status separates an overloaded API from a dropped connection.
        Some("server_error") => match by_status {
            Some(C::Overloaded) => C::Overloaded,
            _ => C::Transient,
        },
        _ => {
            if rate_limit.rejected && status == Some(429) {
                return over_plan_limit();
            }
            // Claude Code refuses a model its build predates with a 400
            // whose type is `unknown`; choosing another model or updating
            // the engine is the remedy.
            if message.contains("does not support this model") {
                C::ModelUnavailable
            } else {
                by_status.unwrap_or(C::Unknown)
            }
        }
    };
    TurnFailure::new(category).with_engine(engine)
}

// --- Codex ------------------------------------------------------------------

/// The reset times Codex's `account/rateLimits` notifications last reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexRateLimits {
    /// The latest reset among the windows that were used up, in Unix
    /// seconds. A limit lifts only when every exhausted window resets.
    exhausted_until: Option<i64>,
}

impl CodexRateLimits {
    /// Fold one `account/rateLimits/updated` notification's `rateLimits`.
    ///
    /// Updates are sparse: a window the update leaves out keeps what the
    /// last one said.
    pub(crate) fn observe(&mut self, limits: &Value) {
        let windows = ["primary", "secondary"]
            .into_iter()
            .filter_map(|key| limits.get(key).filter(|window| window.is_object()))
            .collect::<Vec<_>>();
        if windows.is_empty() {
            return;
        }
        self.exhausted_until = windows
            .into_iter()
            .filter(|window| {
                window
                    .get("usedPercent")
                    .and_then(Value::as_f64)
                    .is_some_and(|used| used >= 100.0)
            })
            .filter_map(|window| window.get("resetsAt").and_then(Value::as_i64))
            .max();
    }
}

/// `codexErrorInfo`'s variant name, with camelCase (the app server) and
/// snake_case (the session log) spelled alike, and the HTTP status the
/// variant carries when it has one.
fn codex_error_info(info: &Value) -> Option<(String, Option<u64>)> {
    let normalize = |name: &str| name.replace('_', "").to_ascii_lowercase();
    match info {
        Value::String(name) => Some((normalize(name), None)),
        Value::Object(map) => {
            let (name, fields) = map.iter().next()?;
            let status = ["httpStatusCode", "http_status_code"]
                .iter()
                .find_map(|key| fields.get(*key).and_then(Value::as_u64));
            Some((normalize(name), status))
        }
        _ => None,
    }
}

/// The status in Codex's own `unexpected status 503 Service Unavailable: …`
/// wording, which it prints for an HTTP failure it files as `other`.
fn codex_unexpected_status(message: &str) -> Option<u64> {
    let rest = message.trim_start().strip_prefix("unexpected status ")?;
    let digits = rest.get(..3)?;
    digits
        .chars()
        .all(|character| character.is_ascii_digit())
        .then(|| digits.parse().ok())
        .flatten()
}

/// Classify a failed Codex turn from its `turn.error`: the message and the
/// `codexErrorInfo` beside it.
pub(crate) fn codex_failure(
    info: Option<&Value>,
    message: &str,
    limits: CodexRateLimits,
) -> TurnFailure {
    let engine = HarnessKind::Codex;
    let with_category = |category| TurnFailure::new(category).with_engine(engine);
    if let Some((name, status)) = info.and_then(codex_error_info) {
        let by_status = status.and_then(|status| category_for_status(status, message));
        let category = match name.as_str() {
            "contextwindowexceeded" => Some(C::ContextOverflow),
            "usagelimitexceeded" | "sessionbudgetexceeded" => {
                return with_category(C::UsageLimit).with_reset_timestamp(limits.exhausted_until);
            }
            "ratelimitexceeded" => Some(C::RateLimited),
            "serveroverloaded" => Some(C::Overloaded),
            "unauthorized" => Some(C::EngineAuth),
            "badrequest" | "cyberpolicy" | "misalignmentpolicyviolation" => {
                Some(C::RequestRejected)
            }
            "internalservererror" => Some(C::Transient),
            "httpconnectionfailed"
            | "responsestreamconnectionfailed"
            | "responsestreamdisconnected" => Some(by_status.unwrap_or(C::Transient)),
            "responsetoomanyfailedattempts" => Some(by_status.unwrap_or(C::Transient)),
            // `other` and anything newer: the message is all there is.
            _ => None,
        };
        if let Some(category) = category {
            return with_category(category);
        }
    }
    with_category(codex_text_category(message))
}

/// What Codex's message says when its error info says nothing.
fn codex_text_category(message: &str) -> TurnFailureCategory {
    if let Some(category) =
        codex_unexpected_status(message).and_then(|status| category_for_status(status, message))
    {
        // A 413 or a 400 that names the context window is an overflow.
        return match category {
            C::RequestRejected if context_overflow_text(message) => C::ContextOverflow,
            category => category,
        };
    }
    if message.starts_with("stream disconnected before completion") {
        return C::Transient;
    }
    // The engine's own credential helper failed: its sign-in is the problem.
    if message.starts_with("provider auth command `") {
        return C::EngineAuth;
    }
    if message.starts_with("Invalid prompt: your prompt was flagged") {
        return C::RequestRejected;
    }
    // A provider's JSON error body passed through verbatim: an invalid
    // argument or request, unless it names the context window.
    if let Ok(body) = serde_json::from_str::<Value>(message) {
        let code = body
            .get("code")
            .or_else(|| body.pointer("/error/code"))
            .or_else(|| body.pointer("/error/type"))
            .and_then(Value::as_str);
        if matches!(code, Some("invalid-argument" | "invalid_request_error")) {
            return if context_overflow_text(message) {
                C::ContextOverflow
            } else {
                C::RequestRejected
            };
        }
    }
    C::Unknown
}

// --- opencode ---------------------------------------------------------------

/// Classify an opencode error from its `name` and `data` (1.18.27 names
/// `ProviderAuthError`, `APIError`, `ContextOverflowError`,
/// `ContentFilterError`, `MessageOutputLengthError`, `StructuredOutputError`,
/// and `UnknownError`).
pub(crate) fn opencode_failure(name: &str, data: &Value, message: &str) -> TurnFailure {
    let category = match name {
        "ProviderAuthError" => C::EngineAuth,
        "ContextOverflowError" => C::ContextOverflow,
        "ContentFilterError" => C::RequestRejected,
        "APIError" => data
            .get("statusCode")
            .and_then(Value::as_u64)
            .and_then(|status| category_for_status(status, message))
            .unwrap_or_else(|| {
                if data.get("isRetryable").and_then(Value::as_bool) == Some(true) {
                    C::Transient
                } else {
                    C::Unknown
                }
            }),
        // opencode files a model it cannot resolve as an unknown error whose
        // message names it ("Model not found: ...").
        "UnknownError" if names_missing_model(message) => C::ModelUnavailable,
        _ => C::Unknown,
    };
    TurnFailure::new(category).with_engine(HarnessKind::Opencode)
}

// --- Grok -------------------------------------------------------------------

/// Classify a JSON-RPC error Grok answered a request with: its `code`, and
/// the `http_status` its internal errors carry in `data`.
pub(crate) fn grok_rpc_failure(error: &Value) -> TurnFailure {
    let engine = HarnessKind::Grok;
    let message = error
        .pointer("/data/message")
        .or_else(|| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let status = error
        .pointer("/data/http_status")
        .and_then(Value::as_u64)
        .and_then(|status| category_for_status(status, message));
    let category = match (error.get("code").and_then(Value::as_i64), status) {
        (_, Some(category)) => category,
        // ACP's "authentication required".
        (Some(-32000), None) => C::EngineAuth,
        // JSON-RPC invalid params: the request itself.
        (Some(-32602), None) => C::RequestRejected,
        _ => grok_text_category(message),
    };
    TurnFailure::new(category).with_engine(engine)
}

/// Classify the message of a Grok `error` stream line.
pub(crate) fn grok_error_failure(message: &str) -> TurnFailure {
    TurnFailure::new(grok_text_category(message)).with_engine(HarnessKind::Grok)
}

/// What a Grok error message says.
///
/// Grok prints an internal error as `Internal error: ` and the error's JSON
/// data, which names the HTTP status; that status is read as a structured
/// field. The phrases below it are the ones Grok prints for a model it does
/// not serve and an effort level it does not take.
fn grok_text_category(message: &str) -> TurnFailureCategory {
    if let Some(data) = message
        .strip_prefix("Internal error: ")
        .and_then(|data| serde_json::from_str::<Value>(data).ok())
    {
        let said = data
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(category) = data
            .get("http_status")
            .and_then(Value::as_u64)
            .and_then(|status| category_for_status(status, said))
        {
            return category;
        }
    }
    if names_missing_model(message) {
        return C::ModelUnavailable;
    }
    if message.contains("unknown effort level") {
        return C::RequestRejected;
    }
    C::Unknown
}

// --- The engine process -----------------------------------------------------

/// Classify an engine process that ended its turn without a result.
///
/// `signal` is the Unix signal that ended it, when one did. Being killed or
/// told to stop from outside — most often the system reclaiming memory — is
/// worth a retry; a crash signal or an error exit is not known to be.
pub(crate) fn exit_failure(engine: HarnessKind, signal: Option<i32>) -> TurnFailure {
    const SIGKILL: i32 = 9;
    const SIGTERM: i32 = 15;
    match signal {
        Some(SIGKILL | SIGTERM) => TurnFailure::new(C::Transient).with_engine(engine),
        _ => unknown(engine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn category(failure: &TurnFailure) -> TurnFailureCategory {
        failure.category
    }

    /// Each case is a failure Claude Code 2.1.2xx reported, with the error
    /// type and status it carried, copied from the engine's own session logs.
    /// Only the inference gateway's host name is replaced.
    #[test]
    fn claude_failures_classify_from_the_engine_error_type_and_status() {
        let cases: [(Option<&str>, Option<u64>, &str, TurnFailureCategory); 9] = [
            (
                Some("server_error"),
                Some(529),
                "API Error: Repeated 529 Overloaded errors. The API is at capacity — this is usually temporary. Try again in a moment. If it persists, check your inference gateway (gateway.example.com).",
                C::Overloaded,
            ),
            (
                Some("server_error"),
                Some(504),
                "API Error: 504 Gateway Time-out. This is a server-side issue, usually temporary — try again in a moment. If it persists, check your inference gateway (gateway.example.com).",
                C::Overloaded,
            ),
            (
                Some("server_error"),
                None,
                "API Error: Server error mid-response. The response above may be incomplete.",
                C::Transient,
            ),
            (
                Some("server_error"),
                None,
                "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)",
                C::Transient,
            ),
            (
                Some("authentication_failed"),
                Some(401),
                "Please run /login · API Error: 401 A live Model Gateway credential is required.",
                C::EngineAuth,
            ),
            (
                Some("invalid_request"),
                Some(401),
                "Your apiKeyHelper script is failing · This usually means you need to re-authenticate with your provider · Run /status to see the script's error output",
                C::EngineAuth,
            ),
            (
                Some("model_not_found"),
                Some(404),
                "There's an issue with the selected model (claude-opus-5-5[1m]). It may not exist or you may not have access to it. Run /model to pick a different model.",
                C::ModelUnavailable,
            ),
            (
                Some("invalid_request"),
                None,
                "API Error: Fable 5.1's safeguards flagged this message (https://www.anthropic.com/legal/aup). This sometimes happens with safe, normal conversations.",
                C::RequestRejected,
            ),
            // The one phrase read as text: the type is `unknown`, and only
            // the words say the engine build predates the model.
            (
                Some("unknown"),
                Some(400),
                "API Error: 400 Claude Code 2.1.234 does not support this model; version 2.1.251 or newer is required. Run 'claude update', or update the Claude desktop app, then try again.",
                C::ModelUnavailable,
            ),
        ];
        for (api_error, status, message, expected) in cases {
            let failure = claude_failure(api_error, status, ClaudeRateLimit::default(), message);
            assert_eq!(category(&failure), expected, "{message}");
            assert_eq!(failure.engine, Some(HarnessKind::ClaudeCode));
        }
    }

    /// A subscription over its plan window and an API throttle are both
    /// `rate_limit` 429s. The `rate_limit_event` line (2.1.259 schema) is
    /// what separates them, and it carries the reset time.
    #[test]
    fn a_claude_plan_limit_is_a_usage_limit_with_its_reset_time() {
        let event = json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "rejected",
                "resetsAt": 1_787_238_354,
                "rateLimitType": "five_hour",
            },
            "uuid": "00000000-0000-0000-0000-000000000001",
            "session_id": "s",
        });
        let limit = ClaudeRateLimit::from_event(&event).expect("a rate-limit line reads");
        let failure = claude_failure(
            Some("rate_limit"),
            Some(429),
            limit,
            "You've hit your session limit · resets 3pm",
        );
        assert_eq!(failure.category, C::UsageLimit);
        assert_eq!(
            failure.resets_at.map(|at| at.timestamp()),
            Some(1_787_238_354)
        );

        let allowed = ClaudeRateLimit::from_event(&json!({
            "rate_limit_info": {"status": "allowed_warning", "resetsAt": 1_787_238_354},
        }))
        .unwrap();
        let throttled = claude_failure(Some("rate_limit"), Some(429), allowed, "rate limited");
        assert_eq!(throttled.category, C::RateLimited);
        assert_eq!(throttled.resets_at, None);
    }

    /// Codex failures as Codex 0.15x recorded them: the message beside the
    /// `codex_error_info` it carried. The app server spells the info in
    /// camelCase and the session log in snake_case; both classify alike.
    /// Only the gateway host, request ids, and a home directory are
    /// replaced.
    #[test]
    fn codex_failures_classify_from_their_error_info() {
        let limits = CodexRateLimits::default();
        let cases = [
            (
                json!("serverOverloaded"),
                "Selected model is at capacity. Please try a different model.",
                C::Overloaded,
            ),
            (
                json!("server_overloaded"),
                "Selected model is at capacity. Please try a different model.",
                C::Overloaded,
            ),
            (
                json!({"responseTooManyFailedAttempts": {"httpStatusCode": 429}}),
                "exceeded retry limit, last status: 429 Too Many Requests, request id: 00000000-0000-0000-0000-000000000000",
                C::RateLimited,
            ),
            (
                json!("contextWindowExceeded"),
                "Codex ran out of room in the model's context window. Start a new thread or clear earlier history before retrying.",
                C::ContextOverflow,
            ),
            (
                json!("cyber_policy"),
                "This content was flagged for possible cybersecurity risk. If this seems wrong, try rephrasing your request.",
                C::RequestRejected,
            ),
            (json!("unauthorized"), "Unauthorized", C::EngineAuth),
        ];
        for (info, message, expected) in cases {
            let failure = codex_failure(Some(&info), message, limits);
            assert_eq!(failure.category, expected, "{info}: {message}");
            assert_eq!(failure.engine, Some(HarnessKind::Codex));
        }
    }

    /// When Codex files a failure as `other`, the message is all there is.
    /// Each message here is one Codex 0.15x recorded under `other`.
    #[test]
    fn codex_other_failures_classify_from_the_words_codex_prints() {
        let other = json!("other");
        let cases = [
            (
                "unexpected status 503 Service Unavailable: <html>\n<head><title>503 Service Temporarily Unavailable</title></head>\n</html>, url: https://gateway.example.com/compat/openai/v1/responses",
                C::Overloaded,
            ),
            (
                "unexpected status 404 Not Found: The requested model does not exist or is not granted to you on this gateway., url: https://gateway.example.com/compat/openai/v1/responses, request id: 00000000-0000-0000-0000-000000000000",
                C::ModelUnavailable,
            ),
            (
                "unexpected status 401 Unauthorized: A live Model Gateway credential is required., url: https://gateway.example.com/compat/openai/v1/responses, request id: 00000000-0000-0000-0000-000000000000",
                C::EngineAuth,
            ),
            (
                "unexpected status 403 Forbidden: {\"code\":\"personal-team-blocked:spending-limit\",\"error\":\"You have run out of credits or need a Grok subscription.\"}, url: https://gateway.example.com/compat/openai/v1/responses",
                C::ProviderAccess,
            ),
            (
                "unexpected status 422 Unprocessable Entity: {\"error\":\"Failed to deserialize the JSON body into the target type: data did not match any variant of untagged enum ModelInput\"}, url: https://gateway.example.com/compat/openai/v1/responses",
                C::RequestRejected,
            ),
            (
                "stream disconnected before completion: error sending request for url (https://gateway.example.com/compat/openai/v1/responses)",
                C::Transient,
            ),
            (
                "provider auth command `/Users/example/.local/bin/modelctl` exited with status exit status: 1: Error: gateway token request failed (400 Bad Request): Refresh-token reuse was detected and the workstation session was revoked.",
                C::EngineAuth,
            ),
            (
                "{\"code\":\"invalid-argument\",\"error\":\"Invalid reasoning effort.\"}",
                C::RequestRejected,
            ),
            (
                "{\"code\":\"invalid-argument\",\"error\":\"This model's maximum prompt length is 500000 but the request contains 545763 tokens.\"}",
                C::ContextOverflow,
            ),
            (
                "Invalid prompt: your prompt was flagged as potentially violating our usage policy. Please try again with a different prompt: https://platform.openai.com/docs/guides/reasoning#advice-on-prompting",
                C::RequestRejected,
            ),
            ("timed out waiting for rpc id 5", C::Unknown),
        ];
        for (message, expected) in cases {
            assert_eq!(
                codex_failure(Some(&other), message, CodexRateLimits::default()).category,
                expected,
                "{message}"
            );
            // A turn with no error info at all reads the same way.
            assert_eq!(
                codex_failure(None, message, CodexRateLimits::default()).category,
                expected,
                "{message}"
            );
        }
    }

    /// A usage limit takes its reset time from the rate-limit snapshot. The
    /// snapshot is the one Codex 0.153.4 sent (see
    /// `fixtures/codex/0.153.4/compaction.ndjson`), with its weekly window
    /// moved from 94 to 100 percent used.
    #[test]
    fn a_codex_usage_limit_carries_the_exhausted_window_reset() {
        let mut limits = CodexRateLimits::default();
        limits.observe(&json!({
            "limitId": "codex",
            "limitName": null,
            "primary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 1_787_238_354},
            "secondary": null,
            "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
            "planType": "pro",
            "rateLimitReachedType": null,
        }));
        let failure = codex_failure(
            Some(&json!("usageLimitExceeded")),
            "You've hit your usage limit.",
            limits,
        );
        assert_eq!(failure.category, C::UsageLimit);
        assert_eq!(
            failure.resets_at.map(|at| at.timestamp()),
            Some(1_787_238_354)
        );
        // A window with room left says nothing about when a limit lifts.
        let mut unspent = CodexRateLimits::default();
        unspent.observe(&json!({
            "primary": {"usedPercent": 94, "windowDurationMins": 10080, "resetsAt": 1_787_238_354},
            "secondary": null,
        }));
        assert_eq!(
            codex_failure(Some(&json!("usageLimitExceeded")), "limit", unspent).resets_at,
            None
        );
    }

    #[test]
    fn opencode_failures_classify_from_the_error_name() {
        // Captured on opencode 1.18.18 (`fixtures/opencode/1.18.18/error.ndjson`).
        let missing = opencode_failure(
            "UnknownError",
            &json!({"message": "Model not found: opencode/this-model-does-not-exist."}),
            "Model not found: opencode/this-model-does-not-exist.",
        );
        assert_eq!(missing.category, C::ModelUnavailable);
        assert_eq!(missing.engine, Some(HarnessKind::Opencode));
        // The 1.18.27 error names, with the fields they declare.
        for (name, data, expected) in [
            (
                "ProviderAuthError",
                json!({"providerID": "anthropic", "message": "invalid key"}),
                C::EngineAuth,
            ),
            (
                "APIError",
                json!({"message": "Too Many Requests", "statusCode": 429, "isRetryable": true}),
                C::RateLimited,
            ),
            (
                "APIError",
                json!({"message": "socket hang up", "isRetryable": true}),
                C::Transient,
            ),
            (
                "ContextOverflowError",
                json!({"message": "too long"}),
                C::ContextOverflow,
            ),
            (
                "ContentFilterError",
                json!({"message": "filtered"}),
                C::RequestRejected,
            ),
            ("UnknownError", json!({"message": "boom"}), C::Unknown),
        ] {
            let message = data["message"].as_str().unwrap_or_default().to_owned();
            assert_eq!(
                opencode_failure(name, &data, &message).category,
                expected,
                "{name}"
            );
        }
    }

    /// Grok errors as Grok printed them: JSON-RPC errors from its ACP
    /// server, and the stream `error` lines of its print mode.
    #[test]
    fn grok_failures_classify_from_the_rpc_code_and_http_status() {
        for (error, expected) in [
            (
                json!({"code": -32000, "message": "Authentication required."}),
                C::EngineAuth,
            ),
            (
                json!({"code": -32602, "message": "invalid turn options"}),
                C::RequestRejected,
            ),
            (
                json!({"code": -32603, "message": "Internal error", "data": {
                    "message": "API error (status 503 Service Unavailable): Grok is temporarily unavailable. Please try again in a moment. (HTTP 503).",
                    "http_status": 503,
                }}),
                C::Overloaded,
            ),
        ] {
            let failure = grok_rpc_failure(&error);
            assert_eq!(failure.category, expected, "{error}");
            assert_eq!(failure.engine, Some(HarnessKind::Grok));
        }
        for (message, expected) in [
            (
                "Internal error: {\n  \"message\": \"Auth recovery succeeded but 4 authenticated inference requests were still rejected (401); giving up after 3 retries. Turn ran 7s wall-clock.\",\n  \"http_status\": 401\n}",
                C::EngineAuth,
            ),
            (
                "Internal error: {\n  \"message\": \"API error (status 503 Service Unavailable): Grok is temporarily unavailable. Please try again in a moment. (HTTP 503).\",\n  \"http_status\": 503\n}",
                C::Overloaded,
            ),
            (
                "Couldn't set model 'definitely-not-a-real-model-xyz': Invalid params: \"unknown model id\". Run 'grok models' to see available models.",
                C::ModelUnavailable,
            ),
            (
                "--effort/--reasoning-effort: unknown effort level 'xhigh'; use one of: low, high, max",
                C::RequestRejected,
            ),
            ("engine io: No such file or directory (os error 2)", C::Unknown),
        ] {
            assert_eq!(grok_error_failure(message).category, expected, "{message}");
        }
    }

    #[test]
    fn an_engine_killed_from_outside_is_worth_a_retry_and_a_crash_is_not() {
        assert_eq!(
            exit_failure(HarnessKind::ClaudeCode, Some(9)).category,
            C::Transient
        );
        assert_eq!(
            exit_failure(HarnessKind::ClaudeCode, Some(15)).category,
            C::Transient
        );
        assert_eq!(
            exit_failure(HarnessKind::ClaudeCode, Some(11)).category,
            C::Unknown
        );
        assert_eq!(exit_failure(HarnessKind::Grok, None).category, C::Unknown);
    }

    #[test]
    fn statuses_map_like_the_chat_router() {
        for (status, expected) in [
            (401, Some(C::EngineAuth)),
            (402, Some(C::ProviderAccess)),
            (403, Some(C::ProviderAccess)),
            (404, Some(C::EndpointNotFound)),
            (413, Some(C::ContextOverflow)),
            (400, Some(C::RequestRejected)),
            (429, Some(C::RateLimited)),
            (529, Some(C::Overloaded)),
            (500, Some(C::Transient)),
            (302, None),
        ] {
            assert_eq!(
                category_for_status(status, "Not Found"),
                expected,
                "{status}"
            );
        }
        // A 404 is the model only when the words name the model.
        assert_eq!(
            category_for_status(
                404,
                "The requested model does not exist or is not granted to you on this gateway."
            ),
            Some(C::ModelUnavailable)
        );
    }
}

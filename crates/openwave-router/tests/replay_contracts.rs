//! Replay contracts: what each provider adapter actually puts on the wire.
//!
//! Adapters translate one normalized [`ChatRequest`] into several provider
//! request shapes, and the translation is where provider drift bites: a new
//! model generation starts rejecting a parameter an older one accepted, and the
//! first thing anyone sees is a 400 in a user's turn. The unit tests in each
//! adapter assert on individual fields, which is exactly the coverage that goes
//! stale — a field can move, vanish, or gain a sibling without any assertion
//! noticing.
//!
//! So these tests run the *real* adapters against a local server that stands in
//! for the provider, capture the exact outbound request — method, path, headers,
//! body — and compare the whole thing against a committed fixture. Any change to
//! a request shape shows up as a readable JSON diff in review rather than as a
//! silent behavior change.
//!
//! The same round trip also feeds a recorded response back through the adapter's
//! decoder for one scenario per provider, so the fixture pins the normalized
//! [`ProviderEvent`] sequence the stream produces end to end — SSE framing,
//! streamed tool-call argument assembly, usage, and stop reason included.
//!
//! **When one of these fails**, read the diff first: it is the change you made,
//! described in the provider's own vocabulary. If the new shape is what you
//! meant to send, re-record and commit the result:
//!
//! ```text
//! UPDATE_FIXTURES=1 cargo test -p openwave-router --test replay_contracts
//! git diff crates/openwave-router/tests/fixtures
//! ```
//!
//! The fixtures are hand-reviewable on purpose. A reviewer who cannot tell from
//! the diff whether a provider would still accept the request is the signal that
//! the change needs a live check, not a bigger fixture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures::StreamExt;
use openwave_core::model::ReasoningEffort;
use openwave_core::{
    ChatMessage, ChatRequest, ContentBlock, MessageReasoning, ModelProvider, ProviderEvent,
    ProviderId, ReasoningOrigin, ResponseFormat, Role, ToolChoice, ToolSpec,
};
use openwave_router::{
    AnthropicProvider, BedrockAuth, BedrockProvider, GeminiProvider, OpenAiCompatProvider,
    OpenAiProvider,
};
use openwave_router::{BearerTokenSource, VertexModelFamily, VertexProvider};
use serde_json::{json, Value};

/// The credential every adapter is handed. It must never reach a fixture: the
/// capture redacts credential headers by name, and this value is distinctive
/// enough that a leak through some *other* header is obvious in review.
const TEST_API_KEY: &str = "test-provider-key-not-a-secret";

/// Headers that describe the transport rather than the adapter's request. They
/// vary with the local port and the encoded body length, so pinning them would
/// make every fixture nondeterministic without adding coverage.
const TRANSPORT_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "accept",
    "accept-encoding",
    "connection",
    "user-agent",
];

/// Header names carrying a credential. The name stays in the fixture — a
/// credential that moves to a different header is a change worth seeing — and
/// only the value is replaced.
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "x-api-key", "x-goog-api-key"];

const REDACTED: &str = "<redacted>";

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

/// The canonical requests every adapter is measured against.
///
/// One list, shared by all adapters, so a fixture diff reads as "this is
/// how provider X shapes the same turn" and the scenarios cannot drift apart
/// per adapter.
fn scenarios(model: &str) -> Vec<(&'static str, ChatRequest)> {
    vec![
        ("minimal_turn", minimal_turn(model)),
        ("tool_schema", tool_schema(model)),
        ("tool_loop_closure", tool_loop_closure(model)),
        ("structured_output", structured_output(model)),
    ]
}

/// The smallest real turn: a system prompt, one user message, and the sampling
/// controls a non-reasoning model takes.
fn minimal_turn(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        system: Some("You are a careful assistant.".into()),
        messages: vec![ChatMessage::text(Role::User, "What changed in this file?")],
        max_tokens: Some(1024),
        temperature: Some(0.2),
        ..Default::default()
    }
}

/// A tool whose schema uses the constructs providers disagree about — a nested
/// object, an enum, an array of objects, an integer bound — plus a forced tool
/// choice.
fn tool_schema(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        system: Some("Use the tool.".into()),
        messages: vec![ChatMessage::text(
            Role::User,
            "Search for the retry policy.",
        )],
        tools: vec![search_tool()],
        tool_choice: Some(ToolChoice::Tool {
            name: "search_documents".into(),
        }),
        max_tokens: Some(2048),
        ..Default::default()
    }
}

/// Closing the loop on two parallel calls: the assistant's step carried text and
/// two `tool_use` blocks, and the next user message answers both — one of them
/// with a failure. The step is a reasoning one with tools advertised but not
/// forced, which is the shape an agentic turn actually has.
fn tool_loop_closure(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        system: Some("Use the tools.".into()),
        messages: vec![
            ChatMessage::text(Role::User, "Read both configs."),
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Reading both.".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        input: json!({ "path": "a.toml" }),
                    },
                    ContentBlock::ToolUse {
                        id: "call_2".into(),
                        name: "read_file".into(),
                        input: json!({ "path": "b.toml" }),
                    },
                ],
                reasoning: MessageReasoning::default(),
            },
            ChatMessage {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "retries = 3".into(),
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call_2".into(),
                        content: "no such file".into(),
                        is_error: true,
                    },
                ],
                reasoning: MessageReasoning::default(),
            },
        ],
        tools: vec![read_file_tool()],
        reasoning_model: true,
        reasoning_effort: Some(ReasoningEffort::High),
        max_tokens: Some(4096),
        ..Default::default()
    }
}

fn gemini_tool_loop_closure(model: &str, provider: &str) -> ChatRequest {
    let mut request = tool_loop_closure(model);
    request.provider = Some(ProviderId::new(provider));
    request.messages[1].reasoning = MessageReasoning::captured(
        ReasoningOrigin {
            provider: Some(ProviderId::new(provider)),
            model: model.to_string(),
        },
        vec![
            json!({
                "functionCall": {"id": "call_1"},
                "thoughtSignature": "c2lnbmF0dXJlLW9uZQ==",
            }),
            json!({
                "functionCall": {"id": "call_2"},
                "thoughtSignature": "c2lnbmF0dXJlLXR3bw==",
            }),
        ],
    );
    request
}

/// A schema-constrained answer from a reasoning model.
///
/// The constraint is the interesting part: three adapters express it three ways,
/// and on Anthropic it is enforced by forcing a tool, which in turn suppresses
/// the thinking block the same reasoning flags produce in `tool_loop_closure`.
fn structured_output(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage::text(Role::User, "Summarize the incident.")],
        reasoning_model: true,
        reasoning_effort: Some(ReasoningEffort::High),
        response_format: Some(ResponseFormat::JsonSchema {
            name: "incident_summary".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "headline": { "type": "string" },
                    "severity": { "type": "string", "enum": ["low", "high"] },
                    "affected": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["headline", "severity", "affected"],
                "additionalProperties": false
            }),
        }),
        max_tokens: Some(1024),
        ..Default::default()
    }
}

fn search_tool() -> ToolSpec {
    ToolSpec {
        name: "search_documents".into(),
        description: "Search the attached documents.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to look for." },
                "scope": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["project", "chat"] },
                        "limit": { "type": "integer" }
                    },
                    "required": ["kind"],
                    "additionalProperties": false
                },
                "filters": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "field": { "type": "string" } },
                        "required": ["field"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

fn read_file_tool() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read a file from the workspace.".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false
        }),
    }
}

// ---------------------------------------------------------------------------
// Per-adapter contracts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_request_contracts() {
    check_adapter("anthropic", "claude-opus-5", |base_url| {
        Arc::new(AnthropicProvider::new(TEST_API_KEY).with_base_url(base_url))
    })
    .await;
}

#[tokio::test]
async fn openai_responses_request_contracts() {
    check_adapter("openai", "gpt-5.6-sol", |base_url| {
        Arc::new(OpenAiProvider::new(TEST_API_KEY).with_base_url(base_url))
    })
    .await;
}

#[tokio::test]
async fn openai_compat_request_contracts() {
    check_adapter("openai_compat", "gpt-5.6-sol", |base_url| {
        Arc::new(OpenAiCompatProvider::compatible(TEST_API_KEY, base_url))
    })
    .await;
}

#[tokio::test]
async fn compatible_reasoning_replays_on_same_route_and_flattens_on_switch() {
    let model = "accounts/fireworks/models/kimi-k3";
    let build = |id: &'static str| {
        move |base_url: &str| {
            Arc::new(OpenAiCompatProvider::compatible(TEST_API_KEY, base_url).with_id(id))
                as Arc<dyn ModelProvider>
        }
    };
    let response = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan carefully\",\"content\":\"answer\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"
    );
    let (_, events) = round_trip(
        build("fireworks"),
        ChatRequest {
            provider: Some(ProviderId::new("fireworks")),
            model: model.into(),
            messages: vec![ChatMessage::text(Role::User, "first")],
            ..Default::default()
        },
        response.into(),
    )
    .await;
    let replay = events
        .into_iter()
        .find_map(|event| match event {
            ProviderEvent::ReasoningBlock { data } => Some(data),
            _ => None,
        })
        .expect("the compatible stream captured native reasoning_content");
    let reasoning = MessageReasoning::captured(
        ReasoningOrigin {
            provider: Some(ProviderId::new("fireworks")),
            model: model.into(),
        },
        vec![replay],
    );

    let (second_turn, _) = round_trip(
        build("fireworks"),
        ChatRequest {
            provider: Some(ProviderId::new("fireworks")),
            model: model.into(),
            messages: vec![
                ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "answer".into(),
                    }],
                    reasoning: reasoning.clone(),
                },
                ChatMessage::text(Role::User, "second"),
            ],
            ..Default::default()
        },
        terminal_frame("openai_compat").into(),
    )
    .await;
    assert_eq!(
        second_turn["body"]["messages"][0]["reasoning_content"],
        "plan carefully"
    );

    let (tool_continuation, _) = round_trip(
        build("fireworks"),
        ChatRequest {
            provider: Some(ProviderId::new("fireworks")),
            model: model.into(),
            messages: vec![
                ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a.toml"}),
                    }],
                    reasoning: reasoning.clone(),
                },
                ChatMessage {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "call_1".into(),
                        content: "ok".into(),
                        is_error: false,
                    }],
                    reasoning: MessageReasoning::default(),
                },
            ],
            ..Default::default()
        },
        terminal_frame("openai_compat").into(),
    )
    .await;
    assert_eq!(
        tool_continuation["body"]["messages"][0]["reasoning_content"],
        "plan carefully"
    );

    let (foreign, _) = round_trip(
        build("together"),
        ChatRequest {
            provider: Some(ProviderId::new("together")),
            model: model.into(),
            messages: vec![ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "answer".into(),
                }],
                reasoning,
            }],
            ..Default::default()
        },
        terminal_frame("openai_compat").into(),
    )
    .await;
    assert!(foreign["body"]["messages"][0]
        .get("reasoning_content")
        .is_none());
}

#[tokio::test]
async fn gemini_request_contracts() {
    check_adapter("gemini", "gemini-3.6-flash", |base_url| {
        Arc::new(GeminiProvider::new(TEST_API_KEY).with_base_url(base_url))
    })
    .await;
}

#[tokio::test]
async fn bedrock_messages_request_and_stream_contract() {
    check_one_contract(
        "bedrock/messages",
        "tool_loop_closure",
        tool_loop_closure("anthropic.claude-sonnet-5"),
        |base_url| {
            Arc::new(
                BedrockProvider::new("us-east-1", BedrockAuth::ApiKey(TEST_API_KEY.into()))
                    .unwrap()
                    .with_base_url(base_url),
            )
        },
    )
    .await;
}

#[tokio::test]
async fn bedrock_responses_request_and_stream_contract() {
    let mut request = tool_loop_closure("openai.gpt-oss-120b");
    request.reasoning_model = false;
    request.reasoning_effort = None;
    check_one_contract(
        "bedrock/responses",
        "tool_loop_closure",
        request,
        |base_url| {
            Arc::new(
                BedrockProvider::new("us-east-1", BedrockAuth::ApiKey(TEST_API_KEY.into()))
                    .unwrap()
                    .with_base_url(base_url),
            )
        },
    )
    .await;
}

struct StaticVertexToken;

#[async_trait::async_trait]
impl BearerTokenSource for StaticVertexToken {
    async fn bearer_token(&self) -> openwave_core::Result<String> {
        Ok("test-vertex-access-token".into())
    }
}

#[tokio::test]
async fn vertex_gemini_replay_request_contract() {
    check_scenario(
        "vertex_gemini",
        "tool_loop_closure",
        gemini_tool_loop_closure("gemini-3.6-flash", "vertex"),
        |base_url| {
            Arc::new(
                VertexProvider::new(
                    "test-project",
                    "global",
                    Arc::new(StaticVertexToken),
                    [("gemini-3.6-flash".to_string(), VertexModelFamily::Gemini)],
                )
                .unwrap()
                .with_base_url(base_url),
            )
        },
    )
    .await;
}

#[tokio::test]
async fn vertex_anthropic_replay_request_contract() {
    check_scenario(
        "vertex_anthropic",
        "tool_loop_closure",
        tool_loop_closure("claude-opus-5"),
        |base_url| {
            Arc::new(
                VertexProvider::new(
                    "test-project",
                    "global",
                    Arc::new(StaticVertexToken),
                    [("claude-opus-5".to_string(), VertexModelFamily::Anthropic)],
                )
                .unwrap()
                .with_base_url(base_url),
            )
        },
    )
    .await;
}

/// Run every scenario through one adapter and compare both directions against
/// the committed fixtures.
async fn check_adapter(
    provider: &str,
    model: &str,
    build: impl Fn(&str) -> Arc<dyn ModelProvider>,
) {
    for (scenario, request) in scenarios(model) {
        let request = if provider == "gemini" && scenario == "tool_loop_closure" {
            gemini_tool_loop_closure(model, "gemini")
        } else {
            request
        };
        let recording = recorded_response(provider, scenario);
        let response_body = recording
            .clone()
            .unwrap_or_else(|| terminal_frame(provider).to_string());
        let (captured, events) = round_trip(&build, request, response_body).await;

        assert_matches_fixture(&format!("{provider}/{scenario}.request.json"), &captured);
        // A scenario with a recorded response also pins what the decoder makes
        // of it; the rest are served a bare terminal frame so the adapter has
        // something to finish on.
        if recording.is_some() {
            let events = serde_json::to_value(&events).expect("provider events serialize");
            assert_matches_fixture(&format!("{provider}/{scenario}.events.json"), &events);
        }
    }
}

/// Run one high-value replay scenario through a hosted protocol route. The
/// direct adapters already pin every ordinary request shape; this fixture is
/// deliberately narrower and proves the provider-specific endpoint/auth/body
/// delta without duplicating all four contracts.
async fn check_scenario(
    provider: &str,
    scenario: &str,
    request: ChatRequest,
    build: impl Fn(&str) -> Arc<dyn ModelProvider>,
) {
    let response_body = terminal_frame(provider).to_string();
    let (captured, _) = round_trip(build, request, response_body).await;
    assert_matches_fixture(&format!("{provider}/{scenario}.request.json"), &captured);
}

async fn check_one_contract(
    provider: &str,
    scenario: &str,
    request: ChatRequest,
    build: impl Fn(&str) -> Arc<dyn ModelProvider>,
) {
    let response_body = recorded_response(provider, scenario)
        .unwrap_or_else(|| panic!("{provider}/{scenario} has no recorded response fixture"));
    let (captured, events) = round_trip(build, request, response_body).await;
    assert_matches_fixture(&format!("{provider}/{scenario}.request.json"), &captured);
    assert_matches_fixture(
        &format!("{provider}/{scenario}.events.json"),
        &serde_json::to_value(&events).expect("provider events serialize"),
    );
}

// ---------------------------------------------------------------------------
// The intercepting transport
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Endpoint {
    captured: Arc<Mutex<Option<Value>>>,
    response: Arc<String>,
}

/// Serve `response` from a local address, point the adapter at it, and return
/// the request it sent alongside the events it decoded.
async fn round_trip(
    build: impl Fn(&str) -> Arc<dyn ModelProvider>,
    request: ChatRequest,
    response: String,
) -> (Value, Vec<ProviderEvent>) {
    let endpoint = Endpoint {
        captured: Arc::new(Mutex::new(None)),
        response: Arc::new(response),
    };
    let app = axum::Router::new()
        .fallback(any(intercept))
        .with_state(endpoint.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let provider = build(&format!("http://{address}"));
    let mut stream = provider
        .stream(request)
        .await
        .expect("the adapter accepted the scenario and the local endpoint answered");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    server.abort();

    let captured = endpoint
        .captured
        .lock()
        .unwrap()
        .take()
        .expect("the adapter sent exactly one request");
    (captured, events)
}

async fn intercept(State(endpoint): State<Endpoint>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let mut headers = BTreeMap::new();
    for (name, value) in &parts.headers {
        let name = name.as_str().to_ascii_lowercase();
        if TRANSPORT_HEADERS.contains(&name.as_str()) {
            continue;
        }
        let value = if CREDENTIAL_HEADERS.contains(&name.as_str()) {
            REDACTED.to_string()
        } else {
            String::from_utf8_lossy(value.as_bytes()).into_owned()
        };
        headers.insert(name, Value::String(value));
    }

    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).expect("adapters send a JSON body");

    let captured = json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "query": parts.uri.query(),
        "headers": Value::Object(headers.into_iter().collect()),
        "body": body,
    });
    *endpoint.captured.lock().unwrap() = Some(captured);

    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from(endpoint.response.as_str().to_owned()),
    )
        .into_response()
}

/// The minimum each adapter needs to end a stream cleanly, for scenarios whose
/// subject is the request rather than the decode path.
fn terminal_frame(provider: &str) -> &'static str {
    match provider {
        "anthropic" | "vertex_anthropic" => {
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
        }
        "openai" => "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        "openai_compat" => "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "gemini" | "vertex_gemini" => "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
        other => panic!("no terminal frame for {other}"),
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The recorded response body for a scenario, if one is committed.
fn recorded_response(provider: &str, scenario: &str) -> Option<String> {
    std::fs::read_to_string(fixtures_dir().join(format!("{provider}/{scenario}.response.sse"))).ok()
}

fn assert_matches_fixture(relative: &str, actual: &Value) {
    let path = fixtures_dir().join(relative);
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(actual).expect("the capture serializes")
    );

    if std::env::var_os("UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rendered).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("{relative} has no committed fixture.\n{}", RERECORD_HINT);
    });
    assert!(
        rendered == expected,
        "{relative} no longer matches what the adapter sends.\n\n{}\n{}",
        diff(&expected, &rendered),
        RERECORD_HINT
    );
}

const RERECORD_HINT: &str = "\
If the new shape is intended, re-record and review the diff:
  UPDATE_FIXTURES=1 cargo test -p openwave-router --test replay_contracts
  git diff crates/openwave-router/tests/fixtures";

/// A line diff, so a failure reads as the change that caused it rather than as
/// two walls of JSON. Matching leading and trailing lines are trimmed first, so
/// an inserted field shows up as one added line instead of shifting everything
/// after it.
fn diff(expected: &str, actual: &str) -> String {
    use std::fmt::Write as _;

    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let shortest = expected.len().min(actual.len());
    let leading = (0..shortest)
        .take_while(|&index| expected[index] == actual[index])
        .count();
    let trailing = (0..shortest - leading)
        .take_while(|&back| expected[expected.len() - 1 - back] == actual[actual.len() - 1 - back])
        .count();

    let mut out = String::new();
    for (offset, line) in expected[leading..expected.len() - trailing]
        .iter()
        .enumerate()
    {
        let _ = writeln!(out, "{:>5} -committed {line}", leading + offset + 1);
    }
    for (offset, line) in actual[leading..actual.len() - trailing].iter().enumerate() {
        let _ = writeln!(out, "{:>5} +sent      {line}", leading + offset + 1);
    }
    out
}

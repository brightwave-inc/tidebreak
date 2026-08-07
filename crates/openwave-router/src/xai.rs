//! First-class xAI provider on its native Responses API surface.

use async_trait::async_trait;
use futures::stream::BoxStream;

use openwave_core::error::Result;
use openwave_core::provider::{ChatRequest, ModelProvider, ProviderEvent, ProviderId};

use crate::openai::{OpenAiProvider, ResponsesProfile};

/// xAI's public API root. The shared Responses transport appends `/responses`.
pub const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";

/// A [`ModelProvider`] for xAI's first-party Responses API.
#[derive(Clone)]
pub struct XaiProvider {
    inner: OpenAiProvider,
}

impl XaiProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            inner: OpenAiProvider::for_profile(api_key, DEFAULT_BASE_URL, ResponsesProfile::Xai),
        }
    }

    #[cfg(test)]
    fn with_base_url_for_test(mut self, base_url: impl Into<String>) -> Self {
        self.inner = self.inner.with_base_url(base_url);
        self
    }

    #[cfg(test)]
    fn base_url_for_test(&self) -> &str {
        self.inner.base_url_for_test()
    }
}

#[async_trait]
impl ModelProvider for XaiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("xai")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        self.inner.stream(req).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use super::*;
    use axum::body::Body;
    use axum::extract::{Request, State};
    use axum::response::{IntoResponse, Response};
    use axum::routing::any;
    use futures::StreamExt;
    use openwave_core::error::AgentError;
    use openwave_core::provider::{
        ContentBlock, MessageReasoning, ProviderToolReplay, ReasoningOrigin,
    };
    use openwave_core::tool::ToolSpec;
    use openwave_core::{
        ChatMessage, ImageAttachments, ImageData, ImageMediaType, ImageRef, ReasoningEffort, Role,
    };
    use serde_json::{json, Value};

    const TEST_API_KEY: &str = "test-provider-key-not-a-secret";

    #[test]
    fn uses_a_distinct_provider_identity() {
        let provider = XaiProvider::new("key");
        assert_eq!(provider.id(), ProviderId::new("xai"));
        assert_eq!(provider.base_url_for_test(), DEFAULT_BASE_URL);
    }

    #[test]
    fn foreign_provider_tools_flatten_to_cleartext_on_xai() {
        let request = ChatRequest {
            provider: Some(ProviderId::new("xai")),
            model: "grok-test".into(),
            messages: vec![ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ProviderExecutedToolCall {
                    name: "web_search".into(),
                    input: serde_json::json!({"query": "release notes"}),
                    output: serde_json::json!({"results": [{"title": "Notes"}]}),
                    is_error: false,
                    replay: Some(ProviderToolReplay::captured(
                        ReasoningOrigin {
                            provider: Some(ProviderId::new("anthropic")),
                            model: "claude-test".into(),
                        },
                        vec![serde_json::json!({"encrypted_content": "opaque"})],
                    )),
                }],
                reasoning: MessageReasoning::default(),
            }],
            ..Default::default()
        };
        let body = crate::openai::build_request_json_for(&request, ResponsesProfile::Xai)
            .expect("foreign provider output flattens to a normal Responses input");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "assistant");
        let cleartext = input[0]["content"].as_str().unwrap();
        assert!(cleartext.contains("web_search"));
        assert!(!body.to_string().contains("encrypted_content"));
        assert!(!body.to_string().contains("opaque"));
    }

    #[test]
    fn same_route_reasoning_replays_for_a_stateless_continuation() {
        let reasoning = reasoning_item("rs_previous", "opaque-previous");
        let request = ChatRequest {
            provider: Some(ProviderId::new("xai")),
            model: "grok-test".into(),
            reasoning_model: true,
            messages: vec![
                ChatMessage::text(Role::User, "first question"),
                ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "first answer".into(),
                    }],
                    reasoning: MessageReasoning::captured(
                        xai_origin("grok-test"),
                        vec![reasoning.clone()],
                    ),
                },
                ChatMessage::text(Role::User, "follow up"),
            ],
            ..Default::default()
        };
        let body = crate::openai::build_request_json_for(&request, ResponsesProfile::Xai)
            .expect("same-route reasoning is a valid xAI input item");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[1], reasoning);
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    }

    #[test]
    fn provider_or_model_switch_omits_xai_reasoning() {
        let step = |origin: ReasoningOrigin| ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "first answer".into(),
            }],
            reasoning: MessageReasoning::captured(
                origin,
                vec![reasoning_item("rs_previous", "opaque-previous")],
            ),
        };
        for message in [
            step(ReasoningOrigin {
                provider: Some(ProviderId::new("openai")),
                model: "grok-test".into(),
            }),
            step(xai_origin("grok-other")),
        ] {
            let request = ChatRequest {
                provider: Some(ProviderId::new("xai")),
                model: "grok-test".into(),
                reasoning_model: true,
                messages: vec![message],
                ..Default::default()
            };
            let body = crate::openai::build_request_json_for(&request, ResponsesProfile::Xai)
                .expect("foreign reasoning flattens to ordinary assistant text");
            assert!(
                !body.to_string().contains("opaque-previous"),
                "foreign provider-native state must not cross routes: {body}"
            );
        }
    }

    #[test]
    fn xai_image_gate_allows_png_and_jpeg_but_refuses_webp_and_gif() {
        for media_type in [ImageMediaType::Webp, ImageMediaType::Gif] {
            let error = crate::openai::build_request_json_for(
                &image_request(media_type),
                ResponsesProfile::Xai,
            )
            .expect_err("xAI documents only PNG and JPEG image input");
            assert!(error.to_string().contains(media_type.as_str()), "{error}");
        }
        for media_type in [ImageMediaType::Png, ImageMediaType::Jpeg] {
            crate::openai::build_request_json_for(
                &image_request(media_type),
                ResponsesProfile::Xai,
            )
            .expect("xAI documents PNG and JPEG image input");
        }
    }

    #[tokio::test]
    async fn request_and_encrypted_reasoning_fixtures_match_the_xai_contract() {
        let (minimal, _) = round_trip(
            minimal_turn(),
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        assert_eq!(
            minimal,
            fixture(include_str!(
                "../tests/fixtures/xai/minimal_turn.request.json"
            ))
        );

        let (tool_request, events) = round_trip(
            tool_loop_closure(),
            include_str!("../tests/fixtures/xai/tool_loop_closure.response.sse"),
        )
        .await;
        assert_eq!(
            tool_request,
            fixture(include_str!(
                "../tests/fixtures/xai/tool_loop_closure.request.json"
            ))
        );
        assert_eq!(
            serde_json::to_value(events).unwrap(),
            fixture(include_str!(
                "../tests/fixtures/xai/tool_loop_closure.events.json"
            ))
        );
    }

    #[tokio::test]
    async fn classifies_xai_incorrect_key_400_as_authentication() {
        let body: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/xai/authentication_error.response.json"
        ))
        .unwrap();
        let app = axum::Router::new().fallback(move || {
            let body = body.clone();
            async move { (axum::http::StatusCode::BAD_REQUEST, axum::Json(body)) }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = XaiProvider::new("bad").with_base_url_for_test(format!("http://{address}"));
        let result = provider
            .stream(ChatRequest {
                model: "grok-test".into(),
                messages: vec![ChatMessage::text(Role::User, "hello")],
                ..Default::default()
            })
            .await;
        server.abort();
        let error = match result {
            Ok(_) => panic!("the provider must reject the bad key"),
            Err(error) => error,
        };
        assert!(matches!(error, AgentError::Authentication(_)), "{error:?}");
        assert!(error.to_string().contains("xai returned 400"));
    }

    #[tokio::test]
    async fn stream_errors_keep_the_xai_identity() {
        let app = axum::Router::new().fallback(|| async {
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"server_error\"}}}\n\n",
            )
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider = XaiProvider::new("key").with_base_url_for_test(format!("http://{address}"));
        let events = provider
            .stream(ChatRequest {
                model: "grok-test".into(),
                messages: vec![ChatMessage::text(Role::User, "hello")],
                ..Default::default()
            })
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        server.abort();
        assert_eq!(events.len(), 1);
        let ProviderEvent::Failed { error } = &events[0] else {
            panic!("expected failed event, got {:?}", events[0]);
        };
        assert!(error.message.contains("xai returned 500"));
    }

    fn xai_origin(model: &str) -> ReasoningOrigin {
        ReasoningOrigin {
            provider: Some(ProviderId::new("xai")),
            model: model.into(),
        }
    }

    fn reasoning_item(id: &str, encrypted_content: &str) -> Value {
        json!({
            "id": id,
            "summary": [],
            "type": "reasoning",
            "status": "completed",
            "encrypted_content": encrypted_content,
        })
    }

    fn read_file_tool() -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a file from the workspace.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false,
            }),
        }
    }

    fn image_request(media_type: ImageMediaType) -> ChatRequest {
        let blob_id = uuid::Uuid::from_u128(match media_type {
            ImageMediaType::Png => 1,
            ImageMediaType::Jpeg => 2,
            ImageMediaType::Webp => 3,
            ImageMediaType::Gif => 4,
        });
        let image = ImageRef {
            blob_id,
            media_type,
            width: 1,
            height: 1,
            byte_len: 3,
        };
        let mut images = ImageAttachments::new();
        images.insert(blob_id, ImageData::new(media_type, vec![1, 2, 3]));
        ChatRequest {
            provider: Some(ProviderId::new("xai")),
            model: "grok-vision".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::Image { image }],
                reasoning: MessageReasoning::default(),
            }],
            images,
            ..Default::default()
        }
    }

    fn minimal_turn() -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("xai")),
            model: "grok-test".into(),
            system: Some("You are a careful assistant.".into()),
            messages: vec![ChatMessage::text(Role::User, "What changed in this file?")],
            max_tokens: Some(1024),
            temperature: Some(0.2),
            ..Default::default()
        }
    }

    fn tool_loop_closure() -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("xai")),
            model: "grok-test".into(),
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
                    reasoning: MessageReasoning::captured(
                        xai_origin("grok-test"),
                        vec![reasoning_item("rs_previous", "opaque-previous")],
                    ),
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
            reasoning_effort: Some(ReasoningEffort::XHigh),
            max_tokens: Some(4096),
            ..Default::default()
        }
    }

    #[derive(Clone)]
    struct Endpoint {
        captured: Arc<Mutex<Option<Value>>>,
        response: Arc<String>,
    }

    async fn round_trip(request: ChatRequest, response: &str) -> (Value, Vec<ProviderEvent>) {
        let endpoint = Endpoint {
            captured: Arc::new(Mutex::new(None)),
            response: Arc::new(response.to_owned()),
        };
        let app = axum::Router::new()
            .fallback(any(intercept))
            .with_state(endpoint.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider =
            XaiProvider::new(TEST_API_KEY).with_base_url_for_test(format!("http://{address}"));
        let events = provider
            .stream(request)
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
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
            if [
                "host",
                "content-length",
                "accept",
                "accept-encoding",
                "connection",
                "user-agent",
            ]
            .contains(&name.as_str())
            {
                continue;
            }
            let value = if name == "authorization" {
                "<redacted>".to_owned()
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            };
            headers.insert(name, Value::String(value));
        }
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        *endpoint.captured.lock().unwrap() = Some(json!({
            "method": parts.method.as_str(),
            "path": parts.uri.path(),
            "query": parts.uri.query(),
            "headers": Value::Object(headers.into_iter().collect()),
            "body": body,
        }));
        (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            Body::from(endpoint.response.as_str().to_owned()),
        )
            .into_response()
    }

    fn fixture(source: &str) -> Value {
        serde_json::from_str(source).unwrap()
    }
}

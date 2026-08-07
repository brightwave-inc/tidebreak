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

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.inner = self.inner.with_base_url(base_url);
        self
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
    use super::*;
    use futures::StreamExt;
    use openwave_core::error::AgentError;
    use openwave_core::provider::{
        ContentBlock, MessageReasoning, ProviderToolReplay, ReasoningOrigin,
    };
    use openwave_core::{ChatMessage, Role};

    #[test]
    fn uses_a_distinct_provider_identity() {
        assert_eq!(XaiProvider::new("key").id(), ProviderId::new("xai"));
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
        let provider = XaiProvider::new("bad").with_base_url(format!("http://{address}"));
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
        let provider = XaiProvider::new("key").with_base_url(format!("http://{address}"));
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
}

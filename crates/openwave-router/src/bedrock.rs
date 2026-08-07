//! Amazon Bedrock Mantle, normalized through the native protocol adapters.
//!
//! Bedrock Mantle deliberately exposes established inference contracts rather
//! than another Bedrock-specific body:
//!
//! - Anthropic model ids use the Messages API at `/anthropic/v1/messages`.
//! - Other explicitly configured text model ids use the OpenAI Responses API
//!   at `/v1/responses`.
//!
//! Those streams are ordinary SSE, so the mature Anthropic and OpenAI adapters
//! continue to own request shaping, replay, and normalization. This module owns
//! the Bedrock endpoint boundary and its two authentication modes: Bedrock API
//! keys and AWS Signature Version 4 credentials.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use ring::{digest, hmac};

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{ChatRequest, ModelProvider, ProviderEvent, ProviderId};

use crate::http::RequestAuthenticator;
use crate::{AnthropicProvider, AwsCredentials, OpenAiProvider};

const BEDROCK_MANTLE_SERVICE: &str = "bedrock-mantle";

/// Authentication accepted by direct Bedrock Mantle endpoints.
#[derive(Clone)]
pub enum BedrockAuth {
    /// A Bedrock API key. Messages sends it as `x-api-key`; Responses uses the
    /// OpenAI-compatible bearer header.
    ApiKey(String),
    /// AWS credentials signed with Signature Version 4.
    AwsCredentials(AwsCredentials),
}

impl std::fmt::Debug for BedrockAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey(_) => f.debug_tuple("ApiKey").field(&"***").finish(),
            Self::AwsCredentials(credentials) => {
                f.debug_tuple("AwsCredentials").field(credentials).finish()
            }
        }
    }
}

/// A direct Amazon Bedrock Mantle model provider.
#[derive(Clone)]
pub struct BedrockProvider {
    auth: BedrockAuth,
    region: String,
    base_url: String,
}

impl BedrockProvider {
    /// Build a provider for the standard Bedrock Mantle endpoint in `region`.
    pub fn new(region: impl Into<String>, auth: BedrockAuth) -> Result<Self> {
        let region = region.into();
        if !valid_aws_region(&region) {
            return Err(AgentError::config("invalid AWS region for Bedrock"));
        }
        Ok(Self {
            auth,
            base_url: format!("https://bedrock-mantle.{region}.api.aws"),
            region,
        })
    }

    /// Override the endpoint root for a local contract test.
    ///
    /// Production routing never reads a stored endpoint for Bedrock: API keys
    /// and SigV4 credentials may only go to the host derived from the configured
    /// AWS region.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn request_auth(&self, protocol: BedrockProtocol) -> Arc<dyn RequestAuthenticator> {
        Arc::new(BedrockRequestAuth {
            auth: self.auth.clone(),
            region: self.region.clone(),
            protocol,
        })
    }
}

/// A conservative AWS region validator suitable for endpoint construction.
pub fn valid_aws_region(region: &str) -> bool {
    let bytes = region.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

#[async_trait]
impl ModelProvider for BedrockProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("bedrock")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        match protocol_for_model(&req.model) {
            BedrockProtocol::Messages => {
                AnthropicProvider::new("")
                    .with_base_url(format!("{}/anthropic", self.base_url.trim_end_matches('/')))
                    .with_request_auth(self.request_auth(BedrockProtocol::Messages), "bedrock")
                    .stream(req)
                    .await
            }
            BedrockProtocol::Responses => {
                OpenAiProvider::new("")
                    .with_base_url(format!("{}/v1", self.base_url.trim_end_matches('/')))
                    .with_request_auth(self.request_auth(BedrockProtocol::Responses), "bedrock")
                    .stream(req)
                    .await
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BedrockProtocol {
    Messages,
    Responses,
}

fn protocol_for_model(model: &str) -> BedrockProtocol {
    let leaf = model.rsplit('/').next().unwrap_or(model);
    let without_region = ["us.", "eu.", "apac.", "jp.", "au.", "global."]
        .iter()
        .find_map(|prefix| leaf.strip_prefix(prefix))
        .unwrap_or(leaf);
    if without_region.starts_with("anthropic.") {
        BedrockProtocol::Messages
    } else {
        BedrockProtocol::Responses
    }
}

#[derive(Clone)]
struct BedrockRequestAuth {
    auth: BedrockAuth,
    region: String,
    protocol: BedrockProtocol,
}

impl RequestAuthenticator for BedrockRequestAuth {
    fn authenticate(
        &self,
        request: reqwest::RequestBuilder,
        url: &reqwest::Url,
        body: &[u8],
    ) -> Result<reqwest::RequestBuilder> {
        match &self.auth {
            BedrockAuth::ApiKey(key) => Ok(match self.protocol {
                BedrockProtocol::Messages => request.header("x-api-key", key),
                BedrockProtocol::Responses => request.bearer_auth(key),
            }),
            BedrockAuth::AwsCredentials(credentials) => {
                let signed = sign_request(url, body, &self.region, credentials, Utc::now())?;
                let mut request = request
                    .header("x-amz-date", signed.amz_date)
                    .header("x-amz-content-sha256", signed.payload_hash)
                    .header(reqwest::header::AUTHORIZATION, signed.authorization);
                if let Some(token) = credentials.session_token() {
                    request = request.header("x-amz-security-token", token);
                }
                Ok(request)
            }
        }
    }
}

struct SignedRequest {
    amz_date: String,
    payload_hash: String,
    authorization: String,
}

fn sign_request(
    url: &reqwest::Url,
    body: &[u8],
    region: &str,
    credentials: &AwsCredentials,
    now: DateTime<Utc>,
) -> Result<SignedRequest> {
    let host = canonical_host(url)?;
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    let payload_hash = sha256_hex(body);
    let mut headers = vec![
        ("content-type", "application/json".to_owned()),
        ("host", host),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(token) = credentials.session_token() {
        headers.push(("x-amz-security-token", token.to_owned()));
    }
    headers.sort_by_key(|(name, _)| *name);
    let signed_headers = headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", collapse_spaces(value)))
        .collect::<String>();
    let canonical_request = format!(
        "POST\n{}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        url.path()
    );
    let scope = format!("{date}/{region}/{BEDROCK_MANTLE_SERVICE}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{}", credentials.secret_access_key()).as_bytes(),
        date.as_bytes(),
    );
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, BEDROCK_MANTLE_SERVICE.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key_id()
    );
    Ok(SignedRequest {
        amz_date,
        payload_hash,
        authorization,
    })
}

fn canonical_host(url: &reqwest::Url) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::config("Bedrock endpoint has no host"))?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn collapse_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message)
        .as_ref()
        .to_vec()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router as AxumRouter;
    use futures::StreamExt as _;
    use openwave_core::provider::ChatMessage;
    use openwave_core::Role;

    #[test]
    fn protocol_selection_keeps_claude_on_messages_and_text_models_on_responses() {
        assert_eq!(
            protocol_for_model("anthropic.claude-sonnet-5"),
            BedrockProtocol::Messages
        );
        assert_eq!(
            protocol_for_model("us.anthropic.claude-sonnet-5"),
            BedrockProtocol::Messages
        );
        assert_eq!(
            protocol_for_model("openai.gpt-oss-120b"),
            BedrockProtocol::Responses
        );
    }

    #[test]
    fn sigv4_signature_is_deterministic_and_redacts_debug() {
        let credentials = AwsCredentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            Some("session-token".into()),
        );
        let url =
            reqwest::Url::parse("https://bedrock-mantle.us-east-1.api.aws/anthropic/v1/messages")
                .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-08-07T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc);
        let signed =
            sign_request(&url, br#"{"messages":[]}"#, "us-east-1", &credentials, now).unwrap();
        assert_eq!(signed.amz_date, "20260807T123456Z");
        assert_eq!(
            signed.payload_hash,
            "5e4ce7b36ba37b78a5d5f9fd08e6b7b54ba6879d651aa46ec9e1d6fa24ebe30a"
        );
        let (authorization, signature) = signed.authorization.rsplit_once("Signature=").unwrap();
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260807/us-east-1/bedrock-mantle/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token, "
        );
        assert_eq!(
            signature,
            hex(&[
                0x34, 0xca, 0x24, 0xb8, 0x07, 0x2c, 0xd0, 0x9c, 0x22, 0x91, 0xb2, 0x0f, 0x7f, 0x83,
                0xf9, 0x65, 0xd8, 0x49, 0xab, 0x2b, 0x7c, 0xc9, 0xdd, 0x7c, 0xfa, 0x65, 0xe9, 0x5a,
                0xc8, 0x38, 0x20, 0x4b,
            ])
        );
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("AKIDEXAMPLE"));
        assert!(!debug.contains("session-token"));
    }

    #[test]
    fn region_validation_rejects_host_escape_characters() {
        assert!(valid_aws_region("us-east-1"));
        assert!(valid_aws_region("us-gov-west-1"));
        assert!(!valid_aws_region("../us-east-1"));
        assert!(!valid_aws_region("US-EAST-1"));
        assert!(!valid_aws_region("us-east-1.example.com"));
    }

    #[tokio::test]
    async fn messages_transport_failures_are_attributed_to_bedrock() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });

        let provider = BedrockProvider::new("us-east-1", BedrockAuth::ApiKey("secret".into()))
            .unwrap()
            .with_base_url(format!("http://{address}"));
        let error = provider
            .stream(ChatRequest {
                model: "anthropic.claude-sonnet-5".into(),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .err()
            .expect("the dropped connection must fail before a response");
        server.await.unwrap();

        assert!(matches!(
            error,
            AgentError::Provider(message) if message == "bedrock request failed"
        ));
    }

    #[tokio::test]
    async fn messages_context_window_stops_are_attributed_to_bedrock() {
        async fn context_overflow() -> impl IntoResponse {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"model_context_window_exceeded\"}}\n\n",
            )
        }

        let app = AxumRouter::new().fallback(post(context_overflow));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider = BedrockProvider::new("us-east-1", BedrockAuth::ApiKey("secret".into()))
            .unwrap()
            .with_base_url(format!("http://{address}"));
        let events: Vec<ProviderEvent> = provider
            .stream(ChatRequest {
                model: "anthropic.claude-sonnet-5".into(),
                messages: vec![ChatMessage::text(Role::User, "hi")],
                ..Default::default()
            })
            .await
            .unwrap()
            .collect()
            .await;
        server.abort();

        assert!(matches!(
            events.as_slice(),
            [ProviderEvent::Failed { error }]
                if error.kind == "prompt_too_long"
                    && error.message
                        == "bedrock: the model's context window was exceeded mid-response"
        ));
    }
}

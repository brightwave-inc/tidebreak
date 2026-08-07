//! Google service-account OAuth for Vertex AI.
//!
//! A service-account key is exchanged for a short-lived access token by
//! signing an RS256 JWT assertion. The token endpoint and scope are fixed:
//! neither is read from the uploaded key file, so credential material cannot
//! be redirected to an attacker-controlled host.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use futures::lock::Mutex;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde::Deserialize;
use serde_json::json;

use openwave_core::error::{AgentError, Result};

use crate::google::valid_resource_segment;
use crate::BearerTokenSource;

const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const JWT_LIFETIME_SECONDS: u64 = 3_600;
const TOKEN_REFRESH_SKEW_SECONDS: u64 = 60;

/// Parsed Google service-account material with a fully redacted `Debug`.
#[derive(Clone)]
pub struct GoogleServiceAccount {
    project_id: String,
    client_email: String,
    private_key_id: Option<String>,
    private_key_der: Arc<[u8]>,
}

impl std::fmt::Debug for GoogleServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleServiceAccount")
            .field("project_id", &"***")
            .field("client_email", &"***")
            .field(
                "private_key_id",
                &self.private_key_id.as_ref().map(|_| "***"),
            )
            .field("private_key_der", &"***")
            .finish()
    }
}

#[derive(Deserialize)]
struct ServiceAccountKeyFile {
    #[serde(rename = "type")]
    key_type: String,
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default)]
    private_key_id: Option<String>,
}

impl GoogleServiceAccount {
    /// Parse and cryptographically validate a Google service-account key file.
    ///
    /// Errors deliberately describe only the malformed field. They never
    /// include serde or crypto-library diagnostics because those can retain
    /// input fragments from the key.
    pub fn from_json(raw: &str) -> Result<Self> {
        let key: ServiceAccountKeyFile = serde_json::from_str(raw)
            .map_err(|_| AgentError::config("Google service-account credential is malformed"))?;
        if key.key_type != "service_account" {
            return Err(AgentError::config(
                "Google service-account credential has an invalid type",
            ));
        }
        if !valid_resource_segment(&key.project_id) {
            return Err(AgentError::config(
                "Google service-account credential has an invalid project_id",
            ));
        }
        if key.client_email.trim() != key.client_email
            || key.client_email.is_empty()
            || key.client_email.len() > 320
            || !key.client_email.is_ascii()
            || !key.client_email.contains('@')
            || key.client_email.chars().any(char::is_control)
        {
            return Err(AgentError::config(
                "Google service-account credential has an invalid client_email",
            ));
        }
        if key.private_key_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        }) {
            return Err(AgentError::config(
                "Google service-account credential has an invalid private_key_id",
            ));
        }

        let private_key_der = decode_pkcs8_pem(&key.private_key)?;
        RsaKeyPair::from_pkcs8(&private_key_der).map_err(|_| {
            AgentError::config("Google service-account credential has an invalid private_key")
        })?;

        Ok(Self {
            project_id: key.project_id,
            client_email: key.client_email,
            private_key_id: key.private_key_id,
            private_key_der: private_key_der.into(),
        })
    }

    /// Project used in the Vertex AI resource path.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

fn decode_pkcs8_pem(pem: &str) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";

    let trimmed = pem.trim();
    let encoded = trimmed
        .strip_prefix(BEGIN)
        .and_then(|value| value.strip_suffix(END))
        .ok_or_else(|| {
            AgentError::config("Google service-account credential private_key must be PKCS#8 PEM")
        })?;
    let encoded = encoded.lines().map(str::trim).collect::<Vec<_>>().join("");
    STANDARD.decode(encoded).map_err(|_| {
        AgentError::config("Google service-account credential has an invalid private_key")
    })
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: u64,
}

/// A serialized, expiry-aware Google OAuth token source.
pub struct GoogleServiceAccountTokenSource {
    account: GoogleServiceAccount,
    client: reqwest::Client,
    token_uri: String,
    cached: Mutex<Option<CachedToken>>,
    #[cfg(test)]
    test_signature: Option<Vec<u8>>,
}

impl std::fmt::Debug for GoogleServiceAccountTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleServiceAccountTokenSource")
            .field("account", &self.account)
            .field("token_uri", &"***")
            .field("cached", &"***")
            .finish()
    }
}

impl GoogleServiceAccountTokenSource {
    /// Create a token source using Google's fixed OAuth token endpoint.
    pub fn new(account: GoogleServiceAccount) -> Self {
        Self {
            account,
            client: crate::http::request_client(),
            token_uri: GOOGLE_TOKEN_URI.to_string(),
            cached: Mutex::new(None),
            #[cfg(test)]
            test_signature: None,
        }
    }

    #[cfg(test)]
    fn with_token_uri(mut self, token_uri: impl Into<String>) -> Self {
        self.token_uri = token_uri.into();
        self
    }

    #[cfg(test)]
    fn with_test_signature(mut self) -> Self {
        self.test_signature = Some(b"test-signature".to_vec());
        self
    }

    fn assertion_signing_input(&self, issued_at: u64) -> Result<String> {
        let mut header = json!({"alg": "RS256", "typ": "JWT"});
        if let Some(key_id) = &self.account.private_key_id {
            header["kid"] = json!(key_id);
        }
        let claims = json!({
            "iss": self.account.client_email,
            "scope": CLOUD_PLATFORM_SCOPE,
            "aud": GOOGLE_TOKEN_URI,
            "iat": issued_at,
            "exp": issued_at.saturating_add(JWT_LIFETIME_SECONDS),
        });
        let header = serde_json::to_vec(&header)
            .map_err(|_| AgentError::config("failed to build Google OAuth assertion"))?;
        let claims = serde_json::to_vec(&claims)
            .map_err(|_| AgentError::config("failed to build Google OAuth assertion"))?;
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        Ok(signing_input)
    }

    fn assertion(&self, issued_at: u64) -> Result<String> {
        let signing_input = self.assertion_signing_input(issued_at)?;
        #[cfg(test)]
        if let Some(signature) = &self.test_signature {
            return Ok(format!(
                "{signing_input}.{}",
                URL_SAFE_NO_PAD.encode(signature)
            ));
        }

        let key_pair = RsaKeyPair::from_pkcs8(&self.account.private_key_der)
            .map_err(|_| AgentError::config("failed to sign Google OAuth assertion"))?;
        let mut signature = vec![0; key_pair.public().modulus_len()];
        key_pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| AgentError::config("failed to sign Google OAuth assertion"))?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    async fn refresh(&self, now: u64) -> Result<CachedToken> {
        let assertion = self.assertion(now)?;
        let response = self
            .client
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|_| AgentError::Provider("Google OAuth token exchange failed".into()))?;
        let status = response.status();
        if !status.is_success() {
            let message = format!("Google OAuth token exchange returned {}", status.as_u16());
            return Err(if matches!(status.as_u16(), 400 | 401 | 403) {
                AgentError::Authentication(message)
            } else {
                AgentError::Provider(message)
            });
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
            token_type: String,
        }

        let body = crate::sse::read_bounded_error_body(response.bytes_stream()).await;
        let token: TokenResponse = serde_json::from_str(&body).map_err(|_| {
            AgentError::Provider("Google OAuth token exchange returned an invalid response".into())
        })?;
        if token.access_token.is_empty()
            || token.expires_in == 0
            || !token.token_type.eq_ignore_ascii_case("bearer")
        {
            return Err(AgentError::Provider(
                "Google OAuth token exchange returned an invalid response".into(),
            ));
        }
        Ok(CachedToken {
            value: token.access_token,
            expires_at: now.saturating_add(token.expires_in),
        })
    }
}

#[async_trait]
impl BearerTokenSource for GoogleServiceAccountTokenSource {
    async fn bearer_token(&self) -> Result<String> {
        let mut cached = self.cached.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AgentError::config("system clock is before the Unix epoch"))?
            .as_secs();
        if let Some(token) = cached.as_ref() {
            if token.expires_at > now.saturating_add(TOKEN_REFRESH_SKEW_SECONDS) {
                return Ok(token.value.clone());
            }
        }
        let token = self.refresh(now).await?;
        let value = token.value.clone();
        *cached = Some(token);
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn account() -> GoogleServiceAccount {
        GoogleServiceAccount {
            project_id: "test-project".to_string(),
            client_email: "service-account@example.test".to_string(),
            private_key_id: Some("test-key".to_string()),
            private_key_der: Arc::from(Vec::<u8>::new()),
        }
    }

    #[test]
    fn assertion_has_google_oauth_claims_without_secret_values() {
        let source = GoogleServiceAccountTokenSource::new(account()).with_test_signature();
        let assertion = source.assertion(1_700_000_000).unwrap();
        let parts = assertion.split('.').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], "test-key");
        assert_eq!(claims["aud"], GOOGLE_TOKEN_URI);
        assert_eq!(claims["scope"], CLOUD_PLATFORM_SCOPE);
        assert_eq!(claims["iat"], 1_700_000_000_u64);
        assert_eq!(claims["exp"], 1_700_003_600_u64);
        assert!(!format!("{source:?}").contains("service-account@example.test"));
        assert!(!format!("{source:?}").contains("test-project"));
    }

    #[test]
    fn malformed_key_material_and_path_components_fail_closed() {
        let error = GoogleServiceAccount::from_json(
            r#"{"type":"service_account","project_id":"../secret","client_email":"secret@example.test","private_key":"private-secret"}"#,
        )
        .unwrap_err();
        assert!(!error.to_string().contains("secret@example.test"));
        assert!(!error.to_string().contains("private-secret"));
        assert!(valid_resource_segment("us-central1"));
        assert!(valid_resource_segment("global"));
        assert!(!valid_resource_segment("../global"));
        assert!(!valid_resource_segment("US-CENTRAL1"));
    }

    #[tokio::test]
    async fn token_exchange_uses_jwt_grant_and_caches_the_bearer() {
        use axum::extract::{Form, State};
        use axum::routing::post;
        use axum::{Json, Router};

        #[derive(Clone, Default)]
        struct TokenServer {
            calls: Arc<AtomicUsize>,
            assertion: Arc<std::sync::Mutex<Option<String>>>,
        }

        async fn token(
            State(state): State<TokenServer>,
            Form(form): Form<HashMap<String, String>>,
        ) -> Json<serde_json::Value> {
            assert_eq!(
                form.get("grant_type").map(String::as_str),
                Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
            );
            *state.assertion.lock().unwrap() = form.get("assertion").cloned();
            state.calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "access_token": "vertex-access-token",
                "expires_in": 3600,
                "token_type": "Bearer"
            }))
        }

        let state = TokenServer::default();
        let app = Router::new()
            .route("/token", post(token))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let source = GoogleServiceAccountTokenSource::new(account())
            .with_test_signature()
            .with_token_uri(format!("http://{address}/token"));
        assert_eq!(source.bearer_token().await.unwrap(), "vertex-access-token");
        assert_eq!(source.bearer_token().await.unwrap(), "vertex-access-token");
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            state
                .assertion
                .lock()
                .unwrap()
                .as_deref()
                .map(|assertion| assertion.split('.').count()),
            Some(3)
        );
        server.abort();
    }
}

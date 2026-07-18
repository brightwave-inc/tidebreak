//! Authenticated loopback client for the durable client-execution API.

use openwave_core::{CallId, ChatId, ToolCallRecord};
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::receipt_store::StoredResolution;

const MAX_EXACT_ATTEMPTS: usize = 3;
const CLIENT_EXECUTOR_HEADER: &str = "x-openwave-client-executor";

#[derive(Clone)]
pub(crate) struct ControlPlaneClient {
    base_url: String,
    token: String,
    executor_token: reqwest::header::HeaderValue,
    http: reqwest::Client,
}

#[derive(Debug, Error)]
pub(crate) enum ControlPlaneError {
    #[error("local control plane request failed")]
    Transport,
    #[error("local control plane rejected the request ({status}): {message}")]
    Http { status: u16, message: String },
    #[error("local control plane returned an invalid response")]
    Protocol,
}

impl ControlPlaneError {
    pub(super) fn is_conflict(&self) -> bool {
        matches!(self, Self::Http { status, .. } if *status == StatusCode::CONFLICT.as_u16())
    }
}

#[derive(Serialize)]
struct ClaimRequest {
    executor_id: Uuid,
    lease_token: Uuid,
}

#[derive(Deserialize)]
pub(super) struct ClaimedExecution {
    pub(super) call: ToolCallRecord,
    pub(super) lease_token: Uuid,
}

#[derive(Serialize)]
struct HeartbeatRequest {
    lease_token: Uuid,
}

#[derive(Serialize)]
struct ResolveRequest<'a> {
    lease_token: Uuid,
    resolution: &'a StoredResolution,
}

impl ControlPlaneClient {
    pub(crate) fn new(
        base_url: String,
        token: String,
        executor_token: String,
    ) -> Result<Self, ControlPlaneError> {
        let base_url = base_url.trim_end_matches('/').to_owned();
        let url = reqwest::Url::parse(&base_url).map_err(|_| ControlPlaneError::Protocol)?;
        let loopback_host = url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if url.scheme() != "http"
            || !loopback_host
            || url.port().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || token.is_empty()
            || executor_token.is_empty()
        {
            return Err(ControlPlaneError::Protocol);
        }
        let mut executor_token =
            reqwest::header::HeaderValue::from_bytes(executor_token.as_bytes())
                .map_err(|_| ControlPlaneError::Protocol)?;
        executor_token.set_sensitive(true);
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|_| ControlPlaneError::Protocol)?;
        Ok(Self {
            base_url,
            token,
            executor_token,
            http,
        })
    }

    pub(super) async fn claim(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        executor_id: Uuid,
        lease_token: Uuid,
    ) -> Result<ClaimedExecution, ControlPlaneError> {
        self.post(
            &format!("/chats/{chat_id}/client-executions/{call_id}/claim"),
            &ClaimRequest {
                executor_id,
                lease_token,
            },
        )
        .await
    }

    pub(super) async fn pending(
        &self,
        chat_id: ChatId,
    ) -> Result<Vec<ToolCallRecord>, ControlPlaneError> {
        self.get(&format!("/chats/{chat_id}/client-executions/pending/raw"))
            .await
    }

    pub(super) async fn heartbeat(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        lease_token: Uuid,
    ) -> Result<(), ControlPlaneError> {
        let _: serde_json::Value = self
            .post(
                &format!("/chats/{chat_id}/client-executions/{call_id}/heartbeat"),
                &HeartbeatRequest { lease_token },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn resolve(
        &self,
        chat_id: ChatId,
        call_id: CallId,
        lease_token: Uuid,
        resolution: &StoredResolution,
    ) -> Result<(), ControlPlaneError> {
        let _: serde_json::Value = self
            .post(
                &format!("/chats/{chat_id}/client-executions/{call_id}/resolve"),
                &ResolveRequest {
                    lease_token,
                    resolution,
                },
            )
            .await?;
        Ok(())
    }

    async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, ControlPlaneError> {
        let mut last_error = ControlPlaneError::Transport;
        for attempt in 0..MAX_EXACT_ATTEMPTS {
            let response = match self
                .http
                .post(format!("{}{path}", self.base_url))
                .bearer_auth(&self.token)
                .header(CLIENT_EXECUTOR_HEADER, self.executor_token.clone())
                .json(body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => {
                    last_error = ControlPlaneError::Transport;
                    retry_pause(attempt).await;
                    continue;
                }
            };
            if !response.status().is_success() {
                let status = response.status().as_u16();
                if response.status().is_server_error() && attempt + 1 < MAX_EXACT_ATTEMPTS {
                    last_error = ControlPlaneError::Http {
                        status,
                        message: "request failed".to_owned(),
                    };
                    retry_pause(attempt).await;
                    continue;
                }
                let message = response
                    .json::<ErrorBody>()
                    .await
                    .map(|body| body.message)
                    .unwrap_or_else(|_| "request failed".to_owned());
                return Err(ControlPlaneError::Http { status, message });
            }
            match response.json().await {
                Ok(body) => return Ok(body),
                Err(_) => {
                    last_error = ControlPlaneError::Protocol;
                    retry_pause(attempt).await;
                }
            }
        }
        Err(last_error)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ControlPlaneError> {
        let mut last_error = ControlPlaneError::Transport;
        for attempt in 0..MAX_EXACT_ATTEMPTS {
            let response = match self
                .http
                .get(format!("{}{path}", self.base_url))
                .bearer_auth(&self.token)
                .header(CLIENT_EXECUTOR_HEADER, self.executor_token.clone())
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => {
                    last_error = ControlPlaneError::Transport;
                    retry_pause(attempt).await;
                    continue;
                }
            };
            if !response.status().is_success() {
                let status = response.status().as_u16();
                if response.status().is_server_error() && attempt + 1 < MAX_EXACT_ATTEMPTS {
                    last_error = ControlPlaneError::Http {
                        status,
                        message: "request failed".to_owned(),
                    };
                    retry_pause(attempt).await;
                    continue;
                }
                let message = response
                    .json::<ErrorBody>()
                    .await
                    .map(|body| body.message)
                    .unwrap_or_else(|_| "request failed".to_owned());
                return Err(ControlPlaneError::Http { status, message });
            }
            match response.json().await {
                Ok(body) => return Ok(body),
                Err(_) => {
                    last_error = ControlPlaneError::Protocol;
                    retry_pause(attempt).await;
                }
            }
        }
        Err(last_error)
    }
}

async fn retry_pause(attempt: usize) {
    if attempt + 1 < MAX_EXACT_ATTEMPTS {
        tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1))).await;
    }
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_accepts_only_authenticated_loopback_http() {
        let client = |url: &str, bearer: &str, executor: &str| {
            ControlPlaneClient::new(url.into(), bearer.into(), executor.into())
        };
        assert!(client("http://127.0.0.1:1234", "token", "native").is_ok());
        assert!(client("http://[::1]:1234", "token", "native").is_ok());
        assert!(client("http://localhost:1234", "token", "native").is_ok());
        assert!(client("http://127.1.2.3:1234", "token", "native").is_ok());
        assert!(client("https://example.com", "token", "native").is_err());
        assert!(client("http://127.0.0.1.example:1234", "token", "native").is_err());
        assert!(client("http://127.0.0.1:1234/path", "token", "native").is_err());
        assert!(client("http://127.0.0.1:1234", "", "native").is_err());
        assert!(client("http://127.0.0.1:1234", "token", "").is_err());
    }
}

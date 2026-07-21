//! Authenticated loopback client for the durable client-execution API.

use chrono::{DateTime, Utc};
use openwave_core::{
    CallId, ChatId, HostRootId, RootAttachmentChangeAction, RootAttachmentChangeFailure,
    RootAttachmentChangeId, RootAttachmentChangePhase, RootAttachmentChangeTerminal,
    RootAttachmentSubjectKind, ToolCallRecord,
};
use reqwest::StatusCode;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::receipt_store::{DelegatedFileResolution, StoredResolution};

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
    #[error("local control plane rejected the request ({status}, {kind}): {message}")]
    Http {
        status: u16,
        kind: String,
        message: String,
    },
    #[error("local control plane returned an invalid response")]
    Protocol,
}

impl ControlPlaneError {
    pub(super) fn is_conflict(&self) -> bool {
        matches!(self, Self::Http { status, .. } if *status == StatusCode::CONFLICT.as_u16())
    }

    pub(super) fn is_kind(&self, expected: &str) -> bool {
        matches!(self, Self::Http { kind, .. } if kind == expected)
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

#[derive(Serialize)]
struct BeginRootAttachmentRequest {
    root_id: HostRootId,
    action: RootAttachmentChangeAction,
    expected_attachment_revision: i64,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct FinishRootAttachmentRequest<'a> {
    terminal: &'a RootAttachmentChangeTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) struct PendingDelegatedFileRead {
    pub(super) call_id: CallId,
    pub(super) claimed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DelegatedFileClaimDisposition {
    Claimed,
    Existing,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClaimedDelegatedFileRead {
    pub(super) disposition: DelegatedFileClaimDisposition,
    pub(super) call_id: CallId,
    pub(super) chat_id: ChatId,
    pub(super) root_id: HostRootId,
    pub(super) relative_path: String,
}

#[derive(Serialize)]
struct DelegatedFileLeaseRequest {
    lease_token: Uuid,
}

#[derive(Serialize)]
struct ResolveDelegatedFileReadRequest<'a> {
    lease_token: Uuid,
    resolution: &'a DelegatedFileResolution,
}

#[derive(Deserialize)]
struct DelegatedFileHeartbeat {
    extended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DelegatedFileResolutionDisposition {
    Resolved,
    Existing,
}

#[derive(Deserialize)]
struct ResolvedDelegatedFileRead {
    disposition: DelegatedFileResolutionDisposition,
}

#[derive(Debug, Deserialize)]
pub(super) struct RootAttachmentChangeResponse {
    pub(super) change: RootAttachmentChangeView,
}

#[derive(Debug, Deserialize)]
struct PendingRootAttachmentChanges {
    changes: Vec<RootAttachmentChangeView>,
}

/// Closed native view of the fields needed to fence exact product/broker
/// reconciliation. Server-only executor metadata is intentionally absent.
#[derive(Debug, Deserialize)]
pub(super) struct RootAttachmentChangeView {
    pub(super) id: RootAttachmentChangeId,
    pub(super) chat_id: ChatId,
    pub(super) root_id: HostRootId,
    pub(super) action: RootAttachmentChangeAction,
    pub(super) subject_kind: RootAttachmentSubjectKind,
    pub(super) subject_id: Uuid,
    pub(super) expected_revision: i64,
    pub(super) before_revision: i64,
    pub(super) intent_revision: i64,
    pub(super) projection_existed_before: bool,
    pub(super) phase: RootAttachmentChangePhase,
    pub(super) result_revision: Option<i64>,
    pub(super) broker_currently_attached: Option<bool>,
    pub(super) failure: Option<RootAttachmentChangeFailure>,
    pub(super) created_at: DateTime<Utc>,
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

    pub(super) async fn pending_delegated_file_reads(
        &self,
    ) -> Result<Vec<PendingDelegatedFileRead>, ControlPlaneError> {
        self.get("/sandbox-file-reads/pending").await
    }

    pub(super) async fn claim_delegated_file_read(
        &self,
        call_id: CallId,
        lease_token: Uuid,
    ) -> Result<ClaimedDelegatedFileRead, ControlPlaneError> {
        self.post(
            &format!("/sandbox-file-reads/{call_id}/claim"),
            &DelegatedFileLeaseRequest { lease_token },
        )
        .await
    }

    pub(super) async fn heartbeat_delegated_file_read(
        &self,
        call_id: CallId,
        lease_token: Uuid,
    ) -> Result<(), ControlPlaneError> {
        let heartbeat: DelegatedFileHeartbeat = self
            .post(
                &format!("/sandbox-file-reads/{call_id}/heartbeat"),
                &DelegatedFileLeaseRequest { lease_token },
            )
            .await?;
        if !heartbeat.extended {
            return Err(ControlPlaneError::Protocol);
        }
        Ok(())
    }

    pub(super) async fn resolve_delegated_file_read(
        &self,
        call_id: CallId,
        lease_token: Uuid,
        resolution: &DelegatedFileResolution,
    ) -> Result<(), ControlPlaneError> {
        let response: ResolvedDelegatedFileRead = self
            .post(
                &format!("/sandbox-file-reads/{call_id}/resolve"),
                &ResolveDelegatedFileReadRequest {
                    lease_token,
                    resolution,
                },
            )
            .await?;
        match response.disposition {
            DelegatedFileResolutionDisposition::Resolved
            | DelegatedFileResolutionDisposition::Existing => Ok(()),
        }
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

    pub(super) async fn begin_root_attachment_change(
        &self,
        chat_id: ChatId,
        change_id: RootAttachmentChangeId,
        root_id: HostRootId,
        action: RootAttachmentChangeAction,
        expected_attachment_revision: i64,
        created_at: DateTime<Utc>,
    ) -> Result<RootAttachmentChangeResponse, ControlPlaneError> {
        self.post(
            &format!("/chats/{chat_id}/root-attachment-changes/{change_id}/begin"),
            &BeginRootAttachmentRequest {
                root_id,
                action,
                expected_attachment_revision,
                created_at,
            },
        )
        .await
    }

    pub(super) async fn pending_root_attachment_changes(
        &self,
        limit: usize,
    ) -> Result<Vec<RootAttachmentChangeView>, ControlPlaneError> {
        let response: PendingRootAttachmentChanges = self
            .get(&format!("/root-attachment-changes/pending?limit={limit}"))
            .await?;
        Ok(response.changes)
    }

    pub(super) async fn finish_root_attachment_change(
        &self,
        change_id: RootAttachmentChangeId,
        terminal: &RootAttachmentChangeTerminal,
    ) -> Result<RootAttachmentChangeResponse, ControlPlaneError> {
        self.post(
            &format!("/root-attachment-changes/{change_id}/finish"),
            &FinishRootAttachmentRequest { terminal },
        )
        .await
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
                        kind: "internal".to_owned(),
                        message: "request failed".to_owned(),
                    };
                    retry_pause(attempt).await;
                    continue;
                }
                let error = response
                    .json::<ErrorBody>()
                    .await
                    .unwrap_or_else(|_| ErrorBody {
                        kind: "protocol".to_owned(),
                        message: "request failed".to_owned(),
                    });
                return Err(ControlPlaneError::Http {
                    status,
                    kind: error.kind,
                    message: error.message,
                });
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
                        kind: "internal".to_owned(),
                        message: "request failed".to_owned(),
                    };
                    retry_pause(attempt).await;
                    continue;
                }
                let error = response
                    .json::<ErrorBody>()
                    .await
                    .unwrap_or_else(|_| ErrorBody {
                        kind: "protocol".to_owned(),
                        message: "request failed".to_owned(),
                    });
                return Err(ControlPlaneError::Http {
                    status,
                    kind: error.kind,
                    message: error.message,
                });
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
    kind: String,
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

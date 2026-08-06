//! The loopback HTTP+WebSocket client — the same contract the desktop webview
//! consumes.

use std::net::SocketAddr;

use openwave_core::{AgentError, AgentRunId, CallId, ChatId, Result, TurnId};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::wire::{
    AgentActivityItem, AgentRunSnapshot, ChatSummary, GrantRung, InboxItem, ModelCatalog,
    PendingApprovalSnapshot, PendingPlan, PendingQuestions, Transcript,
};

/// The chat event stream once the upgrade completes.
pub type EventSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Bearer-authed client for one bound server. Cheap to clone: the reqwest
/// client and the credentials are shared.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// The error body every route answers with on failure.
#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

/// `POST /chats` returns the whole `Chat`; only the id is read.
#[derive(Deserialize)]
struct ChatCreated {
    id: ChatId,
}

impl Client {
    pub fn new(addr: SocketAddr, token: &str) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| AgentError::msg(format!("invalid server token: {error}")))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|error| {
                AgentError::msg(format!("could not build the HTTP client: {error}"))
            })?;
        Ok(Self {
            http,
            base: format!("http://{addr}"),
            token: token.to_owned(),
        })
    }

    /// Create a fresh chat (server-side defaults seed the rest).
    pub async fn create_chat(&self) -> Result<ChatId> {
        let response = self
            .http
            .post(format!("{}/chats", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(request_error)?;
        let chat = Self::expect_success(response)
            .await?
            .json::<ChatCreated>()
            .await
            .map_err(request_error)?;
        Ok(chat.id)
    }

    /// Verify a chat exists, so `tui --chat <id>` fails fast on a typo.
    pub async fn require_chat(&self, chat: ChatId) -> Result<()> {
        let response = self
            .http
            .get(format!("{}/chats/{chat}", self.base))
            .send()
            .await
            .map_err(request_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AgentError::msg(format!("chat {chat} not found")));
        }
        Self::expect_success(response).await?;
        Ok(())
    }

    /// One chat's record (title, model, permission mode, …).
    pub async fn get_chat(&self, chat: ChatId) -> Result<ChatSummary> {
        self.get_json(format!("{}/chats/{chat}", self.base)).await
    }

    /// Every chat, most recently active first (server ordering).
    pub async fn list_chats(&self) -> Result<Vec<ChatSummary>> {
        self.get_json(format!("{}/chats", self.base)).await
    }

    /// Everything parked on the user across chats, oldest first.
    pub async fn list_inbox(&self) -> Result<Vec<InboxItem>> {
        self.get_json(format!("{}/inbox", self.base)).await
    }

    /// The visible transcript plus the journal watermark to resume events at.
    pub async fn get_transcript(&self, chat: ChatId) -> Result<Transcript> {
        self.get_json(format!("{}/chats/{chat}/messages", self.base))
            .await
    }

    /// Rename a chat; `None` clears back to an untitled chat.
    pub async fn rename_chat(&self, chat: ChatId, title: Option<&str>) -> Result<()> {
        let response = self
            .http
            .patch(format!("{}/chats/{chat}", self.base))
            .json(&serde_json::json!({ "title": title }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Patch the chat's model selection; `None` clears back to the default.
    pub async fn set_chat_model(&self, chat: ChatId, model: Option<&str>) -> Result<()> {
        let response = self
            .http
            .patch(format!("{}/chats/{chat}", self.base))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Patch the chat's reasoning-effort override; `None` clears it.
    pub async fn set_chat_effort(&self, chat: ChatId, effort: Option<&str>) -> Result<()> {
        let response = self
            .http
            .patch(format!("{}/chats/{chat}", self.base))
            .json(&serde_json::json!({ "reasoning_effort": effort }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Patch the chat's permission mode; `None` clears back to `ask`.
    pub async fn set_chat_permission_mode(
        &self,
        chat: ChatId,
        mode: Option<&str>,
    ) -> Result<ChatSummary> {
        let response = self
            .http
            .patch(format!("{}/chats/{chat}", self.base))
            .json(&serde_json::json!({ "permission_mode": mode }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<ChatSummary>()
            .await
            .map_err(request_error)
    }

    /// Move a chat between projects (or out of one with `None`).
    pub async fn set_chat_project(
        &self,
        chat: ChatId,
        project: Option<openwave_core::ProjectId>,
    ) -> Result<ChatSummary> {
        let response = self
            .http
            .patch(format!("{}/chats/{chat}", self.base))
            .json(&serde_json::json!({ "project_id": project }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<ChatSummary>()
            .await
            .map_err(request_error)
    }

    /// Every project (workspace), most recently created first.
    pub async fn list_projects(&self) -> Result<Vec<super::wire::ProjectSummary>> {
        self.get_json(format!("{}/projects", self.base)).await
    }

    /// Delete a chat outright.
    pub async fn delete_chat(&self, chat: ChatId) -> Result<()> {
        let response = self
            .http
            .delete(format!("{}/chats/{chat}", self.base))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// The selectable model catalog.
    pub async fn list_models(&self) -> Result<ModelCatalog> {
        self.get_json(format!("{}/models", self.base)).await
    }

    /// Background agent runs for a chat.
    pub async fn list_agent_runs(&self, chat: ChatId) -> Result<Vec<AgentRunSnapshot>> {
        self.get_json(format!("{}/chats/{chat}/agent-runs", self.base))
            .await
    }

    /// A background run's ordered activity timeline.
    pub async fn list_agent_run_activity(
        &self,
        chat: ChatId,
        run: AgentRunId,
    ) -> Result<Vec<AgentActivityItem>> {
        self.get_json(format!(
            "{}/chats/{chat}/agent-runs/{run}/activity",
            self.base
        ))
        .await
    }

    /// Ask a background run to stop (`202`).
    pub async fn cancel_agent_run(&self, chat: ChatId, run: AgentRunId) -> Result<()> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/agent-runs/{run}/cancel",
                self.base
            ))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Approvals parked on this chat right now (recovery on resume).
    pub async fn list_pending_approvals(
        &self,
        chat: ChatId,
    ) -> Result<Vec<PendingApprovalSnapshot>> {
        self.get_json(format!("{}/chats/{chat}/approvals", self.base))
            .await
    }

    /// Plans awaiting review on this chat.
    pub async fn list_pending_plans(&self, chat: ChatId) -> Result<Vec<PendingPlan>> {
        self.get_json(format!("{}/chats/{chat}/plans/pending", self.base))
            .await
    }

    /// Decide a proposed plan. `feedback` rides a reject; `mode` names the
    /// continuation on accept (`None` leaves the server default, `auto`).
    pub async fn decide_plan(
        &self,
        chat: ChatId,
        call_id: CallId,
        accept: bool,
        feedback: Option<&str>,
        mode: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "decision": if accept { "accept" } else { "reject" },
        });
        if let Some(feedback) = feedback {
            body["feedback"] = serde_json::json!(feedback);
        }
        if accept {
            if let Some(mode) = mode {
                body["permission_mode"] = serde_json::json!(mode);
            }
        }
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/plans/{call_id}/decision",
                self.base
            ))
            .json(&body)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Question blocks the model is waiting on.
    pub async fn list_pending_questions(&self, chat: ChatId) -> Result<Vec<PendingQuestions>> {
        self.get_json(format!("{}/chats/{chat}/questions/pending", self.base))
            .await
    }

    /// Answer a parked question block. Each entry is
    /// `{question_id, selections, custom_answer?}`.
    pub async fn answer_questions(
        &self,
        chat: ChatId,
        call_id: CallId,
        answers: serde_json::Value,
    ) -> Result<()> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/questions/{call_id}/answer",
                self.base
            ))
            .json(&answers)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Steer an active turn with more user text (`202`).
    pub async fn steer(
        &self,
        chat: ChatId,
        turn_id: TurnId,
        steer_id: TurnId,
        content: &str,
    ) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/chats/{chat}/steer", self.base))
            .json(&serde_json::json!({
                "steer_id": steer_id,
                "turn_id": turn_id,
                "content": content,
                "interrupt": true,
            }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Accept a user message and queue its turn (`202`).
    pub async fn post_message(&self, chat: ChatId, turn_id: TurnId, content: &str) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/chats/{chat}/messages", self.base))
            .json(&serde_json::json!({ "turn_id": turn_id, "content": content }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Cancel one exact turn (`202`; `409` if it already finished).
    pub async fn cancel_turn(&self, chat: ChatId, turn_id: TurnId) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/chats/{chat}/cancel", self.base))
            .json(&serde_json::json!({ "turn_id": turn_id }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Decide a parked tool call (`204`). `grant` names a standing-grant rung
    /// from the approval's ladder; `reason` is recorded with a rejection and
    /// ignored on approval.
    pub async fn decide_approval(
        &self,
        chat: ChatId,
        call_id: CallId,
        approve: bool,
        reason: &str,
        grant: Option<GrantRung>,
    ) -> Result<()> {
        let body = if approve {
            match grant {
                Some(grant) => serde_json::json!({ "decision": "approve", "grant": grant }),
                None => serde_json::json!({ "decision": "approve" }),
            }
        } else {
            serde_json::json!({ "decision": "reject", "reason": reason })
        };
        let response = self
            .http
            .post(format!("{}/chats/{chat}/approvals/{call_id}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Open the chat event socket: replay of everything after `after`, then
    /// live frames. Auth rides `Sec-WebSocket-Protocol` (token prefix plus the
    /// `openwave-v1` handshake value the server selects back).
    pub async fn open_events(&self, chat: ChatId, after: i64) -> Result<EventSocket> {
        let url = format!(
            "{}/chats/{chat}/events?after={after}",
            self.base.replacen("http", "ws", 1)
        );
        let mut request = url
            .into_client_request()
            .map_err(|error| AgentError::msg(format!("bad event socket URL: {error}")))?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            // The token is a UUID, so it is always a valid header value.
            HeaderValue::from_str(&format!("openwave-v1, openwave-token.{}", self.token))
                .map_err(|error| AgentError::msg(format!("invalid subprotocol header: {error}")))?,
        );
        let (socket, _response) = connect_async(request)
            .await
            .map_err(|error| AgentError::msg(format!("event socket handshake failed: {error}")))?;
        Ok(socket)
    }

    /// Pass through a success, or lift the server's `{ kind, message }` body
    /// into the error.
    async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<ErrorBody>(&body)
            .map(|body| body.message)
            .unwrap_or(body);
        Err(AgentError::msg(format!(
            "request failed ({status}): {message}"
        )))
    }

    /// GET a JSON body, lifting failures the way every other route does.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T> {
        let response = self.http.get(url).send().await.map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<T>()
            .await
            .map_err(request_error)
    }
}

/// reqwest's `Display` already strips nothing for loopback URLs, but keep the
/// mapping in one place.
fn request_error(error: reqwest::Error) -> AgentError {
    AgentError::msg(format!("request failed: {error}"))
}

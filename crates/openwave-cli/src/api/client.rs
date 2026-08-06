//! The loopback HTTP+WebSocket client — the same contract the desktop webview
//! consumes.

use std::net::SocketAddr;

use openwave_core::{AgentError, CallId, ChatId, Result, TurnId};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

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

    /// Decide a parked tool call (`204`). Slice 1 sends no standing grant.
    /// `reason` is recorded with a rejection and ignored on approval.
    pub async fn decide_approval(
        &self,
        chat: ChatId,
        call_id: CallId,
        approve: bool,
        reason: &str,
    ) -> Result<()> {
        let body = if approve {
            serde_json::json!({ "decision": "approve" })
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
}

/// reqwest's `Display` already strips nothing for loopback URLs, but keep the
/// mapping in one place.
fn request_error(error: reqwest::Error) -> AgentError {
    AgentError::msg(format!("request failed: {error}"))
}

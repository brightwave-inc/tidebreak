//! The loopback HTTP+WebSocket client — the same contract the desktop webview
//! consumes.

use std::net::SocketAddr;

use openwave_core::{
    AgentError, AgentRunId, CallId, ChatId, OutputId, OutputRevisionId, Result, TurnId,
};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::wire::{
    AgentActivityItem, AgentRunSnapshot, ChatSummary, GrantRung, InboxItem, ModelCatalog,
    OutputPreview, OutputRevisions, OutputsCatalog, PendingApprovalSnapshot, PendingPlan,
    PendingQuestions, ProviderInfo, ProvidersList, Transcript,
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
    /// A client for a server bound in this process.
    pub fn new(addr: SocketAddr, token: &str) -> Result<Self> {
        Self::attach(format!("http://{addr}"), token)
    }

    /// A client for a server running somewhere else, named by its base URL.
    ///
    /// `base` is already normalized (scheme present, no trailing slash) — see
    /// [`crate::connect`], which is the only thing that builds one from user
    /// input.
    pub fn attach(base: String, token: &str) -> Result<Self> {
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
            base,
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

    /// Pin a model role to a catalog key, or clear it back to automatic with
    /// `None`.
    pub async fn set_model_role(
        &self,
        role: &str,
        selection: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/models/roles/{role}", self.base),
            &serde_json::json!({ "selection": selection }),
        )
        .await
    }

    /// Every provider kind and its current configuration.
    pub async fn list_providers(&self) -> Result<Vec<ProviderInfo>> {
        let list: ProvidersList = self.get_json(format!("{}/providers", self.base)).await?;
        Ok(list.providers)
    }

    /// Store an API key for one provider and enable it for routing.
    ///
    /// The credential goes straight to the server's secret store (the OS
    /// keychain on a desktop profile); the response never carries it back.
    pub async fn set_provider_api_key(&self, kind: &str, key: &str) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/providers/{kind}", self.base),
            &serde_json::json!({
                "enabled": true,
                "credential": { "type": "api_key", "key": key },
            }),
        )
        .await
    }

    /// Remove a provider's stored credential. The provider's other settings
    /// are untouched.
    pub async fn delete_provider_credential(&self, kind: &str) -> Result<()> {
        self.delete_ok(format!("{}/providers/{kind}/credential", self.base))
            .await
    }

    /// Runtime settings (`GET /settings`), verbatim.
    pub async fn get_settings(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/settings", self.base)).await
    }

    /// Host web-search selection and readiness.
    pub async fn get_web_search_config(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/web-search", self.base)).await
    }

    /// Select the host web-search provider; `None` turns host search off.
    pub async fn set_web_search_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/web-search", self.base),
            &serde_json::json!({ "provider": provider }),
        )
        .await
    }

    /// Which web-search provider slots hold a key.
    pub async fn get_web_search_credentials(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/web-search/credentials", self.base))
            .await
    }

    /// Store one web-search provider's key. Selection is unchanged.
    pub async fn set_web_search_credential(
        &self,
        provider: &str,
        key: &str,
    ) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/web-search/credentials/{provider}", self.base),
            &serde_json::json!({ "api_key": key }),
        )
        .await
    }

    /// Remove one web-search provider's key. Selection is unchanged.
    pub async fn delete_web_search_credential(&self, provider: &str) -> Result<serde_json::Value> {
        self.delete_json(format!("{}/web-search/credentials/{provider}", self.base))
            .await
    }

    /// Code-execution backend selection, readiness, and per-provider detail.
    pub async fn get_code_execution_config(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/code-execution", self.base)).await
    }

    /// Select the code-execution backend; `None` disables execution.
    pub async fn set_code_execution_provider(
        &self,
        provider: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/code-execution", self.base),
            &serde_json::json!({ "provider": provider }),
        )
        .await
    }

    /// Which code-execution provider slots hold a key.
    pub async fn get_code_execution_credentials(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/code-execution/credentials", self.base))
            .await
    }

    /// Store one code-execution provider's key. Selection is unchanged.
    pub async fn set_code_execution_credential(
        &self,
        provider: &str,
        key: &str,
    ) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/code-execution/credentials/{provider}", self.base),
            &serde_json::json!({ "api_key": key }),
        )
        .await
    }

    /// Remove one code-execution provider's key. Selection is unchanged.
    pub async fn delete_code_execution_credential(
        &self,
        provider: &str,
    ) -> Result<serde_json::Value> {
        self.delete_json(format!(
            "{}/code-execution/credentials/{provider}",
            self.base
        ))
        .await
    }

    /// Every mounted MCP server, configured and plugin-sourced alike.
    pub async fn get_mcp_servers(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/mcp/servers", self.base)).await
    }

    /// Replace the user-configured MCP server set wholesale — the only shape
    /// the route accepts. Plugin-sourced servers are rebuilt by the server and
    /// must not appear in `servers`.
    pub async fn put_mcp_servers(&self, servers: serde_json::Value) -> Result<serde_json::Value> {
        self.put_json(
            format!("{}/mcp/servers", self.base),
            &serde_json::json!({ "servers": servers }),
        )
        .await
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

    // ------------------------------------------------------------------
    // Conversation outputs. The same routes the desktop reads outputs
    // through; writing the bytes to a path is the caller's job, because only
    // the caller knows where they should land.
    // ------------------------------------------------------------------

    /// The conversation's live outputs.
    pub async fn list_outputs(&self, chat: ChatId) -> Result<OutputsCatalog> {
        self.get_json(format!("{}/chats/{chat}/outputs", self.base))
            .await
    }

    /// One output's bounded text preview: its current revision, or an exact one.
    pub async fn read_output(
        &self,
        chat: ChatId,
        output: OutputId,
        revision: Option<OutputRevisionId>,
    ) -> Result<OutputPreview> {
        let url = match revision {
            None => format!("{}/chats/{chat}/outputs/{output}", self.base),
            Some(revision) => format!(
                "{}/chats/{chat}/outputs/{output}/revisions/{revision}",
                self.base
            ),
        };
        self.get_json(url).await
    }

    /// An output's version history.
    pub async fn list_output_revisions(
        &self,
        chat: ChatId,
        output: OutputId,
    ) -> Result<OutputRevisions> {
        self.get_json(format!(
            "{}/chats/{chat}/outputs/{output}/revisions",
            self.base
        ))
        .await
    }

    /// One revision's complete bytes — what an export writes to disk.
    pub async fn read_output_bytes(
        &self,
        chat: ChatId,
        output: OutputId,
        revision: Option<OutputRevisionId>,
    ) -> Result<Vec<u8>> {
        let mut url = format!("{}/chats/{chat}/outputs/{output}/content", self.base);
        if let Some(revision) = revision {
            url.push_str(&format!("?revision_id={revision}"));
        }
        let response = self.http.get(url).send().await.map_err(request_error)?;
        Ok(Self::expect_success(response)
            .await?
            .bytes()
            .await
            .map_err(request_error)?
            .to_vec())
    }

    /// Attach one local file to a conversation as a source document, returning
    /// its document id. The media type decides which parser the server runs, so
    /// it is sniffed from the bytes rather than taken from the file name.
    pub async fn attach_document(
        &self,
        chat: ChatId,
        title: &str,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/documents/raw?title={}",
                self.base,
                urlencode(title)
            ))
            .header(reqwest::header::CONTENT_TYPE, media_type)
            .body(bytes)
            .send()
            .await
            .map_err(request_error)?;
        let value = Self::expect_success(response)
            .await?
            .json::<serde_json::Value>()
            .await
            .map_err(request_error)?;
        value["document_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AgentError::msg("ingest answered without a document id"))
    }

    /// Publish one local image for a conversation, returning the identity a
    /// later turn references.
    pub async fn attach_image(
        &self,
        chat: ChatId,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<String> {
        let response = self
            .http
            .post(format!("{}/chats/{chat}/attachments/images", self.base))
            // The route re-derives the format from the bytes and refuses a
            // declaration that disagrees, so this must be the sniffed type.
            .header(reqwest::header::CONTENT_TYPE, media_type)
            .body(bytes)
            .send()
            .await
            .map_err(request_error)?;
        let value = Self::expect_success(response)
            .await?
            .json::<serde_json::Value>()
            .await
            .map_err(request_error)?;
        value["attachment_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| AgentError::msg("image publish answered without an attachment id"))
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

    /// PUT a JSON body and decode the route's answer.
    async fn put_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self
            .http
            .put(url)
            .json(body)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<T>()
            .await
            .map_err(request_error)
    }

    /// DELETE, decoding the route's answer.
    async fn delete_json<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T> {
        let response = self.http.delete(url).send().await.map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<T>()
            .await
            .map_err(request_error)
    }

    /// DELETE a route that answers `204`.
    async fn delete_ok(&self, url: String) -> Result<()> {
        let response = self.http.delete(url).send().await.map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }
}

/// reqwest's `Display` already strips nothing for loopback URLs, but keep the
/// mapping in one place.
fn request_error(error: reqwest::Error) -> AgentError {
    AgentError::msg(format!("request failed: {error}"))
}

/// Percent-encode a query value. Titles are file names, so this only has to be
/// correct, not fast.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

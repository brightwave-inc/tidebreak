//! The loopback HTTP+WebSocket client — the same contract the desktop webview
//! consumes.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    AgentError, AgentRunId, CallId, ChatId, DocumentId, MessageId, OutputId, OutputRevisionId,
    Result, TurnId,
};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::wire::{
    AgentActivityItem, AgentRunSnapshot, ChatSummary, GrantRung, ModelCatalog, OutputPreview,
    OutputRevisions, OutputsCatalog, PendingPlan, PendingQuestions, ProviderInfo, ProvidersList,
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
    local_import_token: Option<String>,
    listen_data_dir: Option<PathBuf>,
}

/// The error body every route answers with on failure.
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    kind: Option<String>,
    message: String,
}

/// `POST /chats` returns the whole `Chat`; only the id is read.
#[derive(Deserialize)]
struct ChatCreated {
    id: ChatId,
}

/// What the ingest route says about one published source.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct IngestedSource {
    /// Derived from the origin URI when one was given, so a repeat publish of
    /// the same origin names the same source.
    pub document_id: uuid::Uuid,
    /// Whether the stored source has text a reader can be given.
    pub readiness: tidebreak_core::DocumentReadiness,
}

/// Durable terminal state used when a print-mode event socket cannot recover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableTurn {
    pub status: DurableTurnStatus,
    pub content: String,
    pub last_event_seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableTurnStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Deserialize)]
struct DurableTranscript {
    messages: Vec<DurableMessage>,
    terminal_turns: Vec<DurableTerminalTurn>,
    last_event_seq: i64,
}

#[derive(Deserialize)]
struct DurableMessage {
    id: MessageId,
    content: String,
}

#[derive(Deserialize)]
struct DurableTerminalTurn {
    turn_id: TurnId,
    #[serde(default)]
    message_id: Option<MessageId>,
    status: DurableTurnStatus,
    #[serde(default)]
    partial_content: String,
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
        Self::attach_with_local_import(base, token, None)
    }

    /// Attach with the data-directory capability for publishing caller-held
    /// bytes. The token is stored separately and sent only on document/image
    /// publication requests, never as a default header.
    pub fn attach_with_local_import(
        base: String,
        token: &str,
        local_import_token: Option<&str>,
    ) -> Result<Self> {
        Self::attach_with_reconnect_source(base, token, local_import_token, None)
    }

    /// Attach to a server and remember the profile whose `listen.json` owns
    /// its rotating endpoint credentials.
    pub fn attach_with_reconnect_source(
        base: String,
        token: &str,
        local_import_token: Option<&str>,
        listen_data_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| AgentError::msg(format!("invalid server token: {error}")))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let mut http = reqwest::Client::builder().default_headers(headers);
        if loopback_http_base(&base) {
            // Ambient HTTP_PROXY must not intercept loopback: a proxy that
            // claims 127.0.0.1 black-holes `serve` and `--attach`.
            http = http.no_proxy();
        }
        let http = http.build().map_err(|error| {
            AgentError::msg(format!("could not build the HTTP client: {error}"))
        })?;
        Ok(Self {
            http,
            base,
            token: token.to_owned(),
            local_import_token: local_import_token.map(str::to_owned),
            listen_data_dir,
        })
    }

    /// Re-read a desktop-owned attach endpoint after the desktop restarts.
    /// Explicit `--server` clients have no rotating source and remain unchanged.
    pub fn refresh_attach_endpoint(&mut self) -> Result<()> {
        let Some(data_dir) = self.listen_data_dir.clone() else {
            return Ok(());
        };
        let endpoint = tidebreak_server::listen_endpoint::ListenEndpoint::read(&data_dir)?;
        *self = Self::attach_with_reconnect_source(
            endpoint.base_url.trim_end_matches('/').to_owned(),
            &endpoint.token,
            Some(&endpoint.local_import_token),
            Some(data_dir),
        )?;
        Ok(())
    }

    fn with_local_import(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.local_import_token {
            Some(token) => request.header("x-tidebreak-local-import", token),
            None => request,
        }
    }

    /// Whether this client discovered the scoped local publication capability.
    pub fn has_local_import_capability(&self) -> bool {
        self.local_import_token.is_some()
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

    /// Verify a chat exists before a command operates on it.
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

    /// Tool-call approvals awaiting a decision on this chat.
    pub async fn list_pending_approvals(&self, chat: ChatId) -> Result<Vec<serde_json::Value>> {
        self.get_json(format!("{}/chats/{chat}/approvals", self.base))
            .await
    }

    /// Host-folder access requests awaiting operator consent on this chat.
    ///
    /// This is the renderer-facing pending set (bearer auth), not the native
    /// executor's raw claim list. A call appears here only after it has parked.
    pub async fn list_pending_folder_access(&self, chat: ChatId) -> Result<Vec<serde_json::Value>> {
        self.get_json(format!(
            "{}/chats/{chat}/client-executions/pending",
            self.base
        ))
        .await
    }

    /// The durable transcript plus terminal turns and the journal watermark.
    pub async fn chat_transcript(&self, chat: ChatId) -> Result<serde_json::Value> {
        self.get_json(format!("{}/chats/{chat}/messages", self.base))
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
    ///
    /// `attachments` are image ids returned by [`Self::attach_image`].
    /// `file_attachments` are document ids returned by
    /// [`Self::attach_document`]. Empty slices are the ordinary text-only send.
    pub async fn post_message(
        &self,
        chat: ChatId,
        turn_id: TurnId,
        content: &str,
        attachments: &[uuid::Uuid],
        file_attachments: &[DocumentId],
    ) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/chats/{chat}/messages", self.base))
            .json(&serde_json::json!({
                "turn_id": turn_id,
                "content": content,
                "attachments": attachments,
                "file_attachments": file_attachments,
            }))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Read one turn's durable terminal state after live event delivery fails.
    pub async fn durable_turn(&self, chat: ChatId, turn_id: TurnId) -> Result<Option<DurableTurn>> {
        let transcript: DurableTranscript = self
            .get_json(format!("{}/chats/{chat}/messages", self.base))
            .await?;
        Ok(durable_turn_from_transcript(transcript, turn_id))
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
    /// `tidebreak-v1` handshake value the server selects back).
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
            HeaderValue::from_str(&format!("tidebreak-v1, tidebreak-token.{}", self.token))
                .map_err(|error| AgentError::msg(format!("invalid subprotocol header: {error}")))?,
        );
        let (socket, _response) = connect_async(request)
            .await
            .map_err(|error| AgentError::msg(format!("event socket handshake failed: {error}")))?;
        Ok(socket)
    }

    /// Pass through a success, or lift the server's `{ kind, message }` body
    /// into the error. The kind is part of the message so a script can branch
    /// on the typed failure without scraping status codes.
    pub(crate) async fn expect_success(response: reqwest::Response) -> Result<reqwest::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        match serde_json::from_str::<ErrorBody>(&body) {
            Ok(ErrorBody {
                kind: Some(kind),
                message,
            }) if !kind.is_empty() => Err(AgentError::msg(format!(
                "request failed ({status}): {kind}: {message}"
            ))),
            Ok(ErrorBody { message, .. }) => Err(AgentError::msg(format!(
                "request failed ({status}): {message}"
            ))),
            Err(_) => Err(AgentError::msg(format!(
                "request failed ({status}): {body}"
            ))),
        }
    }

    /// Read the server's bounded process and request measurements.
    pub async fn diagnostics_snapshot(&self) -> Result<serde_json::Value> {
        self.get_json(format!("{}/diagnostics/snapshot", self.base))
            .await
    }

    /// Read the server's OpenMetrics snapshot.
    pub async fn diagnostics_metrics(&self) -> Result<String> {
        let response = self
            .http
            .get(format!("{}/diagnostics/metrics", self.base))
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .text()
            .await
            .map_err(request_error)
    }

    /// Read a ZIP containing the snapshot, OpenMetrics text, and log tails.
    pub async fn diagnostics_export(&self) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(format!("{}/diagnostics/export", self.base))
            .send()
            .await
            .map_err(request_error)?;
        Ok(Self::expect_success(response)
            .await?
            .bytes()
            .await
            .map_err(request_error)?
            .to_vec())
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
    ) -> Result<DocumentId> {
        Ok(DocumentId::from(
            self.publish_document_source(chat, Some(title), None, media_type, bytes)
                .await?
                .document_id,
        ))
    }

    /// Publish one source into a conversation through the ingest route.
    ///
    /// `uri` is the source's durable provenance, and the server derives the
    /// document id from it — so publishing the same origin twice recovers the one
    /// source instead of adding a second. `title` is metadata the route
    /// validates and may refuse; it never becomes a path.
    pub async fn publish_document_source(
        &self,
        chat: ChatId,
        title: Option<&str>,
        uri: Option<&str>,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<IngestedSource> {
        let mut url = format!("{}/chats/{chat}/documents/raw", self.base);
        let mut separator = '?';
        for (name, value) in [("title", title), ("uri", uri)] {
            if let Some(value) = value {
                url.push(separator);
                url.push_str(name);
                url.push('=');
                url.push_str(&urlencode(value));
                separator = '&';
            }
        }
        let response = self
            .with_local_import(self.http.post(url))
            .header(reqwest::header::CONTENT_TYPE, media_type)
            .body(bytes)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<IngestedSource>()
            .await
            .map_err(request_error)
    }

    /// Publish one local image for a conversation, returning the identity a
    /// later turn references.
    pub async fn attach_image(
        &self,
        chat: ChatId,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<uuid::Uuid> {
        let response = self
            .with_local_import(
                self.http
                    .post(format!("{}/chats/{chat}/attachments/images", self.base)),
            )
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
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(|| AgentError::msg("image publish answered without an attachment id"))
    }

    /// GET a JSON body, lifting failures the way every other route does.
    pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(&self, url: String) -> Result<T> {
        let response = self.http.get(url).send().await.map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<T>()
            .await
            .map_err(request_error)
    }

    /// PUT a JSON body and decode the route's answer.
    pub(crate) async fn put_json<T: serde::de::DeserializeOwned>(
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
    pub(crate) async fn delete_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
    ) -> Result<T> {
        let response = self.http.delete(url).send().await.map_err(request_error)?;
        Self::expect_success(response)
            .await?
            .json::<T>()
            .await
            .map_err(request_error)
    }

    /// DELETE a route that answers `204`.
    pub(crate) async fn delete_ok(&self, url: String) -> Result<()> {
        let response = self.http.delete(url).send().await.map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// POST a JSON body and decode the route's answer.
    pub(crate) async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        body: &serde_json::Value,
    ) -> Result<T> {
        let response = self
            .http
            .post(url)
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

    /// POST a JSON body when the route answers with a status only.
    pub(crate) async fn post_ok(&self, url: String, body: &serde_json::Value) -> Result<()> {
        let response = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(request_error)?;
        Self::expect_success(response).await?;
        Ok(())
    }

    /// Open a WebSocket against a path on this server, using the same
    /// `tidebreak-v1` + token subprotocol the chat event socket uses.
    pub(crate) async fn open_ws(&self, path_and_query: &str) -> Result<EventSocket> {
        let url = format!("{}{path_and_query}", self.base.replacen("http", "ws", 1));
        let mut request = url
            .into_client_request()
            .map_err(|error| AgentError::msg(format!("bad event socket URL: {error}")))?;
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&format!("tidebreak-v1, tidebreak-token.{}", self.token))
                .map_err(|error| AgentError::msg(format!("invalid subprotocol header: {error}")))?,
        );
        let (socket, _response) = connect_async(request)
            .await
            .map_err(|error| AgentError::msg(format!("event socket handshake failed: {error}")))?;
        Ok(socket)
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base
    }
}

/// The second per-launch credential, presented on the client-executor routes.
///
/// These routes exist for the trusted surface that executes a client-owned tool
/// call — in the desktop, the native shell that opens the folder picker. A
/// headless run has no such surface, which is exactly why print mode needs
/// them: it must answer a parked call rather than leave the turn hanging on a
/// consent flow that can never appear.
const CLIENT_EXECUTOR_HEADER: &str = "x-tidebreak-client-executor";

/// One parked client-executed tool call, as much as the CLI acts on.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingClientCall {
    pub id: CallId,
    pub name: String,
    #[serde(default)]
    pub client_executor_id: Option<uuid::Uuid>,
}

/// The canonical call one claim installed, plus the receipt needed to operate
/// it.
///
/// The record is the server's own checkpointed `ToolCallRecord`: it is where an
/// executor reads the arguments it is to act on, rather than trusting a
/// restatement from whatever announced the work.
#[derive(Debug, Deserialize)]
pub struct ClaimedClientCall {
    pub call: tidebreak_core::ToolCallRecord,
    pub lease_token: uuid::Uuid,
}

/// The terminal answer an executor publishes for one claimed call.
///
/// The variants and field names are the server's `ClientExecutionResolution`
/// wire shape. `rows` is the card projection the server rebuilds against the
/// call's own stored name, so a completed call may report entries and a refusal
/// reports none.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClientExecutionOutcome {
    Completed {
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<serde_json::Value>,
    },
    Failed {
        result: String,
        error_code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_detail: Option<String>,
    },
    Cancelled {
        result: String,
    },
}

/// Why one client-execution request failed.
///
/// A conflict is separated from everything else because it is the lifecycle's
/// own answer rather than a fault: the call is already claimed, already
/// terminal, or no longer owned by this lease. An executor must recognize it to
/// stand down instead of racing whoever does own the work.
#[derive(Debug)]
pub enum ClientExecutionError {
    Conflict(AgentError),
    Failed(AgentError),
}

impl ClientExecutionError {
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict(_))
    }
}

impl From<ClientExecutionError> for AgentError {
    fn from(error: ClientExecutionError) -> Self {
        match error {
            ClientExecutionError::Conflict(error) | ClientExecutionError::Failed(error) => error,
        }
    }
}

impl std::fmt::Display for ClientExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(error) | Self::Failed(error) => error.fmt(formatter),
        }
    }
}

type ExecutionResult<T> = std::result::Result<T, ClientExecutionError>;

impl Client {
    /// Every client-owned tool call currently parked on this chat.
    pub async fn pending_client_executions(
        &self,
        executor_token: &str,
        chat: ChatId,
    ) -> ExecutionResult<Vec<PendingClientCall>> {
        let response = self
            .http
            .get(format!(
                "{}/chats/{chat}/client-executions/pending/raw",
                self.base
            ))
            .header(CLIENT_EXECUTOR_HEADER, executor_token)
            .send()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))?;
        Self::expect_execution_success(response)
            .await?
            .json::<Vec<PendingClientCall>>()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))
    }

    /// Take ownership of one parked call so it can be resolved.
    ///
    /// The same `(executor_id, lease_token)` pair recovers an existing claim,
    /// which is how an executor picks its own interrupted work back up. No other
    /// executor can ever claim it, so a conflict means the work is not this
    /// caller's.
    pub async fn claim_client_execution(
        &self,
        executor_token: &str,
        chat: ChatId,
        call_id: CallId,
        executor_id: uuid::Uuid,
        lease_token: uuid::Uuid,
    ) -> ExecutionResult<ClaimedClientCall> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/client-executions/{call_id}/claim",
                self.base
            ))
            .header(CLIENT_EXECUTOR_HEADER, executor_token)
            .json(&serde_json::json!({
                "executor_id": executor_id,
                "lease_token": lease_token,
            }))
            .send()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))?;
        Self::expect_execution_success(response)
            .await?
            .json::<ClaimedClientCall>()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))
    }

    /// Renew the lease on a claim before work that will take a while.
    pub async fn heartbeat_client_execution(
        &self,
        executor_token: &str,
        chat: ChatId,
        call_id: CallId,
        lease_token: uuid::Uuid,
    ) -> ExecutionResult<()> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/client-executions/{call_id}/heartbeat",
                self.base
            ))
            .header(CLIENT_EXECUTOR_HEADER, executor_token)
            .json(&serde_json::json!({ "lease_token": lease_token }))
            .send()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))?;
        Self::expect_execution_success(response).await?;
        Ok(())
    }

    /// Report the terminal, model-facing result of one claimed call.
    pub async fn resolve_client_execution(
        &self,
        executor_token: &str,
        chat: ChatId,
        call_id: CallId,
        lease_token: uuid::Uuid,
        outcome: &ClientExecutionOutcome,
    ) -> ExecutionResult<()> {
        let response = self
            .http
            .post(format!(
                "{}/chats/{chat}/client-executions/{call_id}/resolve",
                self.base
            ))
            .header(CLIENT_EXECUTOR_HEADER, executor_token)
            .json(&serde_json::json!({
                "lease_token": lease_token,
                "resolution": outcome,
            }))
            .send()
            .await
            .map_err(|error| ClientExecutionError::Failed(request_error(error)))?;
        Self::expect_execution_success(response).await?;
        Ok(())
    }

    /// [`Self::expect_success`], keeping the lifecycle's conflict distinct.
    async fn expect_execution_success(
        response: reqwest::Response,
    ) -> ExecutionResult<reqwest::Response> {
        let conflict = response.status() == reqwest::StatusCode::CONFLICT;
        match Self::expect_success(response).await {
            Ok(response) => Ok(response),
            Err(error) if conflict => Err(ClientExecutionError::Conflict(error)),
            Err(error) => Err(ClientExecutionError::Failed(error)),
        }
    }
}

/// True when `base` names a loopback HTTP origin.
fn loopback_http_base(base: &str) -> bool {
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .unwrap_or(base);
    let host = rest.split(['/', ':']).next().unwrap_or(rest);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
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

fn durable_turn_from_transcript(
    transcript: DurableTranscript,
    turn_id: TurnId,
) -> Option<DurableTurn> {
    let turn = transcript
        .terminal_turns
        .into_iter()
        .find(|turn| turn.turn_id == turn_id)?;
    let content = turn
        .message_id
        .and_then(|message_id| {
            transcript
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .map(|message| message.content.clone())
        })
        .unwrap_or(turn.partial_content);
    Some(DurableTurn {
        status: turn.status,
        content,
        last_event_seq: transcript.last_event_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshing_an_attach_client_reloads_the_rotated_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        tidebreak_server::listen_endpoint::write(
            dir.path(),
            "http://127.0.0.1:1001",
            "first-token",
            "first-import",
        )
        .unwrap();
        let mut client = Client::attach_with_reconnect_source(
            "http://127.0.0.1:1001".into(),
            "first-token",
            Some("first-import"),
            Some(dir.path().to_path_buf()),
        )
        .unwrap();

        tidebreak_server::listen_endpoint::write(
            dir.path(),
            "http://127.0.0.1:2002/",
            "second-token",
            "second-import",
        )
        .unwrap();
        client.refresh_attach_endpoint().unwrap();

        assert_eq!(client.base, "http://127.0.0.1:2002");
        assert_eq!(client.token, "second-token");
        assert_eq!(client.local_import_token.as_deref(), Some("second-import"));
        assert_eq!(client.listen_data_dir.as_deref(), Some(dir.path()));
    }

    #[test]
    fn durable_reconciliation_prefers_the_committed_message_and_keeps_partial_fallback() {
        let completed_turn = TurnId::new();
        let cancelled_turn = TurnId::new();
        let message_id = MessageId::new();
        let transcript = DurableTranscript {
            messages: vec![DurableMessage {
                id: message_id,
                content: "authoritative answer".into(),
            }],
            terminal_turns: vec![
                DurableTerminalTurn {
                    turn_id: completed_turn,
                    message_id: Some(message_id),
                    status: DurableTurnStatus::Completed,
                    partial_content: "partial".into(),
                },
                DurableTerminalTurn {
                    turn_id: cancelled_turn,
                    message_id: None,
                    status: DurableTurnStatus::Cancelled,
                    partial_content: "visible before cancellation".into(),
                },
            ],
            last_event_seq: 41,
        };

        assert_eq!(
            durable_turn_from_transcript(transcript, completed_turn),
            Some(DurableTurn {
                status: DurableTurnStatus::Completed,
                content: "authoritative answer".into(),
                last_event_seq: 41,
            })
        );

        let transcript = DurableTranscript {
            messages: Vec::new(),
            terminal_turns: vec![DurableTerminalTurn {
                turn_id: cancelled_turn,
                message_id: None,
                status: DurableTurnStatus::Cancelled,
                partial_content: "visible before cancellation".into(),
            }],
            last_event_seq: 42,
        };
        assert_eq!(
            durable_turn_from_transcript(transcript, cancelled_turn),
            Some(DurableTurn {
                status: DurableTurnStatus::Cancelled,
                content: "visible before cancellation".into(),
                last_event_seq: 42,
            })
        );
    }
}

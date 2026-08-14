//! Route handlers extracted from the parent `routes` module.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;

use tidebreak_core::{
    AgentRunId, Chat, ChatId, DeleteChatOutcome, DeleteProjectOutcome, DocumentId,
    Message as StoredMessage, MessageId, MoveChatOutcome, PermissionMode, Project, ProjectId,
    ReasoningEffort, Role, TurnId, CONTEXT_CHECKPOINT_FORMAT_V1, CONTEXT_CHECKPOINT_FORMAT_V2,
};

use crate::error::ServerError;
use crate::exec_write_snapshot::{list_file_change_summaries, ExecFileChangeSummary};
use crate::extract::{Json, Path};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

use super::providers_models::{refuse_permission_mode_over_ceiling, validate_model_selection};
use super::settings::{
    double_option, read_sticky_default, sticky_default_value, write_sticky_default,
    STICKY_MODEL_KEY, STICKY_NETWORK_POLICY_KEY, STICKY_PERMISSION_MODE_KEY,
    STICKY_REASONING_EFFORT_KEY,
};

/// Product-facing project names stay compact across desktop and API clients.
pub const MAX_PROJECT_TITLE_CHARS: usize = 120;
/// The same bound for conversation names, whether a user typed one or the
/// product derived one. A sidebar row is a sidebar row either way.
pub const MAX_CHAT_TITLE_CHARS: usize = MAX_PROJECT_TITLE_CHARS;
/// Project metadata requests need only a compact JSON object.
pub const MAX_PROJECT_METADATA_BODY_BYTES: usize = 1_024;

/// Body of `POST /projects`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// Body of `PATCH /projects/{id}`. An explicit `null` clears the title, while
/// an absent field leaves it unchanged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
}

fn normalize_project_title(title: Option<String>) -> Result<Option<String>, ServerError> {
    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if title
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_PROJECT_TITLE_CHARS)
    {
        return Err(ServerError::bad_request(format!(
            "project title must not exceed {MAX_PROJECT_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// Trim a client-supplied conversation title and hold it to the stored bound.
///
/// An empty title is the same as no title: the sidebar renders "New chat" for
/// both, and storing `Some("")` would also read as "already named" to the
/// derived-title path and suppress it forever.
fn normalize_chat_title(title: Option<String>) -> Result<Option<String>, ServerError> {
    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if title
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_CHAT_TITLE_CHARS)
    {
        return Err(ServerError::bad_request(format!(
            "chat title must not exceed {MAX_CHAT_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    store: ScopedStore,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    let project = Project {
        id: ProjectId::new(),
        title: normalize_project_title(body.title)?,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `PATCH /projects/{id}` — update bounded human-facing project metadata.
pub async fn patch_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
    Json(body): Json<ProjectUpdate>,
) -> Result<Json<Project>, ServerError> {
    let title = body.title.map(normalize_project_title).transpose()?;
    if let Some(title) = title {
        if !store.update_project_title(id, title).await? {
            return Err(ServerError::not_found(format!("project {id} not found")));
        }
    }
    store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

/// `GET /projects` — list projects, most-recently-created first.
pub async fn list_projects(store: ScopedStore) -> Result<Json<Vec<Project>>, ServerError> {
    Ok(Json(store.list_projects().await?))
}

/// `GET /projects/{id}` — fetch one project, or `404`.
pub async fn get_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
) -> Result<Json<Project>, ServerError> {
    Ok(Json(store.require_project(id).await?))
}

/// `DELETE /projects/{id}` — remove an empty project. Owned conversations,
/// documents, and root defaults must be removed through their explicit
/// lifecycle APIs first; this boundary never cascades them.
pub async fn delete_project(
    store: ScopedStore,
    Path(id): Path<ProjectId>,
) -> Result<StatusCode, ServerError> {
    match store.delete_project(id).await? {
        DeleteProjectOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteProjectOutcome::NotFound => {
            Err(ServerError::not_found(format!("project {id} not found")))
        }
        DeleteProjectOutcome::NotEmpty => Err(ServerError::conflict_kind(
            "project_not_empty",
            "remove the project's conversations, documents, and connected folders before deleting it",
        )),
    }
}

/// Body of `POST /chats`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChat {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional project to file this chat under; omitted for a loose chat.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Optional model for this chat; omitted seeds the sticky default, else
    /// the configured default.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional reasoning-effort override for this chat; honored only by models
    /// that expose the control. Omitted seeds the sticky default.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional permission mode for this chat; omitted seeds the sticky
    /// default (clamped to any managed ceiling), else `ask`.
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    /// Code-execution network access for this conversation workspace.
    /// Omitted seeds the sticky default, else open public-internet access.
    #[serde(default)]
    pub network_policy: Option<tidebreak_core::NetworkPolicy>,
}

/// `POST /chats` — create a chat and return it (`201 Created`).
///
/// Fields the request leaves unspecified seed from the sticky defaults — the
/// reader's last explicit choice at these same routes — so a new chat starts
/// the way the reader configured the previous one instead of resetting to the
/// hard defaults. A brand-new install has no sticky state and keeps today's
/// defaults (`ask`, open network, configured model).
pub async fn create_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Json(mut body): Json<CreateChat>,
) -> Result<impl IntoResponse, ServerError> {
    // Return a product-facing 400 for an unknown project. The Store and schema
    // independently enforce the same membership invariant inside insertion.
    if let Some(project_id) = body.project_id {
        store.require_project(project_id).await?;
    }
    if let Some(model) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    refuse_permission_mode_over_ceiling(&state, body.permission_mode).await?;
    if let Some(policy) = body.network_policy.as_mut() {
        crate::code_execution::normalize_network_policy(policy)?;
    }
    let title = normalize_chat_title(body.title)?;
    let managed = crate::managed_policy::resolve(&*state.provisioned_policy, &*state.os_policy)?;
    let model = match body.model.as_ref() {
        Some(model) => Some(model.clone()),
        // On an unmanaged profile, preserve the historical recovery behavior
        // for a stale deregistered sticky value. Under managed routing the
        // sticky value is durable user intent: copy it unchanged so turn
        // admission can resolve a unique gateway equivalent or refuse an
        // ambiguous/unmatched selection honestly.
        None => match read_sticky_default::<String>(&*state.store, STICKY_MODEL_KEY).await? {
            Some(sticky) => match validate_model_selection(&state, &sticky, false).await {
                Ok(model) => Some(model),
                Err(_) if managed.managed => Some(sticky),
                Err(_) => None,
            },
            None => None,
        },
    };
    let reasoning_effort = match body.reasoning_effort.as_ref() {
        Some(effort) => Some(*effort),
        None => read_sticky_default(&*state.store, STICKY_REASONING_EFFORT_KEY).await?,
    };
    let permission_mode = match body.permission_mode.as_ref() {
        Some(mode) => Some(*mode),
        // The managed ceiling clamps a sticky mode recorded before the policy
        // arrived: a remembered `allow` under an `ask` ceiling seeds `ask`,
        // mirroring how the turn gate treats stored over-ceiling modes.
        None => match read_sticky_default(&*state.store, STICKY_PERMISSION_MODE_KEY).await? {
            Some(sticky) => {
                crate::managed_policy::resolve(&*state.provisioned_policy, &*state.os_policy)?
                    .clamp_permission_mode(Some(sticky))
            }
            None => None,
        },
    };
    let network_policy = match body.network_policy.as_ref() {
        Some(policy) => policy.clone(),
        None => {
            let mut sticky = read_sticky_default(&*state.store, STICKY_NETWORK_POLICY_KEY)
                .await?
                .unwrap_or_default();
            // Stored values were normalized at write; a stale one that no
            // longer passes falls back to the product default rather than
            // failing the create.
            if crate::code_execution::normalize_network_policy(&mut sticky).is_err() {
                sticky = tidebreak_core::NetworkPolicy::default();
            }
            sticky
        }
    };
    // An explicit choice at creation is as much "the last-chosen mode" as one
    // made mid-chat — the home composer's pickers land here, never at PATCH.
    // Apply only supplied values, and only in the transaction that successfully
    // inserts the chat, so a project deletion race cannot partially advance
    // the defaults before the create reports its failure.
    let mut sticky_default_updates = Vec::with_capacity(4);
    if let Some(model) = &body.model {
        sticky_default_updates.push((
            STICKY_MODEL_KEY.to_owned(),
            sticky_default_value(Some(model))?,
        ));
    }
    if let Some(effort) = &body.reasoning_effort {
        sticky_default_updates.push((
            STICKY_REASONING_EFFORT_KEY.to_owned(),
            sticky_default_value(Some(effort))?,
        ));
    }
    if let Some(mode) = &body.permission_mode {
        sticky_default_updates.push((
            STICKY_PERMISSION_MODE_KEY.to_owned(),
            sticky_default_value(Some(mode))?,
        ));
    }
    if let Some(policy) = &body.network_policy {
        sticky_default_updates.push((
            STICKY_NETWORK_POLICY_KEY.to_owned(),
            sticky_default_value(Some(policy))?,
        ));
    }
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title,
        model,
        reasoning_effort,
        permission_mode,
        network_policy,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    let chat = store
        .create_chat_with_project_defaults_and_settings(&chat, &sticky_default_updates)
        .await?;
    Ok((StatusCode::CREATED, Json(chat)))
}

/// Body of `PATCH /chats/{id}`. A double option (like `PUT /settings`): absent
/// leaves the model unchanged, `null` clears it (fall back to the default), and a
/// value sets it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatUpdate {
    /// An explicit `null` clears the title. Non-empty titles are trimmed before
    /// persistence so sidebar labels remain stable across clients.
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
    /// An explicit `null` clears the reasoning-effort override; a value sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    /// An explicit `null` clears the permission mode (back to `ask`); a value
    /// sets it.
    #[serde(default, deserialize_with = "double_option")]
    pub permission_mode: Option<Option<PermissionMode>>,
    /// Replace the code-execution network policy. Omitted leaves it unchanged.
    #[serde(default)]
    pub network_policy: Option<tidebreak_core::NetworkPolicy>,
    /// File the conversation under a project, or take it back out with an
    /// explicit `null`. Unlike the fields above this is never recorded as a
    /// sticky default: where one conversation lives says nothing about where
    /// the next one should be created.
    #[serde(default, deserialize_with = "double_option")]
    pub project_id: Option<Option<ProjectId>>,
}

/// `PATCH /chats/{id}` — update the human-facing title and/or model selection.
pub async fn patch_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(mut body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    // Validate every supplied field before touching durable state. This keeps a
    // mixed request all-or-nothing from the user's point of view.
    if let Some(Some(model)) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    if let Some(policy) = body.network_policy.as_mut() {
        crate::code_execution::normalize_network_policy(policy)?;
    }
    // A `null` (clear back to the default) is always allowed: the ceiling
    // caps what the reader may select, and the turn gate clamps whatever the
    // default resolves to.
    refuse_permission_mode_over_ceiling(&state, body.permission_mode.flatten()).await?;
    let title = body.title.map(normalize_chat_title).transpose()?;
    // The move lands first, because it is the one field that can be refused on
    // durable state rather than on its own value, and because the chat read
    // below must see where the conversation ended up.
    if let Some(project_id) = body.project_id {
        match store.move_chat_to_project(id, project_id).await? {
            MoveChatOutcome::Moved => {}
            MoveChatOutcome::ChatNotFound => {
                return Err(ServerError::not_found(format!("chat {id} not found")));
            }
            MoveChatOutcome::ProjectNotFound => {
                return Err(ServerError::not_found(match project_id {
                    Some(project_id) => format!("project {project_id} not found"),
                    None => "project not found".to_owned(),
                }));
            }
            MoveChatOutcome::HasConnectedFolders => {
                return Err(ServerError::conflict_kind(
                    "chat_roots_attached",
                    "disconnect this conversation's folders before moving it",
                ));
            }
            MoveChatOutcome::FolderChangePending => {
                return Err(ServerError::conflict_kind(
                    "chat_root_attachment_unresolved",
                    "a connected-folder change is still finishing; try moving again in a moment",
                ));
            }
        }
    }

    let mut chat = store.require_chat(id).await?;

    if !store
        .update_chat_metadata(
            id,
            title.clone(),
            body.model.clone(),
            body.reasoning_effort,
            body.permission_mode,
            body.network_policy.clone(),
        )
        .await?
    {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    // Each explicit choice here becomes the sticky default a new chat seeds
    // from; an explicit clear (`null`) clears the sticky default the same way,
    // back to the hard default. Recorded server-side so every client benefits.
    if let Some(model) = &body.model {
        write_sticky_default(&*state.store, STICKY_MODEL_KEY, model.as_ref()).await?;
    }
    if let Some(effort) = &body.reasoning_effort {
        write_sticky_default(&*state.store, STICKY_REASONING_EFFORT_KEY, effort.as_ref()).await?;
    }
    if let Some(mode) = &body.permission_mode {
        write_sticky_default(&*state.store, STICKY_PERMISSION_MODE_KEY, mode.as_ref()).await?;
    }
    if let Some(policy) = &body.network_policy {
        write_sticky_default(&*state.store, STICKY_NETWORK_POLICY_KEY, Some(policy)).await?;
    }
    if let Some(title) = title {
        chat.title = title;
    }
    if let Some(model) = body.model {
        chat.model = model;
    }
    if let Some(reasoning_effort) = body.reasoning_effort {
        chat.reasoning_effort = reasoning_effort;
    }
    if let Some(permission_mode) = body.permission_mode {
        chat.permission_mode = permission_mode;
    }
    if let Some(network_policy) = body.network_policy {
        chat.network_policy = network_policy;
    }
    Ok(Json(chat))
}

/// A renderer-safe durable transcript entry. Internal routing and tool state
/// deliberately remain behind the server boundary.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatMessageSnapshot {
    pub id: MessageId,
    pub role: TranscriptRole,
    pub content: String,
    pub created_at: chrono::DateTime<Utc>,
    pub citations: Vec<tidebreak_core::AssistantCitationSnapshot>,
    /// Images submitted with this user message. These are durable identity and
    /// geometry only; image bytes remain behind a chat-scoped authenticated
    /// endpoint and never enter the transcript payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_attachments: Option<Vec<TranscriptImageAttachment>>,
    /// Files submitted with this user message. Their bytes remain behind the
    /// existing chat-scoped document endpoints.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file_attachments: Option<Vec<TranscriptFileAttachment>>,
    /// Skills this user message explicitly invoked, in submitted order. Absent
    /// for the ordinary message that invoked none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invoked_skills: Option<Vec<String>>,
}

/// One renderer-safe image identity attached to a historical user message.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TranscriptImageAttachment {
    /// Content-addressed opaque attachment identity, not a host path.
    pub attachment_id: uuid::Uuid,
    /// Sniffed IANA media type from the trusted image ingest boundary.
    pub media_type: String,
    /// Header-derived dimensions, bounded at image publication.
    pub width: u32,
    pub height: u32,
}

impl From<tidebreak_core::MessageAttachment> for TranscriptImageAttachment {
    fn from(attachment: tidebreak_core::MessageAttachment) -> Self {
        Self {
            attachment_id: attachment.image.blob_id,
            media_type: attachment.image.media_type.as_str().to_owned(),
            width: attachment.image.width,
            height: attachment.image.height,
        }
    }
}

/// One renderer-safe source document attached to a historical user message.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TranscriptFileAttachment {
    pub document_id: DocumentId,
    pub name: String,
    pub media_type: String,
}

impl From<tidebreak_core::MessageDocumentAttachment> for TranscriptFileAttachment {
    fn from(attachment: tidebreak_core::MessageDocumentAttachment) -> Self {
        Self {
            document_id: attachment.document_id,
            name: attachment.title.unwrap_or_else(|| "Attachment".to_owned()),
            media_type: attachment.media_type,
        }
    }
}

/// One terminal turn's renderer-safe status and visible streamed content.
///
/// A completed turn points at its authoritative assistant output. A cancelled
/// turn may point at the last assistant message it committed before stopping;
/// message-less failed and cancelled turns remain first-class transcript entries
/// carrying the partial prose and reasoning the reader already saw live.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatTerminalTurnSnapshot {
    pub turn_id: TurnId,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message_id: Option<MessageId>,
    pub status: ChatTerminalTurnStatus,
    pub partial_content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refusal: Option<crate::event_projection::RendererRefusal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_category: Option<crate::event_projection::TurnFailureCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure_model: Option<crate::event_projection::RendererModelIdentity>,
    pub file_changes: Vec<ExecFileChangeSummary>,
    /// Skills the user explicitly invoked for this turn, in submitted order.
    /// Absent for the ordinary turn that invoked none.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invoked_skills: Option<Vec<String>>,
    /// Token accounting for the turn, so a freshly opened chat can show
    /// context usage without waiting for the next turn to finish.
    pub usage: crate::event_projection::RendererTurnUsage,
    pub voice_input_used: bool,
    pub finished_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatTerminalTurnStatus {
    Completed,
    Failed,
    Cancelled,
}

impl From<tidebreak_core::ChatTerminalTurnSnapshot> for ChatTerminalTurnSnapshot {
    fn from(snapshot: tidebreak_core::ChatTerminalTurnSnapshot) -> Self {
        let status = match snapshot.status {
            tidebreak_core::ChatTerminalTurnStatus::Completed => ChatTerminalTurnStatus::Completed,
            tidebreak_core::ChatTerminalTurnStatus::Failed => ChatTerminalTurnStatus::Failed,
            tidebreak_core::ChatTerminalTurnStatus::Cancelled => ChatTerminalTurnStatus::Cancelled,
        };
        let failure_category = matches!(status, ChatTerminalTurnStatus::Failed).then(|| {
            crate::event_projection::TurnFailureCategory::from_kind(
                snapshot.failure_kind.as_deref().unwrap_or_default(),
            )
        });
        let failure_model =
            failure_category.and_then(|_| crate::event_projection::model_identity(&snapshot.model));
        let failure_detail = snapshot.failure_kind.as_deref().and_then(|kind| {
            crate::event_projection::renderer_provider_failure_detail(
                kind,
                snapshot.failure_detail.as_deref().unwrap_or_default(),
            )
        });
        Self {
            turn_id: snapshot.turn_id,
            message_id: snapshot.message_id,
            status,
            partial_content: snapshot.partial_content,
            reasoning: (!snapshot.reasoning.trim().is_empty()).then_some(snapshot.reasoning),
            refusal: snapshot.refusal.as_ref().map(Into::into),
            failure_category,
            failure_detail,
            failure_model,
            file_changes: Vec::new(),
            invoked_skills: (!snapshot.invoked_skills.is_empty())
                .then_some(snapshot.invoked_skills),
            usage: snapshot.usage.into(),
            voice_input_used: snapshot.voice_input_used,
            finished_at: snapshot.finished_at,
        }
    }
}

/// One visible transcript plus the durable journal watermark that produced it.
/// The renderer uses the watermark to subscribe only to future events, avoiding
/// duplicate text when reopening a completed conversation.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ChatTranscript {
    pub messages: Vec<ChatMessageSnapshot>,
    /// Finished tool activity from terminal turns, projected through a fixed
    /// renderer-safe allowlist. Canonical tool records never cross this API.
    pub tool_activity: Vec<tidebreak_core::ChatToolActivitySnapshot>,
    /// Status and streamed presentation for every terminal turn. This owns
    /// terminal metadata even when no assistant message was committed.
    pub terminal_turns: Vec<ChatTerminalTurnSnapshot>,
    pub last_event_seq: i64,
}

/// The roles a visible transcript entry can have.
///
/// Narrower than [`Role`] on purpose. The transcript shows the conversation, not
/// the model's plumbing, so `System` and `Tool` never appear from storage as
/// tool rows — and that was previously guaranteed only by a `matches!` filter
/// at the one call site, while the snapshot's own type still admitted all four.
/// The renderer mirrored the narrow version and branched on `assistant` with no
/// third arm, so a `system` entry reaching it would have rendered as a user
/// message.
///
/// Encoding it here makes the guarantee the type's rather than the caller's, and
/// makes a new [`Role`] variant a decision in [`Self::for_transcript`] instead of
/// something that silently appears in the transcript.
///
/// [`Self::Compaction`] is synthetic: injected from the current context
/// checkpoint, never stored as a [`StoredMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
    /// A durable host-authored note — e.g. "User restored output 'report.md'…"
    /// — written between turns so the model's next turn learns what happened.
    /// Shown as a subtle inline notice, never as a user or assistant bubble.
    System,
    /// Renderer-only marker that earlier conversation was compacted into a
    /// semantic checkpoint. One current divider per chat, placed after the
    /// checkpoint's source message. Content is empty; the client labels it.
    Compaction,
}

impl TranscriptRole {
    /// `None` for roles the transcript does not show.
    fn for_transcript(role: Role) -> Option<Self> {
        match role {
            Role::User => Some(Self::User),
            Role::Assistant => Some(Self::Assistant),
            Role::System => Some(Self::System),
            Role::Tool => None,
        }
    }
}

impl ChatMessageSnapshot {
    /// `None` when the message is not part of the visible conversation.
    ///
    /// Replaces a separate `matches!` filter followed by an infallible
    /// conversion: the two could disagree, and only the filter was enforcing the
    /// narrowing the type claimed.
    fn for_transcript(message: StoredMessage) -> Option<Self> {
        Some(Self {
            id: message.id,
            role: TranscriptRole::for_transcript(message.role)?,
            content: message.content,
            created_at: message.created_at,
            citations: Vec::new(),
            image_attachments: None,
            file_attachments: None,
            invoked_skills: None,
        })
    }
}

/// `GET /chats/{id}/messages` — replay the visible durable transcript in
/// commit order. The existence check prevents a missing chat from looking like
/// an empty conversation.
pub async fn list_chat_messages(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<ChatTranscript>, ServerError> {
    let transcript = store
        .get_chat_transcript(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let mut citations_by_message = std::collections::HashMap::new();
    for citation in transcript.citations {
        citations_by_message
            .entry(citation.message_id)
            .or_insert_with(Vec::new)
            .push(citation.citation);
    }
    let mut image_attachments_by_message = std::collections::HashMap::new();
    for attachment in transcript.message_attachments {
        image_attachments_by_message
            .entry(attachment.message_id)
            .or_insert_with(Vec::new)
            .push(TranscriptImageAttachment::from(attachment));
    }
    let mut file_attachments_by_message = std::collections::HashMap::new();
    for attachment in transcript.message_document_attachments {
        file_attachments_by_message
            .entry(attachment.message_id)
            .or_insert_with(Vec::new)
            .push(TranscriptFileAttachment::from(attachment));
    }
    let mut invoked_skills_by_message = std::collections::HashMap::new();
    for invoked in transcript.message_invoked_skills {
        invoked_skills_by_message.insert(invoked.message_id, invoked.skills);
    }
    let mut messages: Vec<ChatMessageSnapshot> = transcript
        .messages
        .into_iter()
        .filter_map(|message| {
            let mut snapshot = ChatMessageSnapshot::for_transcript(message)?;
            snapshot.citations = citations_by_message
                .remove(&snapshot.id)
                .unwrap_or_default();
            if snapshot.role == TranscriptRole::User {
                let image_attachments = image_attachments_by_message
                    .remove(&snapshot.id)
                    .unwrap_or_default();
                snapshot.image_attachments =
                    (!image_attachments.is_empty()).then_some(image_attachments);
                let file_attachments = file_attachments_by_message
                    .remove(&snapshot.id)
                    .unwrap_or_default();
                snapshot.file_attachments =
                    (!file_attachments.is_empty()).then_some(file_attachments);
                snapshot.invoked_skills = invoked_skills_by_message
                    .remove(&snapshot.id)
                    .filter(|skills| !skills.is_empty());
            }
            Some(snapshot)
        })
        .collect();
    if let Some(checkpoint) = state
        .store
        .get_context_checkpoint(id)
        .await?
        .filter(|checkpoint| {
            checkpoint.chat_id == id
                && (checkpoint.format_version == CONTEXT_CHECKPOINT_FORMAT_V1
                    || checkpoint.format_version == CONTEXT_CHECKPOINT_FORMAT_V2)
                && checkpoint.validate().is_ok()
        })
    {
        if let Some(insert_at) = messages
            .iter()
            .position(|message| message.id == checkpoint.source_message_id)
            .map(|index| index + 1)
        {
            messages.insert(
                insert_at,
                ChatMessageSnapshot {
                    id: MessageId::compaction_divider(checkpoint.source_message_id),
                    role: TranscriptRole::Compaction,
                    content: String::new(),
                    created_at: checkpoint.created_at,
                    citations: Vec::new(),
                    image_attachments: None,
                    file_attachments: None,
                    invoked_skills: None,
                },
            );
        }
    }
    let mut file_changes_by_turn = match list_file_change_summaries(
        &*state.store,
        &*state.blobs,
        id,
        Some(&crate::turn_worker::private_chat_scratch_path(
            &state.config.data_dir.join("scratch"),
            id,
        )),
    )
    .await
    {
        Ok(summaries) => summaries,
        Err(error) => {
            tracing::warn!(
                chat = %id,
                %error,
                "could not load connected-folder change summaries"
            );
            std::collections::HashMap::new()
        }
    };
    Ok(Json(ChatTranscript {
        messages,
        tool_activity: transcript.tool_activity,
        terminal_turns: transcript
            .terminal_turns
            .into_iter()
            .map(|turn| {
                let mut snapshot = ChatTerminalTurnSnapshot::from(turn);
                snapshot.file_changes = file_changes_by_turn
                    .remove(&snapshot.turn_id)
                    .unwrap_or_default();
                snapshot
            })
            .collect(),
        last_event_seq: transcript.last_event_seq,
    }))
}

/// `GET /chats` — list chats, most-recently-created first.
pub async fn list_chats(store: ScopedStore) -> Result<Json<Vec<Chat>>, ServerError> {
    Ok(Json(store.list_chats().await?))
}

/// `GET /chats/{id}` — fetch one chat, or `404`.
pub async fn get_chat(
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Chat>, ServerError> {
    Ok(Json(store.require_chat(id).await?))
}

/// `DELETE /chats/{id}` — remove a quiesced conversation and its product
/// history. Rooted or active conversations deliberately return a conflict: the
/// caller must first finish cancellation and durable broker detachment.
pub async fn delete_chat(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    match store.delete_chat(id).await? {
        DeleteChatOutcome::Deleted { background_run_ids } => {
            state.blob_retirement_wake.notify_one();
            let scratch_root = state.config.data_dir.join("scratch");
            let cleanup =
                tokio::task::spawn_blocking(move || remove_private_chat_scratch(&scratch_root, id))
                    .await;
            match cleanup {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("tidebreak: could not remove private scratch for chat {id}: {error}");
                }
                Err(error) => {
                    eprintln!(
                        "tidebreak: private scratch cleanup task stopped for chat {id}: {error}"
                    );
                }
            }
            // Background-run workspaces are independent of the chat's private
            // scratch directory. Deletion already refused while any run was
            // non-terminal, so these directories are quiescent; erase them now
            // rather than waiting up to a reaper grace period for privacy.
            if !background_run_ids.is_empty() {
                erase_deleted_chat_agent_run_workspaces(&state, id, background_run_ids).await;
            }
            Ok(StatusCode::NO_CONTENT)
        }
        DeleteChatOutcome::NotFound => Err(ServerError::not_found(format!("chat {id} not found"))),
        DeleteChatOutcome::ActiveWork => Err(ServerError::conflict_kind(
            "chat_active",
            "finish or cancel the active work before deleting this conversation",
        )),
        DeleteChatOutcome::RootsAttached => Err(ServerError::conflict_kind(
            "chat_roots_attached",
            "detach connected folders before deleting this conversation",
        )),
        DeleteChatOutcome::RootAttachmentStateUnresolved => Err(ServerError::conflict_kind(
            "chat_root_attachment_unresolved",
            "a connected-folder change is still finishing; try deleting again in a moment",
        )),
    }
}

/// Remove a deleted chat's private scratch without following a replacement
/// root or chat-directory symlink. Database deletion remains authoritative, so
/// callers log cleanup failure rather than turning a committed delete into an
/// ambiguous HTTP failure.
fn remove_private_chat_scratch(root: &FsPath, id: ChatId) -> std::io::Result<()> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private scratch root is not a regular directory",
        ));
    }
    let directory = Dir::open_ambient_dir(root, ambient_authority())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory.dir_metadata()?;
        if root_metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || root_metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private scratch root changed while it was opened",
            ));
        }
    }
    let chat_name = id.to_string();
    match directory.symlink_metadata(&chat_name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            directory.remove_dir_all(chat_name)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private chat scratch is not a regular directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Destroy the former chat's background-run workspaces.
///
/// Failures are logged and ignored the same way private-chat scratch cleanup
/// is: the durable delete has already committed, so a stuck host directory must
/// not turn a successful response into an ambiguous HTTP failure. The periodic
/// agent-run scratch reaper remains the backstop.
async fn erase_deleted_chat_agent_run_workspaces(
    state: &AppState,
    chat_id: ChatId,
    run_ids: Vec<AgentRunId>,
) {
    let scratch_root = state.config.data_dir.join("scratch");
    if let Some(code_execution) = state.code_execution.clone() {
        for run_id in run_ids {
            if let Err(error) = crate::agent_run_scratch_reaper::destroy_agent_run_workspace(
                &code_execution,
                &scratch_root,
                run_id,
            )
            .await
            {
                eprintln!(
                    "tidebreak: could not destroy agent-run workspace for chat {chat_id} run {run_id}: {error}"
                );
            }
        }
        return;
    }
    // Route tests assemble AppState without a configured provider; still wipe
    // the host directories so local cleanup is not gated on provider resolution.
    let cleanup = tokio::task::spawn_blocking(move || {
        for run_id in run_ids {
            let path = scratch_root.join(format!("agent-run-{run_id}"));
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => eprintln!(
                    "tidebreak: could not remove agent-run scratch for chat {chat_id} run {run_id}: {error}"
                ),
            }
        }
    })
    .await;
    if let Err(error) = cleanup {
        eprintln!("tidebreak: agent-run scratch cleanup task stopped for chat {chat_id}: {error}");
    }
}

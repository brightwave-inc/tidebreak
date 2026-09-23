//! Rerun and branch a Work chat.
//!
//! Three actions, all on settled turns and all through the same admission path
//! an ordinary message takes:
//!
//! - Regenerate answers the latest message again. The earlier answer stays as
//!   a version the reader can page back to.
//! - Edit replaces the latest message and answers it. When the turn it
//!   replaces, or an earlier answer to the same message, changed things
//!   outside the conversation, the edit starts a new conversation instead, so
//!   the original keeps its record of what ran.
//! - Branch starts a new conversation with a copy of the history through one
//!   turn, named after the original and linked back to it.
//!
//! A replaced turn leaves the model's view of the conversation (see
//! `tidebreak_core::replaced_turns`). Its rows stay.

use std::collections::{BTreeSet, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use tidebreak_core::{
    BranchChat, BranchChatOutcome, Chat, ChatBranchPoint, ChatBranchRefusal, ChatListing,
    DocumentId, SessionId, ToolCallStatus, TurnId, TurnReplacementKind,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::principal::AuthContext;
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

use super::providers_models::validate_model_selection;
use super::turn_control::{admit_turn, preflight_turn, TurnInput, TurnSubmission};

/// Body of `POST /chats/{id}/turns/{turn_id}/regenerate`.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RegenerateTurnBody {
    /// Client-generated identity of the new turn, for acceptance and
    /// ambiguous retries.
    pub new_turn_id: TurnId,
    /// Answer with this model instead of the chat's. The chat keeps its own
    /// model for the turns after this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

/// Body of `POST /chats/{id}/turns/{turn_id}/edit`.
///
/// An absent field keeps what the edited message had, so a client that only
/// changes the text sends only the text.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct EditTurnBody {
    /// Client-generated identity of the new turn.
    pub new_turn_id: TurnId,
    /// The new message.
    pub content: String,
    /// Published image attachment ids, in display order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attachments: Option<Vec<uuid::Uuid>>,
    /// The conversation's document ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file_attachments: Option<Vec<DocumentId>>,
    /// Skills the message explicitly invokes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub invoked_skills: Option<Vec<String>>,
    /// Whether any of the text came from voice transcription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub voice_input_used: Option<bool>,
}

/// What a turn did outside the conversation.
///
/// An edit that would replace a turn with any of these starts a new
/// conversation instead: the original keeps the record of what ran, and what
/// ran is not undone either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnSideEffect {
    /// Wrote or changed files: in a connected folder, or in the
    /// conversation's workspace.
    FilesWritten,
    /// Created or revised outputs.
    OutputsCreated,
    /// Called a connected app or an MCP server.
    ConnectedAppsCalled,
    /// Started background agents or code sessions, controlled an app or a
    /// browser page, or created an app.
    OtherActions,
}

/// Answer of the regenerate and edit routes.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ChatTurnStarted {
    /// The conversation the new turn runs in. An edit that starts a new
    /// conversation answers with that conversation.
    pub chat_id: SessionId,
    /// The new turn.
    pub turn_id: TurnId,
    /// Whether the edit started a new conversation instead of replacing the
    /// turn in place.
    pub branched: bool,
    /// What the replaced turn and its earlier answers did outside the
    /// conversation, which is why an edit started a new conversation. Empty
    /// otherwise.
    pub side_effects: Vec<TurnSideEffect>,
}

/// `POST /chats/{id}/turns/{turn_id}/regenerate` — answer the latest message
/// again.
///
/// The new turn reruns the same message, with the same images, files, and
/// skills, under the chat's model or `model`. The replaced answer stays as an
/// earlier version, unless it failed or was stopped before it said anything.
/// `409` unless `turn_id` is the chat's latest settled turn and was not rerun
/// already, and while another turn runs.
pub async fn post_regenerate_turn(
    State(state): State<AppState>,
    auth: AuthContext,
    store: ScopedStore,
    Path((id, turn_id)): Path<(SessionId, TurnId)>,
    Json(body): Json<RegenerateTurnBody>,
) -> Result<(StatusCode, Json<ChatTurnStarted>), ServerError> {
    let model = match body.model.as_deref() {
        Some(model) => Some(
            validate_model_selection(&state, model, false, Some(&auth.principal.owner_id()))
                .await?,
        ),
        None => None,
    };
    let input = Box::pin(rerun_input(&state, &store, id, turn_id, body.new_turn_id)).await?;
    Box::pin(admit_turn(
        &state,
        &store,
        id,
        &input,
        &TurnSubmission::Replacement {
            replaces: turn_id,
            kind: TurnReplacementKind::Regenerate,
            model,
        },
    ))
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ChatTurnStarted {
            chat_id: id,
            turn_id: body.new_turn_id,
            branched: false,
            side_effects: Vec::new(),
        }),
    ))
}

/// `POST /chats/{id}/turns/{turn_id}/edit` — replace the latest message and
/// answer the new one.
///
/// When the replaced turn only talked, the edit replaces it in place and the
/// old turn leaves the conversation. When it, or an earlier answer to the same
/// message, changed things outside the conversation, the edit starts a new
/// conversation with the history before the turn, sends the new message
/// there, and says so with `branched` and `side_effects`. `409` unless
/// `turn_id` is the chat's latest settled turn and was not rerun already, and
/// while another turn runs.
pub async fn post_edit_turn(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((id, turn_id)): Path<(SessionId, TurnId)>,
    Json(body): Json<EditTurnBody>,
) -> Result<(StatusCode, Json<ChatTurnStarted>), ServerError> {
    let original = Box::pin(rerun_input(&state, &store, id, turn_id, body.new_turn_id)).await?;
    let input = TurnInput {
        turn_id: body.new_turn_id,
        content: body.content,
        attachments: body.attachments.unwrap_or(original.attachments),
        file_attachments: body.file_attachments.unwrap_or(original.file_attachments),
        invoked_skills: body.invoked_skills.unwrap_or(original.invoked_skills),
        voice_input_used: body.voice_input_used.unwrap_or(original.voice_input_used),
    };
    // An ambiguous retry of an edit that already started a new conversation
    // finds its turn there, and answers the same way instead of branching
    // twice.
    if let Some(existing) = store.get_turn(input.turn_id).await? {
        if existing.chat_id != id {
            let listing = store.require_chat_listing(existing.chat_id).await?;
            if listing
                .branched_from
                .as_ref()
                .is_some_and(|origin| origin.chat_id == id)
            {
                return Ok((
                    StatusCode::ACCEPTED,
                    Json(ChatTurnStarted {
                        chat_id: existing.chat_id,
                        turn_id: input.turn_id,
                        branched: true,
                        side_effects: turn_side_effects(&store, id, turn_id).await?,
                    }),
                ));
            }
            return Err(ServerError::conflict(format!(
                "turn {} was already reserved by another chat",
                input.turn_id
            )));
        }
    }

    let side_effects = turn_side_effects(&store, id, turn_id).await?;
    if side_effects.is_empty() {
        Box::pin(admit_turn(
            &state,
            &store,
            id,
            &input,
            &TurnSubmission::Replacement {
                replaces: turn_id,
                kind: TurnReplacementKind::Edit,
                model: None,
            },
        ))
        .await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(ChatTurnStarted {
                chat_id: id,
                turn_id: input.turn_id,
                branched: false,
                side_effects,
            }),
        ));
    }

    // The same checks an in-place replacement gets under the chat lock. A
    // branch copies history and leaves the original alone, so there is no
    // lock to take; a turn that starts meanwhile only means the original
    // moved on.
    require_latest_settled(&store, id, turn_id).await?;
    let source = store.require_chat(id).await?;
    Box::pin(preflight_turn(&state, &source, &input)).await?;
    let (branch, documents) = match store
        .branch_chat(&BranchChat {
            source: id,
            point: ChatBranchPoint::Before(turn_id),
            chat: branch_chat_from(&source),
        })
        .await?
    {
        BranchChatOutcome::Branched { chat_id, documents } => (chat_id, documents),
        BranchChatOutcome::NotFound => {
            return Err(ServerError::not_found(format!("chat {id} not found")))
        }
        BranchChatOutcome::Refused(refusal) => return Err(branch_refused(turn_id, refusal)),
    };
    let input = TurnInput {
        file_attachments: input
            .file_attachments
            .iter()
            .map(|document| documents.get(document).copied().unwrap_or(*document))
            .collect(),
        ..input
    };
    let admitted = Box::pin(admit_turn(
        &state,
        &store,
        branch,
        &input,
        &TurnSubmission::Message { queue: false },
    ))
    .await;
    if let Err(error) = admitted {
        // A branch that never got its message is a copy nobody asked for.
        if let Err(cleanup) = store.delete_chat(branch).await {
            tracing::warn!(
                chat = %branch,
                error = %cleanup,
                "could not remove a branch whose first message was refused"
            );
        }
        state.blob_retirement_wake.notify_one();
        return Err(error);
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(ChatTurnStarted {
            chat_id: branch,
            turn_id: input.turn_id,
            branched: true,
            side_effects,
        }),
    ))
}

/// `POST /chats/{id}/turns/{turn_id}/branch` — start a new conversation with
/// a copy of this one's history through `turn_id`.
///
/// The new conversation is named after the original, links back to it, and
/// keeps its model and settings. It starts no turn. `turn_id` can be any
/// settled turn, including an earlier version of a regenerated answer; the
/// copy then carries that version. `409` for a turn that has not finished or
/// that an edit took out of the conversation.
pub async fn post_branch_turn(
    store: ScopedStore,
    Path((id, turn_id)): Path<(SessionId, TurnId)>,
) -> Result<(StatusCode, Json<ChatListing>), ServerError> {
    let source = store.require_chat(id).await?;
    let branch = match store
        .branch_chat(&BranchChat {
            source: id,
            point: ChatBranchPoint::Through(turn_id),
            chat: branch_chat_from(&source),
        })
        .await?
    {
        BranchChatOutcome::Branched { chat_id, .. } => chat_id,
        BranchChatOutcome::NotFound => {
            return Err(ServerError::not_found(format!("chat {id} not found")))
        }
        BranchChatOutcome::Refused(refusal) => return Err(branch_refused(turn_id, refusal)),
    };
    Ok((
        StatusCode::CREATED,
        Json(store.require_chat_listing(branch).await?),
    ))
}

/// What a turn did outside the conversation, from its durable rows.
///
/// The turn's earlier answers count too. An edit takes every one of them out
/// of the conversation, so one that acted is enough to start a new
/// conversation instead.
///
/// A call the reader declined never ran, so it changed nothing. Every other
/// call counts, including one that failed or was stopped partway: it may have
/// acted before it ended.
pub(crate) async fn turn_side_effects(
    store: &ScopedStore,
    chat_id: SessionId,
    turn_id: TurnId,
) -> Result<Vec<TurnSideEffect>, ServerError> {
    let replacements = store.list_turn_replacements(chat_id).await?;
    let mut turns = HashSet::from([turn_id]);
    let mut current = turn_id;
    while let Some(replacement) = replacements
        .iter()
        .find(|replacement| replacement.turn_id == current)
    {
        if !turns.insert(replacement.replaces) {
            break;
        }
        current = replacement.replaces;
    }

    let mut effects = BTreeSet::new();
    let declined = tidebreak_core::ToolErrorCategory::UserDeclined.as_str();
    for call in store.list_tool_calls(chat_id).await? {
        if !turns.contains(&call.turn_id)
            || call.status == ToolCallStatus::Pending
            || call.error_code.as_deref() == Some(declined)
        {
            continue;
        }
        if let Some(effect) = tool_side_effect(&call.name) {
            effects.insert(effect);
        }
        if let Some(tidebreak_core::ToolResultPreview::Exec { outputs, .. }) = &call.result_preview
        {
            if !outputs.is_empty() {
                effects.insert(TurnSideEffect::OutputsCreated);
            }
        }
    }
    if store
        .list_exec_file_snapshots(chat_id)
        .await?
        .iter()
        .any(|snapshot| turns.contains(&snapshot.turn_id))
    {
        effects.insert(TurnSideEffect::FilesWritten);
    }
    Ok(effects.into_iter().collect())
}

/// What calling `name` does outside the conversation, when its name says.
///
/// `exec` is judged by what it left behind instead: the file-change journal
/// and the outputs its result lists. Reading and searching change nothing.
fn tool_side_effect(name: &str) -> Option<TurnSideEffect> {
    match name {
        "write_file"
        | tidebreak_core::IMPORT_CONNECTED_FILE_TOOL
        | tidebreak_core::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL => {
            Some(TurnSideEffect::FilesWritten)
        }
        name if name.starts_with("mcp__") => Some(TurnSideEffect::ConnectedAppsCalled),
        "create_app"
        | tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL
        | "code_session_create"
        | "code_run_turn"
        | tidebreak_core::BROWSER_ACT_TOOL
        | tidebreak_core::BROWSER_UPLOAD_TOOL => Some(TurnSideEffect::OtherActions),
        name if tidebreak_core::is_computer_use_control_tool(name) => {
            Some(TurnSideEffect::OtherActions)
        }
        _ => None,
    }
}

/// The input `turn_id` was accepted with, under the new turn's identity.
///
/// A turn's opening message is what reruns. Guidance sent while it ran stays
/// with the turn it steered.
async fn rerun_input(
    state: &AppState,
    store: &ScopedStore,
    chat_id: SessionId,
    turn_id: TurnId,
    new_turn_id: TurnId,
) -> Result<TurnInput, ServerError> {
    store.require_chat(chat_id).await?;
    let turn = store
        .get_turn(turn_id)
        .await?
        .filter(|turn| turn.chat_id == chat_id)
        .ok_or_else(|| ServerError::not_found(format!("turn {turn_id} not found")))?;
    let message = state
        .store
        .list_messages(chat_id)
        .await?
        .into_iter()
        .find(|message| message.id == turn.input_message_id)
        .ok_or_else(|| {
            ServerError::conflict_kind(
                "turn_input_missing",
                format!("turn {turn_id} has no message to send again"),
            )
        })?;
    let mut images = store
        .list_message_attachments(chat_id)
        .await?
        .into_iter()
        .filter(|attachment| attachment.message_id == message.id)
        .collect::<Vec<_>>();
    images.sort_by_key(|attachment| attachment.ordinal);
    let mut files = state
        .store
        .list_message_document_attachments(chat_id)
        .await?
        .into_iter()
        .filter(|attachment| attachment.message_id == message.id)
        .collect::<Vec<_>>();
    files.sort_by_key(|attachment| attachment.ordinal);
    Ok(TurnInput {
        turn_id: new_turn_id,
        content: message.content,
        attachments: images
            .into_iter()
            .map(|attachment| attachment.image.blob_id)
            .collect(),
        file_attachments: files
            .into_iter()
            .map(|attachment| attachment.document_id)
            .collect(),
        invoked_skills: turn.invoked_skills,
        voice_input_used: turn.voice_input_used,
    })
}

/// Refuse unless `turn_id` is the chat's latest turn and has finished. A turn
/// that was rerun is followed by the turn that reran it, so it is never the
/// latest.
async fn require_latest_settled(
    store: &ScopedStore,
    chat_id: SessionId,
    turn_id: TurnId,
) -> Result<(), ServerError> {
    use tidebreak_core::TurnReplacementRefusal as Refusal;
    let turns = store.list_turns(chat_id).await?;
    let Some(turn) = turns.iter().find(|turn| turn.id == turn_id) else {
        return Err(super::turn_control::replacement_refused(
            turn_id,
            Refusal::UnknownTurn,
        ));
    };
    if !turn.status.is_terminal() {
        return Err(super::turn_control::replacement_refused(
            turn_id,
            Refusal::Unsettled,
        ));
    }
    if turns.last().is_some_and(|latest| latest.id != turn_id) {
        return Err(super::turn_control::replacement_refused(
            turn_id,
            Refusal::NotLatest,
        ));
    }
    Ok(())
}

/// A new conversation that carries `source`'s settings under a branch name.
fn branch_chat_from(source: &Chat) -> Chat {
    Chat {
        id: SessionId::new(),
        project_id: source.project_id,
        title: branch_title(source.title.as_deref()),
        model: source.model.clone(),
        reasoning_effort: source.reasoning_effort,
        permission_mode: source.permission_mode,
        network_policy: source.network_policy.clone(),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        memory_incognito: source.memory_incognito,
        created_at: Utc::now(),
    }
}

/// The title a branch of a conversation titled `title` gets.
///
/// An untitled original gives an untitled branch, which titling names from
/// its first message like any other conversation.
pub(crate) fn branch_title(title: Option<&str>) -> Option<String> {
    const SUFFIX: &str = " (branch)";
    let title = title?.trim();
    if title.is_empty() {
        return None;
    }
    if title.ends_with(SUFFIX.trim_start()) {
        return Some(title.to_owned());
    }
    let room = crate::chat_titling::MAX_CHAT_TITLE_CHARS - SUFFIX.chars().count();
    let base: String = title.chars().take(room).collect();
    Some(format!("{}{SUFFIX}", base.trim_end()))
}

fn branch_refused(turn_id: TurnId, refusal: ChatBranchRefusal) -> ServerError {
    match refusal {
        ChatBranchRefusal::UnknownTurn => {
            ServerError::not_found(format!("turn {turn_id} not found"))
        }
        ChatBranchRefusal::Unsettled => ServerError::conflict_kind(
            "turn_unsettled",
            format!("turn {turn_id} has not finished; branch once it has"),
        ),
        ChatBranchRefusal::NotInConversation => ServerError::conflict_kind(
            "turn_not_in_conversation",
            format!("turn {turn_id} was edited out of this conversation"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_is_named_after_its_original_and_fits_the_title_limit() {
        assert_eq!(
            branch_title(Some("Trip planning")),
            Some("Trip planning (branch)".to_owned())
        );
        assert_eq!(
            branch_title(Some("Trip planning (branch)")),
            Some("Trip planning (branch)".to_owned())
        );
        assert_eq!(branch_title(Some("   ")), None);
        assert_eq!(branch_title(None), None);
        let long = "x".repeat(crate::chat_titling::MAX_CHAT_TITLE_CHARS);
        let named = branch_title(Some(&long)).unwrap();
        assert_eq!(
            named.chars().count(),
            crate::chat_titling::MAX_CHAT_TITLE_CHARS
        );
        assert!(named.ends_with(" (branch)"));
    }

    #[test]
    fn only_tools_that_act_outside_the_conversation_count_as_side_effects() {
        assert_eq!(
            tool_side_effect("write_file"),
            Some(TurnSideEffect::FilesWritten)
        );
        assert_eq!(
            tool_side_effect("mcp__linear__create_issue"),
            Some(TurnSideEffect::ConnectedAppsCalled)
        );
        assert_eq!(
            tool_side_effect(tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL),
            Some(TurnSideEffect::OtherActions)
        );
        for reading in [
            "web_search",
            "read_document",
            "search",
            "exec",
            "update_task_plan",
        ] {
            assert_eq!(tool_side_effect(reading), None, "{reading}");
        }
    }
}

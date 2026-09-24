//! Rerun and branch a Work chat.
//!
//! Four actions, all on settled turns and all through the same admission path
//! an ordinary message takes:
//!
//! - Retry continues the latest turn after it failed or was stopped. The
//!   retried turn stays in the conversation, so the model sees what it said
//!   and every tool call it made, and does not repeat work blind.
//! - Regenerate answers the latest message again. The earlier answer stays as
//!   a version the reader can page back to.
//! - Edit replaces the latest message and answers the new one.
//! - Branch starts a new conversation with a copy of the history through one
//!   turn, named after the original and linked back to it.
//!
//! When the attempt a regenerate or an edit would replace acted outside the
//! conversation, the rerun starts a new conversation instead, so the original
//! keeps its record of what ran (see [`turn_side_effects`]).
//!
//! A regenerated or edited turn leaves the model's view of the conversation
//! (see `tidebreak_core::TurnPlacements`). Its rows stay.

use std::collections::{BTreeSet, HashMap};

use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use tidebreak_core::{
    BranchChat, BranchChatOutcome, Chat, ChatBranchPoint, ChatBranchRefusal, ChatListing,
    DiscardBranchOutcome, DocumentId, SessionId, TurnId, TurnReplacementKind,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::principal::AuthContext;
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

use super::providers_models::validate_model_selection;
use super::turn_control::{admit_turn, preflight_turn, TurnInput, TurnSubmission};

/// Body of `POST /chats/{id}/turns/{turn_id}/retry`.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct RetryTurnBody {
    /// Client-generated identity of the new turn, for acceptance and
    /// ambiguous retries.
    pub new_turn_id: TurnId,
}

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
/// A regenerate or an edit that would replace a turn with any of these starts
/// a new conversation instead: the original keeps the record of what ran, and
/// what ran is not undone either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnSideEffect {
    /// Wrote or imported files: in a connected folder, or in the
    /// conversation's workspace.
    FilesWritten,
    /// Called a connected app or an MCP server.
    ConnectedAppsCalled,
    /// Ran commands, which can change anything they can reach.
    CommandsRun,
    /// Anything else that is not only reading: changing memory or folder
    /// access, controlling an app or a browser, starting agents or code
    /// sessions, updating the task plan, or creating an app.
    OtherActions,
}

/// Answer of the retry, regenerate, and edit routes.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ChatTurnStarted {
    /// The conversation the new turn runs in. A rerun that starts a new
    /// conversation answers with that conversation.
    pub chat_id: SessionId,
    /// The new turn.
    pub turn_id: TurnId,
    /// Whether the rerun started a new conversation instead of replacing the
    /// turn in place.
    pub branched: bool,
    /// What the replaced turn and the answers before it did outside the
    /// conversation, which is why the rerun started a new conversation. Empty
    /// otherwise.
    pub side_effects: Vec<TurnSideEffect>,
}

/// `POST /chats/{id}/turns/{turn_id}/retry` — continue the latest turn after
/// it failed or was stopped.
///
/// The new turn sends the same message, with the same images, files, and
/// skills, and continues from everything the retried turn said and called:
/// the retried turn stays in the conversation, for the model and in the
/// transcript. `409` unless `turn_id` is the chat's latest settled turn and
/// did not finish, and while another turn runs.
pub async fn post_retry_turn(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((id, turn_id)): Path<(SessionId, TurnId)>,
    Json(body): Json<RetryTurnBody>,
) -> Result<(StatusCode, Json<ChatTurnStarted>), ServerError> {
    let input = Box::pin(rerun_input(&state, &store, id, turn_id, body.new_turn_id)).await?;
    Box::pin(admit_turn(
        &state,
        &store,
        id,
        &input,
        &TurnSubmission::Replacement {
            replaces: turn_id,
            kind: TurnReplacementKind::Retry,
            model: None,
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

/// `POST /chats/{id}/turns/{turn_id}/regenerate` — answer the latest message
/// again.
///
/// The new turn reruns the same message, with the same images, files, and
/// skills, under the chat's model or `model`. The replaced answer stays as an
/// earlier version. When it acted outside the conversation, the regenerate
/// starts a new conversation with the history before it and answers there,
/// and says so with `branched` and `side_effects`. `409` unless `turn_id` is
/// the chat's latest settled turn and was not rerun already, and while
/// another turn runs.
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
    Box::pin(rerun_or_branch(
        &state,
        &store,
        id,
        turn_id,
        input,
        TurnReplacementKind::Regenerate,
        model,
    ))
    .await
}

/// `POST /chats/{id}/turns/{turn_id}/edit` — replace the latest message and
/// answer the new one.
///
/// When the replaced turn only talked, the edit replaces it in place and the
/// old turn leaves the conversation. When it, or an earlier answer to the same
/// message, acted outside the conversation, the edit starts a new
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
    Box::pin(rerun_or_branch(
        &state,
        &store,
        id,
        turn_id,
        input,
        TurnReplacementKind::Edit,
        None,
    ))
    .await
}

/// Replace `turn_id` with `input` in place when its attempt only talked, and
/// otherwise answer `input` in a new conversation with the history before it.
async fn rerun_or_branch(
    state: &AppState,
    store: &ScopedStore,
    id: SessionId,
    turn_id: TurnId,
    input: TurnInput,
    kind: TurnReplacementKind,
    model: Option<String>,
) -> Result<(StatusCode, Json<ChatTurnStarted>), ServerError> {
    // An ambiguous retry of a rerun that already started a new conversation
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
                        side_effects: turn_side_effects(store, id, turn_id).await?,
                    }),
                ));
            }
            return Err(ServerError::conflict(format!(
                "turn {} was already reserved by another chat",
                input.turn_id
            )));
        }
    }

    // What a turn did is only final once it has settled: a running turn can
    // still call a tool after the read, and settle before admission takes
    // the lock. A settled turn never runs again, so this check makes the
    // read below final; admission repeats it under the chat lock.
    require_latest_settled(store, id, turn_id).await?;
    let side_effects = turn_side_effects(store, id, turn_id).await?;
    if side_effects.is_empty() {
        Box::pin(admit_turn(
            state,
            store,
            id,
            &input,
            &TurnSubmission::Replacement {
                replaces: turn_id,
                kind,
                model,
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

    // A branch copies history and leaves the original alone, so there is no
    // lock to take; a turn that starts meanwhile only means the original
    // moved on.
    let source = store.require_chat(id).await?;
    let mut answering = source.clone();
    if let Some(model) = &model {
        answering.model = Some(model.clone());
    }
    Box::pin(preflight_turn(state, &answering, &input)).await?;
    let (branch, documents) = match store
        .branch_chat(&BranchChat {
            source: id,
            point: ChatBranchPoint::Before(turn_id),
            chat: branch_chat_from(&source),
            carry_documents: input.file_attachments.clone(),
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
        state,
        store,
        branch,
        &input,
        &TurnSubmission::Message {
            queue: false,
            model,
        },
    ))
    .await;
    if let Err(error) = admitted {
        discard_refused_branch(state, store, branch).await;
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

/// Remove a branch whose first message was refused: a copy nobody asked for.
///
/// The refusal is what the caller hears about, so a branch that cannot be
/// removed is logged rather than reported in its place.
async fn discard_refused_branch(state: &AppState, store: &ScopedStore, branch: SessionId) {
    match store.discard_branch(branch).await {
        Ok(DiscardBranchOutcome::Discarded) => state.blob_retirement_wake.notify_one(),
        // Someone used it before the refusal came back; it is theirs now.
        Ok(DiscardBranchOutcome::Kept | DiscardBranchOutcome::NotFound) => {}
        Err(error) => tracing::warn!(
            chat = %branch,
            %error,
            "could not remove a branch whose first message was refused"
        ),
    }
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
            carry_documents: Vec::new(),
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

/// What a turn did outside the conversation, from its tool calls.
///
/// The turns it replaced count too: the turns it retried and the answers
/// before it. A regenerate or an edit takes every one of them out of the
/// conversation, so one that acted is enough to start a new conversation
/// instead.
///
/// Every call counts unless the tool only reads (see [`READ_ONLY_TOOLS`]) or
/// the call never ran: the reader declined it, or it named no tool, did not
/// parse, or needed setup first. A call that failed or was stopped partway
/// counts, because it may have acted before it ended.
pub(crate) async fn turn_side_effects(
    store: &ScopedStore,
    chat_id: SessionId,
    turn_id: TurnId,
) -> Result<Vec<TurnSideEffect>, ServerError> {
    let replacements = store.list_turn_replacements(chat_id).await?;
    let replaced_by: HashMap<TurnId, TurnId> = replacements
        .iter()
        .map(|replacement| (replacement.turn_id, replacement.replaces))
        .collect();
    let mut turns = vec![turn_id];
    let mut current = turn_id;
    while let Some(&replaced) = replaced_by.get(&current) {
        if turns.contains(&replaced) {
            break;
        }
        turns.push(replaced);
        current = replaced;
    }

    let never_ran = [
        tidebreak_core::ToolErrorCategory::UserDeclined,
        tidebreak_core::ToolErrorCategory::NotFound,
        tidebreak_core::ToolErrorCategory::InvalidArguments,
        tidebreak_core::ToolErrorCategory::ConfigurationRequired,
    ]
    .map(tidebreak_core::ToolErrorCategory::as_str);
    let mut effects = BTreeSet::new();
    for call in store.list_turn_tool_uses(chat_id, &turns).await? {
        if call
            .error_code
            .as_deref()
            .is_some_and(|code| never_ran.contains(&code))
        {
            continue;
        }
        if let Some(effect) = tool_side_effect(&call.name) {
            effects.insert(effect);
        }
    }
    Ok(effects.into_iter().collect())
}

/// The tools that only read or observe, reviewed one by one. Anything not
/// listed here counts as acting outside the conversation, including every
/// tool added later, until it is reviewed and listed.
///
/// Each one reads the conversation's own sources or workspace, a connected
/// folder, the web, a browser or app it does not drive, or the state of work
/// already running, or asks the reader a question. None of them writes,
/// sends, controls, grants, or starts anything.
const READ_ONLY_TOOLS: &[&str] = &[
    // The conversation's workspace and sources.
    "read_file",
    "list_dir",
    "list_documents",
    "read_document",
    "read_tool_result",
    // Connected folders, read without changing them.
    tidebreak_core::LIST_CONNECTED_FOLDERS_TOOL,
    tidebreak_core::LIST_FOLDER_TOOL,
    tidebreak_core::READ_CONNECTED_FILE_TOOL,
    tidebreak_core::SANDBOX_READ_DELEGATED_FILE_TOOL,
    // The web.
    tidebreak_core::WEB_SEARCH_TOOL,
    tidebreak_core::WEB_EXTRACT_TOOL,
    // Questions for the reader, answered in the conversation.
    tidebreak_core::ASK_USER_QUESTIONS_TOOL,
    // Work already running, observed without steering it.
    tidebreak_core::WAIT_FOR_AGENTS_TOOL,
    tidebreak_core::CODE_WAIT_TOOL,
    "code_repos",
    "code_sessions",
    "conversation_read",
    "conversation_attachment",
    // Browser pages, observed without acting on them.
    tidebreak_core::BROWSER_LIST_TOOL,
    tidebreak_core::BROWSER_SNAPSHOT_TOOL,
    tidebreak_core::BROWSER_SCREENSHOT_TOOL,
    tidebreak_core::BROWSER_WAIT_TOOL,
    tidebreak_core::BROWSER_DIAGNOSTICS_TOOL,
    tidebreak_core::CHROME_LIST_TABS_TOOL,
    tidebreak_core::CHROME_SNAPSHOT_TOOL,
    tidebreak_core::CHROME_SCREENSHOT_TOOL,
    tidebreak_core::CHROME_WAIT_TOOL,
    tidebreak_core::CHROME_DIAGNOSTICS_TOOL,
    tidebreak_core::chrome_connection::CHROME_CONNECTION_STATE_TOOL,
    // Apps on screen, observed without controlling them.
    tidebreak_core::COMPUTER_LIST_WINDOWS_TOOL,
    tidebreak_core::COMPUTER_CAPTURE_SCREEN_TOOL,
    tidebreak_core::COMPUTER_READ_APP_CONTENT_TOOL,
    tidebreak_core::COMPUTER_WAIT_TOOL,
];

/// What calling `name` does outside the conversation, or `None` when the
/// tool only reads.
fn tool_side_effect(name: &str) -> Option<TurnSideEffect> {
    if READ_ONLY_TOOLS.contains(&name) {
        return None;
    }
    Some(match name {
        "write_file"
        | tidebreak_core::IMPORT_CONNECTED_FILE_TOOL
        | tidebreak_core::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL => TurnSideEffect::FilesWritten,
        tidebreak_core::SANDBOX_EXEC_TOOL => TurnSideEffect::CommandsRun,
        name if name.starts_with("mcp__") => TurnSideEffect::ConnectedAppsCalled,
        _ => TurnSideEffect::OtherActions,
    })
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
    fn every_tool_counts_as_acting_unless_it_only_reads() {
        assert_eq!(
            tool_side_effect("write_file"),
            Some(TurnSideEffect::FilesWritten)
        );
        assert_eq!(
            tool_side_effect("mcp__billing__send_invoice"),
            Some(TurnSideEffect::ConnectedAppsCalled)
        );
        assert_eq!(tool_side_effect("exec"), Some(TurnSideEffect::CommandsRun));
        // Tools that act on something other than files, commands, or apps,
        // and a tool nobody has reviewed yet.
        for acting in [
            tidebreak_core::CHROME_ACT_TOOL,
            tidebreak_core::CHROME_CLOSE_TAB_TOOL,
            tidebreak_core::MEMORY_TOOL,
            tidebreak_core::REQUEST_FOLDER_ACCESS_TOOL,
            tidebreak_core::UPDATE_TASK_PLAN_TOOL,
            tidebreak_core::SPAWN_SANDBOX_AGENT_TOOL,
            "a_tool_added_next_year",
        ] {
            assert_eq!(
                tool_side_effect(acting),
                Some(TurnSideEffect::OtherActions),
                "{acting}"
            );
        }
        for reading in [
            "web_search",
            "read_document",
            "list_documents",
            tidebreak_core::CHROME_SNAPSHOT_TOOL,
            tidebreak_core::READ_CONNECTED_FILE_TOOL,
        ] {
            assert_eq!(tool_side_effect(reading), None, "{reading}");
        }
    }
}

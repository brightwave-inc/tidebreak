use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Alias, Expr, ExprTrait, Func, LikeExpr};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

use crate::code::{
    ApprovalDecisionKind, CodeEvent, CodeSessionId, CodeTurnId, RepoId, ToolDetail, WorkspaceId,
};
use crate::error::Result;
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

const ID_QUERY_CHUNK: usize = 400;
const EVENT_CANDIDATE_MULTIPLIER: u64 = 8;
const MAX_PREVIEW_CHARS: usize = 500;
const PREVIEW_CONTEXT_CHARS: usize = 80;

/// Default number of repository transcript matches returned by one request.
pub const DEFAULT_TRANSCRIPT_SEARCH_LIMIT: u32 = 200;
/// Hard cap for one repository transcript-search response.
pub const MAX_TRANSCRIPT_SEARCH_LIMIT: u32 = 500;
/// Longest literal accepted by repository transcript search.
pub const MAX_TRANSCRIPT_SEARCH_QUERY_CHARS: usize = 500;

/// Which stored transcript field produced a repository-history match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeTranscriptSearchSource {
    TurnUserInput,
    TurnNarrative,
    Event,
}

/// One text match from a session in one of a repository's workspaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeTranscriptSearchMatch {
    pub workspace_id: WorkspaceId,
    pub workspace_title: String,
    pub session_id: CodeSessionId,
    pub turn_id: Option<CodeTurnId>,
    pub source: CodeTranscriptSearchSource,
    pub preview: String,
    pub created_at: DateTime<Utc>,
}

/// A bounded repository-wide transcript search page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodeTranscriptSearchPage {
    pub matches: Vec<CodeTranscriptSearchMatch>,
    pub truncated: bool,
}

/// Search user turns and journal events across every workspace in one repo.
///
/// Worktree state is not consulted. Archived and released workspaces remain
/// searchable as long as their durable workspace, session, turn, and event
/// rows remain in the database.
pub async fn search_repo_transcripts(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
    query: &str,
    limit: u64,
) -> Result<CodeTranscriptSearchPage> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Ok(CodeTranscriptSearchPage::default());
    }

    let workspace_rows = entities::code_workspace::Entity::find()
        .select_only()
        .column(entities::code_workspace::Column::Id)
        .column(entities::code_workspace::Column::Title)
        .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workspace::Column::RepoId.eq(repo_id.0))
        .into_tuple::<(uuid::Uuid, String)>()
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    if workspace_rows.is_empty() {
        return Ok(CodeTranscriptSearchPage::default());
    }
    let workspace_titles = workspace_rows
        .into_iter()
        .collect::<HashMap<uuid::Uuid, String>>();
    let workspace_ids = workspace_titles.keys().copied().collect::<Vec<_>>();

    let mut session_workspaces = HashMap::new();
    for workspace_chunk in workspace_ids.chunks(ID_QUERY_CHUNK) {
        let rows = entities::code_session::Entity::find()
            .select_only()
            .column(entities::code_session::Column::Id)
            .column(entities::code_session::Column::WorkspaceId)
            .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
            .filter(
                entities::code_session::Column::WorkspaceId.is_in(workspace_chunk.iter().copied()),
            )
            .into_tuple::<(uuid::Uuid, uuid::Uuid)>()
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        session_workspaces.extend(rows);
    }
    if session_workspaces.is_empty() {
        return Ok(CodeTranscriptSearchPage::default());
    }
    let session_ids = session_workspaces.keys().copied().collect::<Vec<_>>();
    let pattern = literal_like_pattern(query);
    let probe = limit.saturating_add(1);
    let event_probe = limit
        .saturating_mul(EVENT_CANDIDATE_MULTIPLIER)
        .saturating_add(1);
    let mut matches = Vec::new();
    let mut source_truncated = false;

    for session_chunk in session_ids.chunks(ID_QUERY_CHUNK) {
        let turn_condition = Condition::any()
            .add(
                Func::lower(Expr::col(entities::code_turn::Column::UserInput))
                    .like(LikeExpr::new(pattern.clone()).escape('\\')),
            )
            .add(
                Func::lower(Expr::col(entities::code_turn::Column::Narrative))
                    .like(LikeExpr::new(pattern.clone()).escape('\\')),
            );
        let turns = entities::code_turn::Entity::find()
            .filter(entities::code_turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_turn::Column::SessionId.is_in(session_chunk.iter().copied()))
            .filter(turn_condition)
            .order_by_desc(entities::code_turn::Column::StartedAt)
            .limit(probe)
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        source_truncated |= turns.len() as u64 >= probe;
        for turn in turns {
            let (source, text) = if matching_excerpt(&turn.user_input, query).is_some() {
                (
                    CodeTranscriptSearchSource::TurnUserInput,
                    turn.user_input.as_str(),
                )
            } else if let Some(narrative) = turn.narrative.as_deref() {
                (CodeTranscriptSearchSource::TurnNarrative, narrative)
            } else {
                continue;
            };
            let Some(preview) = matching_excerpt(text, query) else {
                continue;
            };
            push_match(
                &mut matches,
                &workspace_titles,
                &session_workspaces,
                turn.session_id,
                Some(turn.id),
                source,
                preview,
                turn.started_at,
            );
        }

        let event_text = Expr::col(entities::code_event::Column::Event).cast_as(Alias::new("text"));
        let events = entities::code_event::Entity::find()
            .filter(entities::code_event::Column::Owner.eq(owner.as_str()))
            .filter(entities::code_event::Column::SessionId.is_in(session_chunk.iter().copied()))
            .filter(Func::lower(event_text).like(LikeExpr::new(pattern.clone()).escape('\\')))
            .order_by_desc(entities::code_event::Column::CreatedAt)
            .limit(event_probe)
            .all(&store.conn)
            .await
            .map_err(store_err)?;
        source_truncated |= events.len() as u64 >= event_probe;
        for row in events {
            let event: CodeEvent = serde_json::from_value(row.event)?;
            let Some(preview) = event_search_text(&event)
                .into_iter()
                .find_map(|text| matching_excerpt(text, query))
            else {
                continue;
            };
            push_match(
                &mut matches,
                &workspace_titles,
                &session_workspaces,
                row.session_id,
                None,
                CodeTranscriptSearchSource::Event,
                preview,
                row.created_at,
            );
        }
    }

    matches.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.workspace_id.0.cmp(&right.workspace_id.0))
            .then_with(|| left.session_id.0.cmp(&right.session_id.0))
            .then_with(|| {
                left.turn_id
                    .map(|id| id.0)
                    .cmp(&right.turn_id.map(|id| id.0))
            })
    });
    let truncated = source_truncated || matches.len() as u64 > limit;
    matches.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(CodeTranscriptSearchPage { matches, truncated })
}

#[allow(clippy::too_many_arguments)]
fn push_match(
    matches: &mut Vec<CodeTranscriptSearchMatch>,
    workspace_titles: &HashMap<uuid::Uuid, String>,
    session_workspaces: &HashMap<uuid::Uuid, uuid::Uuid>,
    session_id: uuid::Uuid,
    turn_id: Option<uuid::Uuid>,
    source: CodeTranscriptSearchSource,
    preview: String,
    created_at: DateTime<Utc>,
) {
    let Some(workspace_id) = session_workspaces.get(&session_id).copied() else {
        return;
    };
    let Some(workspace_title) = workspace_titles.get(&workspace_id) else {
        return;
    };
    matches.push(CodeTranscriptSearchMatch {
        workspace_id: WorkspaceId(workspace_id),
        workspace_title: workspace_title.clone(),
        session_id: CodeSessionId(session_id),
        turn_id: turn_id.map(CodeTurnId),
        source,
        preview,
        created_at,
    });
}

fn literal_like_pattern(query: &str) -> String {
    let escaped = query
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn matching_excerpt(text: &str, query: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (matched_start, _) = case_insensitive_char_range(&compact, query)?;
    let chars = compact.chars().collect::<Vec<_>>();
    let start = matched_start.saturating_sub(PREVIEW_CONTEXT_CHARS);
    let end = (start + MAX_PREVIEW_CHARS).min(chars.len());
    let mut preview = chars[start..end].iter().collect::<String>();
    if start > 0 {
        preview.insert(0, '…');
    }
    if end < chars.len() {
        preview.push('…');
    }
    Some(preview)
}

fn case_insensitive_char_range(text: &str, query: &str) -> Option<(usize, usize)> {
    let needle = query
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<Vec<_>>();
    if needle.is_empty() {
        return None;
    }
    let folded = text
        .chars()
        .enumerate()
        .flat_map(|(index, ch)| ch.to_lowercase().map(move |lower| (lower, index)))
        .collect::<Vec<_>>();
    let at = folded
        .windows(needle.len())
        .position(|window| window.iter().map(|(ch, _)| *ch).eq(needle.iter().copied()))?;
    let start = folded[at].1;
    let end = folded[at + needle.len() - 1].1 + 1;
    Some((start, end))
}

fn event_search_text(event: &CodeEvent) -> Vec<&str> {
    match event {
        CodeEvent::AssistantDelta { text }
        | CodeEvent::AssistantMessage { text, .. }
        | CodeEvent::ReasoningDelta { text }
        | CodeEvent::UserSteered { text } => vec![text],
        CodeEvent::ToolStarted { name, detail, .. } => {
            let mut text = vec![name.as_str()];
            text.extend(tool_detail_text(detail));
            text
        }
        CodeEvent::ToolCompleted {
            preview, detail, ..
        } => {
            let mut text = vec![preview.as_str()];
            if let Some(detail) = detail {
                text.extend(tool_detail_text(detail));
            }
            text
        }
        CodeEvent::FileChanged { path, .. } => vec![path],
        CodeEvent::ApprovalResolved {
            decision:
                ApprovalDecisionKind::Deny {
                    feedback: Some(text),
                },
            ..
        } => vec![text],
        CodeEvent::TurnFailed { error } => vec![&error.message],
        CodeEvent::HarnessNotice { message, .. } => vec![message],
        _ => Vec::new(),
    }
}

fn tool_detail_text(detail: &ToolDetail) -> Vec<&str> {
    match detail {
        ToolDetail::Command { cmd, cwd } => vec![cmd, cwd],
        ToolDetail::FileEdit { path } | ToolDetail::FileRead { path } => vec![path],
        ToolDetail::Search { query } => vec![query],
        ToolDetail::Other { summary } => vec![summary],
    }
}

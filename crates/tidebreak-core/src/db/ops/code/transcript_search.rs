use chrono::{DateTime, Utc};

use crate::code::{RepoId, SessionId, TurnId, WorkspaceId};
use crate::error::Result;
use crate::message_search::{MessageSearchSource, SearchTerms};
use crate::OwnerId;

use super::super::super::DbStore;
use super::super::message_search::{matching_rows, row_texts, SearchScope};

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
    /// A turn's recap. The message index does not cover recaps, which restate
    /// what the turn's own messages say, so no search produces this now; it
    /// stays for the wire the Archive page reads.
    TurnNarrative,
    Event,
}

/// One text match from a session in one of a repository's workspaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeTranscriptSearchMatch {
    pub workspace_id: WorkspaceId,
    pub workspace_title: String,
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
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

/// Search what was said across every workspace in one repository: each
/// turn's input, the assistant's messages, steers, and what tool calls acted
/// on.
///
/// The search runs on the message index, so it matches words in what the
/// transcript shows, never the keys of a stored journal row, and reads the
/// index rather than every row the repository ever journaled. Worktree state
/// is not consulted: archived and released workspaces stay searchable as long
/// as their sessions exist. Every word must appear, and the last one also
/// matches as a prefix.
pub async fn search_repo_transcripts(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
    query: &str,
    limit: u64,
) -> Result<CodeTranscriptSearchPage> {
    let terms = SearchTerms::parse(query.trim());
    if terms.is_empty() || limit == 0 {
        return Ok(CodeTranscriptSearchPage::default());
    }
    let mut rows = matching_rows(
        &store.conn,
        owner,
        &terms,
        SearchScope::Repo(repo_id),
        None,
        limit.saturating_add(1),
    )
    .await?;
    let truncated = rows.len() as u64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let texts = row_texts(&store.conn, &rows).await?;
    let mut matches = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(text), Some(workspace_id), Some(workspace_title)) = (
            texts.get(&row.id),
            row.workspace_id,
            row.workspace_title.clone(),
        ) else {
            continue;
        };
        // A turn's own input is the one source the archive names as input;
        // messages, steers, and tool calls read as journal activity.
        let from_input = row.source == MessageSearchSource::User
            && (row.source_key.starts_with("input:") || row.message_id.is_some());
        matches.push(CodeTranscriptSearchMatch {
            workspace_id: WorkspaceId(workspace_id),
            workspace_title,
            session_id: SessionId(row.session_id),
            turn_id: row.turn_id.map(TurnId),
            source: if from_input {
                CodeTranscriptSearchSource::TurnUserInput
            } else {
                CodeTranscriptSearchSource::Event
            },
            preview: terms
                .excerpt(&text.text, MAX_PREVIEW_CHARS, PREVIEW_CONTEXT_CHARS)
                .text,
            created_at: row.created_at(),
        });
    }
    Ok(CodeTranscriptSearchPage { matches, truncated })
}

//! `GET /search/messages`: full-text search over what was said in the caller's
//! own Work chats and code sessions.

use serde::Deserialize;

use tidebreak_core::message_search::{
    MessageSearchCursor, MessageSearchPage, MessageSearchRequest, DEFAULT_SEARCH_LIMIT,
    MAX_QUERY_CHARS, MAX_SEARCH_LIMIT,
};

use crate::error::ServerError;
use crate::extract::{Json, Query};
use crate::scoped_store::ScopedStore;

/// Query of `GET /search/messages`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageSearchQuery {
    /// The words to find. Read as literal words: punctuation separates them
    /// and is never an operator. Every word must match a word, or a part of a
    /// camelCase, `snake_case`, or `kebab-case` name, and the last one also
    /// matches as a prefix.
    pub q: String,
    /// Most hits to answer, from 1 to 50. Defaults to 20.
    #[serde(default)]
    pub limit: Option<u32>,
    /// The `next_cursor` of the page before, to read the page after it.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// `GET /search/messages?q=&limit=&cursor=` — search the caller's own Work
/// chats and code sessions, newest match first.
///
/// A hit is a message the person sent, text the assistant wrote, or what a
/// code session's tool call acted on. It names the conversation and the turn,
/// message, or journal event to open, and carries a plain-text snippet with
/// the matched words as ranges. Conversations the caller does not own, ones
/// with memory incognito on, and answers a regenerate or an edit replaced are
/// never hits. `indexing` says whether older conversations are still being
/// added to the index, and how many could not be added.
pub async fn search_messages(
    store: ScopedStore,
    Query(query): Query<MessageSearchQuery>,
) -> Result<Json<MessageSearchPage>, ServerError> {
    let words = query.q.trim();
    if words.is_empty() {
        return Err(ServerError::bad_request(
            "q must hold the words to search for",
        ));
    }
    if words.chars().count() > MAX_QUERY_CHARS {
        return Err(ServerError::bad_request(format!(
            "q must be at most {MAX_QUERY_CHARS} characters"
        )));
    }
    let limit = query.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(ServerError::bad_request(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}"
        )));
    }
    let cursor = match query.cursor.as_deref() {
        None => None,
        Some(cursor) => Some(MessageSearchCursor::decode(cursor).ok_or_else(|| {
            ServerError::bad_request("cursor must be a next_cursor this search returned")
        })?),
    };
    let page = store
        .search_messages(&MessageSearchRequest {
            query: words.to_owned(),
            limit,
            cursor,
        })
        .await?;
    Ok(Json(page))
}

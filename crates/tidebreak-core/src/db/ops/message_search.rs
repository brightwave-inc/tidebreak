//! Reads and writes on the message index.
//!
//! The index's shape is described with its migration,
//! `m20260924_000004_message_search`; how text becomes terms is
//! [`crate::message_search`]. This module decides what goes in:
//!
//! - A session on the internal engine (a Work chat, or a code session that
//!   engine drives) is indexed from its `message` rows: what the person sent
//!   and what the assistant answered. A retry's copy of the question is left
//!   out, because the transcript shows the question once. A turn a regenerate
//!   or an edit took out of the conversation leaves the index when the rerun
//!   is accepted: an earlier version of an answer is not a hit.
//! - A session on any other engine has no `message` rows. It is indexed from
//!   each turn's input and from its journal: the assistant's messages, the
//!   person's steers, and what each tool call acted on.
//! - A conversation with memory incognito on is not indexed at all. Turning
//!   incognito on removes it; turning it off adds its history back.
//!
//! Every write runs in the transaction that writes the message, turn, or
//! event it indexes, so a piece of a conversation is searchable exactly when
//! it is committed. Streamed deltas are never journaled, so they are never
//! indexed either.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait, Value,
};
use serde::Deserialize;

use crate::code::{Event, HarnessKind, RepoId};
use crate::error::{AgentError, Result};
use crate::id::{MessageId, SessionId, TurnId};
use crate::message_search::{
    code_event_text, index_terms, MessageSearchCursor, MessageSearchHit, MessageSearchIndexing,
    MessageSearchKind, MessageSearchPage, MessageSearchRequest, MessageSearchSource, SearchTerms,
    INDEXED_EVENT_TYPES, MAX_SEARCH_LIMIT, SNIPPET_CHARS, SNIPPET_LEAD_CHARS,
};
use crate::model::{TurnPlacements, TurnReplacementKind};
use crate::OwnerId;

use super::super::{entities, store_err, DbStore};

/// Most rows one insert statement carries.
const INSERT_CHUNK: usize = 64;
/// Most source rows one rebuild reads at a time.
const REBUILD_PAGE: u64 = 500;
/// Most ids one `IN` list carries.
const ID_CHUNK: usize = 400;

/// One piece of a conversation, ready to index.
#[derive(Debug, Clone)]
struct Piece {
    source_key: String,
    source: MessageSearchSource,
    turn_id: Option<uuid::Uuid>,
    message_id: Option<uuid::Uuid>,
    event_seq: Option<i64>,
    created_at_micros: i64,
    terms: String,
}

fn message_key(id: uuid::Uuid) -> String {
    format!("message:{id}")
}

fn input_key(turn: uuid::Uuid) -> String {
    format!("input:{turn}")
}

fn event_key(seq: i64) -> String {
    format!("event:{seq}")
}

fn call_key(call_id: &str) -> String {
    format!("call:{call_id}")
}

fn micros(at: DateTime<Utc>) -> i64 {
    at.timestamp_micros()
}

fn placeholder(backend: DbBackend, number: usize) -> String {
    match backend {
        DbBackend::Postgres => format!("${number}"),
        _ => "?".to_owned(),
    }
}

/// What indexing needs to know about a session.
struct SessionFacts {
    owner: String,
    /// The internal engine writes `message` rows; every other engine does not.
    internal: bool,
    incognito: bool,
}

async fn session_facts_on<C>(conn: &C, session_id: uuid::Uuid) -> Result<Option<SessionFacts>>
where
    C: ConnectionTrait,
{
    Ok(entities::session::Entity::find_by_id(session_id)
        .select_only()
        .column(entities::session::Column::Owner)
        .column(entities::session::Column::HarnessKind)
        .column(entities::session::Column::MemoryIncognito)
        .into_tuple::<(String, String, bool)>()
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|(owner, harness_kind, incognito)| SessionFacts {
            owner,
            internal: harness_kind == HarnessKind::Internal.as_str(),
            incognito,
        }))
}

/// Insert `pieces`, skipping any the index already holds.
async fn insert_pieces_on<C>(
    conn: &C,
    owner: &str,
    session_id: uuid::Uuid,
    pieces: &[Piece],
) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    for chunk in pieces.chunks(INSERT_CHUNK) {
        let mut sql = String::from(
            "INSERT INTO \"message_search\" (\"owner\", \"session_id\", \"source_key\", \
             \"source\", \"turn_id\", \"message_id\", \"event_seq\", \"created_at_micros\"",
        );
        if backend == DbBackend::Postgres {
            sql.push_str(", \"search_vector\"");
        }
        sql.push_str(") VALUES ");
        let mut values: Vec<Value> = Vec::with_capacity(chunk.len() * 9);
        for (index, piece) in chunk.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            let row: [Value; 8] = [
                owner.into(),
                session_id.into(),
                piece.source_key.clone().into(),
                piece.source.as_str().into(),
                piece.turn_id.into(),
                piece.message_id.into(),
                piece.event_seq.into(),
                piece.created_at_micros.into(),
            ];
            sql.push('(');
            for (column, value) in row.into_iter().enumerate() {
                if column > 0 {
                    sql.push_str(", ");
                }
                values.push(value);
                sql.push_str(&placeholder(backend, values.len()));
            }
            if backend == DbBackend::Postgres {
                values.push(piece.terms.clone().into());
                sql.push_str(&format!(
                    ", CAST({} AS tsvector)",
                    placeholder(backend, values.len())
                ));
            }
            sql.push(')');
        }
        sql.push_str(" ON CONFLICT (\"session_id\", \"source_key\") DO NOTHING");
        if backend != DbBackend::Sqlite {
            conn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
                .await
                .map_err(store_err)?;
            continue;
        }
        // SQLite keeps the terms in the FTS5 table under the row's id. Only
        // rows this statement inserted come back, so a piece already indexed
        // keeps the terms it has.
        sql.push_str(" RETURNING \"id\", \"source_key\"");
        let inserted = conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
            .await
            .map_err(store_err)?;
        if inserted.is_empty() {
            continue;
        }
        let terms: HashMap<&str, &str> = chunk
            .iter()
            .map(|piece| (piece.source_key.as_str(), piece.terms.as_str()))
            .collect();
        let mut fts = String::from("INSERT INTO \"message_search_fts\" (rowid, \"terms\") VALUES ");
        let mut fts_values: Vec<Value> = Vec::with_capacity(inserted.len() * 2);
        for (index, row) in inserted.iter().enumerate() {
            let id: i64 = row.try_get("", "id").map_err(store_err)?;
            let key: String = row.try_get("", "source_key").map_err(store_err)?;
            let Some(row_terms) = terms.get(key.as_str()) else {
                return Err(AgentError::Store(format!(
                    "the message index returned a row it was not given: {key}"
                )));
            };
            if index > 0 {
                fts.push_str(", ");
            }
            fts.push_str("(?, ?)");
            fts_values.push(id.into());
            fts_values.push((*row_terms).into());
        }
        conn.execute_raw(Statement::from_sql_and_values(backend, fts, fts_values))
            .await
            .map_err(store_err)?;
    }
    Ok(())
}

/// Remove the piece `source_key` names, if the index holds it.
async fn delete_piece_on<C>(conn: &C, session_id: uuid::Uuid, source_key: &str) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        format!(
            "DELETE FROM \"message_search\" WHERE \"session_id\" = {} AND \"source_key\" = {}",
            placeholder(backend, 1),
            placeholder(backend, 2)
        ),
        [session_id.into(), source_key.into()],
    ))
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Remove everything the index holds for one session.
pub(in crate::db) async fn clear_session_on<C>(conn: &C, session_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        format!(
            "DELETE FROM \"message_search\" WHERE \"session_id\" = {}",
            placeholder(backend, 1)
        ),
        [session_id.0.into()],
    ))
    .await
    .map_err(store_err)?;
    Ok(())
}

/// The turns of `session_id` that a regenerate or an edit took out of the
/// conversation.
async fn turns_outside_on<C>(conn: &C, session_id: SessionId) -> Result<HashSet<TurnId>>
where
    C: ConnectionTrait,
{
    let replacements = super::turn::list_turn_replacements_on(conn, session_id).await?;
    Ok(TurnPlacements::new(&replacements).outside_conversation())
}

/// Index one chat message, just written.
///
/// Only what the transcript shows is indexed: the person's messages and the
/// assistant's, on an internal-engine session without memory incognito, in a
/// turn that is still part of the conversation. A retry's copy of the
/// question is skipped, because the transcript shows the question once.
pub(in crate::db) async fn index_chat_message_on<C>(conn: &C, message_id: MessageId) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(message) = entities::message::Entity::find_by_id(message_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(());
    };
    let source = match message.role.as_str() {
        "user" => MessageSearchSource::User,
        "assistant" => MessageSearchSource::Assistant,
        _ => return Ok(()),
    };
    let terms = index_terms(&message.content);
    if terms.is_empty() {
        return Ok(());
    }
    let Some(facts) = session_facts_on(conn, message.chat_id).await? else {
        return Ok(());
    };
    if !facts.internal || facts.incognito {
        return Ok(());
    }
    if source == MessageSearchSource::User {
        let retried_copy = entities::turn::Entity::find_by_id(message.turn_id)
            .one(conn)
            .await
            .map_err(store_err)?
            .is_some_and(|turn| {
                turn.input_message_id == Some(message.id)
                    && turn.replacement.as_deref() == Some(TurnReplacementKind::Retry.as_str())
            });
        if retried_copy {
            return Ok(());
        }
    }
    if turns_outside_on(conn, SessionId(message.chat_id))
        .await?
        .contains(&TurnId(message.turn_id))
    {
        return Ok(());
    }
    insert_pieces_on(
        conn,
        &facts.owner,
        message.chat_id,
        &[Piece {
            source_key: message_key(message.id),
            source,
            turn_id: Some(message.turn_id),
            message_id: Some(message.id),
            event_seq: None,
            created_at_micros: micros(message.created_at),
            terms,
        }],
    )
    .await
}

/// Take the turns a regenerate or an edit replaced out of the index. Run in
/// the transaction that accepts the rerun.
pub(in crate::db) async fn drop_replaced_turns_on<C>(conn: &C, session_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let outside: Vec<uuid::Uuid> = turns_outside_on(conn, session_id)
        .await?
        .into_iter()
        .map(|turn| turn.0)
        .collect();
    let backend = conn.get_database_backend();
    for chunk in outside.chunks(ID_CHUNK) {
        let mut values: Vec<Value> = vec![session_id.0.into()];
        let mut list = Vec::with_capacity(chunk.len());
        for turn in chunk {
            values.push((*turn).into());
            list.push(placeholder(backend, values.len()));
        }
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "DELETE FROM \"message_search\" WHERE \"session_id\" = {} AND \"turn_id\" IN ({})",
                placeholder(backend, 1),
                list.join(", ")
            ),
            values,
        ))
        .await
        .map_err(store_err)?;
    }
    Ok(())
}

/// Index a code turn's input, on a session whose engine keeps no `message`
/// rows. `replace` drops what the index held for the turn first, for a
/// write that may have changed the input.
pub(in crate::db) async fn index_code_turn_input_on<C>(
    conn: &C,
    session_id: SessionId,
    turn_id: TurnId,
    user_input: &str,
    started_at: DateTime<Utc>,
    replace: bool,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(facts) = session_facts_on(conn, session_id.0).await? else {
        return Ok(());
    };
    if facts.internal || facts.incognito {
        return Ok(());
    }
    let key = input_key(turn_id.0);
    if replace {
        delete_piece_on(conn, session_id.0, &key).await?;
    }
    let terms = index_terms(user_input);
    if terms.is_empty() {
        return Ok(());
    }
    insert_pieces_on(
        conn,
        &facts.owner,
        session_id.0,
        &[Piece {
            source_key: key,
            source: MessageSearchSource::User,
            turn_id: Some(turn_id.0),
            message_id: None,
            event_seq: None,
            created_at_micros: micros(started_at),
            terms,
        }],
    )
    .await
}

/// Whether a stored journal event is one that can carry searchable text,
/// read without decoding the rest of it.
fn indexed_event_type(event: &serde_json::Value) -> bool {
    event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| INDEXED_EVENT_TYPES.contains(&kind))
}

/// Index the searchable events among `events`, just journaled for one
/// session: `(seq, event, created_at)` in sequence order.
///
/// A tool call's piece is replaced by each later event that restates what it
/// acted on, so the index holds the call's final arguments once.
pub(in crate::db) async fn index_code_events_on<C>(
    conn: &C,
    session_id: SessionId,
    events: &[(i64, &serde_json::Value, DateTime<Utc>)],
) -> Result<()>
where
    C: ConnectionTrait,
{
    let decoded: Vec<(i64, Event, DateTime<Utc>)> = events
        .iter()
        .filter(|(_, event, _)| indexed_event_type(event))
        .filter_map(|(seq, event, at)| {
            Event::deserialize(*event)
                .ok()
                .map(|event| (*seq, event, *at))
        })
        .collect();
    if decoded.is_empty() {
        return Ok(());
    }
    let Some(facts) = session_facts_on(conn, session_id.0).await? else {
        return Ok(());
    };
    if facts.internal || facts.incognito {
        return Ok(());
    }
    for (seq, event, at) in &decoded {
        let Some(piece) = event_piece(*seq, event, *at) else {
            continue;
        };
        if piece.source_key.starts_with("call:") {
            delete_piece_on(conn, session_id.0, &piece.source_key).await?;
        }
        insert_pieces_on(conn, &facts.owner, session_id.0, &[piece]).await?;
    }
    Ok(())
}

/// The piece one journal event puts in the index, if any.
fn event_piece(seq: i64, event: &Event, at: DateTime<Utc>) -> Option<Piece> {
    let text = code_event_text(event)?;
    let terms = index_terms(text.text);
    if terms.is_empty() {
        return None;
    }
    Some(Piece {
        source_key: text.call_id.map_or_else(|| event_key(seq), call_key),
        source: text.source,
        turn_id: None,
        message_id: None,
        event_seq: Some(seq),
        created_at_micros: micros(at),
        terms,
    })
}

/// Rebuild one session's part of the index from its rows.
///
/// Used for history the index never saw: sessions that predate it, a
/// conversation leaving memory incognito, and a branch's copied history.
pub(in crate::db) async fn rebuild_session_on<C>(conn: &C, session_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    clear_session_on(conn, session_id).await?;
    let Some(facts) = session_facts_on(conn, session_id.0).await? else {
        return Ok(());
    };
    if facts.incognito {
        return Ok(());
    }
    let pieces = if facts.internal {
        chat_pieces_on(conn, session_id).await?
    } else {
        code_pieces_on(conn, session_id).await?
    };
    insert_pieces_on(conn, &facts.owner, session_id.0, &pieces).await
}

/// Every piece of an internal-engine session the transcript shows.
async fn chat_pieces_on<C>(conn: &C, session_id: SessionId) -> Result<Vec<Piece>>
where
    C: ConnectionTrait,
{
    let outside = turns_outside_on(conn, session_id).await?;
    let retried_copies: HashSet<uuid::Uuid> = entities::turn::Entity::find()
        .select_only()
        .column(entities::turn::Column::InputMessageId)
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .filter(entities::turn::Column::Replacement.eq(TurnReplacementKind::Retry.as_str()))
        .into_tuple::<Option<uuid::Uuid>>()
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .flatten()
        .collect();
    let mut pieces = Vec::new();
    let mut after = 0_i64;
    loop {
        let page = entities::message::Entity::find()
            .filter(entities::message::Column::ChatId.eq(session_id.0))
            .filter(entities::message::Column::Seq.gt(after))
            .filter(entities::message::Column::Role.is_in(["user", "assistant"]))
            .order_by_asc(entities::message::Column::Seq)
            .limit(REBUILD_PAGE)
            .all(conn)
            .await
            .map_err(store_err)?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.seq;
        let full = page.len() as u64 >= REBUILD_PAGE;
        for message in page {
            if outside.contains(&TurnId(message.turn_id)) || retried_copies.contains(&message.id)
            {
                continue;
            }
            let terms = index_terms(&message.content);
            if terms.is_empty() {
                continue;
            }
            pieces.push(Piece {
                source_key: message_key(message.id),
                source: if message.role == "user" {
                    MessageSearchSource::User
                } else {
                    MessageSearchSource::Assistant
                },
                turn_id: Some(message.turn_id),
                message_id: Some(message.id),
                event_seq: None,
                created_at_micros: micros(message.created_at),
                terms,
            });
        }
        if !full {
            break;
        }
    }
    Ok(pieces)
}

/// The SQL that keeps only the journal events that can carry searchable
/// text, so a rebuild never reads a delta or a tool's output.
fn indexed_event_filter(backend: DbBackend) -> String {
    let types = INDEXED_EVENT_TYPES
        .iter()
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ");
    match backend {
        DbBackend::Postgres => format!("\"event\".\"event\"->>'type' IN ({types})"),
        _ => format!("json_extract(\"event\".\"event\", '$.type') IN ({types})"),
    }
}

/// Every piece of a session on an engine that keeps no `message` rows: each
/// turn's input and the searchable events in its journal.
async fn code_pieces_on<C>(conn: &C, session_id: SessionId) -> Result<Vec<Piece>>
where
    C: ConnectionTrait,
{
    let mut pieces = Vec::new();
    let turns = entities::turn::Entity::find()
        .select_only()
        .column(entities::turn::Column::Id)
        .column(entities::turn::Column::UserInput)
        .column(entities::turn::Column::StartedAt)
        .filter(entities::turn::Column::SessionId.eq(session_id.0))
        .order_by_asc(entities::turn::Column::Ordinal)
        .into_tuple::<(uuid::Uuid, String, DateTime<Utc>)>()
        .all(conn)
        .await
        .map_err(store_err)?;
    for (turn_id, user_input, started_at) in turns {
        let terms = index_terms(&user_input);
        if terms.is_empty() {
            continue;
        }
        pieces.push(Piece {
            source_key: input_key(turn_id),
            source: MessageSearchSource::User,
            turn_id: Some(turn_id),
            message_id: None,
            event_seq: None,
            created_at_micros: micros(started_at),
            terms,
        });
    }
    let filter = indexed_event_filter(conn.get_database_backend());
    let mut calls: HashMap<String, usize> = HashMap::new();
    let mut after = 0_i64;
    loop {
        let page = entities::event::Entity::find()
            .select_only()
            .column(entities::event::Column::Seq)
            .column(entities::event::Column::Event)
            .column(entities::event::Column::CreatedAt)
            .filter(entities::event::Column::SessionId.eq(session_id.0))
            .filter(entities::event::Column::Seq.gt(after))
            .filter(Expr::cust(filter.clone()))
            .order_by_asc(entities::event::Column::Seq)
            .limit(REBUILD_PAGE)
            .into_tuple::<(i64, serde_json::Value, DateTime<Utc>)>()
            .all(conn)
            .await
            .map_err(store_err)?;
        let Some((last, _, _)) = page.last() else {
            break;
        };
        after = *last;
        let full = page.len() as u64 >= REBUILD_PAGE;
        for (seq, value, at) in page {
            let Ok(event) = Event::deserialize(&value) else {
                continue;
            };
            let Some(piece) = event_piece(seq, &event, at) else {
                continue;
            };
            if piece.source_key.starts_with("call:") {
                // A call's later restatement replaces what its start said.
                if let Some(&index) = calls.get(&piece.source_key) {
                    pieces[index] = piece;
                    continue;
                }
                calls.insert(piece.source_key.clone(), pieces.len());
            }
            pieces.push(piece);
        }
        if !full {
            break;
        }
    }
    Ok(pieces)
}

/// Add the history of up to `sessions` conversations that predate the index,
/// newest activity first, and answer how many are still waiting.
///
/// Each session is rebuilt in its own transaction under the session's write
/// lock, so a message committed meanwhile is indexed once either way.
pub(in crate::db) async fn backfill(store: &DbStore, sessions: u64) -> Result<u64> {
    let backend = store.conn.get_database_backend();
    let waiting = store
        .conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT \"message_search_backfill\".\"session_id\" AS \"session_id\" \
                 FROM \"message_search_backfill\" \
                 JOIN \"session\" ON \"session\".\"id\" = \"message_search_backfill\".\"session_id\" \
                 ORDER BY COALESCE(\"session\".\"last_activity_at\", \"session\".\"created_at\") DESC, \
                 \"session\".\"id\" \
                 LIMIT {}",
                placeholder(backend, 1)
            ),
            [i64::try_from(sessions).unwrap_or(i64::MAX).into()],
        ))
        .await
        .map_err(store_err)?;
    let done = |session_id: uuid::Uuid| {
        Statement::from_sql_and_values(
            backend,
            format!(
                "DELETE FROM \"message_search_backfill\" WHERE \"session_id\" = {}",
                placeholder(backend, 1)
            ),
            [session_id.into()],
        )
    };
    for row in waiting {
        let session_id: uuid::Uuid = row.try_get("", "session_id").map_err(store_err)?;
        let transaction = store.conn.begin().await.map_err(store_err)?;
        let rebuilt = async {
            if super::acquire_session_write_lock(&transaction, session_id).await? {
                rebuild_session_on(&transaction, SessionId(session_id)).await?;
            }
            transaction
                .execute_raw(done(session_id))
                .await
                .map_err(store_err)?;
            Ok::<(), AgentError>(())
        }
        .await;
        match rebuilt {
            Ok(()) => transaction.commit().await.map_err(store_err)?,
            Err(error) => {
                // One session that cannot be rebuilt must not hold back every
                // other. Its new messages are still indexed as they land.
                transaction.rollback().await.map_err(store_err)?;
                tracing::warn!(
                    session = %session_id,
                    %error,
                    "could not add a conversation's history to the message index"
                );
                store
                    .conn
                    .execute_raw(done(session_id))
                    .await
                    .map_err(store_err)?;
            }
        }
    }
    let remaining = store
        .conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) AS \"waiting\" FROM \"message_search_backfill\"",
        ))
        .await
        .map_err(store_err)?
        .map(|row| row.try_get::<i64>("", "waiting"))
        .transpose()
        .map_err(store_err)?
        .unwrap_or_default();
    Ok(u64::try_from(remaining).unwrap_or_default())
}

/// How many of `owner`'s conversations the backfill has not reached.
async fn pending_for_owner(store: &DbStore, owner: &OwnerId) -> Result<u64> {
    let backend = store.conn.get_database_backend();
    let waiting = store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT COUNT(*) AS \"waiting\" FROM \"message_search_backfill\" \
                 JOIN \"session\" ON \"session\".\"id\" = \"message_search_backfill\".\"session_id\" \
                 WHERE \"session\".\"owner\" = {}",
                placeholder(backend, 1)
            ),
            [owner.as_str().into()],
        ))
        .await
        .map_err(store_err)?
        .map(|row| row.try_get::<i64>("", "waiting"))
        .transpose()
        .map_err(store_err)?
        .unwrap_or_default();
    Ok(u64::try_from(waiting).unwrap_or_default())
}

/// Which conversations a search reads.
#[derive(Debug, Clone, Copy)]
pub(in crate::db) enum SearchScope {
    /// Every conversation the owner owns.
    Owner,
    /// The owner's code sessions in one repository's workspaces.
    Repo(RepoId),
}

/// One index row that matched, with what the hit needs about its session.
#[derive(Debug, Clone)]
pub(in crate::db) struct MatchedRow {
    pub(in crate::db) id: i64,
    pub(in crate::db) session_id: uuid::Uuid,
    pub(in crate::db) source_key: String,
    pub(in crate::db) source: MessageSearchSource,
    pub(in crate::db) turn_id: Option<uuid::Uuid>,
    pub(in crate::db) message_id: Option<uuid::Uuid>,
    pub(in crate::db) event_seq: Option<i64>,
    pub(in crate::db) created_at_micros: i64,
    pub(in crate::db) session_title: Option<String>,
    pub(in crate::db) workspace_id: Option<uuid::Uuid>,
    pub(in crate::db) workspace_title: Option<String>,
    pub(in crate::db) internal: bool,
    pub(in crate::db) archived: bool,
}

impl MatchedRow {
    pub(in crate::db) fn created_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.created_at_micros).unwrap_or_default()
    }

    fn cursor(&self) -> MessageSearchCursor {
        MessageSearchCursor {
            created_at_micros: self.created_at_micros,
            row: self.id,
        }
    }
}

/// The owner's index rows matching `terms`, newest first, after `cursor`.
///
/// Scoping is by the session's owner, the rule every conversation read
/// follows: a row is read only when its session belongs to `owner`. Memory
/// incognito sessions are never indexed; the filter here is the backstop.
pub(in crate::db) async fn matching_rows<C>(
    conn: &C,
    owner: &OwnerId,
    terms: &SearchTerms,
    scope: SearchScope,
    cursor: Option<MessageSearchCursor>,
    limit: u64,
) -> Result<Vec<MatchedRow>>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    let mut values: Vec<Value> = Vec::new();
    let next = |values: &mut Vec<Value>, value: Value| {
        values.push(value);
        placeholder(backend, values.len())
    };
    let (from, matches) = match backend {
        DbBackend::Postgres => (
            "\"message_search\"".to_owned(),
            format!(
                "\"message_search\".\"search_vector\" @@ CAST({} AS tsquery)",
                next(&mut values, terms.tsquery().into())
            ),
        ),
        _ => (
            "\"message_search_fts\" JOIN \"message_search\" \
             ON \"message_search\".\"id\" = \"message_search_fts\".rowid"
                .to_owned(),
            format!(
                "\"message_search_fts\" MATCH {}",
                next(&mut values, terms.fts5_match().into())
            ),
        ),
    };
    let workspace_join = match scope {
        SearchScope::Owner => "LEFT JOIN",
        SearchScope::Repo(_) => "JOIN",
    };
    let mut filters = vec![
        matches,
        format!(
            "\"message_search\".\"owner\" = {}",
            next(&mut values, owner.as_str().into())
        ),
        format!(
            "\"session\".\"owner\" = {}",
            next(&mut values, owner.as_str().into())
        ),
        format!(
            "\"session\".\"memory_incognito\" = {}",
            next(&mut values, false.into())
        ),
    ];
    if let SearchScope::Repo(repo_id) = scope {
        filters.push(format!(
            "\"code_workspace\".\"repo_id\" = {}",
            next(&mut values, repo_id.0.into())
        ));
        filters.push(format!(
            "\"code_workspace\".\"owner\" = {}",
            next(&mut values, owner.as_str().into())
        ));
    }
    if let Some(cursor) = cursor {
        let at = next(&mut values, cursor.created_at_micros.into());
        let same_at = next(&mut values, cursor.created_at_micros.into());
        let row = next(&mut values, cursor.row.into());
        filters.push(format!(
            "(\"message_search\".\"created_at_micros\" < {at} OR \
             (\"message_search\".\"created_at_micros\" = {same_at} AND \"message_search\".\"id\" < {row}))"
        ));
    }
    let limit = next(&mut values, i64::try_from(limit).unwrap_or(i64::MAX).into());
    let sql = format!(
        "SELECT \"message_search\".\"id\" AS \"id\", \
         \"message_search\".\"session_id\" AS \"session_id\", \
         \"message_search\".\"source_key\" AS \"source_key\", \
         \"message_search\".\"source\" AS \"source\", \
         \"message_search\".\"turn_id\" AS \"turn_id\", \
         \"message_search\".\"message_id\" AS \"message_id\", \
         \"message_search\".\"event_seq\" AS \"event_seq\", \
         \"message_search\".\"created_at_micros\" AS \"created_at_micros\", \
         \"session\".\"title\" AS \"session_title\", \
         \"session\".\"workspace_id\" AS \"workspace_id\", \
         \"session\".\"harness_kind\" AS \"harness_kind\", \
         CASE WHEN \"session\".\"archived_at\" IS NULL AND \"code_workspace\".\"archived_at\" IS NULL \
              THEN 0 ELSE 1 END AS \"archived\", \
         \"code_workspace\".\"title\" AS \"workspace_title\" \
         FROM {from} \
         JOIN \"session\" ON \"session\".\"id\" = \"message_search\".\"session_id\" \
         {workspace_join} \"code_workspace\" ON \"code_workspace\".\"id\" = \"session\".\"workspace_id\" \
         WHERE {} \
         ORDER BY \"message_search\".\"created_at_micros\" DESC, \"message_search\".\"id\" DESC \
         LIMIT {limit}",
        filters.join(" AND ")
    );
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .map(|row| {
            let source: String = row.try_get("", "source").map_err(store_err)?;
            let harness_kind: String = row.try_get("", "harness_kind").map_err(store_err)?;
            let archived: i32 = row.try_get("", "archived").map_err(store_err)?;
            Ok(MatchedRow {
                id: row.try_get("", "id").map_err(store_err)?,
                session_id: row.try_get("", "session_id").map_err(store_err)?,
                source_key: row.try_get("", "source_key").map_err(store_err)?,
                source: MessageSearchSource::from_db(&source).ok_or_else(|| {
                    AgentError::Store(format!("message index row has unknown source {source}"))
                })?,
                turn_id: row.try_get("", "turn_id").map_err(store_err)?,
                message_id: row.try_get("", "message_id").map_err(store_err)?,
                event_seq: row.try_get("", "event_seq").map_err(store_err)?,
                created_at_micros: row.try_get("", "created_at_micros").map_err(store_err)?,
                session_title: row.try_get("", "session_title").map_err(store_err)?,
                workspace_id: row.try_get("", "workspace_id").map_err(store_err)?,
                workspace_title: row.try_get("", "workspace_title").map_err(store_err)?,
                internal: harness_kind == HarnessKind::Internal.as_str(),
                archived: archived != 0,
            })
        })
        .collect()
}

/// The text a matched row was indexed from, and whether a journal event was
/// the engine's own work outside the person's turn.
#[derive(Debug, Clone)]
pub(in crate::db) struct RowText {
    pub(in crate::db) text: String,
    pub(in crate::db) background: bool,
}

/// Read back the text every row in `rows` was indexed from, keyed by row id.
/// A row whose source is gone is left out.
pub(in crate::db) async fn row_texts<C>(conn: &C, rows: &[MatchedRow]) -> Result<HashMap<i64, RowText>>
where
    C: ConnectionTrait,
{
    let mut texts = HashMap::new();
    let message_ids: Vec<uuid::Uuid> = rows.iter().filter_map(|row| row.message_id).collect();
    let mut messages: HashMap<uuid::Uuid, String> = HashMap::new();
    for chunk in message_ids.chunks(ID_CHUNK) {
        messages.extend(
            entities::message::Entity::find()
                .select_only()
                .column(entities::message::Column::Id)
                .column(entities::message::Column::Content)
                .filter(entities::message::Column::Id.is_in(chunk.iter().copied()))
                .into_tuple::<(uuid::Uuid, String)>()
                .all(conn)
                .await
                .map_err(store_err)?,
        );
    }
    let input_turns: Vec<uuid::Uuid> = rows
        .iter()
        .filter(|row| row.source_key.starts_with("input:"))
        .filter_map(|row| row.turn_id)
        .collect();
    let mut inputs: HashMap<uuid::Uuid, String> = HashMap::new();
    for chunk in input_turns.chunks(ID_CHUNK) {
        inputs.extend(
            entities::turn::Entity::find()
                .select_only()
                .column(entities::turn::Column::Id)
                .column(entities::turn::Column::UserInput)
                .filter(entities::turn::Column::Id.is_in(chunk.iter().copied()))
                .into_tuple::<(uuid::Uuid, String)>()
                .all(conn)
                .await
                .map_err(store_err)?,
        );
    }
    let mut seqs_by_session: HashMap<uuid::Uuid, Vec<i64>> = HashMap::new();
    for row in rows {
        if let Some(seq) = row.event_seq {
            seqs_by_session.entry(row.session_id).or_default().push(seq);
        }
    }
    let mut events: HashMap<(uuid::Uuid, i64), serde_json::Value> = HashMap::new();
    for (session_id, seqs) in seqs_by_session {
        for chunk in seqs.chunks(ID_CHUNK) {
            let found = entities::event::Entity::find()
                .select_only()
                .column(entities::event::Column::Seq)
                .column(entities::event::Column::Event)
                .filter(entities::event::Column::SessionId.eq(session_id))
                .filter(entities::event::Column::Seq.is_in(chunk.iter().copied()))
                .into_tuple::<(i64, serde_json::Value)>()
                .all(conn)
                .await
                .map_err(store_err)?;
            events.extend(
                found
                    .into_iter()
                    .map(|(seq, event)| ((session_id, seq), event)),
            );
        }
    }
    for row in rows {
        let text = if let Some(message_id) = row.message_id {
            messages.get(&message_id).map(|text| RowText {
                text: text.clone(),
                background: false,
            })
        } else if row.source_key.starts_with("input:") {
            row.turn_id
                .and_then(|turn| inputs.get(&turn))
                .map(|text| RowText {
                    text: text.clone(),
                    background: false,
                })
        } else {
            row.event_seq
                .and_then(|seq| events.get(&(row.session_id, seq)))
                .and_then(|value| Event::deserialize(value).ok())
                .and_then(|event| {
                    code_event_text(&event).map(|text| RowText {
                        text: text.text.to_owned(),
                        background: text.background,
                    })
                })
        };
        if let Some(text) = text {
            texts.insert(row.id, text);
        }
    }
    Ok(texts)
}

/// The turn a journal event at `seq` ran in: the last turn that started or
/// resumed before it.
async fn event_turn<C>(conn: &C, session_id: uuid::Uuid, seq: i64) -> Result<Option<TurnId>>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    let (turn, kind) = match backend {
        DbBackend::Postgres => (
            "\"event\"->>'turn_id'".to_owned(),
            "\"event\"->>'type'".to_owned(),
        ),
        _ => (
            "json_extract(\"event\", '$.turn_id')".to_owned(),
            "json_extract(\"event\", '$.type')".to_owned(),
        ),
    };
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT {turn} AS \"turn_id\" FROM \"event\" \
                 WHERE \"session_id\" = {} AND \"seq\" < {} \
                 AND {kind} IN ('turn_started', 'turn_resumed') \
                 ORDER BY \"seq\" DESC LIMIT 1",
                placeholder(backend, 1),
                placeholder(backend, 2)
            ),
            [session_id.into(), seq.into()],
        ))
        .await
        .map_err(store_err)?;
    Ok(row
        .and_then(|row| row.try_get::<Option<String>>("", "turn_id").ok().flatten())
        .and_then(|turn| uuid::Uuid::parse_str(&turn).ok())
        .map(TurnId))
}

/// Search `owner`'s conversations, newest match first.
pub(in crate::db) async fn search_messages(
    store: &DbStore,
    owner: &OwnerId,
    request: &MessageSearchRequest,
) -> Result<MessageSearchPage> {
    let pending = pending_for_owner(store, owner).await?;
    let indexing = MessageSearchIndexing {
        complete: pending == 0,
        pending_conversations: pending,
    };
    let terms = SearchTerms::parse(&request.query);
    if terms.is_empty() {
        return Ok(MessageSearchPage {
            hits: Vec::new(),
            next_cursor: None,
            indexing,
        });
    }
    let limit = usize::try_from(request.limit.clamp(1, MAX_SEARCH_LIMIT)).unwrap_or(1);
    let mut rows = matching_rows(
        &store.conn,
        owner,
        &terms,
        SearchScope::Owner,
        request.cursor,
        u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1),
    )
    .await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = more
        .then(|| rows.last().map(|row| row.cursor().encode()))
        .flatten();
    let texts = row_texts(&store.conn, &rows).await?;

    // A retry's question stands for its whole attempt in the transcript, so
    // a hit on it names the turn that can be rerun.
    let retried_sessions: Vec<uuid::Uuid> = rows
        .iter()
        .filter(|row| row.message_id.is_some() && row.source == MessageSearchSource::User)
        .map(|row| row.session_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut placements: HashMap<uuid::Uuid, TurnPlacements> = HashMap::new();
    for session_id in retried_sessions {
        let replacements =
            super::turn::list_turn_replacements_on(&store.conn, SessionId(session_id)).await?;
        if !replacements.is_empty() {
            placements.insert(session_id, TurnPlacements::new(&replacements));
        }
    }

    let mut hits = Vec::with_capacity(rows.len());
    for row in &rows {
        let Some(text) = texts.get(&row.id) else {
            continue;
        };
        let turn_id = if row.message_id.is_some() {
            row.turn_id.map(|turn| {
                let turn = TurnId(turn);
                match placements.get(&row.session_id) {
                    Some(placements) if row.source == MessageSearchSource::User => {
                        placements.attempt(turn)
                    }
                    _ => turn,
                }
            })
        } else if let Some(turn) = row.turn_id {
            Some(TurnId(turn))
        } else if let (Some(seq), false) = (row.event_seq, text.background) {
            event_turn(&store.conn, row.session_id, seq).await?
        } else {
            None
        };
        let excerpt = terms.excerpt(&text.text, SNIPPET_CHARS, SNIPPET_LEAD_CHARS);
        let kind = if row.internal && row.workspace_id.is_none() {
            MessageSearchKind::Chat
        } else {
            MessageSearchKind::Code
        };
        let title = row
            .session_title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .or_else(|| {
                row.workspace_title
                    .clone()
                    .filter(|title| !title.trim().is_empty())
            });
        hits.push(MessageSearchHit {
            kind,
            session_id: SessionId(row.session_id),
            workspace_id: row.workspace_id.map(crate::code::WorkspaceId),
            title,
            turn_id,
            message_id: row.message_id.map(MessageId),
            event_seq: row.event_seq,
            source: row.source,
            snippet: excerpt.text,
            ranges: excerpt.ranges,
            created_at: row.created_at(),
            archived: row.archived,
        });
    }
    Ok(MessageSearchPage {
        hits,
        next_cursor,
        indexing,
    })
}

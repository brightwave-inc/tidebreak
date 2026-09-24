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
//!
//! A write that changes a session's part of the index holds that session's
//! row lock, and so does a rebuild of the session, for its whole transaction.
//! The two never interleave: a live write either commits before the rebuild
//! reads the session's rows, or waits for the rebuild to commit and then
//! replaces what it wrote. Code journal appends take the lock before they
//! allocate a sequence number on PostgreSQL, and chat writes take the chat
//! lock, which is the same row. On SQLite every write, the journal's group
//! commit included, runs on the one writer connection under `BEGIN
//! IMMEDIATE`, which serializes them the same way.

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
    code_event_text, index_terms, MessageSearchBackfill, MessageSearchCursor, MessageSearchHit,
    MessageSearchIndexing, MessageSearchKind, MessageSearchPage, MessageSearchRequest,
    MessageSearchSource, SearchTerms, INDEXED_EVENT_TYPES, MAX_SEARCH_LIMIT, SNIPPET_CHARS,
    SNIPPET_LEAD_CHARS,
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
/// The journal event types that start or resume a turn. The index reads them
/// to know which turn each later event ran in, and indexes no text from them.
const TURN_EVENT_TYPES: &[&str] = &["turn_started", "turn_resumed"];
/// Failed attempts after which the backfill gives up on a conversation. The
/// waits between them double from [`BACKFILL_RETRY_MICROS`], so the last one
/// comes about an hour after the first: a lock or statement timeout, a
/// deadlock, or a dropped connection has passed long before then, and an
/// error still there is one that will not pass. An attempt counts only once
/// the database has recorded it, so an outage or a full disk, which fails
/// that write too, uses up no conversation's attempts.
pub(in crate::db) const MAX_BACKFILL_ATTEMPTS: i32 = 8;
/// How long the backfill waits after a conversation's first failed attempt.
const BACKFILL_RETRY_MICROS: i64 = 30 * 1_000_000;
/// Longest wait between two attempts at one conversation.
const MAX_BACKFILL_RETRY_MICROS: i64 = 60 * 60 * 1_000_000;
/// Longest error a queued conversation keeps, in characters.
const MAX_BACKFILL_ERROR_CHARS: usize = 1_000;

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

/// Remove everything the index holds for one session, the turn its journal
/// is in included.
pub(in crate::db) async fn clear_session_on<C>(conn: &C, session_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    for table in ["message_search", "message_search_turn"] {
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "DELETE FROM \"{table}\" WHERE \"session_id\" = {}",
                placeholder(backend, 1)
            ),
            [session_id.0.into()],
        ))
        .await
        .map_err(store_err)?;
    }
    Ok(())
}

/// The turn a code session's journal is in, as the last `turn_started` or
/// `turn_resumed` it indexed named it. `None` before the first one, and for a
/// session whose history the backfill has not reached, which the backfill
/// then reads in full.
async fn journal_turn_on<C>(conn: &C, session_id: SessionId) -> Result<Option<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        format!(
            "SELECT \"turn_id\" FROM \"message_search_turn\" WHERE \"session_id\" = {}",
            placeholder(backend, 1)
        ),
        [session_id.0.into()],
    ))
    .await
    .map_err(store_err)?
    .map(|row| row.try_get::<uuid::Uuid>("", "turn_id"))
    .transpose()
    .map_err(store_err)
}

/// Record that the journal event at `seq` started or resumed `turn`. An
/// older event never replaces a newer one.
async fn set_journal_turn_on<C>(
    conn: &C,
    session_id: SessionId,
    turn: uuid::Uuid,
    seq: i64,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        format!(
            "INSERT INTO \"message_search_turn\" (\"session_id\", \"turn_id\", \"seq\") \
             VALUES ({}, {}, {}) \
             ON CONFLICT (\"session_id\") DO UPDATE SET \
             \"turn_id\" = excluded.\"turn_id\", \"seq\" = excluded.\"seq\" \
             WHERE excluded.\"seq\" > \"message_search_turn\".\"seq\"",
            placeholder(backend, 1),
            placeholder(backend, 2),
            placeholder(backend, 3)
        ),
        [session_id.0.into(), turn.into(), seq.into()],
    ))
    .await
    .map_err(store_err)?;
    Ok(())
}

/// The turn a journal event starts or resumes, if it does either.
fn turn_boundary(event: &Event) -> Option<TurnId> {
    match event {
        Event::TurnStarted { turn_id } | Event::TurnResumed { turn_id } => Some(*turn_id),
        _ => None,
    }
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
    retry_on_new_content(conn, message.chat_id).await?;
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
    retry_on_new_content(conn, session_id.0).await?;
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

/// Whether a stored journal event is one the index reads, read without
/// decoding the rest of it: one that can carry searchable text, or one that
/// starts or resumes a turn.
pub(in crate::db) fn indexed_event_type(event: &serde_json::Value) -> bool {
    event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| INDEXED_EVENT_TYPES.contains(&kind) || TURN_EVENT_TYPES.contains(&kind))
}

/// One event as it was just journaled: its sequence number, its stored JSON,
/// and when it was written.
pub(in crate::db) type JournaledEvent<'a> = (i64, &'a serde_json::Value, DateTime<Utc>);

/// Index the searchable events among `events`, just journaled for one
/// session, in sequence order.
///
/// Each piece stores the turn its event ran in: the turn the last
/// `turn_started` or `turn_resumed` before it named, whether that event is in
/// `events` or was journaled earlier. A search reads the turn off the row and
/// never walks the journal back to find it.
///
/// A tool call's piece is replaced by each later event that restates what it
/// acted on, so the index holds the call's final arguments once.
pub(in crate::db) async fn index_code_events_on<C>(
    conn: &C,
    session_id: SessionId,
    events: &[JournaledEvent<'_>],
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
    // The turn in effect before `events`, read once and only if an event
    // needs it before one of `events` starts a turn.
    let mut turn: Option<Option<uuid::Uuid>> = None;
    let mut boundary: Option<(uuid::Uuid, i64)> = None;
    let mut retried = false;
    for (seq, event, at) in &decoded {
        if let Some(started) = turn_boundary(event) {
            turn = Some(Some(started.0));
            boundary = Some((started.0, *seq));
            continue;
        }
        let in_turn = match code_event_text(event) {
            None => continue,
            Some(text) if text.background => None,
            Some(_) => match turn {
                Some(known) => known,
                None => {
                    let known = journal_turn_on(conn, session_id).await?;
                    turn = Some(known);
                    known
                }
            },
        };
        let Some(piece) = event_piece(*seq, event, *at, in_turn) else {
            continue;
        };
        if !retried {
            retry_on_new_content(conn, session_id.0).await?;
            retried = true;
        }
        if piece.source_key.starts_with("call:") {
            delete_piece_on(conn, session_id.0, &piece.source_key).await?;
        }
        insert_pieces_on(conn, &facts.owner, session_id.0, &[piece]).await?;
    }
    if let Some((started, seq)) = boundary {
        set_journal_turn_on(conn, session_id, started, seq).await?;
    }
    Ok(())
}

/// The piece one journal event puts in the index, if any, stored against
/// `turn`: the turn the event ran in, or `None` for the engine's own work
/// outside the person's turn.
fn event_piece(
    seq: i64,
    event: &Event,
    at: DateTime<Utc>,
    turn: Option<uuid::Uuid>,
) -> Option<Piece> {
    let text = code_event_text(event)?;
    let terms = index_terms(text.text);
    if terms.is_empty() {
        return None;
    }
    Some(Piece {
        source_key: text.call_id.map_or_else(|| event_key(seq), call_key),
        source: text.source,
        turn_id: if text.background { None } else { turn },
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
/// Callers hold the session's row lock, so no live write lands between the
/// rows this reads and the pieces it writes.
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
    if facts.internal {
        let pieces = chat_pieces_on(conn, session_id).await?;
        return insert_pieces_on(conn, &facts.owner, session_id.0, &pieces).await;
    }
    let (pieces, boundary) = code_pieces_on(conn, session_id).await?;
    insert_pieces_on(conn, &facts.owner, session_id.0, &pieces).await?;
    if let Some((started, seq)) = boundary {
        set_journal_turn_on(conn, session_id, started, seq).await?;
    }
    Ok(())
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
            if outside.contains(&TurnId(message.turn_id)) || retried_copies.contains(&message.id) {
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

/// The SQL that keeps only the journal events the index reads, so a rebuild
/// never reads a delta or a tool's output.
fn indexed_event_filter(backend: DbBackend) -> String {
    let types = INDEXED_EVENT_TYPES
        .iter()
        .chain(TURN_EVENT_TYPES)
        .map(|kind| format!("'{kind}'"))
        .collect::<Vec<_>>()
        .join(", ");
    match backend {
        DbBackend::Postgres => format!("\"event\".\"event\"->>'type' IN ({types})"),
        _ => format!("json_extract(\"event\".\"event\", '$.type') IN ({types})"),
    }
}

/// Every piece of a session on an engine that keeps no `message` rows: each
/// turn's input and the searchable events in its journal, each event stored
/// against the turn it ran in. Also the last event that started or resumed a
/// turn, as `(turn, seq)`, so live writes carry on from it.
async fn code_pieces_on<C>(
    conn: &C,
    session_id: SessionId,
) -> Result<(Vec<Piece>, Option<(uuid::Uuid, i64)>)>
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
    let mut turn: Option<uuid::Uuid> = None;
    let mut boundary: Option<(uuid::Uuid, i64)> = None;
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
            if let Some(started) = turn_boundary(&event) {
                turn = Some(started.0);
                boundary = Some((started.0, seq));
                continue;
            }
            let Some(piece) = event_piece(seq, &event, at, turn) else {
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
    Ok((pieces, boundary))
}

/// Add the history of up to `sessions` conversations that predate the index,
/// newest activity first, and answer where the backfill stands.
///
/// Each session is rebuilt in its own transaction under the session's write
/// lock, so a message committed meanwhile is indexed once either way. A
/// session whose rebuild fails keeps its place in the queue: the attempt and
/// its error are recorded, and the session is tried again after a wait that
/// doubles with each failure. After [`MAX_BACKFILL_ATTEMPTS`] failures the
/// session is marked failed and left out of the index's history, and a search
/// reports it. Its new messages are still indexed as they land.
pub(in crate::db) async fn backfill(
    store: &DbStore,
    sessions: u64,
    now: DateTime<Utc>,
) -> Result<MessageSearchBackfill> {
    let backend = store.conn.get_database_backend();
    let now_micros = micros(now);
    let due = store
        .conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT \"message_search_backfill\".\"session_id\" AS \"session_id\", \
                 \"message_search_backfill\".\"attempts\" AS \"attempts\" \
                 FROM \"message_search_backfill\" \
                 JOIN \"session\" ON \"session\".\"id\" = \"message_search_backfill\".\"session_id\" \
                 WHERE \"message_search_backfill\".\"failed_at_micros\" IS NULL \
                 AND (\"message_search_backfill\".\"retry_at_micros\" IS NULL \
                      OR \"message_search_backfill\".\"retry_at_micros\" <= {}) \
                 ORDER BY COALESCE(\"session\".\"last_activity_at\", \"session\".\"created_at\") DESC, \
                 \"session\".\"id\" \
                 LIMIT {}",
                placeholder(backend, 1),
                placeholder(backend, 2)
            ),
            [
                now_micros.into(),
                i64::try_from(sessions).unwrap_or(i64::MAX).into(),
            ],
        ))
        .await
        .map_err(store_err)?;
    for row in due {
        let session_id: uuid::Uuid = row.try_get("", "session_id").map_err(store_err)?;
        let attempts: i32 = row.try_get("", "attempts").map_err(store_err)?;
        let transaction = store.conn.begin().await.map_err(store_err)?;
        let rebuilt = async {
            if super::acquire_session_write_lock(&transaction, session_id).await? {
                rebuild_session_on(&transaction, SessionId(session_id)).await?;
            }
            transaction
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    format!(
                        "DELETE FROM \"message_search_backfill\" WHERE \"session_id\" = {}",
                        placeholder(backend, 1)
                    ),
                    [session_id.into()],
                ))
                .await
                .map_err(store_err)?;
            Ok::<(), AgentError>(())
        }
        .await;
        let failed = match rebuilt {
            Ok(()) => transaction.commit().await.map_err(store_err).err(),
            Err(error) => {
                // A rollback fails when the connection it ran on is gone, and
                // the transaction went with it. The attempt still counts, on
                // another connection: returning here would leave this session
                // due, so it would head the queue on every step and hold back
                // every session behind it.
                if let Err(rollback) = transaction.rollback().await {
                    tracing::warn!(
                        session = %session_id,
                        error = %rollback,
                        "could not roll back a failed message index rebuild"
                    );
                }
                Some(error)
            }
        };
        if let Some(error) = failed {
            // One session that cannot be rebuilt right now must not hold back
            // every other, and must not be dropped for a failure that passes.
            record_failed_attempt(store, session_id, attempts.saturating_add(1), &error, now)
                .await?;
        }
    }
    backfill_state(store, now).await
}

/// Record a failed attempt at one queued session: when to try it again, or,
/// after the last attempt, that the backfill gave up on it.
async fn record_failed_attempt(
    store: &DbStore,
    session_id: uuid::Uuid,
    attempts: i32,
    error: &AgentError,
    now: DateTime<Utc>,
) -> Result<()> {
    let backend = store.conn.get_database_backend();
    let gave_up = attempts >= MAX_BACKFILL_ATTEMPTS;
    let wait = BACKFILL_RETRY_MICROS
        .saturating_mul(1_i64 << (attempts - 1).clamp(0, 20))
        .min(MAX_BACKFILL_RETRY_MICROS);
    let (retry_at, failed_at) = if gave_up {
        (None, Some(micros(now)))
    } else {
        (Some(micros(now).saturating_add(wait)), None)
    };
    let message: String = error
        .to_string()
        .chars()
        .take(MAX_BACKFILL_ERROR_CHARS)
        .collect();
    store
        .conn
        .execute_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "UPDATE \"message_search_backfill\" SET \"attempts\" = {}, \
                 \"retry_at_micros\" = {}, \"failed_at_micros\" = {}, \"last_error\" = {} \
                 WHERE \"session_id\" = {}",
                placeholder(backend, 1),
                placeholder(backend, 2),
                placeholder(backend, 3),
                placeholder(backend, 4),
                placeholder(backend, 5)
            ),
            [
                attempts.into(),
                retry_at.into(),
                failed_at.into(),
                message.into(),
                session_id.into(),
            ],
        ))
        .await
        .map_err(store_err)?;
    if gave_up {
        tracing::warn!(
            session = %session_id,
            attempts,
            %error,
            "gave up adding a conversation's history to the message index"
        );
    } else {
        tracing::warn!(
            session = %session_id,
            attempts,
            retry_in_secs = wait / 1_000_000,
            %error,
            "could not add a conversation's history to the message index; will try again"
        );
    }
    Ok(())
}

/// Wakes the backfill worker when a conversation it gave up on is due again.
///
/// A given-up conversation leaves nothing due, so the worker waits for this
/// instead of polling. The wake can land before the write that caused it
/// commits; the worker lets such a write settle before it looks again.
static BACKFILL_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Wait until a conversation the backfill gave up on is due again.
pub(in crate::db) async fn backfill_woken() {
    BACKFILL_WAKE.notified().await;
}

/// The `setting` key holding the newest app version the backfill ran under.
const BACKFILL_VERSION_SETTING: &str = "message_search.backfill_version";

/// Make a conversation the backfill gave up on due again, with a fresh count
/// of attempts. Its last error stays, for anyone reading the queue.
async fn retry_given_up_on<C>(conn: &C, session_id: Option<uuid::Uuid>) -> Result<u64>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    let mut sql = String::from(
        "UPDATE \"message_search_backfill\" SET \"attempts\" = 0, \
         \"retry_at_micros\" = NULL, \"failed_at_micros\" = NULL \
         WHERE \"failed_at_micros\" IS NOT NULL",
    );
    let mut values: Vec<Value> = Vec::new();
    if let Some(session_id) = session_id {
        values.push(session_id.into());
        sql.push_str(&format!(
            " AND \"session_id\" = {}",
            placeholder(backend, 1)
        ));
    }
    let retried = conn
        .execute_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
        .map_err(store_err)?
        .rows_affected();
    if retried > 0 {
        BACKFILL_WAKE.notify_one();
    }
    Ok(retried)
}

/// A conversation that has something new in it gets another try at its
/// history, if the backfill gave up on it. Runs with every live index write,
/// and costs one primary-key lookup when there is nothing to retry.
async fn retry_on_new_content<C>(conn: &C, session_id: uuid::Uuid) -> Result<()>
where
    C: ConnectionTrait,
{
    retry_given_up_on(conn, Some(session_id)).await.map(|_| ())
}

/// Take a conversation out of the backfill queue, after a write that settled
/// its history: a rebuild that indexed all of it, or memory incognito, which
/// keeps it out of the index altogether.
pub(in crate::db) async fn settle_backfill_on<C>(conn: &C, session_id: SessionId) -> Result<()>
where
    C: ConnectionTrait,
{
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        format!(
            "DELETE FROM \"message_search_backfill\" WHERE \"session_id\" = {}",
            placeholder(backend, 1)
        ),
        [session_id.0.into()],
    ))
    .await
    .map_err(store_err)?;
    Ok(())
}

/// The release a version string names, as `(major, minor, patch)`, and
/// whether it is a pre-release. `None` for anything else.
fn release_of(version: &str) -> Option<([u64; 3], bool)> {
    let version = version.split('+').next()?;
    let (core, pre_release) = match version.split_once('-') {
        Some((core, _)) => (core, true),
        None => (version, false),
    };
    let mut parts = core.split('.');
    let mut release = [0_u64; 3];
    for part in &mut release {
        *part = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some((release, pre_release))
}

/// Whether `current` is a newer release than `previous`. A pre-release comes
/// before its release. Versions that do not parse are newer when they differ.
pub(in crate::db) fn newer_release(current: &str, previous: &str) -> bool {
    match (release_of(current), release_of(previous)) {
        (Some((current, current_pre)), Some((previous, previous_pre))) => {
            current > previous || (current == previous && previous_pre && !current_pre)
        }
        _ => current != previous,
    }
}

/// Give every conversation the backfill gave up on another try when a newer
/// app version starts than the one that last ran the backfill, since the new
/// version may no longer fail on them. Answers how many it retried.
///
/// The newest version is kept in `setting`, so the same version starting
/// again, or an older one, retries nothing.
pub(in crate::db) async fn retry_after_upgrade(store: &DbStore, version: &str) -> Result<u64> {
    use sea_orm::sea_query::OnConflict;
    use sea_orm::ActiveValue::Set;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    let previous = entities::setting::Entity::find_by_id(BACKFILL_VERSION_SETTING.to_owned())
        .one(&transaction)
        .await
        .map_err(store_err)?
        .and_then(|row| row.value_json.as_str().map(str::to_owned));
    if previous
        .as_deref()
        .is_some_and(|previous| !newer_release(version, previous))
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(0);
    }
    let retried = retry_given_up_on(&transaction, None).await?;
    entities::setting::Entity::insert(entities::setting::ActiveModel {
        key: Set(BACKFILL_VERSION_SETTING.to_owned()),
        value_json: Set(serde_json::Value::String(version.to_owned())),
    })
    .on_conflict(
        OnConflict::column(entities::setting::Column::Key)
            .update_column(entities::setting::Column::ValueJson)
            .to_owned(),
    )
    .exec(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    if retried > 0 {
        tracing::info!(
            retried,
            version,
            "trying again to add conversations the message index gave up on, under a newer version"
        );
    }
    Ok(retried)
}

/// Where the backfill queue stands at `now`.
async fn backfill_state(store: &DbStore, now: DateTime<Utc>) -> Result<MessageSearchBackfill> {
    let backend = store.conn.get_database_backend();
    let row = store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT \
                 COUNT(CASE WHEN \"failed_at_micros\" IS NULL THEN 1 END) AS \"waiting\", \
                 COUNT(\"failed_at_micros\") AS \"failed\", \
                 COUNT(CASE WHEN \"failed_at_micros\" IS NULL AND (\"retry_at_micros\" IS NULL \
                      OR \"retry_at_micros\" <= {}) THEN 1 END) AS \"due\", \
                 MIN(CASE WHEN \"failed_at_micros\" IS NULL THEN \"retry_at_micros\" END) \
                      AS \"next_retry\" \
                 FROM \"message_search_backfill\"",
                placeholder(backend, 1)
            ),
            [micros(now).into()],
        ))
        .await
        .map_err(store_err)?;
    let Some(row) = row else {
        return Ok(MessageSearchBackfill {
            waiting: 0,
            failed: 0,
            next_attempt_at: None,
        });
    };
    let count = |column: &str| -> Result<u64> {
        let value: i64 = row.try_get("", column).map_err(store_err)?;
        Ok(u64::try_from(value).unwrap_or_default())
    };
    let waiting = count("waiting")?;
    let due = count("due")?;
    let next_retry: Option<i64> = row.try_get("", "next_retry").map_err(store_err)?;
    Ok(MessageSearchBackfill {
        waiting,
        failed: count("failed")?,
        next_attempt_at: if waiting == 0 || due > 0 {
            None
        } else {
            next_retry.and_then(DateTime::from_timestamp_micros)
        },
    })
}

/// How many of `owner`'s conversations the backfill has not added yet, and
/// how many it gave up on.
async fn indexing_for_owner(store: &DbStore, owner: &OwnerId) -> Result<MessageSearchIndexing> {
    let backend = store.conn.get_database_backend();
    let row = store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT \
                 COUNT(CASE WHEN \"message_search_backfill\".\"failed_at_micros\" IS NULL \
                      THEN 1 END) AS \"waiting\", \
                 COUNT(\"message_search_backfill\".\"failed_at_micros\") AS \"failed\" \
                 FROM \"message_search_backfill\" \
                 JOIN \"session\" ON \"session\".\"id\" = \"message_search_backfill\".\"session_id\" \
                 WHERE \"session\".\"owner\" = {}",
                placeholder(backend, 1)
            ),
            [owner.as_str().into()],
        ))
        .await
        .map_err(store_err)?;
    let count = |column: &str| -> Result<u64> {
        let Some(row) = &row else {
            return Ok(0);
        };
        let value: i64 = row.try_get("", column).map_err(store_err)?;
        Ok(u64::try_from(value).unwrap_or_default())
    };
    let pending = count("waiting")?;
    let failed = count("failed")?;
    Ok(MessageSearchIndexing {
        complete: pending == 0 && failed == 0,
        pending_conversations: pending,
        failed_conversations: failed,
    })
}

/// Whether one of `owner`'s conversations is still waiting for the backfill,
/// or was given up on. A conversation someone else owns, or one that was
/// never queued, counts as neither.
async fn indexing_for_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<MessageSearchIndexing> {
    let backend = store.conn.get_database_backend();
    let row = store
        .conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!(
                "SELECT \"message_search_backfill\".\"failed_at_micros\" AS \"failed_at_micros\" \
                 FROM \"message_search_backfill\" \
                 JOIN \"session\" ON \"session\".\"id\" = \"message_search_backfill\".\"session_id\" \
                 WHERE \"message_search_backfill\".\"session_id\" = {} AND \"session\".\"owner\" = {}",
                placeholder(backend, 1),
                placeholder(backend, 2)
            ),
            [session_id.0.into(), owner.as_str().into()],
        ))
        .await
        .map_err(store_err)?;
    let (pending, failed) = match row {
        None => (0, 0),
        Some(row) => {
            let failed_at: Option<i64> = row.try_get("", "failed_at_micros").map_err(store_err)?;
            if failed_at.is_some() {
                (0, 1)
            } else {
                (1, 0)
            }
        }
    };
    Ok(MessageSearchIndexing {
        complete: pending == 0 && failed == 0,
        pending_conversations: pending,
        failed_conversations: failed,
    })
}

/// Which conversations a search reads.
#[derive(Debug, Clone, Copy)]
pub(in crate::db) enum SearchScope {
    /// Every conversation the owner owns.
    Owner,
    /// One of the owner's conversations.
    Session(SessionId),
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
    // SQLite answers the match once, as a list of row ids, and the rows are
    // then read newest first. Joined directly, the planner can walk the
    // recency index and probe FTS5 for each row, which restarts the whole
    // match for every row a prefix query visits.
    let matches = match backend {
        DbBackend::Postgres => format!(
            "\"message_search\".\"search_vector\" @@ CAST({} AS tsquery)",
            next(&mut values, terms.tsquery().into())
        ),
        _ => format!(
            "\"message_search\".\"id\" IN (SELECT rowid FROM \"message_search_fts\" \
             WHERE \"message_search_fts\" MATCH {})",
            next(&mut values, terms.fts5_match().into())
        ),
    };
    let workspace_join = match scope {
        SearchScope::Owner | SearchScope::Session(_) => "LEFT JOIN",
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
    if let SearchScope::Session(session_id) = scope {
        filters.push(format!(
            "\"message_search\".\"session_id\" = {}",
            next(&mut values, session_id.0.into())
        ));
    }
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
         FROM \"message_search\" \
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

/// The text a matched row was indexed from.
#[derive(Debug, Clone)]
pub(in crate::db) struct RowText {
    pub(in crate::db) text: String,
}

/// Read back the text every row in `rows` was indexed from, keyed by row id.
/// A row whose source is gone is left out.
pub(in crate::db) async fn row_texts<C>(
    conn: &C,
    rows: &[MatchedRow],
) -> Result<HashMap<i64, RowText>>
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
            messages
                .get(&message_id)
                .map(|text| RowText { text: text.clone() })
        } else if row.source_key.starts_with("input:") {
            row.turn_id
                .and_then(|turn| inputs.get(&turn))
                .map(|text| RowText { text: text.clone() })
        } else {
            row.event_seq
                .and_then(|seq| events.get(&(row.session_id, seq)))
                .and_then(|value| Event::deserialize(value).ok())
                .and_then(|event| {
                    code_event_text(&event).map(|text| RowText {
                        text: text.text.to_owned(),
                    })
                })
        };
        if let Some(text) = text {
            texts.insert(row.id, text);
        }
    }
    Ok(texts)
}

/// Search `owner`'s conversations, newest match first.
pub(in crate::db) async fn search_messages(
    store: &DbStore,
    owner: &OwnerId,
    request: &MessageSearchRequest,
) -> Result<MessageSearchPage> {
    let indexing = match request.session_id {
        Some(session_id) => indexing_for_session(store, owner, session_id).await?,
        None => indexing_for_owner(store, owner).await?,
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
        request
            .session_id
            .map_or(SearchScope::Owner, SearchScope::Session),
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
        } else {
            // A code turn's input, or a journal event stored against the turn
            // it ran in when it was indexed.
            row.turn_id.map(TurnId)
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

//! Group commit for code journal appends on SQLite.
//!
//! Each event an engine streams used to be its own transaction: lock the
//! session row, read the session, read the last sequence number, insert,
//! commit. On SQLite the row lock is the database-wide write lock, so a fleet
//! of sessions streaming at once queued hundreds of small transactions a
//! second for the one writer.
//!
//! Appends now queue here, and one drain task commits everything queued in a
//! single transaction. The drain task does not wait for a batch to fill:
//! appends that arrive while one batch commits become the next batch. An idle
//! store commits an append straight away, and a busy one commits many per
//! transaction. A fixed wait would add its length to every event a session
//! streams, because a session worker waits for each append before it
//! publishes the next event.
//!
//! The guarantees a per-event transaction gave still hold:
//!
//! - Order. A session's events take consecutive sequence numbers in the order
//!   they were queued. A batch reads each session's last sequence number once,
//!   inside its transaction, and counts up from there in memory.
//! - Fencing. A batch reads each session's spawn epoch inside a transaction
//!   that holds SQLite's write lock from `BEGIN IMMEDIATE`, so nothing moves
//!   the epoch between the check and the insert. An append from a stale epoch
//!   is rejected and takes no sequence number.
//! - Durable before visible. An append returns its sequence number only after
//!   its batch commits, and callers publish to live readers only after that.
//!   A batch that fails to commit hands out no sequence numbers.
//!
//! When a batch fails, each of its appends is retried in a transaction of its
//! own, so one bad append fails alone instead of taking its neighbours with
//! it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use tokio::sync::oneshot;

use crate::code::{SessionId, SessionKind, TurnId, WorkspaceId};
use crate::error::{AgentError, Result};
use crate::{NotificationKind, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::journal::event_row_at;
use super::JournalError;

/// Most appends one transaction commits. A longer queue waits for the next
/// batch, so one flush never holds the writer for long.
const MAX_BATCH: usize = 128;

/// The queue of appends waiting for the writer, shared by every clone of one
/// [`DbStore`].
#[derive(Default)]
pub(in crate::db) struct JournalWriter {
    state: Mutex<WriterState>,
    /// Batches committed so far, so tests can see how appends were grouped.
    #[cfg(test)]
    committed_batches: std::sync::atomic::AtomicUsize,
}

#[derive(Default)]
struct WriterState {
    queue: VecDeque<Pending>,
    /// Whether a drain task is running. At most one is.
    draining: bool,
}

/// One event to journal.
pub(in crate::db) struct Append {
    pub(in crate::db) owner: OwnerId,
    pub(in crate::db) session_id: SessionId,
    pub(in crate::db) spawn_epoch: i64,
    pub(in crate::db) event: serde_json::Value,
    /// The turn a terminal event closes, when the append also mints the
    /// turn's notification.
    pub(in crate::db) notification: Option<TurnNotification>,
}

/// The notification a terminal append mints in the same transaction.
pub(in crate::db) struct TurnNotification {
    pub(in crate::db) turn_id: TurnId,
    /// `None` when the event is not one that may mint a notification. The
    /// append is then rejected after its fence check, where the per-event
    /// path rejected it too.
    pub(in crate::db) kind: Option<NotificationKind>,
}

struct Pending {
    append: Append,
    reply: oneshot::Sender<std::result::Result<i64, JournalError>>,
}

impl JournalWriter {
    /// Queue `append` and wait until the batch that carries it commits.
    ///
    /// Returns the event's sequence number once it is durable. A caller that
    /// stops waiting does not withdraw the append: it still commits with its
    /// batch.
    pub(in crate::db) async fn append(
        store: &DbStore,
        append: Append,
    ) -> std::result::Result<i64, JournalError> {
        let (reply, answer) = oneshot::channel();
        let start_drain = {
            let mut state = store.journal.lock();
            state.queue.push_back(Pending { append, reply });
            !std::mem::replace(&mut state.draining, true)
        };
        if start_drain {
            let store = store.clone();
            tokio::spawn(async move { drain(&store).await });
        }
        answer.await.unwrap_or_else(|_| Err(writer_stopped()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WriterState> {
        // The lock is never held across code that can panic, so a poisoned
        // lock still guards a consistent queue.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many appends are waiting for a batch.
    #[cfg(test)]
    pub(in crate::db) fn queued(&self) -> usize {
        self.lock().queue.len()
    }

    /// How many batches have committed.
    #[cfg(test)]
    pub(in crate::db) fn committed_batches(&self) -> usize {
        self.committed_batches
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Commit queued appends, batch after batch, until the queue is empty.
async fn drain(store: &DbStore) {
    let mut guard = DrainGuard {
        writer: &store.journal,
        finished: false,
    };
    loop {
        // Appends that are ready to queue right now join this batch rather
        // than the next one.
        tokio::task::yield_now().await;
        let batch: Vec<Pending> = {
            let mut state = store.journal.lock();
            if state.queue.is_empty() {
                state.draining = false;
                guard.finished = true;
                return;
            }
            let take = state.queue.len().min(MAX_BATCH);
            state.queue.drain(..take).collect()
        };
        commit(store, batch).await;
    }
}

/// Fails what is still queued if the drain task ends without emptying the
/// queue: it panicked, or its runtime shut down. Callers get an error rather
/// than wait on a writer that is gone, and the next append starts a new
/// drain task.
struct DrainGuard<'a> {
    writer: &'a JournalWriter,
    finished: bool,
}

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let queued = {
            let mut state = self.writer.lock();
            state.draining = false;
            std::mem::take(&mut state.queue)
        };
        for pending in queued {
            let _ = pending.reply.send(Err(writer_stopped()));
        }
    }
}

/// Commit one batch and answer every append in it.
async fn commit(store: &DbStore, mut batch: Vec<Pending>) {
    let error = match write_batch(store, &batch).await {
        Ok(outcomes) => {
            for (pending, outcome) in batch.into_iter().zip(outcomes) {
                let _ = pending.reply.send(outcome);
            }
            return;
        }
        Err(error) => error,
    };
    if batch.len() == 1 {
        if let Some(pending) = batch.pop() {
            let _ = pending.reply.send(Err(JournalError::Store(error)));
        }
        return;
    }
    tracing::warn!(
        appends = batch.len(),
        %error,
        "a journal batch failed; retrying its appends one at a time"
    );
    for pending in batch {
        let outcome = match write_batch(store, std::slice::from_ref(&pending)).await {
            Ok(mut outcomes) => outcomes.pop().unwrap_or_else(|| Err(writer_stopped())),
            Err(error) => Err(JournalError::Store(error)),
        };
        let _ = pending.reply.send(outcome);
    }
}

/// A session's newest sequence number, read in the fence query below. The
/// primary key `(session_id, seq)` answers it with one index probe.
const LAST_SEQ: &str = r#"(SELECT "event"."seq" FROM "event" WHERE "event"."session_id" = "session"."id" AND "event"."owner" = "session"."owner" ORDER BY "event"."seq" DESC LIMIT 1)"#;

/// What one batch knows about a session it appends to.
struct SessionFence {
    owner: String,
    spawn_epoch: i64,
    kind: String,
    workspace_id: Option<WorkspaceId>,
    /// The next sequence number to hand out, or `None` once the sequence is
    /// exhausted.
    next_seq: Option<i64>,
}

/// An append that passed its fence and holds a sequence number.
struct Admitted {
    seq: i64,
    notification: Option<Mint>,
}

/// A notification an admitted terminal append mints.
struct Mint {
    turn_id: TurnId,
    kind: NotificationKind,
    workspace_id: Option<WorkspaceId>,
}

/// Write `batch` in one transaction.
///
/// However many appends it carries, a batch reads every session it touches in
/// one query and inserts every event in one statement, so its cost barely
/// grows with its size. Only a terminal append that mints a notification adds
/// statements of its own.
///
/// `Ok` means the transaction committed, and carries each append's outcome:
/// its sequence number, or why it was rejected. A rejected append writes
/// nothing. `Err` means the transaction did not commit, so no append in it
/// took effect.
async fn write_batch(
    store: &DbStore,
    batch: &[Pending],
) -> Result<Vec<std::result::Result<i64, JournalError>>> {
    // `BEGIN IMMEDIATE`: the write lock is held before the first read, so the
    // fences read below cannot go stale before the insert.
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let mut fences = load_fences(&transaction, batch).await?;
    let mut outcomes = Vec::with_capacity(batch.len());
    let mut rows = Vec::with_capacity(batch.len());
    let mut mints = Vec::new();
    // What the message index reads, per session, in sequence order.
    let mut journaled: HashMap<SessionId, Vec<(i64, &serde_json::Value, chrono::DateTime<chrono::Utc>)>> =
        HashMap::new();
    for pending in batch {
        let append = &pending.append;
        let fence = fences
            .get_mut(&append.session_id)
            .filter(|fence| fence.owner == append.owner.as_str());
        match admit(&transaction, append, fence).await? {
            Ok(Admitted { seq, notification }) => {
                let created_at = chrono::Utc::now();
                journaled.entry(append.session_id).or_default().push((
                    seq,
                    &append.event,
                    created_at,
                ));
                rows.push(event_row_at(
                    &append.owner,
                    append.session_id,
                    seq,
                    append.event.clone(),
                    created_at,
                ));
                if let Some(mint) = notification {
                    mints.push((append, mint));
                }
                outcomes.push(Ok(seq));
            }
            Err(rejected) => outcomes.push(Err(rejected)),
        }
    }
    if !rows.is_empty() {
        entities::event::Entity::insert_many(rows)
            .exec_without_returning(&transaction)
            .await
            .map_err(store_err)?;
    }
    for (session_id, events) in &journaled {
        super::super::message_search::index_code_events_on(&transaction, *session_id, events)
            .await?;
    }
    for (append, mint) in mints {
        record_notification(&transaction, append, mint).await?;
    }
    transaction.commit().await.map_err(store_err)?;
    #[cfg(test)]
    store
        .journal
        .committed_batches
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(outcomes)
}

/// Read the fence of every session `batch` appends to, in one query.
async fn load_fences(
    transaction: &DatabaseTransaction,
    batch: &[Pending],
) -> Result<HashMap<SessionId, SessionFence>> {
    let mut ids: Vec<uuid::Uuid> = batch
        .iter()
        .map(|pending| pending.append.session_id.0)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let rows = entities::session::Entity::find()
        .select_only()
        .column(entities::session::Column::Id)
        .column(entities::session::Column::Owner)
        .column(entities::session::Column::SpawnEpoch)
        .column(entities::session::Column::Kind)
        .column(entities::session::Column::WorkspaceId)
        .expr_as(Expr::cust(LAST_SEQ), "last_seq")
        .filter(entities::session::Column::Id.is_in(ids))
        .into_tuple::<(
            uuid::Uuid,
            String,
            i64,
            String,
            Option<uuid::Uuid>,
            Option<i64>,
        )>()
        .all(transaction)
        .await
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .map(|(id, owner, spawn_epoch, kind, workspace_id, last_seq)| {
            let fence = SessionFence {
                owner,
                spawn_epoch,
                kind,
                workspace_id: workspace_id.map(WorkspaceId),
                next_seq: match last_seq {
                    Some(last) => last.checked_add(1),
                    None => Some(1),
                },
            };
            (SessionId(id), fence)
        })
        .collect())
}

/// Check one append against its session's fence and give it a sequence
/// number.
///
/// `fence` is `None` when the owner has no such session. The outer `Result`
/// is a failed statement, which aborts the batch. The inner one is this
/// append's own outcome.
async fn admit(
    transaction: &DatabaseTransaction,
    append: &Append,
    fence: Option<&mut SessionFence>,
) -> Result<std::result::Result<Admitted, JournalError>> {
    let session_id = append.session_id;
    let Some(fence) = fence else {
        return Ok(Err(JournalError::SessionNotFound { session_id }));
    };
    if fence.spawn_epoch != append.spawn_epoch {
        return Ok(Err(JournalError::StaleSpawnEpoch {
            session_id,
            attempted: append.spawn_epoch,
            current: fence.spawn_epoch,
        }));
    }
    // Check everything a notification needs before handing out a sequence
    // number, so a rejected append leaves no row behind.
    let notification = match &append.notification {
        None => None,
        Some(notification) => {
            let Some(kind) = notification.kind else {
                return Ok(Err(AgentError::Store(
                    "only completed or failed Code turns mint notifications".into(),
                )
                .into()));
            };
            let turn_exists = entities::turn::Entity::find_by_id(notification.turn_id.0)
                .filter(entities::turn::Column::Owner.eq(append.owner.as_str()))
                .filter(entities::turn::Column::SessionId.eq(session_id.0))
                .one(transaction)
                .await
                .map_err(store_err)?
                .is_some();
            if !turn_exists {
                return Ok(Err(AgentError::Store(format!(
                    "code turn {} does not belong to session {session_id}",
                    notification.turn_id
                ))
                .into()));
            }
            let Some(session_kind) = SessionKind::from_str(&fence.kind) else {
                return Ok(Err(AgentError::Store(format!(
                    "session {session_id} has unknown kind {}",
                    fence.kind
                ))
                .into()));
            };
            crate::code_session_mints_notification(session_kind).then_some(Mint {
                turn_id: notification.turn_id,
                kind,
                workspace_id: fence.workspace_id,
            })
        }
    };
    let Some(seq) = fence.next_seq else {
        return Ok(Err(AgentError::Store(format!(
            "event sequence exhausted for code session {session_id}"
        ))
        .into()));
    };
    fence.next_seq = seq.checked_add(1);
    Ok(Ok(Admitted { seq, notification }))
}

/// Mint the notification a terminal append owes, in the batch's transaction.
async fn record_notification(
    transaction: &DatabaseTransaction,
    append: &Append,
    mint: Mint,
) -> Result<()> {
    // The admitted event is the terminal one. It decodes here only to say why
    // a failed turn failed; a completed turn's body comes from the journal.
    let event = serde_json::from_value::<crate::code::Event>(append.event.clone()).ok();
    match mint.workspace_id {
        Some(workspace_id) => {
            let workspace_title = entities::code_workspace::Entity::find_by_id(workspace_id.0)
                .filter(entities::code_workspace::Column::Owner.eq(append.owner.as_str()))
                .one(transaction)
                .await
                .map_err(store_err)?
                .map(|workspace| workspace.title);
            super::super::notification::record_code_turn_notification_on(
                transaction,
                &append.owner,
                append.session_id,
                workspace_id,
                mint.turn_id,
                workspace_title.as_deref(),
                mint.kind,
                event.as_ref().and_then(super::journal::failure_message),
            )
            .await?;
        }
        None => {
            // Internal sessions open through the chat route. Share its dedupe
            // key so a native terminal write cannot mint a second row.
            super::super::notification::record_work_turn_notification_on(
                transaction,
                append.session_id,
                mint.turn_id,
                mint.kind,
                event.as_ref().and_then(super::journal::failure_detail),
            )
            .await?;
        }
    }
    Ok(())
}

fn writer_stopped() -> JournalError {
    JournalError::Store(AgentError::Store(
        "the journal writer stopped before this event was committed".into(),
    ))
}

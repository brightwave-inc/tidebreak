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

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use tokio::sync::oneshot;

use crate::code::{SessionId, SessionKind, TurnId, WorkspaceId};
use crate::error::{AgentError, Result};
use crate::{NotificationKind, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::journal::{insert_event_row_on, last_seq_on};
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

/// What one batch knows about a session it appends to.
enum SessionFence {
    /// No such session for this owner.
    Missing,
    Live {
        spawn_epoch: i64,
        kind: String,
        workspace_id: Option<WorkspaceId>,
        /// The next sequence number to hand out, or `None` once the sequence
        /// is exhausted.
        next_seq: Option<i64>,
    },
}

/// Write `batch` in one transaction.
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
    // fence and sequence reads below cannot go stale before the inserts.
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let mut sessions: HashMap<(String, SessionId), SessionFence> = HashMap::new();
    let mut outcomes = Vec::with_capacity(batch.len());
    for pending in batch {
        let append = &pending.append;
        let fence = match sessions.entry((append.owner.as_str().to_owned(), append.session_id)) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                entry.insert(load_fence(&transaction, &append.owner, append.session_id).await?)
            }
        };
        outcomes.push(append_one(&transaction, append, fence).await?);
    }
    transaction.commit().await.map_err(store_err)?;
    #[cfg(test)]
    store
        .journal
        .committed_batches
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(outcomes)
}

async fn load_fence(
    transaction: &sea_orm::DatabaseTransaction,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<SessionFence> {
    let Some(session) = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(transaction)
        .await
        .map_err(store_err)?
    else {
        return Ok(SessionFence::Missing);
    };
    let next_seq = match last_seq_on(transaction, owner, session_id).await? {
        Some(last) => last.checked_add(1),
        None => Some(1),
    };
    Ok(SessionFence::Live {
        spawn_epoch: session.spawn_epoch,
        kind: session.kind,
        workspace_id: session.workspace_id.map(WorkspaceId),
        next_seq,
    })
}

/// Fence and write one append.
///
/// The outer `Result` is a failed statement, which aborts the batch. The
/// inner one is this append's own outcome.
async fn append_one(
    transaction: &sea_orm::DatabaseTransaction,
    append: &Append,
    fence: &mut SessionFence,
) -> Result<std::result::Result<i64, JournalError>> {
    let session_id = append.session_id;
    let SessionFence::Live {
        spawn_epoch,
        kind,
        workspace_id,
        next_seq,
    } = fence
    else {
        return Ok(Err(JournalError::SessionNotFound { session_id }));
    };
    if *spawn_epoch != append.spawn_epoch {
        return Ok(Err(JournalError::StaleSpawnEpoch {
            session_id,
            attempted: append.spawn_epoch,
            current: *spawn_epoch,
        }));
    }
    // Check everything a notification needs before writing anything, so a
    // rejected append leaves no row behind.
    let notification = match &append.notification {
        None => None,
        Some(notification) => {
            let Some(notification_kind) = notification.kind else {
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
            let Some(session_kind) = SessionKind::from_str(kind) else {
                return Ok(Err(AgentError::Store(format!(
                    "session {session_id} has unknown kind {kind}"
                ))
                .into()));
            };
            Some((notification.turn_id, notification_kind, session_kind))
        }
    };
    let Some(seq) = *next_seq else {
        return Ok(Err(AgentError::Store(format!(
            "event sequence exhausted for code session {session_id}"
        ))
        .into()));
    };
    insert_event_row_on(
        transaction,
        &append.owner,
        session_id,
        seq,
        append.event.clone(),
    )
    .await?;
    *next_seq = seq.checked_add(1);
    if let Some((turn_id, notification_kind, session_kind)) = notification {
        if crate::code_session_mints_notification(session_kind) {
            match *workspace_id {
                Some(workspace_id) => {
                    let workspace_title =
                        entities::code_workspace::Entity::find_by_id(workspace_id.0)
                            .filter(
                                entities::code_workspace::Column::Owner.eq(append.owner.as_str()),
                            )
                            .one(transaction)
                            .await
                            .map_err(store_err)?
                            .map(|workspace| workspace.title);
                    super::super::notification::record_code_turn_notification_on(
                        transaction,
                        &append.owner,
                        session_id,
                        workspace_id,
                        turn_id,
                        workspace_title.as_deref(),
                        notification_kind,
                    )
                    .await?;
                }
                None => {
                    // Internal sessions open through the chat route. Share its
                    // dedupe key so a native terminal write cannot mint a
                    // second row.
                    super::super::notification::record_work_turn_notification_on(
                        transaction,
                        session_id,
                        turn_id,
                        notification_kind,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(Ok(seq))
}

fn writer_stopped() -> JournalError {
    JournalError::Store(AgentError::Store(
        "the journal writer stopped before this event was committed".into(),
    ))
}

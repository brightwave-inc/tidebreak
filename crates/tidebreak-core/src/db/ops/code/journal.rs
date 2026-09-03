use std::collections::{HashMap, HashSet};

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::code::{Event, SequencedEvent, SessionId, SessionKind, TurnId, TurnStatus, WorkspaceId};
use crate::error::{AgentError, Result};
use crate::{NotificationKind, OwnerId};

use super::super::super::{entities, store_err, DbStore};
use super::{acquire_code_session_write_lock, JournalError};

/// Append one journal event under the session's spawn-epoch fence.
///
/// Sequence numbers are allocated while holding the session row lock, the
/// same discipline the chat journal uses on the chat row. An append whose
/// `spawn_epoch` does not match the session row is rejected so a superseded
/// worker cannot corrupt the stream.
pub async fn append_event(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: &Event,
) -> std::result::Result<i64, JournalError> {
    append_event_inner(store, owner, session_id, spawn_epoch, event, None).await
}

/// Append a terminal event and mint its user notification in one transaction.
pub async fn append_event_with_notification(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    turn_id: TurnId,
    event: &Event,
) -> std::result::Result<i64, JournalError> {
    append_event_inner(store, owner, session_id, spawn_epoch, event, Some(turn_id)).await
}

async fn append_event_inner(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
    event: &Event,
    notification_turn_id: Option<TurnId>,
) -> std::result::Result<i64, JournalError> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_code_session_write_lock(&transaction, session_id).await? {
        return Err(JournalError::SessionNotFound { session_id });
    }
    let Some(session) = entities::session::Entity::find_by_id(session_id.0)
        .filter(entities::session::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        return Err(JournalError::SessionNotFound { session_id });
    };
    if session.spawn_epoch != spawn_epoch {
        return Err(JournalError::StaleSpawnEpoch {
            session_id,
            attempted: spawn_epoch,
            current: session.spawn_epoch,
        });
    }
    let seq = append_event_on_locked(&transaction, owner, session_id, event).await?;
    if let Some(turn_id) = notification_turn_id {
        let kind = match event {
            Event::TurnCompleted { .. } => NotificationKind::AgentCompleted,
            Event::TurnFailed { .. } => NotificationKind::AgentFailed,
            _ => {
                return Err(AgentError::Store(
                    "only completed or failed Code turns mint notifications".into(),
                )
                .into())
            }
        };
        let turn_exists = entities::turn::Entity::find_by_id(turn_id.0)
            .filter(entities::turn::Column::Owner.eq(owner.as_str()))
            .filter(entities::turn::Column::SessionId.eq(session_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .is_some();
        if !turn_exists {
            return Err(AgentError::Store(format!(
                "code turn {turn_id} does not belong to session {session_id}"
            ))
            .into());
        }
        let session_kind = SessionKind::from_str(&session.kind).ok_or_else(|| {
            AgentError::Store(format!(
                "session {} has unknown kind {}",
                session.id, session.kind
            ))
        })?;
        // A session with no workspace mints no notification yet: the record
        // names the workspace the turn ran in, and one without a workspace
        // has nothing to name until the entity merge reshapes it.
        if let Some(workspace_id) = session
            .workspace_id
            .map(WorkspaceId)
            .filter(|_| crate::code_session_mints_notification(session_kind))
        {
            let workspace_title = entities::code_workspace::Entity::find_by_id(workspace_id.0)
                .filter(entities::code_workspace::Column::Owner.eq(owner.as_str()))
                .one(&transaction)
                .await
                .map_err(store_err)?
                .map(|workspace| workspace.title);
            super::super::notification::record_code_turn_notification_on(
                &transaction,
                owner,
                session_id,
                workspace_id,
                turn_id,
                workspace_title.as_deref(),
                kind,
            )
            .await?;
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

/// Append after the caller has locked and fenced the session row.
pub(in crate::db) async fn append_event_on_locked<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
    event: &Event,
) -> Result<i64>
where
    C: ConnectionTrait,
{
    let last = entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?;
    let seq = last
        .map_or(Some(1), |model| model.seq.checked_add(1))
        .ok_or_else(|| {
            AgentError::Store(format!(
                "event sequence exhausted for code session {session_id}"
            ))
        })?;
    entities::event::ActiveModel {
        owner: Set(owner.as_str().to_owned()),
        session_id: Set(session_id.0),
        seq: Set(seq),
        event: Set(serde_json::to_value(event).map_err(AgentError::from)?),
        created_at: Set(Utc::now()),
        // The chat lane's recovery receipts; an engine fenced by its spawn
        // epoch writes none.
        turn_id: Set(None),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(false),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(seq)
}

/// Created-at of the newest journal row, if the session has any.
pub async fn latest_event_created_at(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<Option<chrono::DateTime<Utc>>> {
    Ok(entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(|model| model.created_at))
}

/// Default replay cap for [`list_events`].
///
/// A session journal grows for as long as the session lives, and a client
/// that connects with `after = 0` asks for all of it. Two thousand events is
/// more than any transcript a reader scrolls through, and it bounds what one
/// reconnect can cost the server.
pub const MAX_REPLAY_EVENTS: u64 = 2_000;

/// One bounded window of a session journal.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventPage {
    /// Events in ascending sequence order.
    pub events: Vec<SequencedEvent>,
    /// True when older events above the cursor were dropped to honor the cap.
    ///
    /// The window keeps the newest events, so a truncated page leaves a hole
    /// between the caller's cursor and the first event it carries. Say so
    /// rather than let a reader believe it holds the whole history.
    pub truncated: bool,
}

/// A fork-specific journal window made only of complete reconstructable turns.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForkEventPage {
    /// Kept turn events in ascending sequence order.
    pub events: Vec<SequencedEvent>,
    /// Turn ids whose complete event records fit in [`events`].
    pub complete_turns: HashSet<TurnId>,
    /// Terminal status observed for the requested fork boundary.
    ///
    /// This remains present when that turn is too large to retain, so fork
    /// settlement does not depend on returning a partial event window.
    pub boundary_status: Option<TurnStatus>,
    /// True when one or more whole turns were omitted at the replay boundary.
    pub truncated: bool,
}

/// Events for one of the owner's sessions with `seq > after`, in order.
///
/// At most `limit` events come back, and the window keeps the *newest* ones:
/// a client that fell far behind resumes at the live tail instead of paying
/// for history it would scroll past. Pass [`MAX_REPLAY_EVENTS`] unless you
/// have a reason to want a different bound.
pub async fn list_events(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    after: i64,
    limit: u64,
) -> Result<EventPage> {
    // Read one past the cap so a full window is distinguishable from a window
    // that happens to end exactly on it.
    let probe = limit.saturating_add(1);
    let mut rows = entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .filter(entities::event::Column::Seq.gt(after))
        .order_by_desc(entities::event::Column::Seq)
        .limit(probe)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let truncated = rows.len() as u64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    rows.reverse();
    let events = rows
        .into_iter()
        .map(|model| {
            Ok(SequencedEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(EventPage { events, truncated })
}

/// The oldest events for one of the owner's sessions with `seq > after`, in
/// order, at most `limit` of them.
///
/// The forward-reading companion to [`list_events`]: a follower that must
/// not skip a row — the internal engine catching up after a lagged
/// subscription — pages through the journal from its cursor with this,
/// where the replay window would hand it the newest rows and drop the ones
/// in between.
pub async fn list_events_from(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    after: i64,
    limit: u64,
) -> Result<Vec<SequencedEvent>> {
    entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .filter(entities::event::Column::Seq.gt(after))
        .order_by_asc(entities::event::Column::Seq)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|model| {
            Ok(SequencedEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            })
        })
        .collect()
}

/// Complete reconstructable turn events through `through_turn`, newest first
/// for budget decisions and ascending in the returned page.
///
/// The ordinary replay API keeps an event-count tail. A fork cannot use that
/// tail directly because its first retained row may sit inside a turn or
/// inside a nested tool call. This scan walks backwards to each `TurnStarted`
/// frame, keeps only a contiguous suffix of whole turns that fits `limit`, and
/// omits the whole boundary turn when that turn alone exceeds the cap.
/// Events from turns after `through_turn` are scanned but never returned.
pub async fn list_fork_events(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    through_turn: TurnId,
    limit: u64,
) -> Result<ForkEventPage> {
    const MIN_SCAN_BATCH: u64 = 128;

    let mut builder =
        ForkEventPageBuilder::new(through_turn, usize::try_from(limit).unwrap_or(usize::MAX));
    let batch = limit.clamp(MIN_SCAN_BATCH, MAX_REPLAY_EVENTS);
    let mut before = None;

    loop {
        let mut query = entities::event::Entity::find()
            .filter(entities::event::Column::Owner.eq(owner.as_str()))
            .filter(entities::event::Column::SessionId.eq(session_id.0))
            .order_by_desc(entities::event::Column::Seq)
            .limit(batch);
        if let Some(seq) = before {
            query = query.filter(entities::event::Column::Seq.lt(seq));
        }
        let rows = query.all(&store.conn).await.map_err(store_err)?;
        if rows.is_empty() {
            break;
        }
        before = rows.last().map(|row| row.seq);
        let short_batch = rows.len() < usize::try_from(batch).unwrap_or(usize::MAX);
        for model in rows {
            builder.push(SequencedEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            });
            if builder.done() {
                break;
            }
        }
        if builder.done() || short_batch {
            break;
        }
    }

    Ok(builder.finish())
}

/// Incrementally selects a newest contiguous suffix of complete turns while
/// the database scan moves from newest events to oldest events.
struct ForkEventPageBuilder {
    through_turn: TurnId,
    limit: usize,
    used: usize,
    target_found: bool,
    done: bool,
    truncated: bool,
    boundary_status: Option<TurnStatus>,
    terminal_status: Option<TurnStatus>,
    terminal_count: usize,
    segment_rev: Vec<SequencedEvent>,
    segment_oversized: bool,
    kept_rev: Vec<Vec<SequencedEvent>>,
    complete_turns: HashSet<TurnId>,
}

impl ForkEventPageBuilder {
    fn new(through_turn: TurnId, limit: usize) -> Self {
        Self {
            through_turn,
            limit,
            used: 0,
            target_found: false,
            done: false,
            truncated: false,
            boundary_status: None,
            terminal_status: None,
            terminal_count: 0,
            segment_rev: Vec::new(),
            segment_oversized: false,
            kept_rev: Vec::new(),
            complete_turns: HashSet::new(),
        }
    }

    fn push(&mut self, entry: SequencedEvent) {
        if self.done {
            return;
        }
        if let Event::TurnStarted { turn_id } = &entry.event {
            self.finish_segment(*turn_id, entry.seq);
            return;
        }

        if let Some(status) = terminal_status(&entry.event) {
            self.terminal_count += 1;
            if self.terminal_status.is_none() {
                self.terminal_status = Some(status);
            }
        } else if self.terminal_status.is_none() {
            // Session-level rows after a terminal event do not belong to the
            // turn. Ignore them until the backwards scan reaches its end.
            return;
        }

        let remaining = self.limit.saturating_sub(self.used);
        let max_after_start = remaining.saturating_sub(1);
        if self.segment_rev.len() < max_after_start {
            self.segment_rev.push(entry);
        } else {
            self.segment_oversized = true;
        }
    }

    fn finish_segment(&mut self, turn_id: TurnId, start_seq: i64) {
        let is_target = turn_id == self.through_turn;
        if !self.target_found {
            if !is_target {
                self.reset_segment();
                return;
            }
            self.target_found = true;
            self.boundary_status = if self.terminal_count == 1 {
                self.terminal_status
            } else {
                None
            };
        }

        let remaining = self.limit.saturating_sub(self.used);
        if self.terminal_status.is_none()
            || self.terminal_count != 1
            || self.segment_oversized
            || self.segment_rev.len().saturating_add(1) > remaining
        {
            self.truncated = true;
            self.done = true;
            self.reset_segment();
            return;
        }

        let mut events = Vec::with_capacity(self.segment_rev.len() + 1);
        events.push(SequencedEvent {
            seq: start_seq,
            event: Event::TurnStarted { turn_id },
        });
        events.extend(self.segment_rev.drain(..).rev());
        if !turn_is_reconstructable(turn_id, &events) {
            self.truncated = true;
            self.done = true;
            self.reset_segment();
            return;
        }

        self.used += events.len();
        self.complete_turns.insert(turn_id);
        self.kept_rev.push(events);
        self.reset_segment();
    }

    fn reset_segment(&mut self) {
        self.terminal_status = None;
        self.terminal_count = 0;
        self.segment_rev.clear();
        self.segment_oversized = false;
    }

    const fn done(&self) -> bool {
        self.done
    }

    fn finish(mut self) -> ForkEventPage {
        self.kept_rev.reverse();
        ForkEventPage {
            events: self.kept_rev.into_iter().flatten().collect(),
            complete_turns: self.complete_turns,
            boundary_status: self.boundary_status,
            truncated: self.truncated,
        }
    }
}

fn terminal_status(event: &Event) -> Option<TurnStatus> {
    match event {
        Event::TurnCompleted { .. } | Event::TurnRefused { .. } => Some(TurnStatus::Completed),
        Event::TurnFailed { .. } => Some(TurnStatus::Failed),
        Event::TurnInterrupted { .. } => Some(TurnStatus::Interrupted),
        _ => None,
    }
}

/// Confirm that every retained child and completion still has the start and
/// parent frame that gives it meaning.
fn turn_is_reconstructable(turn_id: TurnId, events: &[SequencedEvent]) -> bool {
    let Some((first, rest)) = events.split_first() else {
        return false;
    };
    if !matches!(&first.event, Event::TurnStarted { turn_id: id } if *id == turn_id)
        || rest
            .last()
            .and_then(|entry| terminal_status(&entry.event))
            .is_none()
    {
        return false;
    }

    let mut calls: HashMap<&str, Option<&str>> = HashMap::new();
    let mut completed = HashSet::new();
    for entry in rest {
        match &entry.event {
            Event::TurnStarted { .. } => return false,
            Event::ToolStarted {
                call_id,
                parent_call_id,
                ..
            } => {
                if calls.contains_key(call_id.as_str())
                    || parent_call_id
                        .as_deref()
                        .is_some_and(|parent| !calls.contains_key(parent))
                {
                    return false;
                }
                calls.insert(call_id, parent_call_id.as_deref());
            }
            Event::ToolCompleted {
                call_id,
                parent_call_id,
                ..
            } => {
                if calls.get(call_id.as_str()).copied() != Some(parent_call_id.as_deref())
                    || !completed.insert(call_id.as_str())
                {
                    return false;
                }
            }
            Event::AssistantMessage {
                parent_call_id: Some(parent),
                ..
            } if !calls.contains_key(parent.as_str()) => return false,
            _ => {}
        }
    }
    true
}

/// Newest journal events for one session, newest first. Digests use this
/// bounded tail to identify an unresolved top-level tool without replaying a
/// long conversation on every updates-socket connection.
pub async fn list_recent_events(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    limit: u64,
) -> Result<Vec<SequencedEvent>> {
    entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|model| {
            Ok(SequencedEvent {
                seq: model.seq,
                event: serde_json::from_value(model.event)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod fork_replay_tests {
    use super::*;
    use crate::code::{Diffstat, HarnessNoticeLevel, ToolDetail, ToolOutcome};

    fn sequenced(events: Vec<Event>) -> Vec<SequencedEvent> {
        events
            .into_iter()
            .enumerate()
            .map(|(index, event)| SequencedEvent {
                seq: index as i64 + 1,
                event,
            })
            .collect()
    }

    fn completed_turn(turn_id: TurnId, middle: Vec<Event>) -> Vec<Event> {
        let mut events = Vec::with_capacity(middle.len() + 2);
        events.push(Event::TurnStarted { turn_id });
        events.extend(middle);
        events.push(Event::TurnCompleted {
            usage: Default::default(),
            checkpoint: None,
            stop_reason: None,
        });
        events
    }

    fn select(events: Vec<Event>, through_turn: TurnId, limit: usize) -> ForkEventPage {
        let mut builder = ForkEventPageBuilder::new(through_turn, limit);
        for entry in sequenced(events).into_iter().rev() {
            builder.push(entry);
        }
        builder.finish()
    }

    #[test]
    fn omits_one_oversized_newest_turn_as_a_whole() {
        let newest = TurnId::new();
        let events = completed_turn(
            newest,
            (0..5)
                .map(|index| Event::ReasoningDelta {
                    text: format!("step {index}"),
                })
                .collect(),
        );

        let page = select(events, newest, 4);

        assert_eq!(page.boundary_status, Some(TurnStatus::Completed));
        assert!(page.events.is_empty());
        assert!(page.complete_turns.is_empty());
        assert!(page.truncated);
    }

    #[test]
    fn keeps_nested_tool_events_with_their_parent_and_start_frames() {
        let newest = TurnId::new();
        let events = completed_turn(
            newest,
            vec![
                Event::ToolStarted {
                    call_id: "task-1".to_owned(),
                    name: "Task".to_owned(),
                    detail: ToolDetail::Other {
                        summary: "inspect".to_owned(),
                    },
                    parent_call_id: None,
                },
                Event::ToolStarted {
                    call_id: "read-1".to_owned(),
                    name: "Read".to_owned(),
                    detail: ToolDetail::Other {
                        summary: "src/lib.rs".to_owned(),
                    },
                    parent_call_id: Some("task-1".to_owned()),
                },
                Event::AssistantMessage {
                    text: "found the boundary".to_owned(),
                    parent_call_id: Some("task-1".to_owned()),
                },
                Event::ToolCompleted {
                    call_id: "read-1".to_owned(),
                    outcome: ToolOutcome::Succeeded,
                    preview: "contents".to_owned(),
                    output: None,
                    action: None,
                    result: None,
                    detail: None,
                    parent_call_id: Some("task-1".to_owned()),
                },
                Event::ToolCompleted {
                    call_id: "task-1".to_owned(),
                    outcome: ToolOutcome::Succeeded,
                    preview: "done".to_owned(),
                    output: None,
                    action: None,
                    result: None,
                    detail: None,
                    parent_call_id: None,
                },
            ],
        );
        let limit = events.len();

        let page = select(events, newest, limit);

        assert_eq!(page.events.len(), limit);
        assert_eq!(page.complete_turns, HashSet::from([newest]));
        assert!(!page.truncated);
    }

    #[test]
    fn keeps_a_complete_turn_when_checkpoint_work_follows_its_terminal_event() {
        for checkpoint_event in [
            Event::CheckpointRecorded {
                turn_id: TurnId::new(),
                diffstat: Diffstat {
                    files: 1,
                    insertions: 2,
                    deletions: 0,
                    truncated: false,
                },
            },
            Event::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                message: "checkpoint failed".to_owned(),
            },
        ] {
            let turn_id = match &checkpoint_event {
                Event::CheckpointRecorded { turn_id, .. } => *turn_id,
                _ => TurnId::new(),
            };
            let mut events = completed_turn(
                turn_id,
                vec![Event::AssistantMessage {
                    text: "done".to_owned(),
                    parent_call_id: None,
                }],
            );
            events.push(checkpoint_event);

            let page = select(events, turn_id, 3);

            assert_eq!(page.events.len(), 3);
            assert_eq!(page.complete_turns, HashSet::from([turn_id]));
            assert_eq!(page.boundary_status, Some(TurnStatus::Completed));
            assert!(!page.truncated);
        }
    }

    #[test]
    fn omits_a_turn_whose_nested_event_has_no_parent_frame() {
        let newest = TurnId::new();
        let events = completed_turn(
            newest,
            vec![Event::ToolStarted {
                call_id: "read-1".to_owned(),
                name: "Read".to_owned(),
                detail: ToolDetail::Other {
                    summary: "src/lib.rs".to_owned(),
                },
                parent_call_id: Some("missing-task".to_owned()),
            }],
        );

        let page = select(events, newest, 3);

        assert_eq!(page.boundary_status, Some(TurnStatus::Completed));
        assert!(page.events.is_empty());
        assert!(page.complete_turns.is_empty());
        assert!(page.truncated);
    }

    #[test]
    fn keeps_only_complete_turns_that_fit_around_the_cap() {
        let oldest = TurnId::new();
        let middle = TurnId::new();
        let newest = TurnId::new();
        let mut events = Vec::new();
        for (turn_id, text) in [(oldest, "oldest"), (middle, "middle"), (newest, "newest")] {
            events.extend(completed_turn(
                turn_id,
                vec![Event::AssistantMessage {
                    text: text.to_owned(),
                    parent_call_id: None,
                }],
            ));
        }

        let page = select(events, newest, 6);

        assert_eq!(page.events.len(), 6);
        assert_eq!(page.events.first().map(|event| event.seq), Some(4));
        assert_eq!(page.events.last().map(|event| event.seq), Some(9));
        assert_eq!(page.complete_turns, HashSet::from([middle, newest]));
        assert!(page.truncated);
    }
}

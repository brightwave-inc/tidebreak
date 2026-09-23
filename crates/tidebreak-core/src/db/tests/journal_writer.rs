//! Group commit for code journal appends, and the SQLite read pool beside the
//! writer.

use std::time::Duration;

use chrono::Utc;
use sea_orm::{ConnectionTrait, Database, TransactionTrait};

use super::temp_store_with_max_connections;
use crate::attention::{Attention, AttentionSource};
use crate::code::{
    Event, ExecutionLocation, HarnessKind, Session, SessionId, SessionKind, SessionLifecycle,
};
use crate::db::code::{
    append_event, bump_spawn_epoch, insert_session, list_events, JournalError, MAX_REPLAY_EVENTS,
};
use crate::db::DbStore;
use crate::{OwnerId, PermissionMode, SessionVisibility};

fn session(owner: &OwnerId) -> Session {
    Session {
        visibility: SessionVisibility::Private,
        id: SessionId::new(),
        owner: owner.clone(),
        owner_kind: None,
        workspace_id: None,
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Running,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 0,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: Utc::now(),
        execution_location: ExecutionLocation::Machine,
        acts_as: None,
    }
}

async fn seeded_session(store: &DbStore, owner: &OwnerId) -> SessionId {
    let session = session(owner);
    insert_session(store, &session).await.unwrap();
    session.id
}

fn message(text: &str) -> Event {
    Event::AssistantMessage {
        text: text.to_owned(),
        parent_call_id: None,
    }
}

/// The journal of one session as `(seq, text)` pairs, read from the store.
async fn journal(store: &DbStore, owner: &OwnerId, session_id: SessionId) -> Vec<(i64, String)> {
    list_events(store, owner, session_id, 0, MAX_REPLAY_EVENTS)
        .await
        .unwrap()
        .events
        .into_iter()
        .map(|entry| match entry.event {
            Event::AssistantMessage { text, .. } => (entry.seq, text),
            other => panic!("unexpected journal event {other:?}"),
        })
        .collect()
}

/// Queue every append, in order, before the drain task gets a chance to run,
/// then wait for all of them. Polling an append once queues it.
async fn append_together(
    store: &DbStore,
    owner: &OwnerId,
    appends: &[(SessionId, i64, &str)],
) -> Vec<Result<i64, JournalError>> {
    let events: Vec<Event> = appends.iter().map(|(_, _, text)| message(text)).collect();
    let mut pending: Vec<_> = appends
        .iter()
        .zip(&events)
        .map(|((session_id, epoch, _), event)| {
            Box::pin(append_event(store, owner, *session_id, *epoch, event))
        })
        .collect();
    for append in &mut pending {
        assert!(futures::poll!(append).is_pending());
    }
    assert_eq!(store.journal.queued(), appends.len());
    futures::future::join_all(pending).await
}

#[tokio::test]
async fn one_batch_numbers_each_session_in_the_order_its_appends_queued() {
    let (_dir, store) = temp_store_with_max_connections(2).await;
    let owner = OwnerId::local();
    let left = seeded_session(&store, &owner).await;
    let right = seeded_session(&store, &owner).await;
    let before = store.journal.committed_batches();

    let seqs: Vec<i64> = append_together(
        &store,
        &owner,
        &[
            (left, 0, "left 1"),
            (right, 0, "right 1"),
            (left, 0, "left 2"),
            (left, 0, "left 3"),
            (right, 0, "right 2"),
        ],
    )
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect();

    assert_eq!(seqs, vec![1, 1, 2, 3, 2]);
    assert_eq!(store.journal.committed_batches() - before, 1);
    assert_eq!(
        journal(&store, &owner, left).await,
        vec![
            (1, "left 1".to_owned()),
            (2, "left 2".to_owned()),
            (3, "left 3".to_owned()),
        ]
    );
    assert_eq!(
        journal(&store, &owner, right).await,
        vec![(1, "right 1".to_owned()), (2, "right 2".to_owned())]
    );

    // The next batch counts on from what the last one committed.
    assert_eq!(
        append_event(&store, &owner, left, 0, &message("left 4"))
            .await
            .unwrap(),
        4
    );
}

#[tokio::test]
async fn a_stale_epoch_in_a_batch_is_rejected_without_taking_a_sequence_number() {
    let (_dir, store) = temp_store_with_max_connections(2).await;
    let owner = OwnerId::local();
    let fenced = seeded_session(&store, &owner).await;
    let other = seeded_session(&store, &owner).await;
    assert_eq!(bump_spawn_epoch(&store, fenced, None).await.unwrap(), 1);
    let missing = SessionId::new();

    let mut outcomes = append_together(
        &store,
        &owner,
        &[
            (fenced, 1, "current 1"),
            (fenced, 0, "superseded"),
            (other, 0, "other 1"),
            (missing, 0, "nowhere"),
            (fenced, 1, "current 2"),
        ],
    )
    .await
    .into_iter();

    assert_eq!(outcomes.next().unwrap().unwrap(), 1);
    match outcomes.next().unwrap() {
        Err(JournalError::StaleSpawnEpoch {
            session_id,
            attempted,
            current,
        }) => {
            assert_eq!(session_id, fenced);
            assert_eq!((attempted, current), (0, 1));
        }
        other => panic!("expected a stale epoch, got {other:?}"),
    }
    assert_eq!(outcomes.next().unwrap().unwrap(), 1);
    match outcomes.next().unwrap() {
        Err(JournalError::SessionNotFound { session_id }) => assert_eq!(session_id, missing),
        other => panic!("expected a missing session, got {other:?}"),
    }
    assert_eq!(outcomes.next().unwrap().unwrap(), 2);
    assert_eq!(
        journal(&store, &owner, fenced).await,
        vec![(1, "current 1".to_owned()), (2, "current 2".to_owned())]
    );

    // An epoch that moves between batches fences the next batch.
    assert_eq!(bump_spawn_epoch(&store, fenced, None).await.unwrap(), 2);
    assert!(matches!(
        append_event(&store, &owner, fenced, 1, &message("late")).await,
        Err(JournalError::StaleSpawnEpoch { current: 2, .. })
    ));
}

#[tokio::test]
async fn an_append_answers_only_after_its_batch_commits() {
    let (dir, store) = temp_store_with_max_connections(2).await;
    let owner = OwnerId::local();
    let session_id = seeded_session(&store, &owner).await;

    // Occupy the writer, the way a long write from elsewhere would.
    let writer = store.conn.begin().await.unwrap();
    let pending = tokio::spawn({
        let store = store.clone();
        let owner = owner.clone();
        async move { append_event(&store, &owner, session_id, 0, &message("held")).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !pending.is_finished(),
        "an append answered while its batch could not commit"
    );
    // Reads use the read pool, so they neither wait for the writer nor see
    // the uncommitted event.
    let read = tokio::time::timeout(Duration::from_secs(1), journal(&store, &owner, session_id))
        .await
        .expect("a read waited behind the writer");
    assert!(read.is_empty());

    writer.commit().await.unwrap();
    assert_eq!(pending.await.unwrap().unwrap(), 1);

    // By the time the sequence number came back, any new connection sees
    // the row: nothing a live reader is shown can vanish with the process.
    let fresh = Database::connect(format!(
        "sqlite://{}?mode=ro",
        dir.path().join("test.db").display()
    ))
    .await
    .unwrap();
    let row = fresh
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT seq FROM event WHERE session_id = ?",
            [session_id.0.into()],
        ))
        .await
        .unwrap()
        .expect("the committed event is visible to a new connection");
    assert_eq!(row.try_get::<i64>("", "seq").unwrap(), 1);
}

#[tokio::test]
async fn an_append_that_fails_does_not_sink_the_rest_of_its_batch() {
    let (_dir, store) = temp_store_with_max_connections(2).await;
    let owner = OwnerId::local();
    let left = seeded_session(&store, &owner).await;
    let right = seeded_session(&store, &owner).await;
    // A trigger on the writer's own connection refuses one payload.
    store
        .conn
        .execute_unprepared(
            "CREATE TEMP TRIGGER refuse_poison BEFORE INSERT ON main.event \
             WHEN NEW.event LIKE '%poison%' BEGIN SELECT RAISE(ABORT, 'poisoned event'); END",
        )
        .await
        .unwrap();

    let outcomes = append_together(
        &store,
        &owner,
        &[
            (left, 0, "left 1"),
            (left, 0, "poison"),
            (right, 0, "right 1"),
            (left, 0, "left 2"),
        ],
    )
    .await;

    assert_eq!(outcomes[0].as_ref().unwrap(), &1);
    assert!(
        matches!(&outcomes[1], Err(JournalError::Store(error)) if error.to_string().contains("poisoned event")),
        "{:?}",
        outcomes[1]
    );
    assert_eq!(outcomes[2].as_ref().unwrap(), &1);
    assert_eq!(outcomes[3].as_ref().unwrap(), &2);
    assert_eq!(
        journal(&store, &owner, left).await,
        vec![(1, "left 1".to_owned()), (2, "left 2".to_owned())]
    );
    assert_eq!(
        journal(&store, &owner, right).await,
        vec![(1, "right 1".to_owned())]
    );
}

#[tokio::test]
async fn the_read_pool_runs_beside_the_writer_and_refuses_writes() {
    let (_dir, store) = temp_store_with_max_connections(2).await;
    let owner = OwnerId::local();
    let session_id = seeded_session(&store, &owner).await;

    let writer = store.conn.begin().await.unwrap();
    writer
        .execute_unprepared("UPDATE advisory_lock SET name = name WHERE name = 'turn_claim'")
        .await
        .unwrap();
    let found = tokio::time::timeout(
        Duration::from_secs(1),
        crate::db::code::get_session(&store, &owner, session_id),
    )
    .await
    .expect("a plain read waited behind the writer")
    .unwrap();
    assert!(found.is_some());
    writer.rollback().await.unwrap();

    let refused = store
        .conn
        .read_pool()
        .expect("a pooled SQLite store opens a read pool")
        .execute_unprepared("UPDATE advisory_lock SET name = name WHERE name = 'turn_claim'")
        .await
        .expect_err("a read-pool connection accepted a write");
    assert!(
        refused.to_string().contains("readonly"),
        "unexpected refusal: {refused}"
    );
}

#[tokio::test]
async fn a_single_connection_store_opens_no_read_pool() {
    let (_dir, store) = super::temp_store().await;
    assert!(store.conn.read_pool().is_none());
    let owner = OwnerId::local();
    let session_id = seeded_session(&store, &owner).await;
    assert_eq!(
        append_event(&store, &owner, session_id, 0, &message("alone"))
            .await
            .unwrap(),
        1
    );
}

/// Five sessions stream events while four readers poll the store, on the
/// desktop's production pool settings. Reads must stay fast however busy the
/// writer is.
///
/// Run with `cargo test -p tidebreak-core --lib journal_load -- --ignored
/// --nocapture` to see the numbers. Set `TIDEBREAK_JOURNAL_LOAD_SESSIONS` to
/// stream more sessions at once than the store has connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load test; run on demand"]
async fn journal_load_keeps_reads_fast_while_five_sessions_stream() {
    const EVENTS_PER_SESSION: usize = 3_000;
    const READERS: usize = 4;
    let sessions_streaming: usize = std::env::var("TIDEBREAK_JOURNAL_LOAD_SESSIONS")
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(5);

    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("load.db").display());
    let mut options = sea_orm::ConnectOptions::new(url);
    options
        .max_connections(8)
        .min_connections(8)
        .acquire_timeout(Duration::from_secs(30))
        .idle_timeout(None)
        .max_lifetime(None)
        .sqlx_logging(false);
    let store = DbStore::connect_with_options(options).await.unwrap();
    let owner = OwnerId::local();
    let mut sessions = Vec::new();
    for _ in 0..sessions_streaming {
        sessions.push(seeded_session(&store, &owner).await);
    }

    let streaming = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(sessions_streaming));
    let started = std::time::Instant::now();
    let mut writers = Vec::new();
    for session_id in sessions.clone() {
        let store = store.clone();
        let owner = owner.clone();
        let streaming = streaming.clone();
        writers.push(tokio::spawn(async move {
            let mut slowest = Duration::ZERO;
            for index in 0..EVENTS_PER_SESSION {
                let event = Event::ReasoningDelta {
                    text: format!("thinking about step {index} of the plan, in some detail"),
                };
                let appended = std::time::Instant::now();
                append_event(&store, &owner, session_id, 0, &event)
                    .await
                    .unwrap();
                slowest = slowest.max(appended.elapsed());
            }
            streaming.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            slowest
        }));
    }
    let mut readers = Vec::new();
    for reader in 0..READERS {
        let store = store.clone();
        let owner = owner.clone();
        let sessions = sessions.clone();
        let streaming = streaming.clone();
        readers.push(tokio::spawn(async move {
            let mut latencies = Vec::new();
            let mut turn = reader;
            while streaming.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                let session_id = sessions[turn % sessions.len()];
                turn += 1;
                let read = std::time::Instant::now();
                crate::db::code::get_session(&store, &owner, session_id)
                    .await
                    .unwrap();
                list_events(&store, &owner, session_id, 0, 50)
                    .await
                    .unwrap();
                latencies.push(read.elapsed());
            }
            latencies
        }));
    }

    let mut slowest_append = Duration::ZERO;
    for writer in writers {
        slowest_append = slowest_append.max(writer.await.unwrap());
    }
    let elapsed = started.elapsed();
    let mut latencies = Vec::new();
    for reader in readers {
        latencies.extend(reader.await.unwrap());
    }
    latencies.sort();
    let percentile = |fraction: f64| {
        let index = ((latencies.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
        latencies[index.min(latencies.len() - 1)]
    };
    let appends = sessions_streaming * EVENTS_PER_SESSION;
    println!(
        "{appends} appends in {:.2}s ({:.0}/s), slowest append {:.1}ms; \
         {} reads: p50 {:.1}ms, p99 {:.1}ms, max {:.1}ms; {} batches",
        elapsed.as_secs_f64(),
        appends as f64 / elapsed.as_secs_f64(),
        slowest_append.as_secs_f64() * 1e3,
        latencies.len(),
        percentile(0.50).as_secs_f64() * 1e3,
        percentile(0.99).as_secs_f64() * 1e3,
        latencies.last().unwrap().as_secs_f64() * 1e3,
        store.journal.committed_batches(),
    );
    for session_id in sessions {
        let journal = list_events(&store, &owner, session_id, 0, EVENTS_PER_SESSION as u64)
            .await
            .unwrap();
        let seqs: Vec<i64> = journal.events.iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs, (1..=EVENTS_PER_SESSION as i64).collect::<Vec<_>>());
    }
    assert!(
        percentile(0.99) < Duration::from_millis(250),
        "read p99 {:?} is over 250ms",
        percentile(0.99)
    );
}

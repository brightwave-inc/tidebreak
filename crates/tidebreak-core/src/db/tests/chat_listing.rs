//! The list of work: ordering, pins, the archive, unread, and pruning.

use super::*;

const OWNER: fn() -> OwnerId = OwnerId::local;

fn untitled_chat(created_at: i64) -> Chat {
    Chat {
        title: None,
        created_at: DateTime::<Utc>::from_timestamp(created_at, 0).unwrap(),
        ..sample_chat()
    }
}

fn ids(listings: &[ChatListing]) -> Vec<SessionId> {
    listings.iter().map(|listing| listing.chat.id).collect()
}

async fn listing(store: &DbStore, id: SessionId) -> ChatListing {
    store
        .get_chat_listing_scoped(&OWNER(), id)
        .await
        .unwrap()
        .expect("the conversation is listed")
}

async fn accept(store: &DbStore, chat: SessionId) -> TurnRun {
    match store
        .accept_turn(TurnId::new(), chat, "test", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    }
}

/// Claim and complete the one due turn, the way the turn worker does.
async fn finish(store: &DbStore, turn: &TurnRun) {
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the accepted turn is due");
    let output = Message {
        id: MessageId::new(),
        chat_id: turn.chat_id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "done".into(),
        llm_content: None,
        created_at: claimed_at,
    };
    store
        .complete_turn_and_append_event(
            turn.id,
            lease_token,
            0,
            claimed_at,
            &output,
            0,
            crate::provider::Usage::default(),
            crate::provider::StopReason::EndTurn,
        )
        .await
        .unwrap()
        .expect("the claimed turn completes");
}

#[tokio::test]
async fn the_list_follows_activity_not_creation() {
    let (_dir, store) = temp_store().await;
    let older = untitled_chat(1_000);
    let newer = untitled_chat(2_000);
    store.create_chat(&older).await.unwrap();
    store.create_chat(&newer).await.unwrap();

    // Nothing has happened yet, so creation orders the list.
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![newer.id, older.id]);
    assert_eq!(listed[1].last_activity_at, older.created_at);
    assert_eq!(listed[1].turn_count, 0);

    // A turn in the older conversation moves it to the top, running.
    let turn = accept(&store, older.id).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![older.id, newer.id]);
    assert!(listed[0].running);
    assert_eq!(listed[0].turn_count, 1);
    assert!(listed[0].last_activity_at > newer.created_at);

    // Renaming the newer one is activity too.
    assert!(store
        .update_chat_metadata(
            newer.id,
            Some(Some("Renamed".into())),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap());
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![newer.id, older.id]);

    // Finishing the turn moves the older one back up and stops it running.
    finish(&store, &turn).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![older.id, newer.id]);
    assert!(!listed[0].running);
}

#[tokio::test]
async fn a_finished_turn_stays_unread_until_the_owner_opens_it() {
    let (_dir, store) = temp_store().await;
    let chat = untitled_chat(1_000);
    store.create_chat(&chat).await.unwrap();

    let turn = accept(&store, chat.id).await;
    assert!(
        !listing(&store, chat.id).await.unread,
        "a running turn has not finished yet"
    );
    finish(&store, &turn).await;
    assert!(listing(&store, chat.id).await.unread);

    assert!(store
        .mark_chat_read_scoped(&OWNER(), chat.id)
        .await
        .unwrap());
    assert!(!listing(&store, chat.id).await.unread);

    // Someone else cannot clear it, and cannot tell it exists.
    let stranger = OwnerId::new("user:stranger").unwrap();
    assert!(!store
        .mark_chat_read_scoped(&stranger, chat.id)
        .await
        .unwrap());
}

/// Bringing a conversation back, or starting a new turn in it, is the owner
/// working in it again: neither returns an old result to the list as news.
#[tokio::test]
async fn coming_back_to_a_conversation_clears_what_it_left_unread() {
    let (_dir, store) = temp_store().await;
    let chat = untitled_chat(1_000);
    store.create_chat(&chat).await.unwrap();
    let turn = accept(&store, chat.id).await;
    finish(&store, &turn).await;
    assert!(listing(&store, chat.id).await.unread);

    assert!(store
        .set_chat_archived_scoped(&OWNER(), chat.id, true)
        .await
        .unwrap());
    assert!(store
        .set_chat_archived_scoped(&OWNER(), chat.id, false)
        .await
        .unwrap());
    assert!(!listing(&store, chat.id).await.unread, "unarchived");

    let turn = accept(&store, chat.id).await;
    finish(&store, &turn).await;
    assert!(listing(&store, chat.id).await.unread);
    let turn = accept(&store, chat.id).await;
    assert!(!listing(&store, chat.id).await.unread, "a new turn started");
    finish(&store, &turn).await;
    assert!(
        listing(&store, chat.id).await.unread,
        "its own end marks it again"
    );
}

/// A turn the code runtime starts, a Slack message or a queued one, moves the
/// conversation like a turn from the chat routes: to the top, out of the
/// archive, and no longer unread.
#[tokio::test]
async fn a_turn_the_runtime_starts_moves_the_conversation_too() {
    let (_dir, store) = temp_store().await;
    let chat = untitled_chat(1_000);
    store.create_chat(&chat).await.unwrap();
    let turn = accept(&store, chat.id).await;
    finish(&store, &turn).await;
    assert!(store
        .set_chat_archived_scoped(&OWNER(), chat.id, true)
        .await
        .unwrap());

    let started_at = DateTime::<Utc>::from_timestamp(9_000, 0).unwrap();
    crate::db::code::insert_turn(
        &store,
        &OWNER(),
        &crate::code::Turn {
            actor: None,
            id: TurnId::new(),
            session_id: chat.id,
            ordinal: 2,
            status: crate::code::TurnStatus::Running,
            model: None,
            fast_mode: false,
            user_input: "from the channel".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at,
            ended_at: None,
            park_ref: None,
            park_wait: None,
        },
    )
    .await
    .unwrap();
    let moved = listing(&store, chat.id).await;
    assert_eq!(moved.last_activity_at, started_at);
    assert_eq!(moved.archived_at, None);
    assert!(!moved.unread);
}

#[tokio::test]
async fn pins_sit_on_top_and_archiving_moves_a_chat_out_of_the_list() {
    let (_dir, store) = temp_store().await;
    let first = untitled_chat(1_000);
    let second = untitled_chat(2_000);
    let third = untitled_chat(3_000);
    for chat in [&first, &second, &third] {
        store.create_chat(chat).await.unwrap();
    }

    assert!(store
        .set_chat_pinned_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed), vec![first.id, third.id, second.id]);
    let pinned_at = listed[0].pinned_at.expect("pinned");

    // Pinning again keeps the first pin, so the pinned group does not shuffle.
    assert!(store
        .set_chat_pinned_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    assert_eq!(listing(&store, first.id).await.pinned_at, Some(pinned_at));

    // Archiving unpins and leaves the list for the archive.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), first.id, true)
        .await
        .unwrap());
    let archived = listing(&store, first.id).await;
    assert_eq!(archived.pinned_at, None);
    assert!(archived.archived_at.is_some());
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), false)
            .await
            .unwrap()),
        vec![third.id, second.id]
    );
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), true)
            .await
            .unwrap()),
        vec![first.id]
    );

    // Unarchiving brings it back where its activity puts it.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), first.id, false)
        .await
        .unwrap());
    assert_eq!(
        ids(&store
            .list_chat_listings_scoped(&OWNER(), false)
            .await
            .unwrap()),
        vec![third.id, second.id, first.id]
    );

    // A new turn in an archived conversation brings it back too.
    assert!(store
        .set_chat_archived_scoped(&OWNER(), second.id, true)
        .await
        .unwrap());
    accept(&store, second.id).await;
    let listed = store
        .list_chat_listings_scoped(&OWNER(), false)
        .await
        .unwrap();
    assert_eq!(ids(&listed)[0], second.id);
    assert_eq!(listed[0].archived_at, None);

    // Someone else's conversation cannot be pinned or archived.
    let stranger = OwnerId::new("user:stranger").unwrap();
    assert!(!store
        .set_chat_pinned_scoped(&stranger, third.id, true)
        .await
        .unwrap());
    assert!(!store
        .set_chat_archived_scoped(&stranger, third.id, true)
        .await
        .unwrap());
}

#[tokio::test]
async fn pruning_deletes_only_conversations_nothing_happened_in() {
    let (_dir, store) = temp_store().await;
    let empty = untitled_chat(1_000);
    let named = Chat {
        title: Some("Keep me".into()),
        ..untitled_chat(1_000)
    };
    let pinned = untitled_chat(1_000);
    let archived = untitled_chat(1_000);
    let worked = untitled_chat(1_000);
    let recent = untitled_chat(5_000);
    for chat in [&empty, &named, &pinned, &archived, &worked, &recent] {
        store.create_chat(chat).await.unwrap();
    }
    assert!(store
        .set_chat_pinned_scoped(&OWNER(), pinned.id, true)
        .await
        .unwrap());
    assert!(store
        .set_chat_archived_scoped(&OWNER(), archived.id, true)
        .await
        .unwrap());
    let turn = accept(&store, worked.id).await;
    finish(&store, &turn).await;

    let cutoff = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    assert_eq!(
        store.prune_empty_chats(cutoff).await.unwrap(),
        vec![empty.id]
    );
    assert_eq!(store.get_chat(empty.id).await.unwrap(), None);
    for kept in [&named, &pinned, &archived, &worked, &recent] {
        assert!(
            store.get_chat(kept.id).await.unwrap().is_some(),
            "pruning removed a conversation that held something"
        );
    }
    assert!(store.prune_empty_chats(cutoff).await.unwrap().is_empty());
}

/// An idle session with no worker yet, the shape a Slack thread has until the
/// runtime claims it.
fn unclaimed_session(created_at: i64) -> crate::code::Session {
    crate::code::Session {
        visibility: crate::SessionVisibility::Private,
        id: SessionId::new(),
        owner: OWNER(),
        owner_kind: None,
        workspace_id: None,
        kind: crate::code::SessionKind::Interactive,
        harness_kind: crate::code::HarnessKind::Internal,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: crate::PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: crate::code::SessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 0,
        attention: crate::attention::Attention::working(
            crate::attention::AttentionSource::Lifecycle,
        ),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: DateTime::<Utc>::from_timestamp(created_at, 0).unwrap(),
        execution_location: crate::code::ExecutionLocation::Machine,
        acts_as: None,
    }
}

/// A Slack thread whose first message still waits in the queue looks as idle
/// as an empty chat: no turn, no title, no worker. The sweep keeps it, and it
/// keeps a session a worker drives even when nothing else points at it.
#[tokio::test]
async fn pruning_keeps_sessions_the_code_runtime_holds() {
    use crate::db::code::{
        get_session, insert_session, list_queued_turns, mint_external_grant,
        record_external_message, resolve_external_machine_session, MintGrantSubject,
    };

    let (_dir, store) = temp_store().await;
    // The grant stores digests, never secrets: any 64 hex digits will do.
    let digest = |byte: u8| format!("{byte:02x}").repeat(32);
    let grant = mint_external_grant(
        &store,
        &OWNER(),
        MintGrantSubject {
            channel_kind: "slack",
            external_identity: "U1",
            workspace_identity: "T1",
            kind: crate::code::CodeGrantKind::Person,
        },
        &digest(1),
        &digest(2),
    )
    .await
    .unwrap();
    let thread = unclaimed_session(1_000);
    assert!(matches!(
        resolve_external_machine_session(&store, &OWNER(), grant.id, "slack", "T1/C1/1", &thread)
            .await
            .unwrap(),
        crate::code::ExternalSessionResolution::Created(_)
    ));
    assert!(matches!(
        record_external_message(
            &store,
            &OWNER(),
            thread.id,
            "Ev1",
            "1.000100",
            "hello",
            &crate::code::TurnActor::default(),
        )
        .await
        .unwrap(),
        crate::code::ExternalMessageRecord::Recorded(_)
    ));
    let driven = crate::code::Session {
        spawn_epoch: 1,
        ..unclaimed_session(1_000)
    };
    insert_session(&store, &driven).await.unwrap();
    let empty = untitled_chat(1_000);
    store.create_chat(&empty).await.unwrap();

    let cutoff = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    assert_eq!(
        store.prune_empty_chats(cutoff).await.unwrap(),
        vec![empty.id],
        "the sweep takes only the conversation nothing points at"
    );
    assert!(get_session(&store, &OWNER(), thread.id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        list_queued_turns(&store, &OWNER(), thread.id)
            .await
            .unwrap()
            .len(),
        1,
        "the waiting first message is still queued"
    );
    assert!(get_session(&store, &OWNER(), driven.id)
        .await
        .unwrap()
        .is_some());
}

/// Every column that names a session or an agent run keeps a conversation
/// from the sweep. A new table that points at either fails here until it
/// joins the sweep's lists.
#[tokio::test]
async fn every_row_that_names_a_conversation_keeps_it_from_the_sweep() {
    use crate::db::ops::conversation::{AGENT_RUN_REFERENCES, CHAT_REFERENCES};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use std::collections::BTreeSet;

    let (_dir, store) = temp_store().await;
    let query = |sql: String| {
        store
            .conn
            .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql))
    };
    let tables = query(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' ORDER BY name"
            .into(),
    )
    .await
    .unwrap();
    let mut names_session = BTreeSet::new();
    let mut names_run = BTreeSet::new();
    for table in tables
        .iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
    {
        let keys = query(format!(
            "SELECT \"table\" AS target, \"from\" AS source, \"to\" AS target_column \
             FROM pragma_foreign_key_list('{table}')"
        ))
        .await
        .unwrap();
        for key in keys {
            let target = key.try_get::<String>("", "target").unwrap();
            let source = key.try_get::<String>("", "source").unwrap();
            let target_column = key.try_get::<String>("", "target_column").unwrap();
            match (target.as_str(), target_column.as_str()) {
                ("session", "id") => {
                    names_session.insert((table.clone(), source));
                }
                ("agent_run", "id") => {
                    names_run.insert((table.clone(), source));
                }
                _ => {}
            }
        }
        let columns = query(format!("SELECT name FROM pragma_table_info('{table}')"))
            .await
            .unwrap();
        for column in columns {
            let column = column.try_get::<String>("", "name").unwrap();
            if column == "chat_id" || column == "session_id" || column.ends_with("_session_id") {
                names_session.insert((table.clone(), column));
            } else if column == "agent_run_id" || column.ends_with("_run_id") {
                names_run.insert((table.clone(), column));
            }
        }
    }
    // The foreground run is judged by its own rule, and the session table's
    // own columns are the conversation itself.
    names_session.remove(&("agent_run".to_owned(), "chat_id".to_owned()));
    names_session.retain(|(table, _)| table != "session");
    names_run.retain(|(table, _)| table != "agent_run");

    let listed = CHAT_REFERENCES
        .iter()
        .map(|(table, column)| ((*table).to_owned(), (*column).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(names_session, listed, "a table names a session");
    let listed_runs = AGENT_RUN_REFERENCES
        .iter()
        .map(|(table, column)| ((*table).to_owned(), (*column).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(names_run, listed_runs, "a table names an agent run");
}

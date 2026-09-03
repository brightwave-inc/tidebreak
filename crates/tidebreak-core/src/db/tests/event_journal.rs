use super::*;

#[tokio::test]
async fn event_journal_assigns_per_chat_seq_and_replays_after_cursor() {
    use crate::event::AgentEvent;
    use crate::id::TurnId;
    use crate::provider::{StopReason, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let started = AgentEvent::TurnStarted {
        turn_id: TurnId::new(),
    };
    let completed = AgentEvent::TurnCompleted {
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    };
    assert_eq!(store.append_event(chat.id, &started).await.unwrap(), 1);
    assert_eq!(store.append_event(chat.id, &completed).await.unwrap(), 2);

    // From the start: both events, in order, with their seq.
    let all = store.list_events(chat.id, 0).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].seq, all[1].seq), (1, 2));
    assert_eq!(all[0].event, started);

    // After a cursor: only the newer event (what a reconnecting client needs).
    let tail = store.list_events(chat.id, 1).await.unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].seq, 2);
    assert_eq!(tail[0].event, completed);

    // A second chat's seq restarts at 1 and its journal is isolated.
    let other = sample_chat();
    store.create_chat(&other).await.unwrap();
    assert_eq!(store.append_event(other.id, &started).await.unwrap(), 1);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 2);
}

#[tokio::test]
async fn list_events_for_call_returns_only_that_calls_args_and_completion() {
    use crate::event::AgentEvent;
    use crate::id::CallId;
    use crate::tool::ToolOutput;

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let wanted = CallId::new();
    let other = CallId::new();
    store
        .append_event(
            chat.id,
            &AgentEvent::ToolCallArgsDelta {
                call_id: other,
                fragment: "{\"skip\":true}".into(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::TextDelta {
                text: "noise".into(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::ToolCallArgsDelta {
                call_id: wanted,
                fragment: "{\"operation\":".into(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::ToolCallArgsDelta {
                call_id: wanted,
                fragment: "\"list\"}".into(),
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::ToolCallCompleted {
                call_id: wanted,
                output: ToolOutput::text("ok"),
                action: None,
                result: None,
            },
        )
        .await
        .unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::ToolCallCompleted {
                call_id: other,
                output: ToolOutput::text("other"),
                action: None,
                result: None,
            },
        )
        .await
        .unwrap();

    let events = store.list_events_for_call(chat.id, wanted).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    match &events[0].event {
        AgentEvent::ToolCallArgsDelta { call_id, fragment } => {
            assert_eq!(*call_id, wanted);
            assert_eq!(fragment, "{\"operation\":");
        }
        other => panic!("expected args delta, got {other:?}"),
    }
    match &events[2].event {
        AgentEvent::ToolCallCompleted {
            call_id, output, ..
        } => {
            assert_eq!(*call_id, wanted);
            assert_eq!(output.content, "ok");
        }
        other => panic!("expected completion, got {other:?}"),
    }
    assert!(store
        .list_events_for_call(chat.id, CallId::new())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn durable_turn_events_are_bound_and_reserve_one_terminal_slot() {
    use crate::event::AgentEvent;
    use crate::provider::{StopReason, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn(lease_token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let started = AgentEvent::TurnStarted { turn_id: turn.id };
    assert_eq!(
        store
            .append_turn_event(chat.id, turn.id, lease_token, 1, claimed_at, &started)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        store
            .append_turn_event(
                chat.id,
                turn.id,
                lease_token,
                1,
                lease_expires_at + chrono::Duration::seconds(1),
                &started,
            )
            .await
            .unwrap(),
        Some(1),
        "an exact ambiguous retry recovers its original sequence"
    );
    let stored = entities::code_event::Entity::find_by_id((chat.id.0, 1))
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_id, Some(turn.id.0));
    assert_eq!(stored.lease_token, Some(lease_token));
    assert_eq!(stored.attempt_event_ordinal, Some(1));
    assert!(!stored.terminal);
    assert_eq!(
        crate::chat_journal::decode_chat_event_required(stored.event).unwrap(),
        started
    );

    let terminal = AgentEvent::TurnCompleted {
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    };
    assert!(store
        .append_turn_event(chat.id, turn.id, lease_token, 2, claimed_at, &terminal)
        .await
        .is_err());
    assert!(store
        .append_turn_event(
            chat.id,
            turn.id,
            lease_token,
            1,
            claimed_at,
            &AgentEvent::ContextTruncated {
                original_tokens: 20,
                fitted_tokens: 10,
            },
        )
        .await
        .is_err());
    assert!(store
        .append_turn_event(
            chat.id,
            turn.id,
            lease_token,
            2,
            claimed_at,
            &AgentEvent::TurnStarted {
                turn_id: TurnId::new(),
            },
        )
        .await
        .is_err());
    assert_eq!(
        store
            .append_turn_event(
                chat.id,
                turn.id,
                lease_token,
                2,
                lease_expires_at + chrono::Duration::seconds(1),
                &AgentEvent::ContextTruncated {
                    original_tokens: 20,
                    fitted_tokens: 10,
                },
            )
            .await
            .unwrap(),
        None,
        "a stale lease cannot append a new event"
    );
    assert!(store.append_event(chat.id, &started).await.is_err());

    entities::code_event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(2),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(lease_token)),
        attempt_event_ordinal: Set(Some(2)),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(&terminal)).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(entities::code_event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(3),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(lease_token)),
        attempt_event_ordinal: Set(Some(3)),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(&terminal)).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .is_err());
    assert!(entities::code_event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(4),
        turn_id: Set(None),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(&terminal)).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .is_err());
    assert!(entities::code_event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(5),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(false),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(&started)).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .is_err());
}

#[tokio::test]
async fn concurrent_event_writers_allocate_one_contiguous_chat_sequence() {
    use crate::event::AgentEvent;

    const WRITERS: i64 = 16;
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(WRITERS as usize));
    let mut tasks = Vec::new();
    for index in 0..WRITERS {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let event = AgentEvent::TextDelta {
                text: format!("delta {index}"),
            };
            (index, store.append_event(chat.id, &event).await.unwrap())
        }));
    }

    let mut assigned = Vec::new();
    for task in tasks {
        assigned.push(task.await.unwrap().1);
    }
    assigned.sort_unstable();
    assert_eq!(assigned, (1..=WRITERS).collect::<Vec<_>>());

    let events = store.list_events(chat.id, 0).await.unwrap();
    assert_eq!(events.len(), WRITERS as usize);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=WRITERS).collect::<Vec<_>>()
    );
    let mut payloads = events
        .into_iter()
        .map(|event| match event.event {
            AgentEvent::TextDelta { text } => text,
            event => panic!("unexpected event: {event:?}"),
        })
        .collect::<Vec<_>>();
    payloads.sort();
    let mut expected = (0..WRITERS)
        .map(|index| format!("delta {index}"))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(payloads, expected);
}

#[tokio::test]
async fn event_for_unknown_chat_is_rejected() {
    use crate::event::AgentEvent;

    let (_dir, store) = temp_store().await;
    // No create_chat first: the `event -> chat` foreign key must reject
    // the orphan write. (The in-memory MemStore test double does *not* model
    // this constraint, so orphan-rejection is only guaranteed by DbStore.)
    let event = AgentEvent::TurnStarted {
        turn_id: TurnId::new(),
    };
    assert!(store.append_event(ChatId::new(), &event).await.is_err());
}

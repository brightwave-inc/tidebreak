use super::*;
use crate::event::AgentEvent;
use crate::storage::TurnEventAppend;

fn delta(attempt_event_ordinal: i32, text: &str) -> TurnEventAppend {
    TurnEventAppend {
        attempt_event_ordinal,
        event: AgentEvent::TextDelta { text: text.into() },
    }
}

/// Batched appends share one transaction but keep the per-event identity a
/// single append has: contiguous sequences, exact-retry recovery, refusal to
/// reuse an ordinal with different data, and the lease fence.
#[tokio::test]
async fn batched_turn_events_keep_single_append_identity() {
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
        .claim_turn_run(lease_token, claimed_at, lease_expires_at)
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

    let batch = [delta(2, "Hel"), delta(3, "lo, "), delta(4, "world")];
    assert_eq!(
        store
            .append_turn_events(chat.id, turn.id, lease_token, claimed_at, &batch)
            .await
            .unwrap(),
        Some(vec![2, 3, 4]),
        "a batch takes contiguous sequences in order"
    );
    assert_eq!(
        store
            .append_turn_events(
                chat.id,
                turn.id,
                lease_token,
                lease_expires_at + chrono::Duration::seconds(1),
                &batch,
            )
            .await
            .unwrap(),
        Some(vec![2, 3, 4]),
        "an exact ambiguous retry recovers every original sequence, even after lease loss"
    );
    assert_eq!(
        store
            .append_turn_events(
                chat.id,
                turn.id,
                lease_token,
                claimed_at,
                &[delta(3, "lo, "), delta(4, "world"), delta(5, "!")],
            )
            .await
            .unwrap(),
        Some(vec![3, 4, 5]),
        "a retry that overlaps a committed prefix appends only the fresh tail"
    );
    assert!(
        store
            .append_turn_events(
                chat.id,
                turn.id,
                lease_token,
                claimed_at,
                &[delta(5, "?"), delta(6, "")],
            )
            .await
            .is_err(),
        "an ordinal reused with different data is refused"
    );
    assert!(
        store
            .append_turn_events(
                chat.id,
                turn.id,
                lease_token,
                claimed_at,
                &[delta(7, "a"), delta(6, "b")],
            )
            .await
            .is_err(),
        "batch ordinals must ascend"
    );
    assert!(store
        .append_turn_events(chat.id, turn.id, lease_token, claimed_at, &[])
        .await
        .is_err());
    assert_eq!(
        store
            .append_turn_events(
                chat.id,
                turn.id,
                lease_token,
                lease_expires_at + chrono::Duration::seconds(1),
                &[delta(6, "late")],
            )
            .await
            .unwrap(),
        None,
        "a stale lease cannot append fresh events"
    );

    let events = store.list_events(chat.id, 0).await.unwrap();
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    let replayed: String = events
        .iter()
        .filter_map(|event| match &event.event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(replayed, "Hello, world!");
    for (seq, ordinal) in [(2, 2), (3, 3), (4, 4), (5, 5)] {
        let stored = entities::event::Entity::find_by_id((chat.id.0, seq))
            .one(&store.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.turn_id, Some(turn.id.0));
        assert_eq!(stored.lease_token, Some(lease_token));
        assert_eq!(stored.attempt_event_ordinal, Some(ordinal));
        assert!(!stored.terminal);
    }
}

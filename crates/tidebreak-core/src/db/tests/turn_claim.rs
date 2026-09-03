use super::*;

#[tokio::test]
async fn empty_turn_claim_does_not_wait_for_sqlite_writer() {
    let (_dir, store) = temp_store_with_max_connections(2).await;

    // Hold SQLite's single writer with an unrelated transaction. A fresh scan
    // with no turn to claim must remain read-only and leave the writer queue
    // available for real work.
    let writer = store.conn.begin().await.unwrap();
    writer
        .execute_unprepared("UPDATE advisory_lock SET name = name WHERE name = 'agent_run_claim'")
        .await
        .unwrap();

    let now = Utc::now();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.claim_turn(
            uuid::Uuid::new_v4(),
            now,
            now + chrono::Duration::minutes(1),
        ),
    )
    .await
    .expect("an empty turn scan waited for SQLite's writer")
    .unwrap();
    assert!(outcome.turn.is_none());
    assert!(outcome.terminal_event.is_none());

    writer.rollback().await.unwrap();
}

#[tokio::test]
async fn chat_claim_scan_skips_turns_owned_by_code_session_workers() {
    let (_dir, store) = temp_store().await;

    let runtime_chat = sample_chat();
    store.create_chat(&runtime_chat).await.unwrap();
    let runtime_turn = match store
        .accept_turn(TurnId::new(), runtime_chat.id, "gpt-5", "runtime turn")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected runtime acceptance outcome: {outcome:?}"),
    };
    assert_eq!(
        crate::db::code::bump_spawn_epoch(&store, crate::code::SessionId(runtime_chat.id.0), None,)
            .await
            .unwrap(),
        1
    );

    let plain_chat = sample_chat();
    store.create_chat(&plain_chat).await.unwrap();
    let plain_turn = match store
        .accept_turn(TurnId::new(), plain_chat.id, "gpt-5", "plain turn")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected plain acceptance outcome: {outcome:?}"),
    };

    let due_at = Utc::now();
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Resuming.as_str()),
        )
        .col_expr(
            entities::turn::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(due_at - chrono::Duration::seconds(1)),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(due_at - chrono::Duration::seconds(1)),
        )
        .col_expr(
            entities::turn::Column::ParkRef,
            sea_orm::sea_query::Expr::value(Some("approval".to_owned())),
        )
        .filter(entities::turn::Column::Id.eq(runtime_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(due_at),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(due_at),
        )
        .filter(entities::turn::Column::Id.eq(plain_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let claim_at = due_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            claim_at,
            claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the plain chat stays claimable");
    assert_eq!(claimed.id, plain_turn.id);

    let second = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            claim_at,
            claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(second.turn.is_none());
    assert!(second.terminal_event.is_none());

    assert_eq!(
        store
            .take_lease_on_turn(
                runtime_turn.id,
                uuid::Uuid::new_v4(),
                claim_at,
                claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        Some(())
    );
}

#[tokio::test]
async fn fence_turn_lease_reports_only_the_exact_live_segment() {
    use crate::TurnLeaseFence;

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let expiry = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_turn(token, claimed_at, expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.lease_token, Some(token));

    // Nil identities are rejected outright rather than reported as a state.
    assert!(store
        .fence_turn_lease(TurnId(uuid::Uuid::nil()), token, claimed_at)
        .await
        .is_err());
    assert!(store
        .fence_turn_lease(turn_id, uuid::Uuid::nil(), claimed_at)
        .await
        .is_err());

    // The exact live token owns the segment until its lease expires.
    assert_eq!(
        store
            .fence_turn_lease(turn_id, token, claimed_at + chrono::Duration::seconds(1))
            .await
            .unwrap(),
        TurnLeaseFence::Current
    );
    assert_eq!(
        store
            .fence_turn_lease(turn_id, token, expiry)
            .await
            .unwrap(),
        TurnLeaseFence::Stale
    );

    // A token that never claimed this turn — or claimed a different one — never
    // owns its segment.
    assert_eq!(
        store
            .fence_turn_lease(turn_id, uuid::Uuid::new_v4(), claimed_at)
            .await
            .unwrap(),
        TurnLeaseFence::Stale
    );
    let other_turn = TurnId::new();
    match store
        .accept_turn(other_turn, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::ChatBusy(_) => {}
        outcome => panic!("second turn should observe a busy chat: {outcome:?}"),
    }

    // A cancellation request keeps the same worker's lease live: the segment
    // still owns the turn and winds down under its own cancel signal.
    store
        .request_turn_cancellation(turn_id, claimed_at + chrono::Duration::seconds(2))
        .await
        .unwrap();
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelling
    );
    assert_eq!(
        store
            .fence_turn_lease(turn_id, token, claimed_at + chrono::Duration::seconds(3))
            .await
            .unwrap(),
        TurnLeaseFence::Current
    );

    // Once the expired lease is reclaimed at the attempt limit, the turn is
    // terminalized and the original token no longer owns anything.
    let past_expiry = expiry + chrono::Duration::seconds(1);
    let steal_token = uuid::Uuid::new_v4();
    let outcome = store
        .claim_turn(
            steal_token,
            past_expiry,
            past_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(outcome.turn.is_none());
    assert!(outcome.terminal_event.is_some());
    assert_eq!(
        store
            .fence_turn_lease(turn_id, token, past_expiry + chrono::Duration::seconds(1))
            .await
            .unwrap(),
        TurnLeaseFence::Stale
    );
}

#[tokio::test]
async fn an_expired_lease_handback_makes_the_turn_immediately_reclaimable() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    let expiry = claimed_at + chrono::Duration::minutes(1);
    let claimed = store
        .claim_turn(token, claimed_at, expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.lease_token, Some(token));

    // A wrong token hands nothing back; the live lease stays untouched.
    let handback_at = claimed_at + chrono::Duration::seconds(5);
    assert!(!store
        .expire_turn_lease(turn_id, uuid::Uuid::new_v4(), handback_at)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_turn(turn_id)
            .await
            .unwrap()
            .unwrap()
            .lease_expires_at,
        Some(expiry)
    );

    // The exact token hands the lease back: the expiry drops to now, and the
    // next scan — the relaunched process — reclaims without waiting it out.
    assert!(store
        .expire_turn_lease(turn_id, token, handback_at)
        .await
        .unwrap());
    let reclaim_at = handback_at + chrono::Duration::seconds(1);
    let second_token = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn(
            second_token,
            reclaim_at,
            reclaim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(reclaimed.id, turn_id);
    assert_eq!(reclaimed.status, TurnRunStatus::Running);
    assert_eq!(reclaimed.lease_token, Some(second_token));

    // Handing back an already-superseded lease is a no-op.
    assert!(!store
        .expire_turn_lease(turn_id, token, reclaim_at + chrono::Duration::seconds(1))
        .await
        .unwrap());
}

#[tokio::test]
async fn turn_claim_and_heartbeat_require_the_exact_live_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    assert!(store
        .claim_turn(
            uuid::Uuid::nil(),
            claimed_at,
            claimed_at + chrono::Duration::seconds(1)
        )
        .await
        .is_err());
    let first_token = uuid::Uuid::new_v4();
    assert!(store
        .claim_turn(first_token, claimed_at, claimed_at)
        .await
        .is_err());
    let first_expiry = claimed_at + chrono::Duration::minutes(1);
    let first = store
        .claim_turn(first_token, claimed_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(first.lease_token, Some(first_token));
    assert_eq!(
        store
            .claim_turn(first_token, claimed_at, first_expiry)
            .await
            .unwrap()
            .turn,
        Some(first.clone())
    );
    assert_eq!(first.status, TurnRunStatus::Running);
    assert_eq!(first.attempt_count, 1);
    assert_eq!(first.claim_count, 1);
    assert_eq!(first.started_at, Some(claimed_at));
    assert_eq!(first.lease_expires_at, Some(first_expiry));

    let heartbeat_at =
        claimed_at + chrono::Duration::seconds(10) + chrono::Duration::nanoseconds(900);
    let canonical_heartbeat_at =
        DateTime::<Utc>::from_timestamp_micros(heartbeat_at.timestamp_micros()).unwrap();
    assert!(!store
        .heartbeat_turn(
            turn_id,
            uuid::Uuid::new_v4(),
            heartbeat_at,
            first_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert!(!store
        .heartbeat_turn(turn_id, first_token, heartbeat_at, first_expiry)
        .await
        .unwrap());
    assert!(store
        .heartbeat_turn(turn_id, first_token, heartbeat_at, heartbeat_at)
        .await
        .is_err());

    let extended = first_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn(turn_id, first_token, heartbeat_at, extended)
        .await
        .unwrap());
    assert_eq!(
        store.get_turn(turn_id).await.unwrap().unwrap().updated_at,
        canonical_heartbeat_at
    );
    assert!(!store
        .heartbeat_turn(
            turn_id,
            first_token,
            heartbeat_at - chrono::Duration::seconds(1),
            extended + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert_eq!(
        store
            .claim_turn(
                first_token,
                extended,
                extended + chrono::Duration::minutes(1)
            )
            .await
            .unwrap()
            .turn,
        None
    );
    let second_expiry = extended + chrono::Duration::minutes(1);
    let second_token = uuid::Uuid::new_v4();
    let second = store
        .claim_turn(second_token, extended, second_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(second.id, turn_id);
    assert_eq!(second.status, TurnRunStatus::Running);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.claim_count, 2);
    assert_eq!(second.started_at, first.started_at);
    assert_eq!(second.lease_token, Some(second_token));
    assert_eq!(second.last_error_code, None);
    assert!(!store
        .heartbeat_turn(
            turn_id,
            first_token,
            extended + chrono::Duration::seconds(1),
            second_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());

    let exhausted = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            second_expiry,
            second_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(exhausted.turn, None);
    let terminal = exhausted
        .terminal_event
        .expect("exhausted attempt must publish a terminal event");
    assert_eq!(terminal.chat_id, chat.id);
    assert_eq!(terminal.turn_id, turn_id);
    assert_eq!(
        terminal.event.event,
        AgentEvent::TurnFailed {
            error: crate::error::AgentErrorInfo {
                kind: "lease_expired".into(),
                message: "final worker lease expired".into(),
            }
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap(),
        vec![terminal.event.clone()]
    );
    let failed = store.get_turn(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.attempt_count, 2);
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
    assert_eq!(failed.finished_at, Some(second_expiry));
    assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
    let recovered = store
        .record_turn_failure_and_append_event(
            turn_id,
            second_token,
            second_expiry + chrono::Duration::hours(1),
            TurnFailureRetry::Permanent,
            failed.model_steps,
            failed.usage,
            "lease_expired",
            Some("final worker lease expired"),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovered.outcome,
        RecordTurnFailureOutcome::Existing(_)
    ));
    assert_eq!(recovered.terminal_event, Some(terminal.event));

    let next_chat = sample_chat();
    store.create_chat(&next_chat).await.unwrap();
    let next = match store
        .accept_turn(TurnId::new(), next_chat.id, "gpt-5", "next")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let delayed_retry_at = next.available_at + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_turn(
                first_token,
                delayed_retry_at,
                delayed_retry_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn,
        None
    );
    assert_eq!(
        store.get_turn(next.id).await.unwrap().unwrap().status,
        TurnRunStatus::Queued
    );
}

#[tokio::test]
async fn resuming_turn_claims_a_new_lease_without_consuming_failure_budget() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let first_claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let first_token = uuid::Uuid::new_v4();
    let first = store
        .claim_turn(
            first_token,
            first_claimed_at,
            first_claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!((first.attempt_count, first.claim_count), (1, 1));
    assert_eq!(first.max_attempts, TurnRun::DEFAULT_MAX_ATTEMPTS);

    let resume_at = first_claimed_at + chrono::Duration::seconds(10);
    let parked = entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Resuming.as_str()),
        )
        .col_expr(
            entities::turn::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(resume_at),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(resume_at),
        )
        .filter(entities::turn::Column::Id.eq(turn_id.0))
        .filter(entities::turn::Column::LeaseToken.eq(first.lease_token))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(parked.rows_affected, 1);
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "must stay busy")
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(existing) if existing.id == turn_id
    ));

    let resumed_token = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn(
            resumed_token,
            resume_at,
            resume_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(resumed.id, turn_id);
    assert_eq!(resumed.status, TurnRunStatus::Running);
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    assert_eq!(resumed.max_attempts, TurnRun::DEFAULT_MAX_ATTEMPTS);

    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "resumed answer".into(),
        llm_content: None,
        created_at: resume_at + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn(turn_id, first_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        None,
        "the earlier lease segment must not complete the resumed turn"
    );
    assert!(matches!(
        store
            .complete_turn(turn_id, resumed_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert!(store
        .complete_turn(turn_id, first_token, 0, output.created_at, &output)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn turn_claim_rejects_a_receipt_for_an_attempt_that_never_advanced() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let accepted = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    entities::code_turn_claim::ActiveModel {
        token: Set(uuid::Uuid::new_v4()),
        turn_id: Set(accepted.id.0),
        owner: Set("local".into()),
        attempt_count: Set(1),
        claim_count: Set(1),
        claimed_at: Set(claimed_at),
        lease_expires_at: Set(claimed_at + chrono::Duration::minutes(1)),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        store.claim_turn(
            uuid::Uuid::new_v4(),
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        ),
    )
    .await
    .expect("claim must not spin on an inconsistent attempt receipt");
    let AgentError::Store(message) = result.unwrap_err() else {
        panic!("unexpected claim error")
    };
    assert!(message.contains("exists before the turn advanced"));
    assert_eq!(
        store.get_turn(accepted.id).await.unwrap().unwrap().status,
        TurnRunStatus::Queued
    );
}

#[tokio::test]
async fn claim_scan_terminalizes_an_expired_cancelling_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel and crash")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let expires_at = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert!(matches!(
        store
            .request_turn_cancellation(turn.id, claimed_at + chrono::Duration::seconds(1))
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Requested(_))
    ));

    let scan = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(scan.turn, None);
    let terminal = scan
        .terminal_event
        .expect("expired cancellation must publish a terminal event");
    assert_eq!(terminal.chat_id, chat.id);
    assert_eq!(terminal.turn_id, turn.id);
    assert_eq!(
        terminal.event.event,
        AgentEvent::TurnCancelled {
            usage: Usage::default()
        }
    );
    assert_eq!(
        store.list_events(chat.id, 0).await.unwrap(),
        vec![terminal.event.clone()]
    );
    let cancelled = store.get_turn(turn.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.finished_at, Some(expires_at));
    assert_eq!(cancelled.lease_token, None);
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            expires_at + chrono::Duration::hours(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        FinishTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal.event));
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "slot recovered")
            .await
            .unwrap(),
        AcceptTurnOutcome::Accepted(_)
    ));
}

#[tokio::test]
async fn claim_scan_rolls_back_terminal_state_when_event_append_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "expire and roll back")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    set_turn_max_attempts(&store, turn.id, 1).await;
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let expires_at = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn(token, claimed_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        session_id: Set(chat.id.0),
        owner: Set("local".to_owned()),
        seq: Set(1),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        event: Set(serde_json::to_value(crate::chat_journal::journal_row(
            &AgentEvent::TurnFailed {
                error: crate::error::AgentErrorInfo {
                    kind: "forced".into(),
                    message: "occupy terminal slot".into(),
                },
            },
        ))
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .claim_turn(
            uuid::Uuid::new_v4(),
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .is_err());
    let still_running = store.get_turn(turn.id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.lease_token, Some(token));
    assert_eq!(still_running.lease_expires_at, Some(expires_at));
    assert_eq!(still_running.finished_at, None);
    assert_eq!(still_running.last_error_code, None);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn claim_scan_returns_one_routable_terminal_action_at_a_time() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let first_turn = match store
        .accept_turn(TurnId::new(), first_chat.id, "gpt-5", "first expired turn")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let second_turn = match store
        .accept_turn(
            TurnId::new(),
            second_chat.id,
            "gpt-5",
            "second expired turn",
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    set_turn_max_attempts(&store, first_turn.id, 1).await;
    set_turn_max_attempts(&store, second_turn.id, 1).await;
    let claimed_at =
        first_turn.available_at.max(second_turn.available_at) + chrono::Duration::seconds(1);
    let expires_at = claimed_at + chrono::Duration::minutes(1);
    for _ in 0..2 {
        store
            .claim_turn(uuid::Uuid::new_v4(), claimed_at, expires_at)
            .await
            .unwrap()
            .turn
            .expect("both turns must be claimable");
    }

    let scan_token = uuid::Uuid::new_v4();
    let first_action = store
        .claim_turn(
            scan_token,
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(first_action.turn, None);
    assert_eq!(
        store
            .claim_turn(
                scan_token,
                expires_at + chrono::Duration::seconds(1),
                expires_at + chrono::Duration::minutes(2),
            )
            .await
            .unwrap(),
        first_action,
        "an ambiguous scan retry must recover the same routed event"
    );
    let second_action = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            expires_at + chrono::Duration::seconds(1),
            expires_at + chrono::Duration::minutes(2),
        )
        .await
        .unwrap();
    assert_eq!(second_action.turn, None);
    let routed = vec![
        first_action
            .terminal_event
            .expect("first scan must return its committed action"),
        second_action
            .terminal_event
            .expect("second scan must return its committed action"),
    ];
    assert_ne!(routed[0].turn_id, routed[1].turn_id);
    for terminal in routed {
        let expected_chat = if terminal.turn_id == first_turn.id {
            first_chat.id
        } else if terminal.turn_id == second_turn.id {
            second_chat.id
        } else {
            panic!("scan returned an unknown turn")
        };
        assert_eq!(terminal.chat_id, expected_chat);
        assert!(matches!(
            terminal.event.event,
            AgentEvent::TurnFailed { .. }
        ));
        assert_eq!(
            store.list_events(expected_chat, 0).await.unwrap(),
            vec![terminal.event]
        );
    }
}

#[tokio::test]
async fn stale_turn_attempt_cannot_complete_a_reclaimed_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = accepted.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    let first_token = uuid::Uuid::new_v4();
    store
        .claim_turn(first_token, first_claim_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let second_token = uuid::Uuid::new_v4();
    let second_expiry = first_expiry + chrono::Duration::minutes(1);
    let reclaimed = store
        .claim_turn(second_token, first_expiry, second_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(reclaimed.attempt_count, 2);

    let stale_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "stale answer".into(),
        llm_content: None,
        created_at: first_expiry + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn(
                turn_id,
                first_token,
                0,
                stale_output.created_at,
                &stale_output
            )
            .await
            .unwrap(),
        None
    );
    let output = Message {
        id: MessageId::new(),
        content: "current answer".into(),
        llm_content: None,
        ..stale_output
    };
    assert!(matches!(
        store
            .complete_turn(turn_id, second_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_turn_claimers_never_share_a_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let accepted = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let store = std::sync::Arc::new(store);
    let claim_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claim_at + chrono::Duration::minutes(1);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let lease_token = uuid::Uuid::new_v4();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .claim_turn(lease_token, claim_at, lease_expires_at)
                .await
        }));
    }

    let mut claimed = Vec::new();
    let mut empty = 0;
    for task in tasks {
        match task.await.unwrap().unwrap().turn {
            Some(turn) => claimed.push(turn),
            None => empty += 1,
        }
    }
    assert_eq!(claimed.len(), 1);
    assert_eq!(empty, 7);
    assert_eq!(claimed[0].id, accepted.id);
    assert_eq!(claimed[0].attempt_count, 1);
    assert!(claimed[0].lease_token.is_some());
}

#[tokio::test]
async fn turn_claim_orders_queued_and_expired_work_by_effective_due_time() {
    let (_dir, store) = temp_store().await;
    let expired_chat = sample_chat();
    store.create_chat(&expired_chat).await.unwrap();
    let expired_turn = match store
        .accept_turn(TurnId::new(), expired_chat.id, "gpt-5", "first")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(expired_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    store
        .claim_turn(uuid::Uuid::new_v4(), first_claim_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();

    let queued_chat = sample_chat();
    store.create_chat(&queued_chat).await.unwrap();
    let queued_turn = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "second")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let queued_due_at = first_expiry - chrono::Duration::seconds(1);
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .filter(entities::turn::Column::Id.eq(queued_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let claimed_queued = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            first_expiry,
            first_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed_queued.id, queued_turn.id);
    let reclaimed_expired = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            first_expiry,
            first_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(reclaimed_expired.id, expired_turn.id);
    assert_eq!(reclaimed_expired.attempt_count, 2);
}

#[tokio::test]
async fn turn_claim_prefers_an_earlier_expired_lease_over_queued_work() {
    let (_dir, store) = temp_store().await;
    let expired_chat = sample_chat();
    store.create_chat(&expired_chat).await.unwrap();
    let expired_turn = match store
        .accept_turn(TurnId::new(), expired_chat.id, "gpt-5", "first")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn::Column::Id.eq(expired_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    store
        .claim_turn(uuid::Uuid::new_v4(), first_claim_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();

    let queued_chat = sample_chat();
    store.create_chat(&queued_chat).await.unwrap();
    let queued_turn = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "second")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let queued_due_at = first_expiry + chrono::Duration::seconds(1);
    entities::turn::Entity::update_many()
        .col_expr(
            entities::turn::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .col_expr(
            entities::turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .filter(entities::turn::Column::Id.eq(queued_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let reclaimed = store
        .claim_turn(
            uuid::Uuid::new_v4(),
            queued_due_at,
            queued_due_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(reclaimed.id, expired_turn.id);
    assert_eq!(reclaimed.attempt_count, 2);
    assert_eq!(
        store
            .get_turn(queued_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Queued
    );
}

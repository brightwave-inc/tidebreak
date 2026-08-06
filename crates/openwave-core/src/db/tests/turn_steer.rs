use super::*;
use crate::storage::ApplyTurnSteerOutcome;

fn pending_turn_steer(
    turn: &crate::model::TurnRun,
    id: crate::id::TurnSteerId,
    content: &str,
    now: DateTime<Utc>,
) -> entities::turn_steer::ActiveModel {
    entities::turn_steer::ActiveModel {
        id: Set(id.0),
        turn_id: Set(turn.id.0),
        chat_id: Set(turn.chat_id.0),
        content: Set(content.into()),
        voice_input_used: Set(false),
        interrupt: Set(false),
        status: Set(TurnSteerStatus::Pending.as_str().into()),
        applied_lease_token: Set(None),
        message_id: Set(None),
        preceding_assistant_message_id: Set(None),
        created_at: Set(now),
        resolved_at: Set(None),
    }
}

#[tokio::test]
async fn turn_steer_schema_enforces_durable_delivery_identity() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued = store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap();
    let queued = match queued {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected acceptance: {other:?}"),
    };
    let now = Utc::now();
    let claim_token = uuid::Uuid::new_v4();
    let claimed = store
        .claim_turn_run(claim_token, now, now + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("turn claimed");
    assert_eq!(claimed.id, queued.id);

    let first_id = crate::id::TurnSteerId::new();
    pending_turn_steer(&claimed, first_id, "change course", now)
        .insert(&store.conn)
        .await
        .unwrap();

    let mut empty = pending_turn_steer(&claimed, crate::id::TurnSteerId::new(), "", now);
    assert!(empty.clone().insert(&store.conn).await.is_err());
    empty.content = Set("x".repeat(crate::model::TurnSteer::MAX_CONTENT_LEN + 1));
    assert!(empty.insert(&store.conn).await.is_err());

    let mut pending_with_resolution =
        pending_turn_steer(&claimed, crate::id::TurnSteerId::new(), "pending", now);
    pending_with_resolution.resolved_at = Set(Some(now));
    assert!(pending_with_resolution.insert(&store.conn).await.is_err());

    let mut applied_without_receipt =
        pending_turn_steer(&claimed, crate::id::TurnSteerId::new(), "applied", now);
    applied_without_receipt.status = Set(TurnSteerStatus::Applied.as_str().into());
    applied_without_receipt.resolved_at = Set(Some(now));
    assert!(applied_without_receipt.insert(&store.conn).await.is_err());

    let message_id = MessageId(first_id.0);
    entities::message::ActiveModel {
        id: Set(message_id.0),
        chat_id: Set(chat.id.0),
        turn_id: Set(claimed.id.0),
        seq: Set(2),
        role: Set("user".into()),
        reasoning: Default::default(),
        content: Set("change course".into()),
        llm_content: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    entities::turn_steer::Entity::update_many()
        .col_expr(
            entities::turn_steer::Column::Status,
            sea_orm::sea_query::Expr::value(TurnSteerStatus::Applied.as_str()),
        )
        .col_expr(
            entities::turn_steer::Column::AppliedLeaseToken,
            sea_orm::sea_query::Expr::value(Some(claim_token)),
        )
        .col_expr(
            entities::turn_steer::Column::MessageId,
            sea_orm::sea_query::Expr::value(Some(message_id.0)),
        )
        .col_expr(
            entities::turn_steer::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::turn_steer::Column::Id.eq(first_id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let applied = entities::turn_steer::Entity::find_by_id(first_id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(applied.status, TurnSteerStatus::Applied.as_str());
    assert_eq!(applied.applied_lease_token, Some(claim_token));
    assert_eq!(applied.message_id, Some(message_id.0));

    let mismatched_message_id = MessageId::new();
    entities::message::ActiveModel {
        id: Set(mismatched_message_id.0),
        chat_id: Set(chat.id.0),
        turn_id: Set(claimed.id.0),
        seq: Set(3),
        role: Set("user".into()),
        reasoning: Default::default(),
        content: Set("mismatched identity".into()),
        llm_content: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    let mut mismatched_receipt = pending_turn_steer(
        &claimed,
        crate::id::TurnSteerId::new(),
        "mismatched identity",
        now,
    );
    mismatched_receipt.status = Set(TurnSteerStatus::Applied.as_str().into());
    mismatched_receipt.applied_lease_token = Set(Some(claim_token));
    mismatched_receipt.message_id = Set(Some(mismatched_message_id.0));
    mismatched_receipt.resolved_at = Set(Some(now));
    assert!(mismatched_receipt.insert(&store.conn).await.is_err());

    let mut wrong_turn =
        pending_turn_steer(&claimed, crate::id::TurnSteerId::new(), "wrong turn", now);
    wrong_turn.turn_id = Set(TurnId::new().0);
    assert!(wrong_turn.insert(&store.conn).await.is_err());

    let mut rejected_with_message =
        pending_turn_steer(&claimed, crate::id::TurnSteerId::new(), "rejected", now);
    rejected_with_message.status = Set(TurnSteerStatus::Rejected.as_str().into());
    rejected_with_message.message_id = Set(Some(message_id.0));
    rejected_with_message.resolved_at = Set(Some(now));
    assert!(rejected_with_message.insert(&store.conn).await.is_err());
}

fn accepted_steer(outcome: AcceptTurnSteerOutcome) -> crate::model::TurnSteer {
    match outcome {
        AcceptTurnSteerOutcome::Accepted(steer) => steer,
        other => panic!("unexpected steer acceptance: {other:?}"),
    }
}

fn existing_steer(outcome: AcceptTurnSteerOutcome) -> crate::model::TurnSteer {
    match outcome {
        AcceptTurnSteerOutcome::Existing(steer) => steer,
        other => panic!("unexpected steer retry: {other:?}"),
    }
}

#[tokio::test]
async fn durable_turn_steer_applies_exactly_and_preserves_transcript_order() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let steer_id = crate::id::TurnSteerId::new();
    let pending = accepted_steer(
        store
            .accept_turn_steer_with_message_context(
                steer_id,
                turn.id,
                chat.id,
                "change course",
                false,
                true,
            )
            .await
            .unwrap(),
    );
    assert_eq!(pending.status, TurnSteerStatus::Pending);
    assert!(pending.voice_input_used);
    assert_eq!(pending.message_id, None);

    let claim_at = Utc::now();
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease_token,
            claim_at,
            claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("turn claimed");
    let pending = store
        .list_pending_turn_steers(turn.id, lease_token, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.iter().map(|steer| steer.id).collect::<Vec<_>>(),
        vec![steer_id]
    );
    assert_eq!(
        store
            .list_pending_turn_steers(turn.id, uuid::Uuid::new_v4(), Utc::now())
            .await
            .unwrap(),
        None
    );

    let mut document = sample_document(None);
    document.chat_id = Some(chat.id);
    store.create_document(&document).await.unwrap();
    let citation = crate::AssistantCitationInput {
        document_id: document.id,
        locator: crate::CitationLocator::Lines { start: 1, end: 1 },
    };
    let candidate = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: format!(
            "candidate before steer :cit[candidate]{{doc={} lines=1-1}}",
            document.id
        ),
        llm_content: None,
        created_at: Utc::now(),
    };
    let apply_at = Utc::now();
    let applied = store
        .apply_turn_steer(
            turn.id,
            lease_token,
            steer_id,
            1,
            Some(&candidate),
            std::slice::from_ref(&citation),
            apply_at,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        applied.event.event,
        AgentEvent::UserSteered {
            message_id: MessageId(steer_id.0),
            content: "change course".into(),
        }
    );
    let applied_event = applied.event.clone();
    let applied = match applied.outcome {
        ApplyTurnSteerOutcome::Applied(steer) => steer,
        other => panic!("unexpected steer application: {other:?}"),
    };
    assert_eq!(applied.status, TurnSteerStatus::Applied);
    assert_eq!(applied.applied_lease_token, Some(lease_token));
    assert_eq!(applied.message_id, Some(MessageId(steer_id.0)));
    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(transcript.citations.len(), 1);
    assert_eq!(transcript.citations[0].message_id, candidate.id);
    assert_eq!(
        applied.resolved_at,
        Some(DateTime::<Utc>::from_timestamp_micros(apply_at.timestamp_micros()).unwrap())
    );
    assert_eq!(
        store
            .get_turn_run(turn.id)
            .await
            .unwrap()
            .unwrap()
            .steer_revision,
        1
    );
    assert_eq!(
        store
            .list_pending_turn_steers(turn.id, lease_token, Utc::now())
            .await
            .unwrap(),
        Some(vec![])
    );

    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Role::User, "initial input"),
            (Role::Assistant, candidate.content.as_str()),
            (Role::User, "change course"),
        ]
    );
    assert_eq!(messages[2].created_at, applied.resolved_at.unwrap());
    assert!(messages[2]
        .llm_content
        .as_deref()
        .is_some_and(|content| content.contains("The user dictated this message")));

    let second_id = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(second_id, turn.id, chat.id, "one more thing", true)
            .await
            .unwrap(),
    );
    let completed_at = Utc::now();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "final answer".into(),
        llm_content: None,
        created_at: completed_at,
    };
    assert!(matches!(
        store
            .complete_turn_run(turn.id, lease_token, 1, completed_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::SteerPending(_))
    ));

    let recovered = store
        .apply_turn_steer(
            turn.id,
            lease_token,
            steer_id,
            1,
            Some(&candidate),
            std::slice::from_ref(&citation),
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        recovered.outcome,
        ApplyTurnSteerOutcome::Existing(_)
    ));
    assert_eq!(recovered.event, applied_event);
    assert!(store
        .apply_turn_steer(
            turn.id,
            lease_token,
            steer_id,
            1,
            Some(&candidate),
            &[crate::AssistantCitationInput {
                document_id: document.id,
                locator: crate::CitationLocator::Lines { start: 2, end: 2 },
            }],
            Utc::now(),
        )
        .await
        .is_err());
    assert!(store
        .apply_turn_steer(turn.id, lease_token, steer_id, 1, None, &[], Utc::now(),)
        .await
        .is_err());
    let second = store
        .apply_turn_steer(turn.id, lease_token, second_id, 2, None, &[], Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second.event.event,
        AgentEvent::UserSteered {
            message_id: MessageId(second_id.0),
            content: "one more thing".into(),
        }
    );
    let second = match second.outcome {
        ApplyTurnSteerOutcome::Applied(steer) => steer,
        other => panic!("unexpected second steer application: {other:?}"),
    };
    assert_eq!(
        store
            .get_turn_run(turn.id)
            .await
            .unwrap()
            .unwrap()
            .steer_revision,
        2
    );
    let stale_after_apply_at = second.resolved_at.unwrap() + chrono::Duration::microseconds(1);
    let stale_after_apply = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "generated from revision one".into(),
        llm_content: None,
        created_at: stale_after_apply_at,
    };
    assert!(matches!(
        store
            .complete_turn_run(
                turn.id,
                lease_token,
                1,
                stale_after_apply_at,
                &stale_after_apply,
            )
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::OutputSuperseded(_))
    ));
    let fresh_completed_at = second.resolved_at.unwrap() + chrono::Duration::microseconds(1);
    let fresh_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: format!("fresh final :cit[answer]{{doc={} lines=1-1}}", document.id),
        llm_content: None,
        created_at: fresh_completed_at,
    };
    assert!(matches!(
        store
            .complete_turn_run_with_citations_and_append_event(
                turn.id,
                lease_token,
                2,
                fresh_completed_at,
                &fresh_output,
                std::slice::from_ref(&citation),
                Usage::default(),
                StopReason::EndTurn,
            )
            .await
            .unwrap(),
        Some(JournaledTurnOutcome {
            outcome: CompleteTurnRunOutcome::Completed(_),
            terminal_event: Some(_),
        })
    ));
    assert!(matches!(
        store
            .complete_turn_run_with_citations_and_append_event(
                turn.id,
                lease_token,
                2,
                Utc::now(),
                &fresh_output,
                &[citation],
                Usage::default(),
                StopReason::EndTurn,
            )
            .await
            .unwrap(),
        Some(JournaledTurnOutcome {
            outcome: CompleteTurnRunOutcome::Existing(_),
            terminal_event: Some(_),
        })
    ));
    assert!(store
        .complete_turn_run_with_citations_and_append_event(
            turn.id,
            lease_token,
            2,
            Utc::now(),
            &fresh_output,
            &[],
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .is_err());
    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(transcript.citations.len(), 2);
    assert!(transcript
        .citations
        .iter()
        .any(|citation| citation.message_id == fresh_output.id));
    assert!(matches!(
        store
            .accept_turn_steer(
                crate::id::TurnSteerId::new(),
                turn.id,
                chat.id,
                "too late",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::TurnUnavailable
    ));
}

#[tokio::test]
async fn turn_steer_admission_validates_identity_payload_and_monotonic_time() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    assert!(store
        .accept_turn_steer(
            crate::id::TurnSteerId(uuid::Uuid::nil()),
            turn.id,
            chat.id,
            "valid",
            false,
        )
        .await
        .is_err());
    for invalid in ["", "   ", "nul\0content"] {
        assert!(store
            .accept_turn_steer(
                crate::id::TurnSteerId::new(),
                turn.id,
                chat.id,
                invalid,
                false,
            )
            .await
            .is_err());
    }
    assert!(store
        .accept_turn_steer(
            crate::id::TurnSteerId::new(),
            turn.id,
            chat.id,
            &"x".repeat(crate::model::TurnSteer::MAX_CONTENT_LEN + 1),
            false,
        )
        .await
        .is_err());

    let id = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(id, turn.id, chat.id, "exact", false)
            .await
            .unwrap(),
    );
    assert_eq!(
        existing_steer(
            store
                .accept_turn_steer(id, turn.id, chat.id, "exact", false)
                .await
                .unwrap()
        )
        .id,
        id
    );
    assert!(matches!(
        store
            .accept_turn_steer(id, turn.id, chat.id, "different", false)
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn_steer(id, turn.id, chat.id, "exact", true)
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn_steer_with_message_context(id, turn.id, chat.id, "exact", false, true,)
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn_steer(id, TurnId::new(), ChatId::new(), "exact", false,)
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::IdentityConflict
    ));

    let collision_id = crate::id::TurnSteerId::new();
    store
        .append_message(&Message {
            id: MessageId(collision_id.0),
            chat_id: chat.id,
            turn_id: turn.id,
            role: Role::User,
            reasoning: Default::default(),
            content: "already used".into(),
            llm_content: None,
            created_at: Utc::now(),
        })
        .await
        .unwrap();
    assert!(matches!(
        store
            .accept_turn_steer(collision_id, turn.id, chat.id, "collision", false)
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::IdentityConflict
    ));

    let future = DateTime::from_timestamp_micros(
        (Utc::now() + chrono::Duration::hours(1)).timestamp_micros(),
    )
    .unwrap();
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(future),
        )
        .filter(entities::turn_run::Column::Id.eq(turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(matches!(
        store
            .accept_turn_steer(
                crate::id::TurnSteerId::new(),
                turn.id,
                chat.id,
                "clock skew",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::TurnUnavailable
    ));
    assert_eq!(
        store
            .get_turn_run(turn.id)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        future
    );
}

#[tokio::test]
async fn turn_steer_application_enforces_fifo_and_message_sequence_on_timestamp_ties() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let high_id = crate::id::TurnSteerId(uuid::Uuid::from_u128(2));
    let low_id = crate::id::TurnSteerId(uuid::Uuid::from_u128(1));
    let high = accepted_steer(
        store
            .accept_turn_steer(high_id, turn.id, chat.id, "high id", false)
            .await
            .unwrap(),
    );
    let low = accepted_steer(
        store
            .accept_turn_steer(low_id, turn.id, chat.id, "low id", false)
            .await
            .unwrap(),
    );
    let tied_at = high.created_at.min(low.created_at);
    entities::turn_steer::Entity::update_many()
        .col_expr(
            entities::turn_steer::Column::CreatedAt,
            sea_orm::sea_query::Expr::value(tied_at),
        )
        .filter(entities::turn_steer::Column::Id.is_in([high_id.0, low_id.0]))
        .exec(&store.conn)
        .await
        .unwrap();

    let lease = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let pending = store
        .list_pending_turn_steers(turn.id, lease, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.iter().map(|steer| steer.id).collect::<Vec<_>>(),
        vec![low_id, high_id]
    );
    let applied_at = Utc::now();
    assert_eq!(
        store
            .apply_turn_steer(turn.id, lease, high_id, 1, None, &[], applied_at)
            .await
            .unwrap(),
        None
    );
    let low = store
        .apply_turn_steer(turn.id, lease, low_id, 1, None, &[], applied_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(low.outcome, ApplyTurnSteerOutcome::Applied(_)));
    let high = store
        .apply_turn_steer(turn.id, lease, high_id, 2, None, &[], applied_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(high.outcome, ApplyTurnSteerOutcome::Applied(_)));
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["initial input", "low id", "high id"]
    );
}

#[tokio::test]
async fn concurrent_apply_and_completion_leave_no_pending_steer() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claim_at = Utc::now();
    store
        .claim_turn_run(
            lease_token,
            claim_at,
            claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let steer_id = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "race", true)
            .await
            .unwrap(),
    );
    let stale_output_at = Utc::now();
    let stale_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "stale completion".into(),
        llm_content: None,
        created_at: stale_output_at,
    };

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let apply_store = store.clone();
    let apply_barrier = barrier.clone();
    let apply = tokio::spawn(async move {
        apply_barrier.wait().await;
        apply_store
            .apply_turn_steer(turn.id, lease_token, steer_id, 1, None, &[], Utc::now())
            .await
            .unwrap()
    });
    let complete_store = store.clone();
    let complete_barrier = barrier.clone();
    let complete = tokio::spawn(async move {
        complete_barrier.wait().await;
        complete_store
            .complete_turn_run(turn.id, lease_token, 0, Utc::now(), &stale_output)
            .await
            .unwrap()
    });
    let applied = apply.await.unwrap().unwrap();
    let applied = match applied.outcome {
        ApplyTurnSteerOutcome::Applied(steer) => steer,
        other => panic!("pending steer did not win completion race: {other:?}"),
    };
    assert!(matches!(
        complete.await.unwrap(),
        Some(CompleteTurnRunOutcome::SteerPending(_))
            | Some(CompleteTurnRunOutcome::OutputSuperseded(_))
    ));

    let steer = existing_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "race", true)
            .await
            .unwrap(),
    );
    assert_eq!(steer.status, TurnSteerStatus::Applied);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);

    let completed_at = applied.resolved_at.unwrap() + chrono::Duration::microseconds(1);
    assert!(store
        .complete_turn_run(
            turn.id,
            lease_token,
            1,
            completed_at,
            &Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id: turn.id,
                role: Role::Assistant,
                reasoning: Default::default(),
                content: "fresh completion".into(),
                llm_content: None,
                created_at: completed_at,
            },
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 3);
}

#[tokio::test]
async fn concurrent_turn_steer_admission_converges_by_exact_identity() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let steer_id = crate::id::TurnSteerId::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn_steer(steer_id, turn.id, chat.id, "same request", true)
                .await
                .unwrap()
        }));
    }
    let mut accepted = 0;
    let mut existing = 0;
    for task in tasks {
        match task.await.unwrap() {
            AcceptTurnSteerOutcome::Accepted(_) => accepted += 1,
            AcceptTurnSteerOutcome::Existing(_) => existing += 1,
            other => panic!("unexpected concurrent outcome: {other:?}"),
        }
    }
    assert_eq!((accepted, existing), (1, 7));

    let conflict_id = crate::id::TurnSteerId::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for content in ["first payload", "second payload"] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn_steer(conflict_id, turn.id, chat.id, content, false)
                .await
                .unwrap()
        }));
    }
    let outcomes = futures::future::join_all(tasks)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, AcceptTurnSteerOutcome::Accepted(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, AcceptTurnSteerOutcome::IdentityConflict))
            .count(),
        1
    );
}

#[tokio::test]
async fn concurrent_message_and_steer_reserve_one_shared_identity() {
    let (_dir, store) = temp_store().await;
    let steer_chat = sample_chat();
    let message_chat = sample_chat();
    store.create_chat(&steer_chat).await.unwrap();
    store.create_chat(&message_chat).await.unwrap();
    let steer_turn = match store
        .accept_turn(TurnId::new(), steer_chat.id, "gpt-5", "steer turn")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    assert_eq!(
        store
            .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
            .await
            .unwrap()
            .turn
            .unwrap()
            .id,
        steer_turn.id
    );
    let message_turn = match store
        .accept_turn(TurnId::new(), message_chat.id, "gpt-5", "message turn")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let shared_id = crate::id::TurnSteerId::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let steer_store = store.clone();
    let steer_barrier = barrier.clone();
    let steer = tokio::spawn(async move {
        steer_barrier.wait().await;
        steer_store
            .accept_turn_steer(
                shared_id,
                steer_turn.id,
                steer_chat.id,
                "shared identity",
                false,
            )
            .await
            .unwrap()
    });
    let message_store = store.clone();
    let message_barrier = barrier.clone();
    let message = tokio::spawn(async move {
        message_barrier.wait().await;
        message_store
            .append_message(&Message {
                id: MessageId(shared_id.0),
                chat_id: message_chat.id,
                turn_id: message_turn.id,
                role: Role::Assistant,
                reasoning: Default::default(),
                content: "shared identity".into(),
                llm_content: None,
                created_at: Utc::now(),
            })
            .await
    });
    let steer = steer.await.unwrap();
    let message = message.await.unwrap();
    match steer {
        AcceptTurnSteerOutcome::Accepted(_) => {
            assert!(message.is_err());
            let applied = store
                .apply_turn_steer(steer_turn.id, lease, shared_id, 1, None, &[], Utc::now())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(applied.outcome, ApplyTurnSteerOutcome::Applied(_)));
        }
        AcceptTurnSteerOutcome::IdentityConflict => {
            message.expect("message won the shared reservation");
            assert!(matches!(
                store
                    .accept_turn_steer(
                        shared_id,
                        steer_turn.id,
                        steer_chat.id,
                        "shared identity",
                        false,
                    )
                    .await
                    .unwrap(),
                AcceptTurnSteerOutcome::IdentityConflict
            ));
        }
        other => panic!("unexpected steer reservation outcome: {other:?}"),
    }
}

#[tokio::test]
async fn terminal_turn_paths_reject_pending_steers_but_retry_wait_preserves_them() {
    let (_dir, store) = temp_store().await;

    let cancelled_chat = sample_chat();
    store.create_chat(&cancelled_chat).await.unwrap();
    let cancelled_turn = match store
        .accept_turn(TurnId::new(), cancelled_chat.id, "gpt-5", "cancel queued")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let cancelled_steer = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(
                cancelled_steer,
                cancelled_turn.id,
                cancelled_chat.id,
                "pending cancellation",
                false,
            )
            .await
            .unwrap(),
    );
    store
        .request_turn_cancellation(cancelled_turn.id, Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        existing_steer(
            store
                .accept_turn_steer(
                    cancelled_steer,
                    cancelled_turn.id,
                    cancelled_chat.id,
                    "pending cancellation",
                    false,
                )
                .await
                .unwrap()
        )
        .status,
        TurnSteerStatus::Rejected
    );

    let retry_chat = sample_chat();
    store.create_chat(&retry_chat).await.unwrap();
    let retry_turn = match store
        .accept_turn(TurnId::new(), retry_chat.id, "gpt-5", "retry once")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(retry_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_lease = uuid::Uuid::new_v4();
    let first_claim_at = Utc::now();
    store
        .claim_turn_run(
            first_lease,
            first_claim_at,
            first_claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let retry_steer = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(
                retry_steer,
                retry_turn.id,
                retry_chat.id,
                "survive retry",
                false,
            )
            .await
            .unwrap(),
    );
    let failed_at = Utc::now();
    let retry_at = failed_at + chrono::Duration::seconds(1);
    let failure = store
        .record_turn_run_failure(
            retry_turn.id,
            first_lease,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "retryable",
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        failure,
        RecordTurnFailureOutcome::Recorded(receipt)
            if receipt.result_status == TurnRunStatus::RetryWait
    ));
    assert_eq!(
        existing_steer(
            store
                .accept_turn_steer(
                    retry_steer,
                    retry_turn.id,
                    retry_chat.id,
                    "survive retry",
                    false,
                )
                .await
                .unwrap()
        )
        .status,
        TurnSteerStatus::Pending
    );
    let second_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            second_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(
        store
            .list_pending_turn_steers(retry_turn.id, second_lease, retry_at)
            .await
            .unwrap()
            .unwrap()
            .iter()
            .map(|steer| steer.id)
            .collect::<Vec<_>>(),
        vec![retry_steer]
    );
    let terminal_at = retry_at + chrono::Duration::seconds(1);
    store
        .record_turn_run_failure(
            retry_turn.id,
            second_lease,
            terminal_at,
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "permanent",
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        existing_steer(
            store
                .accept_turn_steer(
                    retry_steer,
                    retry_turn.id,
                    retry_chat.id,
                    "survive retry",
                    false,
                )
                .await
                .unwrap()
        )
        .status,
        TurnSteerStatus::Rejected
    );

    let expired_chat = sample_chat();
    store.create_chat(&expired_chat).await.unwrap();
    let expired_turn = match store
        .accept_turn(TurnId::new(), expired_chat.id, "gpt-5", "expire")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    set_turn_max_attempts(&store, expired_turn.id, 1).await;
    let expired_lease = uuid::Uuid::new_v4();
    let expired_claim_at = Utc::now();
    let expires_at = expired_claim_at + chrono::Duration::seconds(1);
    store
        .claim_turn_run(expired_lease, expired_claim_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let expired_steer = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(
                expired_steer,
                expired_turn.id,
                expired_chat.id,
                "scanner pending",
                true,
            )
            .await
            .unwrap(),
    );
    store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .terminal_event
        .expect("scanner terminalized final attempt");
    assert_eq!(
        existing_steer(
            store
                .accept_turn_steer(
                    expired_steer,
                    expired_turn.id,
                    expired_chat.id,
                    "scanner pending",
                    true,
                )
                .await
                .unwrap()
        )
        .status,
        TurnSteerStatus::Rejected
    );
}

#[tokio::test]
async fn failed_steer_message_insert_rolls_back_the_application_receipt() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let steer_id = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "will collide", false)
            .await
            .unwrap(),
    );
    entities::message::ActiveModel {
        id: Set(steer_id.0),
        chat_id: Set(chat.id.0),
        turn_id: Set(turn.id.0),
        seq: Set(2),
        role: Set("assistant".into()),
        reasoning: Default::default(),
        content: Set("occupy identity".into()),
        llm_content: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(store
        .apply_turn_steer(turn.id, lease, steer_id, 1, None, &[], Utc::now())
        .await
        .is_err());
    let pending = existing_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "will collide", false)
            .await
            .unwrap(),
    );
    assert_eq!(pending.status, TurnSteerStatus::Pending);
    assert_eq!(pending.applied_lease_token, None);
    assert_eq!(pending.message_id, None);
    assert_eq!(pending.resolved_at, None);
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn failed_steer_event_insert_rolls_back_message_receipt_and_revision() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "initial input")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        other => panic!("unexpected turn acceptance: {other:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let steer_id = crate::id::TurnSteerId::new();
    accepted_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "must stay atomic", false)
            .await
            .unwrap(),
    );
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_steer_event BEFORE INSERT ON event
             BEGIN SELECT RAISE(FAIL, 'injected steer event failure'); END",
        )
        .await
        .unwrap();

    assert!(store
        .apply_turn_steer(turn.id, lease, steer_id, 1, None, &[], Utc::now())
        .await
        .is_err());

    let pending = existing_steer(
        store
            .accept_turn_steer(steer_id, turn.id, chat.id, "must stay atomic", false)
            .await
            .unwrap(),
    );
    assert_eq!(pending.status, TurnSteerStatus::Pending);
    assert_eq!(pending.applied_lease_token, None);
    assert_eq!(pending.message_id, None);
    assert_eq!(pending.resolved_at, None);
    assert_eq!(
        store
            .get_turn_run(turn.id)
            .await
            .unwrap()
            .unwrap()
            .steer_revision,
        0
    );
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["initial input"]
    );
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
}

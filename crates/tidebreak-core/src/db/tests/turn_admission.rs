use super::*;

async fn make_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    model: &str,
    now: DateTime<Utc>,
) -> entities::code_turn::ActiveModel {
    let turn_id = TurnId::new();
    let input_message_id = MessageId::new();
    let seq = super::ops::conversation::next_message_seq_on(&store.conn, chat_id)
        .await
        .unwrap();
    entities::message::ActiveModel {
        id: Set(input_message_id.0),
        chat_id: Set(chat_id.0),
        turn_id: Set(turn_id.0),
        seq: Set(seq),
        role: Set("user".into()),
        reasoning: Default::default(),
        content: Set("turn input".into()),
        llm_content: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    let session = entities::code_session::Entity::find_by_id(chat_id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    let last = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(chat_id.0))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .one(&store.conn)
        .await
        .unwrap();
    let ordinal = last.map_or(1, |row| row.ordinal + 1);
    entities::code_turn::ActiveModel {
        id: Set(turn_id.0),
        owner: Set(session.owner),
        session_id: Set(chat_id.0),
        ordinal: Set(ordinal),
        status: Set(TurnRunStatus::Queued.as_str().into()),
        model: Set(Some(model.into())),
        fast_mode: Set(false),
        user_input: Set("turn input".into()),
        user_input_blob_id: Set(None),
        checkpoint_ref: Set(None),
        diffstat: Set(None),
        usage: Set(None),
        narrative: Set(None),
        rewrite: Set(None),
        started_at: Set(now),
        ended_at: Set(None),
        park_ref: Set(None),
        park_wait: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(crate::model::TurnRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        model_steps: Set(0),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        available_at: Set(Some(now)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        steer_revision: Set(0),
        last_steer_applied_at: Set(None),
        invoked_skills: Set(serde_json::json!([])),
        voice_input_used: Set(false),
        input_message_id: Set(Some(input_message_id.0)),
        output_message_id: Set(None),
        updated_at: Set(Some(now)),
        fingerprint: Set(None),
    }
}

#[tokio::test]
async fn turn_run_schema_enforces_delivery_and_single_writer_invariants() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let now = DateTime::<Utc>::from_timestamp(1_752_408_000, 0).unwrap();
    let first = make_queued_turn(&store, chat.id, "claude-sonnet-4-5", now)
        .await
        .insert(&store.conn)
        .await
        .unwrap();
    let stored = store.get_turn(TurnId(first.id)).await.unwrap().unwrap();
    assert_eq!(stored.id, TurnId(first.id));
    assert_eq!(stored.chat_id, chat.id);
    assert_eq!(
        stored.agent_run_id,
        crate::id::AgentRunId::foreground_for_chat(chat.id)
    );
    assert_eq!(stored.model, "claude-sonnet-4-5");
    assert_eq!(stored.status, TurnRunStatus::Queued);
    assert_eq!(stored.model_steps, 0);
    assert_eq!(stored.usage, crate::provider::Usage::default());
    assert_eq!(store.list_turns(chat.id).await.unwrap(), vec![stored]);

    // The database, not a process-local map, owns the one-live-turn invariant.
    assert!(make_queued_turn(&store, chat.id, "gpt-5", now)
        .await
        .insert(&store.conn)
        .await
        .is_err());

    let first_output_id = MessageId::new();
    let first_output_seq = super::ops::conversation::next_message_seq_on(&store.conn, chat.id)
        .await
        .unwrap();
    entities::message::ActiveModel {
        id: Set(first_output_id.0),
        chat_id: Set(chat.id.0),
        turn_id: Set(first.id),
        seq: Set(first_output_seq),
        role: Set("assistant".into()),
        reasoning: Default::default(),
        content: Set("done".into()),
        llm_content: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::code_turn::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::code_turn::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::code_turn::Column::OutputMessageId,
            sea_orm::sea_query::Expr::value(Some(first_output_id.0)),
        )
        .col_expr(
            entities::code_turn::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::code_turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::code_turn::Column::Id.eq(first.id))
        .exec(&store.conn)
        .await
        .unwrap();
    make_queued_turn(&store, chat.id, "gpt-5", now)
        .await
        .insert(&store.conn)
        .await
        .unwrap();

    let invalid_chat = sample_chat();
    store.create_chat(&invalid_chat).await.unwrap();

    let mut negative_accounting = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    negative_accounting.input_tokens = Set(-1);
    assert!(negative_accounting.insert(&store.conn).await.is_err());
    let mut oversized_accounting = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    oversized_accounting.input_tokens = Set(i64::from(u32::MAX) + 1);
    assert!(oversized_accounting.insert(&store.conn).await.is_err());

    let mut running_without_lease = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    running_without_lease.status = Set(TurnRunStatus::Running.as_str().into());
    running_without_lease.attempt_count = Set(1);
    running_without_lease.claim_count = Set(1);
    running_without_lease.started_at = Set(now);
    assert!(running_without_lease.insert(&store.conn).await.is_err());

    let mut cancelling_without_lease =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    cancelling_without_lease.status = Set(TurnRunStatus::Cancelling.as_str().into());
    cancelling_without_lease.attempt_count = Set(1);
    cancelling_without_lease.claim_count = Set(1);
    cancelling_without_lease.started_at = Set(now);
    assert!(cancelling_without_lease.insert(&store.conn).await.is_err());

    let mut retry_without_error = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    retry_without_error.status = Set(TurnRunStatus::RetryWait.as_str().into());
    retry_without_error.attempt_count = Set(1);
    retry_without_error.claim_count = Set(1);
    retry_without_error.max_attempts = Set(2);
    retry_without_error.started_at = Set(now);
    assert!(retry_without_error.insert(&store.conn).await.is_err());

    let mut failed_without_finish = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    failed_without_finish.status = Set(TurnRunStatus::Failed.as_str().into());
    failed_without_finish.attempt_count = Set(1);
    failed_without_finish.claim_count = Set(1);
    failed_without_finish.started_at = Set(now);
    failed_without_finish.last_error_code = Set(Some("provider_error".into()));
    assert!(failed_without_finish.insert(&store.conn).await.is_err());

    let mut completed_without_output =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    completed_without_output.status = Set(TurnRunStatus::Completed.as_str().into());
    completed_without_output.attempt_count = Set(1);
    completed_without_output.claim_count = Set(1);
    completed_without_output.started_at = Set(now);
    completed_without_output.ended_at = Set(Some(now));
    assert!(completed_without_output.insert(&store.conn).await.is_err());

    let mut queued_with_output = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    queued_with_output.output_message_id = Set(Some(first_output_id.0));
    assert!(queued_with_output.insert(&store.conn).await.is_err());

    let mut completed_with_wrong_output =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    completed_with_wrong_output.status = Set(TurnRunStatus::Completed.as_str().into());
    completed_with_wrong_output.attempt_count = Set(1);
    completed_with_wrong_output.claim_count = Set(1);
    completed_with_wrong_output.started_at = Set(now);
    completed_with_wrong_output.ended_at = Set(Some(now));
    completed_with_wrong_output.output_message_id = Set(Some(first_output_id.0));
    assert!(completed_with_wrong_output
        .insert(&store.conn)
        .await
        .is_err());

    let mut unknown_status = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    unknown_status.status = Set("waiting_for_magic".into());
    assert!(unknown_status.insert(&store.conn).await.is_err());
    let mut negative_steer_revision = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    negative_steer_revision.steer_revision = Set(-1);
    assert!(negative_steer_revision.insert(&store.conn).await.is_err());
    let mut missing_steer_timestamp = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    missing_steer_timestamp.steer_revision = Set(1);
    assert!(missing_steer_timestamp.insert(&store.conn).await.is_err());
    assert!(make_queued_turn(&store, invalid_chat.id, "", now)
        .await
        .insert(&store.conn)
        .await
        .is_err());
    assert!(make_queued_turn(
        &store,
        invalid_chat.id,
        &"m".repeat(crate::model::TurnRun::MAX_MODEL_LEN + 1),
        now,
    )
    .await
    .insert(&store.conn)
    .await
    .is_err());

    let mut oversized_error = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    oversized_error.status = Set(TurnRunStatus::Failed.as_str().into());
    oversized_error.attempt_count = Set(1);
    oversized_error.claim_count = Set(1);
    oversized_error.started_at = Set(now);
    oversized_error.ended_at = Set(Some(now));
    oversized_error.last_error_code = Set(Some(
        "e".repeat(crate::model::TurnRun::MAX_ERROR_CODE_LEN + 1),
    ));
    assert!(oversized_error.insert(&store.conn).await.is_err());

    // The turn identity is global and cannot be replayed against another chat.
    let other = sample_chat();
    store.create_chat(&other).await.unwrap();
    let mut duplicate_identity = make_queued_turn(&store, other.id, "gpt-5", now).await;
    duplicate_identity.id = Set(first.id);
    assert!(duplicate_identity.insert(&store.conn).await.is_err());

    // Every valid non-queued state is representable; later transition methods
    // must reach these shapes atomically under exact predicates.
    let running_chat = sample_chat();
    store.create_chat(&running_chat).await.unwrap();
    let mut running = make_queued_turn(&store, running_chat.id, "gpt-5", now).await;
    let running_turn_id = running.id.clone().unwrap();
    running.status = Set(TurnRunStatus::Running.as_str().into());
    running.attempt_count = Set(1);
    running.claim_count = Set(1);
    running.started_at = Set(now);
    let running_token = uuid::Uuid::new_v4();
    running.lease_token = Set(Some(running_token));
    running.lease_expires_at = Set(Some(now + chrono::Duration::minutes(1)));
    entities::code_turn_claim::ActiveModel {
        token: Set(running_token),
        turn_id: Set(running_turn_id),
        owner: Set("local".into()),
        attempt_count: Set(1),
        claim_count: Set(1),
        claimed_at: Set(now),
        lease_expires_at: Set(now + chrono::Duration::minutes(1)),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    running.insert(&store.conn).await.unwrap();
    entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Cancelling.as_str()),
        )
        .filter(entities::code_turn::Column::Id.eq(running_turn_id))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(make_queued_turn(&store, running_chat.id, "gpt-5", now)
        .await
        .insert(&store.conn)
        .await
        .is_err());

    let valid_failure = entities::code_turn_failure::ActiveModel {
        lease_token: Set(running_token),
        turn_id: Set(running_turn_id),
        owner: Set("local".into()),
        attempt_count: Set(1),
        model_steps: Set(0),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        requested_retry_at: Set(Some(now + chrono::Duration::minutes(2))),
        error_code: Set("provider_unavailable".into()),
        error_detail: Set(Some("temporary outage".into())),
        resolved_at: Set(now + chrono::Duration::seconds(1)),
        result_status: Set(TurnRunStatus::RetryWait.as_str().into()),
    };
    let mut retry_without_time = valid_failure.clone();
    retry_without_time.requested_retry_at = Set(None);
    assert!(retry_without_time.insert(&store.conn).await.is_err());
    let mut nonfuture_retry = valid_failure.clone();
    nonfuture_retry.requested_retry_at = Set(Some(now));
    assert!(nonfuture_retry.insert(&store.conn).await.is_err());
    let mut unknown_failure_status = valid_failure.clone();
    unknown_failure_status.result_status = Set("lost".into());
    assert!(unknown_failure_status.insert(&store.conn).await.is_err());
    let mut mismatched_failure_claim = valid_failure.clone();
    mismatched_failure_claim.attempt_count = Set(2);
    assert!(mismatched_failure_claim.insert(&store.conn).await.is_err());
    let mut negative_failure_steps = valid_failure.clone();
    negative_failure_steps.model_steps = Set(-1);
    assert!(negative_failure_steps.insert(&store.conn).await.is_err());
    store
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO code_turn_failure (
                lease_token, turn_id, owner, attempt_count, model_steps,
                input_tokens, output_tokens, cache_read_input_tokens,
                cache_creation_input_tokens, requested_retry_at, error_code,
                error_detail, resolved_at, result_status
            ) VALUES (
                '{running_token}', '{running_turn_id}', 'local', 1, {},
                0, 0, 0, 0, '{}', 'provider_unavailable',
                NULL, '{}', '{}'
            )",
            i64::from(i32::MAX) + 1,
            (now + chrono::Duration::minutes(2)).to_rfc3339(),
            (now + chrono::Duration::seconds(1)).to_rfc3339(),
            TurnRunStatus::RetryWait.as_str(),
        ))
        .await
        .expect_err("failure model steps above i32::MAX must be rejected");
    let mut oversized_failure_usage = valid_failure.clone();
    oversized_failure_usage.input_tokens = Set(i64::from(u32::MAX) + 1);
    assert!(oversized_failure_usage.insert(&store.conn).await.is_err());
    valid_failure.insert(&store.conn).await.unwrap();

    assert!(entities::code_turn_claim::ActiveModel {
        token: Set(uuid::Uuid::new_v4()),
        turn_id: Set(running_turn_id),
        owner: Set("local".into()),
        attempt_count: Set(1),
        claim_count: Set(1),
        claimed_at: Set(now),
        lease_expires_at: Set(now + chrono::Duration::minutes(1)),
    }
    .insert(&store.conn)
    .await
    .is_err());

    let duplicate_lease_chat = sample_chat();
    store.create_chat(&duplicate_lease_chat).await.unwrap();
    let mut duplicate_lease = make_queued_turn(&store, duplicate_lease_chat.id, "gpt-5", now).await;
    duplicate_lease.status = Set(TurnRunStatus::Running.as_str().into());
    duplicate_lease.attempt_count = Set(1);
    duplicate_lease.claim_count = Set(1);
    duplicate_lease.started_at = Set(now);
    duplicate_lease.lease_token = Set(Some(running_token));
    duplicate_lease.lease_expires_at = Set(Some(now + chrono::Duration::minutes(1)));
    assert!(duplicate_lease.insert(&store.conn).await.is_err());

    let mismatched_receipt_chat = sample_chat();
    store.create_chat(&mismatched_receipt_chat).await.unwrap();
    let mut mismatched_receipt =
        make_queued_turn(&store, mismatched_receipt_chat.id, "gpt-5", now).await;
    let mismatched_turn_id = mismatched_receipt.id.clone().unwrap();
    let mismatched_token = uuid::Uuid::new_v4();
    entities::code_turn_claim::ActiveModel {
        token: Set(mismatched_token),
        turn_id: Set(mismatched_turn_id),
        owner: Set("local".into()),
        attempt_count: Set(2),
        claim_count: Set(2),
        claimed_at: Set(now),
        lease_expires_at: Set(now + chrono::Duration::minutes(1)),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    mismatched_receipt.status = Set(TurnRunStatus::Running.as_str().into());
    mismatched_receipt.attempt_count = Set(1);
    mismatched_receipt.claim_count = Set(2);
    mismatched_receipt.started_at = Set(now);
    mismatched_receipt.lease_token = Set(Some(mismatched_token));
    mismatched_receipt.lease_expires_at = Set(Some(now + chrono::Duration::minutes(1)));
    assert!(mismatched_receipt.insert(&store.conn).await.is_err());

    let retry_chat = sample_chat();
    store.create_chat(&retry_chat).await.unwrap();
    let mut retry_wait = make_queued_turn(&store, retry_chat.id, "gpt-5", now).await;
    retry_wait.status = Set(TurnRunStatus::RetryWait.as_str().into());
    retry_wait.attempt_count = Set(1);
    retry_wait.claim_count = Set(1);
    retry_wait.max_attempts = Set(2);
    retry_wait.started_at = Set(now);
    retry_wait.last_error_code = Set(Some("provider_unavailable".into()));
    retry_wait.insert(&store.conn).await.unwrap();

    let failed_chat = sample_chat();
    store.create_chat(&failed_chat).await.unwrap();
    let mut failed = make_queued_turn(&store, failed_chat.id, "gpt-5", now).await;
    failed.status = Set(TurnRunStatus::Failed.as_str().into());
    failed.attempt_count = Set(1);
    failed.claim_count = Set(1);
    failed.started_at = Set(now);
    failed.ended_at = Set(Some(now));
    failed.last_error_code = Set(Some("unsafe_to_retry".into()));
    failed.last_error_detail = Set(Some("tool outcome is ambiguous".into()));
    failed.insert(&store.conn).await.unwrap();

    for started in [false, true] {
        let cancelled_chat = sample_chat();
        store.create_chat(&cancelled_chat).await.unwrap();
        let mut cancelled = make_queued_turn(&store, cancelled_chat.id, "gpt-5", now).await;
        cancelled.status = Set("interrupted".into());
        cancelled.ended_at = Set(Some(now));
        if started {
            cancelled.attempt_count = Set(1);
            cancelled.claim_count = Set(1);
            cancelled.started_at = Set(now);
        }
        cancelled.insert(&store.conn).await.unwrap();
    }
}

#[tokio::test]
async fn turn_run_input_message_must_match_its_chat_and_turn() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let now = DateTime::<Utc>::from_timestamp(1_752_408_000, 0).unwrap();

    let mut missing = make_queued_turn(&store, first_chat.id, "gpt-5", now).await;
    missing.input_message_id = Set(Some(MessageId::new().0));
    assert!(missing.insert(&store.conn).await.is_err());

    let mut wrong_chat = make_queued_turn(&store, first_chat.id, "gpt-5", now).await;
    wrong_chat.session_id = Set(second_chat.id.0);
    assert!(wrong_chat.insert(&store.conn).await.is_err());

    let mut wrong_turn = make_queued_turn(&store, first_chat.id, "gpt-5", now).await;
    wrong_turn.id = Set(TurnId::new().0);
    assert!(wrong_turn.insert(&store.conn).await.is_err());
}

#[tokio::test]
#[ignore = "admission ledger retired in D4a"]
async fn turn_admission_reservation_is_global_exact_and_recoverable() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let turn_id = TurnId::new();
    let request = TurnAdmissionRequest {
        id: turn_id,
        chat_id: first_chat.id,
        content: "reserved input".into(),
        attachments: vec![uuid::Uuid::new_v4()],
        file_attachments: vec![DocumentId::new()],
        invoked_skills: vec!["presentations".into()],
        voice_input_used: true,
    };
    let first_token = uuid::Uuid::new_v4();
    let first_lease = match store
        .begin_turn_admission(&request, first_token, chrono::Duration::seconds(30))
        .await
        .unwrap()
    {
        BeginTurnAdmissionOutcome::Acquired(lease) => lease,
        outcome => panic!("unexpected first reservation: {outcome:?}"),
    };

    assert!(matches!(
        store
            .begin_turn_admission(&request, uuid::Uuid::new_v4(), chrono::Duration::seconds(1),)
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::Pending { .. }
    ));
    let mut changed = request.clone();
    changed.content = "different".into();
    assert_eq!(
        store
            .begin_turn_admission(&changed, uuid::Uuid::new_v4(), chrono::Duration::seconds(1),)
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::IdentityConflict
    );
    let mut cross_chat = request.clone();
    cross_chat.chat_id = second_chat.id;
    assert_eq!(
        store
            .begin_turn_admission(
                &cross_chat,
                uuid::Uuid::new_v4(),
                chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::IdentityConflict
    );

    // Expire the reservation explicitly instead of asking a loaded runner to
    // finish all assertions inside a tiny wall-clock lease.
    let takeover_token = uuid::Uuid::new_v4();
    let takeover_lease = match store
        .begin_turn_admission(&request, takeover_token, chrono::Duration::seconds(1))
        .await
        .unwrap()
    {
        BeginTurnAdmissionOutcome::Acquired(lease) if lease.lease_token == takeover_token => lease,
        outcome => panic!("unexpected takeover reservation: {outcome:?}"),
    };
    assert!(!store.release_turn_admission(first_lease).await.unwrap());
    assert!(store.release_turn_admission(takeover_lease).await.unwrap());
}

#[tokio::test]
#[ignore = "admission ledger retired in D4a"]
async fn turn_admission_rejects_an_unbounded_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let request = TurnAdmissionRequest {
        id: TurnId::new(),
        chat_id: chat.id,
        content: "bounded lease".into(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
    };

    let error = store
        .begin_turn_admission(
            &request,
            uuid::Uuid::new_v4(),
            chrono::Duration::minutes(5) + chrono::Duration::milliseconds(1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, AgentError::Store(message) if message.contains("at most five minutes"))
    );
}

#[tokio::test]
#[ignore = "admission ledger retired in D4a"]
async fn reserved_queue_promotion_keeps_one_global_turn_owner() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    let other_chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store.create_chat(&other_chat).await.unwrap();
    let queued = QueuedTurn {
        id: TurnId::new(),
        chat_id: chat.id,
        content: "queued admission".into(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
        position: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let request = TurnAdmissionRequest {
        id: queued.id,
        chat_id: queued.chat_id,
        content: queued.content.clone(),
        attachments: queued.attachments.clone(),
        file_attachments: queued.file_attachments.clone(),
        invoked_skills: queued.invoked_skills.clone(),
        voice_input_used: queued.voice_input_used,
    };
    let lease = match store
        .begin_turn_admission(&request, uuid::Uuid::new_v4(), chrono::Duration::seconds(1))
        .await
        .unwrap()
    {
        BeginTurnAdmissionOutcome::Acquired(lease) => lease,
        outcome => panic!("unexpected reservation outcome: {outcome:?}"),
    };
    assert!(matches!(
        store.enqueue_reserved_turn(lease, &queued).await.unwrap(),
        ReservedQueuedTurnOutcome::Queued(_)
    ));
    assert_eq!(
        store
            .begin_turn_admission(&request, uuid::Uuid::new_v4(), chrono::Duration::seconds(1),)
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::Queued
    );
    let mut cross_chat = request.clone();
    cross_chat.chat_id = other_chat.id;
    assert_eq!(
        store
            .begin_turn_admission(
                &cross_chat,
                uuid::Uuid::new_v4(),
                chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::IdentityConflict
    );

    let queued = store
        .list_queued_turns(chat.id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(matches!(
        store
            .promote_queued_turn_with_message_context(&queued, "gpt-5", &[])
            .await
            .unwrap(),
        PromoteQueuedTurnOutcome::Promoted(_)
    ));
    assert_eq!(
        store
            .begin_turn_admission(&request, uuid::Uuid::new_v4(), chrono::Duration::seconds(1),)
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::Accepted
    );
    assert!(store.list_queued_turns(chat.id).await.unwrap().is_empty());
    assert_eq!(
        store
            .begin_turn_admission(&request, uuid::Uuid::new_v4(), chrono::Duration::seconds(1),)
            .await
            .unwrap(),
        BeginTurnAdmissionOutcome::Accepted
    );
}

#[tokio::test]
async fn queued_promotion_refuses_deleted_edited_and_reordered_snapshots() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let make_queued = |content: &str| QueuedTurn {
        id: TurnId::new(),
        chat_id: chat.id,
        content: content.into(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
        position: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let deleted = store
        .enqueue_queued_turn(&make_queued("delete me"))
        .await
        .unwrap();
    assert!(store.delete_queued_turn(chat.id, deleted.id).await.unwrap());
    assert_eq!(
        store
            .promote_queued_turn_with_message_context(&deleted, "gpt-5", &[])
            .await
            .unwrap(),
        PromoteQueuedTurnOutcome::Stale
    );
    assert!(store.get_turn(deleted.id).await.unwrap().is_none());

    let edited = store
        .enqueue_queued_turn(&make_queued("before edit"))
        .await
        .unwrap();
    let updated = store
        .update_queued_turn(chat.id, edited.id, Some("after edit"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .promote_queued_turn_with_message_context(&edited, "gpt-5", &[])
            .await
            .unwrap(),
        PromoteQueuedTurnOutcome::Stale
    );
    assert_eq!(
        store.list_queued_turns(chat.id).await.unwrap(),
        vec![updated]
    );

    assert!(store.delete_queued_turn(chat.id, edited.id).await.unwrap());
    let first = store
        .enqueue_queued_turn(&make_queued("first"))
        .await
        .unwrap();
    let second = store
        .enqueue_queued_turn(&make_queued("second"))
        .await
        .unwrap();
    store
        .update_queued_turn(chat.id, second.id, None, Some(0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .promote_queued_turn_with_message_context(&first, "gpt-5", &[])
            .await
            .unwrap(),
        PromoteQueuedTurnOutcome::Stale
    );
    let remaining = store.list_queued_turns(chat.id).await.unwrap();
    assert_eq!(
        remaining.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![second.id, first.id]
    );
}

#[tokio::test]
#[ignore = "admission ledger retired in D4a"]
async fn expired_turn_admission_lease_cannot_queue_or_release() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let queued = QueuedTurn {
        id: TurnId::new(),
        chat_id: chat.id,
        content: "lease expires".into(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
        position: 0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let request = TurnAdmissionRequest {
        id: queued.id,
        chat_id: queued.chat_id,
        content: queued.content.clone(),
        attachments: Vec::new(),
        file_attachments: Vec::new(),
        invoked_skills: Vec::new(),
        voice_input_used: false,
    };
    let lease = match store
        .begin_turn_admission(
            &request,
            uuid::Uuid::new_v4(),
            chrono::Duration::milliseconds(20),
        )
        .await
        .unwrap()
    {
        BeginTurnAdmissionOutcome::Acquired(lease) => lease,
        outcome => panic!("unexpected reservation outcome: {outcome:?}"),
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    assert_eq!(
        store.enqueue_reserved_turn(lease, &queued).await.unwrap(),
        ReservedQueuedTurnOutcome::LeaseLost
    );
    assert!(!store.release_turn_admission(lease).await.unwrap());
    assert!(store.list_queued_turns(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn turn_acceptance_is_atomic_idempotent_and_chat_scoped() {
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
        outcome => panic!("unexpected first acceptance outcome: {outcome:?}"),
    };
    assert_eq!(accepted.id, turn_id);
    assert_eq!(accepted.chat_id, chat.id);
    assert_eq!(accepted.model, "gpt-5");
    assert_eq!(accepted.status, TurnRunStatus::Queued);
    assert_eq!(accepted.attempt_count, 0);
    assert_eq!(accepted.max_attempts, TurnRun::DEFAULT_MAX_ATTEMPTS);
    assert_eq!(accepted.lease_token, None);

    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, accepted.input_message_id);
    assert_eq!(messages[0].turn_id, turn_id);
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[0].content, "hello");

    let existing = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Existing(turn) => turn,
        outcome => panic!("unexpected retry outcome: {outcome:?}"),
    };
    assert_eq!(existing, accepted);
    assert_eq!(store.list_turns(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "different")
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "other-model", "hello")
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));

    let busy = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "next")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::ChatBusy(turn) => turn,
        outcome => panic!("unexpected busy outcome: {outcome:?}"),
    };
    assert_eq!(busy, accepted);

    let other = sample_chat();
    store.create_chat(&other).await.unwrap();
    assert!(matches!(
        store
            .accept_turn(turn_id, other.id, "gpt-5", "hello")
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));

    let missing = ChatId::new();
    assert!(store
        .accept_turn(TurnId::new(), missing, "gpt-5", "hello")
        .await
        .is_err());
    assert!(store
        .accept_turn(TurnId(uuid::Uuid::nil()), other.id, "gpt-5", "hello")
        .await
        .is_err());
    assert!(store
        .accept_turn(TurnId::new(), other.id, "", "hello")
        .await
        .is_err());
    assert!(store
        .accept_turn(TurnId::new(), other.id, "gpt-5", " \n\t")
        .await
        .is_err());
    assert!(store
        .accept_turn(TurnId::new(), other.id, "gpt\0-5", "hello")
        .await
        .is_err());
    assert!(store
        .accept_turn(TurnId::new(), other.id, "gpt-5", "hello\0world")
        .await
        .is_err());
    assert!(store.list_turns(other.id).await.unwrap().is_empty());
    assert!(store.list_messages(other.id).await.unwrap().is_empty());

    entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(accepted.updated_at)),
        )
        .filter(entities::agent_run::Column::Id.eq(accepted.agent_run_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "hello")
            .await
            .unwrap(),
        AcceptTurnOutcome::Existing(turn) if turn == accepted
    ));
    assert!(store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "new work")
        .await
        .is_err());
    entities::message::Entity::update_many()
        .col_expr(
            entities::message::Column::Role,
            sea_orm::sea_query::Expr::value("assistant"),
        )
        .filter(entities::message::Column::Id.eq(accepted.input_message_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(store
        .accept_turn(turn_id, chat.id, "gpt-5", "hello")
        .await
        .is_err());
}

#[tokio::test]
async fn concurrent_turn_acceptance_commits_one_request_and_one_message() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let store = std::sync::Arc::new(store);
    let turn_id = TurnId::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn(turn_id, chat.id, "gpt-5", "same input")
                .await
                .unwrap()
        }));
    }

    let mut accepted = 0;
    let mut existing = 0;
    for task in tasks {
        match task.await.unwrap() {
            AcceptTurnOutcome::Accepted(_) => accepted += 1,
            AcceptTurnOutcome::Existing(_) => existing += 1,
            outcome => panic!("unexpected concurrent outcome: {outcome:?}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(existing, 7);
    assert_eq!(store.list_turns(chat.id).await.unwrap().len(), 1);
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    let competing_chat = sample_chat();
    store.create_chat(&competing_chat).await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn(
                    TurnId::new(),
                    competing_chat.id,
                    "gpt-5",
                    &format!("input {index}"),
                )
                .await
                .unwrap()
        }));
    }
    let mut accepted = 0;
    let mut busy = 0;
    for task in tasks {
        match task.await.unwrap() {
            AcceptTurnOutcome::Accepted(_) => accepted += 1,
            AcceptTurnOutcome::ChatBusy(_) => busy += 1,
            outcome => panic!("unexpected competing outcome: {outcome:?}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(busy, 7);
    assert_eq!(store.list_turns(competing_chat.id).await.unwrap().len(), 1);
    assert_eq!(
        store.list_messages(competing_chat.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn concurrent_cross_chat_reuse_of_a_turn_id_commits_once() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let store = std::sync::Arc::new(store);
    let turn_id = TurnId::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));

    let mut tasks = Vec::new();
    for chat_id in [first_chat.id, second_chat.id] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .accept_turn(turn_id, chat_id, "gpt-5", "same input")
                .await
        }));
    }

    let mut accepted = 0;
    let mut conflicted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(AcceptTurnOutcome::Accepted(_)) => accepted += 1,
            Ok(AcceptTurnOutcome::IdentityConflict) => conflicted += 1,
            outcome => panic!("unexpected cross-chat outcome: {outcome:?}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(conflicted, 1);
    assert_eq!(store.get_turn(turn_id).await.unwrap().unwrap().id, turn_id);
    assert_eq!(
        store.list_messages(first_chat.id).await.unwrap().len()
            + store.list_messages(second_chat.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn turn_acceptance_rolls_back_when_input_message_insert_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_input
             BEFORE INSERT ON message
             WHEN NEW.content = 'force failure'
             BEGIN
               SELECT RAISE(ABORT, 'forced input failure');
             END;",
        )
        .await
        .unwrap();

    let turn_id = TurnId::new();
    assert!(store
        .accept_turn(turn_id, chat.id, "gpt-5", "force failure")
        .await
        .is_err());
    assert_eq!(store.get_turn(turn_id).await.unwrap(), None);
    assert!(store.list_turns(chat.id).await.unwrap().is_empty());
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn turn_acceptance_rolls_back_message_when_turn_insert_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_run
             BEFORE INSERT ON code_turn
             WHEN NEW.model = 'force-run-failure'
             BEGIN
               SELECT RAISE(ABORT, 'forced turn failure');
             END;",
        )
        .await
        .unwrap();

    let turn_id = TurnId::new();
    assert!(store
        .accept_turn(turn_id, chat.id, "force-run-failure", "input was inserted")
        .await
        .is_err());
    assert_eq!(store.get_turn(turn_id).await.unwrap(), None);
    assert!(store.list_turns(chat.id).await.unwrap().is_empty());
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
}

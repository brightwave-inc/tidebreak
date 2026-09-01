use super::*;

#[tokio::test]
async fn blob_retirement_coalesces_candidates_and_live_writes_cancel_episodes() {
    let (_dir, store) = temp_store().await;
    let shared_blob = DocumentBlob::from_digest([0x51; 32], 51);
    let source_a = sample_raw_source(
        DocumentId::new(),
        "file:///shared-a.bin",
        shared_blob.clone(),
    );
    let source_b = sample_raw_source(
        DocumentId::new(),
        "file:///shared-b.bin",
        shared_blob.clone(),
    );
    let original_a = store.accept_document_source(&source_a).await.unwrap();
    store.accept_document_source(&source_b).await.unwrap();
    assert_eq!(
        store.get_blob_retirement(shared_blob.id).await.unwrap(),
        None
    );

    let replacement_b = sample_raw_source(
        source_b.id,
        source_b.origin_uri.as_deref().unwrap(),
        DocumentBlob::from_digest([0x52; 32], 52),
    );
    store.accept_document_source(&replacement_b).await.unwrap();
    let queued = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    // A dropped reference creates a candidate even while another document
    // still shares the blob. Claim must perform the authoritative indexed
    // reference check before this candidate can become running.
    assert_eq!(queued.status, BlobRetirementStatus::Queued);
    assert_eq!(queued.attempt_count, 0);
    assert_eq!(queued.max_attempts, BlobRetirement::DEFAULT_MAX_ATTEMPTS);
    assert_eq!(queued.lease_token, None);
    assert_eq!(queued.finished_at, None);

    let repeated = store.accept_document_source(&source_a).await.unwrap();
    assert_eq!(repeated, original_a);
    let cancelled = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, BlobRetirementStatus::Cancelled);
    assert_eq!(cancelled.created_at, queued.created_at);
    assert!(cancelled.finished_at.is_some());

    let replacement_a = sample_raw_source(
        source_a.id,
        source_a.origin_uri.as_deref().unwrap(),
        DocumentBlob::from_digest([0x53; 32], 53),
    );
    store.accept_document_source(&replacement_a).await.unwrap();
    let requeued = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requeued.status, BlobRetirementStatus::Queued);
    assert_eq!(requeued.created_at, queued.created_at);
    assert_eq!(requeued.attempt_count, 0);
    assert_eq!(requeued.started_at, None);
    assert_eq!(requeued.finished_at, None);

    store.delete_document(replacement_a.id).await.unwrap();
    let replacement_retirement = store
        .get_blob_retirement(replacement_a.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replacement_retirement.status, BlobRetirementStatus::Queued);
    let source_c = sample_raw_source(
        DocumentId::new(),
        "file:///shared-c.bin",
        replacement_a.source_blob.clone(),
    );
    store.accept_document_source(&source_c).await.unwrap();
    assert_eq!(
        store
            .get_blob_retirement(replacement_a.source_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );
}

#[tokio::test]
async fn source_replacement_rolls_back_when_blob_retirement_cannot_be_enqueued() {
    let (_dir, store) = temp_store().await;
    let original = sample_raw_source(
        DocumentId::new(),
        "file:///rollback-source.bin",
        DocumentBlob::from_digest([0x61; 32], 61),
    );
    let original_record = store.accept_document_source(&original).await.unwrap();
    let replacement = sample_raw_source(
        original.id,
        original.origin_uri.as_deref().unwrap(),
        DocumentBlob::from_digest([0x62; 32], 62),
    );
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_replacement_retirement_insert
             BEFORE INSERT ON blob_retirement
             BEGIN SELECT RAISE(FAIL, 'injected replacement failure'); END",
        )
        .await
        .unwrap();
    assert!(store.accept_document_source(&replacement).await.is_err());
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_replacement_retirement_insert")
        .await
        .unwrap();

    assert_eq!(
        store.get_document(original.id).await.unwrap(),
        Some(original_record)
    );
    assert_eq!(
        store
            .get_blob_retirement(original.source_blob.id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .get_blob_retirement(replacement.source_blob.id)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn document_delete_rolls_back_when_blob_retirement_cannot_be_enqueued() {
    let (_dir, store) = temp_store().await;
    let source = sample_raw_source(
        DocumentId::new(),
        "file:///rollback-delete.bin",
        DocumentBlob::from_digest([0x63; 32], 63),
    );
    let record = store.accept_document_source(&source).await.unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_blob_retirement_insert
             BEFORE INSERT ON blob_retirement
             BEGIN SELECT RAISE(FAIL, 'injected retirement failure'); END",
        )
        .await
        .unwrap();
    assert!(store.delete_document(source.id).await.is_err());
    assert_eq!(store.get_document(source.id).await.unwrap(), Some(record));
    assert_eq!(
        store
            .get_blob_retirement(source.source_blob.id)
            .await
            .unwrap(),
        None
    );
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_blob_retirement_insert")
        .await
        .unwrap();
    store.delete_document(source.id).await.unwrap();
    assert_eq!(
        store
            .get_blob_retirement(source.source_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
}

#[tokio::test]
async fn blob_retirement_constraints_reject_impossible_delivery_state() {
    let (_dir, store) = temp_store().await;
    let now = Utc::now();
    let invalid_queued_lease = entities::blob_retirement::ActiveModel {
        blob_id: Set(uuid::Uuid::new_v4()),
        status: Set(BlobRetirementStatus::Queued.as_str().into()),
        attempt_count: Set(0),
        max_attempts: Set(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
        available_at: Set(now),
        lease_token: Set(Some(uuid::Uuid::new_v4())),
        lease_expires_at: Set(Some(now + chrono::Duration::minutes(1))),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    assert!(invalid_queued_lease.insert(&store.conn).await.is_err());

    let exhausted_retry = entities::blob_retirement::ActiveModel {
        blob_id: Set(uuid::Uuid::new_v4()),
        status: Set(BlobRetirementStatus::RetryWait.as_str().into()),
        attempt_count: Set(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
        max_attempts: Set(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
        available_at: Set(now),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(Some(now)),
        finished_at: Set(None),
        last_error_code: Set(Some("transient_delete".into())),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    assert!(exhausted_retry.insert(&store.conn).await.is_err());
}

#[tokio::test]
async fn orphan_blob_retirement_only_queues_missing_or_completed_episodes() {
    let (_dir, store) = temp_store().await;
    let blob_id = uuid::Uuid::new_v4();
    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    assert!(!store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let queued = store.get_blob_retirement(blob_id).await.unwrap().unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(claimed_at, claimed_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert!(store
        .complete_blob_retirement(
            claimed.blob_id,
            claimed.lease_token.unwrap(),
            claimed_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let requeued = store.get_blob_retirement(blob_id).await.unwrap().unwrap();
    assert_eq!(requeued.status, BlobRetirementStatus::Queued);
    assert_eq!(requeued.attempt_count, 0);
    assert_eq!(requeued.started_at, None);
    assert_eq!(requeued.finished_at, None);

    let referenced = sample_raw_source(
        DocumentId::new(),
        "file:///referenced-orphan-audit.bin",
        DocumentBlob::from_digest([0x7b; 32], 81),
    );
    store.accept_document_source(&referenced).await.unwrap();
    assert!(!store
        .ensure_orphan_blob_retirement(referenced.source_blob.id)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_blob_retirement(referenced.source_blob.id)
            .await
            .unwrap(),
        None
    );
}

/// An exec card's preview images are a live reference like any other. The
/// auditor reads them back out of the stored preview, and a row it cannot
/// parse — written by a build that knew a shape this one does not — has to
/// count as a reference: guessing "no reference" deletes the only copy of an
/// image the card still renders, while guessing "reference" only keeps bytes
/// around longer than they were needed.
#[tokio::test]
async fn an_unreadable_tool_preview_keeps_its_blob_alive() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let blob_id = uuid::Uuid::new_v4();

    let created = DateTime::<Utc>::from_timestamp(1_700_000_020, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_preview".into(),
        name: "exec".into(),
        arguments: serde_json::json!({"command": "python3 plot.py"}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: created,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let preview = crate::ToolResultPreview::Exec {
        exit_code: Some(0),
        timed_out: false,
        output_truncated: false,
        stdout: "wrote preview/overview.png".into(),
        stderr: String::new(),
        images: vec![crate::ImageRef {
            blob_id,
            media_type: crate::ImageMediaType::Png,
            width: 800,
            height: 600,
            byte_len: 4_096,
        }],
        outputs: Vec::new(),
        degraded: None,
        backend: None,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            call.id,
            &ToolCallResolution::Completed {
                result: "ran".into(),
            },
            created + chrono::Duration::seconds(1),
            Some(&preview),
        )
        .await
        .unwrap();
    assert!(!store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    assert_eq!(store.get_blob_retirement(blob_id).await.unwrap(), None);

    // The same row, now in a shape this build cannot read.
    entities::tool_call::Entity::update_many()
        .col_expr(
            entities::tool_call::Column::ResultPreview,
            sea_orm::sea_query::Expr::value(serde_json::json!({
                "tool": "exec",
                "attachments": [{ "blob_id": blob_id }],
            })),
        )
        .filter(entities::tool_call::Column::Id.eq(call.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(!store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    assert_eq!(store.get_blob_retirement(blob_id).await.unwrap(), None);
}

/// A screen-capture card's image is the same kind of live reference as an
/// exec card's: the stored preview is the only row pointing at the capture's
/// blob, so the auditor missing the variant would retire the only copy of a
/// screenshot the transcript still renders.
#[tokio::test]
async fn a_screen_capture_preview_keeps_its_blob_alive() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let blob_id = uuid::Uuid::new_v4();

    let created = DateTime::<Utc>::from_timestamp(1_700_000_030, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_capture".into(),
        name: "screen_capture".into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,
        provider_replay: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: created,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let preview = crate::ToolResultPreview::ScreenCapture {
        image: crate::ImageRef {
            blob_id,
            media_type: crate::ImageMediaType::Png,
            width: 2056,
            height: 1329,
            byte_len: 979_437,
        },
        mark_count: 12,
    };
    store
        .resolve_server_tool_call_with_artifacts(
            call.id,
            &ToolCallResolution::Completed {
                result: "captured".into(),
            },
            created + chrono::Duration::seconds(1),
            Some(&preview),
        )
        .await
        .unwrap();
    assert!(!store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    assert_eq!(store.get_blob_retirement(blob_id).await.unwrap(), None);
}

#[tokio::test]
async fn stale_orphan_snapshot_cannot_reset_a_new_worker_lease() {
    let (_dir, store) = temp_store().await;
    let blob_id = uuid::Uuid::new_v4();
    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let first_claim_at = Utc::now() + chrono::Duration::seconds(1);
    let first = store
        .claim_blob_retirement(
            first_claim_at,
            first_claim_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(store
        .complete_blob_retirement(
            blob_id,
            first.lease_token.unwrap(),
            first_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    let stale_completed = entities::blob_retirement::Entity::find_by_id(blob_id)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();

    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let second_claim_at = first_claim_at + chrono::Duration::seconds(3);
    let second = store
        .claim_blob_retirement(
            second_claim_at,
            second_claim_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap()
        .unwrap();

    assert!(
        !ops::blob::requeue_candidate_on(&store.conn, &stale_completed, Utc::now())
            .await
            .unwrap()
    );
    let still_running = store.get_blob_retirement(blob_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, BlobRetirementStatus::Running);
    assert_eq!(still_running.lease_token, second.lease_token);
    assert_eq!(still_running.attempt_count, second.attempt_count);
}

#[tokio::test]
async fn blob_retirement_claim_cancels_referenced_candidates() {
    let (_dir, store) = temp_store().await;
    let shared_blob = DocumentBlob::from_digest([0x71; 32], 71);
    let source_a = sample_raw_source(
        DocumentId::new(),
        "file:///claim-shared-a.bin",
        shared_blob.clone(),
    );
    let source_b = sample_raw_source(
        DocumentId::new(),
        "file:///claim-shared-b.bin",
        shared_blob.clone(),
    );
    store.accept_document_source(&source_a).await.unwrap();
    store.accept_document_source(&source_b).await.unwrap();
    let replacement = sample_raw_source(
        source_a.id,
        source_a.origin_uri.as_deref().unwrap(),
        DocumentBlob::from_digest([0x72; 32], 72),
    );
    store.accept_document_source(&replacement).await.unwrap();

    let queued = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    let now = queued.available_at + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_blob_retirement(now, now + chrono::Duration::minutes(5))
            .await
            .unwrap(),
        None
    );
    let cancelled = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, BlobRetirementStatus::Cancelled);
    assert_eq!(cancelled.attempt_count, 0);
    assert_eq!(cancelled.lease_token, None);
    assert_eq!(cancelled.finished_at, Some(now));
}

#[tokio::test]
async fn blob_retirement_claim_and_heartbeat_require_the_live_lease() {
    let (_dir, store) = temp_store().await;
    let source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-claim.bin",
        DocumentBlob::from_digest([0x73; 32], 73),
    );
    store.accept_document_source(&source).await.unwrap();
    store.delete_document(source.id).await.unwrap();
    let queued = store
        .get_blob_retirement(source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let now = queued.available_at + chrono::Duration::seconds(1);
    assert!(store.claim_blob_retirement(now, now).await.is_err());

    let first_expiry = now + chrono::Duration::minutes(5);
    let claimed = store
        .claim_blob_retirement(now, first_expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.blob_id, source.source_blob.id);
    assert_eq!(claimed.status, BlobRetirementStatus::Running);
    assert_eq!(claimed.attempt_count, 1);
    assert_eq!(claimed.started_at, Some(now));
    assert_eq!(claimed.lease_expires_at, Some(first_expiry));
    let lease_token = claimed.lease_token.unwrap();
    assert!(!store
        .heartbeat_blob_retirement(
            claimed.blob_id,
            uuid::Uuid::new_v4(),
            now + chrono::Duration::minutes(1),
            first_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert!(!store
        .heartbeat_blob_retirement(
            claimed.blob_id,
            lease_token,
            now + chrono::Duration::minutes(1),
            first_expiry,
        )
        .await
        .unwrap());
    assert!(store
        .heartbeat_blob_retirement(claimed.blob_id, lease_token, now, now)
        .await
        .is_err());

    let heartbeat_at = now + chrono::Duration::minutes(1);
    let extended = first_expiry + chrono::Duration::minutes(5);
    assert!(store
        .heartbeat_blob_retirement(claimed.blob_id, lease_token, heartbeat_at, extended)
        .await
        .unwrap());
    let heartbeated = store
        .get_blob_retirement(claimed.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heartbeated.lease_expires_at, Some(extended));
    assert_eq!(heartbeated.updated_at, heartbeat_at);
    assert!(!store
        .heartbeat_blob_retirement(
            claimed.blob_id,
            lease_token,
            extended,
            extended + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn blob_retirement_completion_requires_the_exact_live_lease() {
    let (_dir, store) = temp_store().await;
    let source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-complete.bin",
        DocumentBlob::from_digest([0x75; 32], 75),
    );
    store.accept_document_source(&source).await.unwrap();
    store.delete_document(source.id).await.unwrap();
    let queued = store
        .get_blob_retirement(source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(5);
    let claimed = store
        .claim_blob_retirement(claimed_at, lease_expires_at)
        .await
        .unwrap()
        .unwrap();
    let lease_token = claimed.lease_token.unwrap();
    assert!(!store
        .complete_blob_retirement(
            claimed.blob_id,
            uuid::Uuid::new_v4(),
            claimed_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    assert!(!store
        .complete_blob_retirement(
            claimed.blob_id,
            lease_token,
            claimed_at - chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    assert!(!store
        .complete_blob_retirement(claimed.blob_id, lease_token, lease_expires_at)
        .await
        .unwrap());

    let completed_at = claimed_at + chrono::Duration::seconds(2);
    assert!(store
        .complete_blob_retirement(claimed.blob_id, lease_token, completed_at)
        .await
        .unwrap());
    let succeeded = store
        .get_blob_retirement(claimed.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(succeeded.status, BlobRetirementStatus::Succeeded);
    assert_eq!(succeeded.lease_token, None);
    assert_eq!(succeeded.lease_expires_at, None);
    assert_eq!(succeeded.finished_at, Some(completed_at));
    assert_eq!(succeeded.last_error_code, None);
    assert_eq!(succeeded.last_error_detail, None);
    assert!(!store
        .complete_blob_retirement(claimed.blob_id, lease_token, completed_at)
        .await
        .unwrap());
}

#[tokio::test]
async fn blob_retirement_final_validation_cancels_a_new_authoritative_reference() {
    let (_dir, store) = temp_store().await;
    let retired_source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-final-check.bin",
        DocumentBlob::from_digest([0x79; 32], 79),
    );
    store.accept_document_source(&retired_source).await.unwrap();
    store.delete_document(retired_source.id).await.unwrap();
    let queued = store
        .get_blob_retirement(retired_source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(claimed_at, claimed_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();

    let live_source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-final-check-live.bin",
        DocumentBlob::from_digest([0x7a; 32], 80),
    );
    store.accept_document_source(&live_source).await.unwrap();
    // Simulate an uncoordinated external catalog writer to exercise the final
    // indexed reference check itself, rather than the normal write-time cancel.
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::SourceBlobId,
            sea_orm::sea_query::Expr::value(Some(retired_source.source_blob.id)),
        )
        .col_expr(
            entities::document::Column::SourceSha256,
            sea_orm::sea_query::Expr::value(Some(retired_source.source_blob.sha256.to_vec())),
        )
        .col_expr(
            entities::document::Column::SourceByteLen,
            sea_orm::sea_query::Expr::value(Some(
                i64::try_from(retired_source.source_blob.byte_len).unwrap(),
            )),
        )
        .filter(entities::document::Column::Id.eq(live_source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let validated_at = claimed_at + chrono::Duration::seconds(1);
    assert!(
        !store
            .validate_blob_retirement_lease(
                claimed.blob_id,
                claimed.lease_token.unwrap(),
                validated_at,
            )
            .await
            .unwrap()
    );
    let cancelled = store
        .get_blob_retirement(claimed.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, BlobRetirementStatus::Cancelled);
    assert_eq!(cancelled.finished_at, Some(validated_at));
    assert_eq!(cancelled.lease_token, None);
}

#[tokio::test]
async fn blob_retirement_failure_retries_then_exhausts_the_attempt_budget() {
    let (_dir, store) = temp_store().await;
    let source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-failure.bin",
        DocumentBlob::from_digest([0x76; 32], 76),
    );
    store.accept_document_source(&source).await.unwrap();
    store.delete_document(source.id).await.unwrap();
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2_i32),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(source.source_blob.id))
        .exec(&store.conn)
        .await
        .unwrap();
    let queued = store
        .get_blob_retirement(source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let first_at = queued.available_at + chrono::Duration::seconds(1);
    let first = store
        .claim_blob_retirement(first_at, first_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    let first_token = first.lease_token.unwrap();
    let failed_at = first_at + chrono::Duration::seconds(1);
    let retry_at = failed_at + chrono::Duration::minutes(1);
    assert!(store
        .record_blob_retirement_failure(
            first.blob_id,
            first_token,
            failed_at,
            Some(failed_at),
            "delete_failed",
            None,
        )
        .await
        .is_err());
    assert!(store
        .record_blob_retirement_failure(
            first.blob_id,
            first_token,
            failed_at,
            Some(retry_at),
            "",
            None,
        )
        .await
        .is_err());
    assert_eq!(
        store
            .record_blob_retirement_failure(
                first.blob_id,
                uuid::Uuid::new_v4(),
                failed_at,
                Some(retry_at),
                "delete_failed",
                Some("temporary filesystem error"),
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .record_blob_retirement_failure(
                first.blob_id,
                first_token,
                failed_at,
                Some(retry_at),
                "delete_failed",
                Some("temporary filesystem error"),
            )
            .await
            .unwrap(),
        Some(BlobRetirementStatus::RetryWait)
    );
    let waiting = store
        .get_blob_retirement(first.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waiting.status, BlobRetirementStatus::RetryWait);
    assert_eq!(waiting.available_at, retry_at);
    assert_eq!(waiting.finished_at, None);
    assert_eq!(waiting.last_error_code.as_deref(), Some("delete_failed"));
    assert_eq!(
        store
            .claim_blob_retirement(failed_at, failed_at + chrono::Duration::minutes(5))
            .await
            .unwrap(),
        None
    );

    let second = store
        .claim_blob_retirement(retry_at, retry_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.blob_id, first.blob_id);
    assert_eq!(second.attempt_count, 2);
    assert_ne!(second.lease_token, first.lease_token);
    assert_eq!(second.last_error_code.as_deref(), Some("delete_failed"));
    let terminal_at = retry_at + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .record_blob_retirement_failure(
                second.blob_id,
                second.lease_token.unwrap(),
                terminal_at,
                Some(terminal_at + chrono::Duration::minutes(1)),
                "delete_failed",
                None,
            )
            .await
            .unwrap(),
        Some(BlobRetirementStatus::Failed)
    );
    let failed = store
        .get_blob_retirement(second.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, BlobRetirementStatus::Failed);
    assert_eq!(failed.attempt_count, 2);
    assert_eq!(failed.finished_at, Some(terminal_at));
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
}

#[tokio::test]
async fn blob_retirement_claim_skips_an_exhausted_lease_and_returns_later_work() {
    let (_dir, store) = temp_store().await;
    let exhausted_source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-exhausted-first.bin",
        DocumentBlob::from_digest([0x77; 32], 77),
    );
    store
        .accept_document_source(&exhausted_source)
        .await
        .unwrap();
    store.delete_document(exhausted_source.id).await.unwrap();
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(1_i32),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(exhausted_source.source_blob.id))
        .exec(&store.conn)
        .await
        .unwrap();
    let exhausted_queued = store
        .get_blob_retirement(exhausted_source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let first_at = exhausted_queued.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_at + chrono::Duration::minutes(1);
    store
        .claim_blob_retirement(first_at, first_expiry)
        .await
        .unwrap()
        .unwrap();

    let later_source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-later-work.bin",
        DocumentBlob::from_digest([0x78; 32], 78),
    );
    store.accept_document_source(&later_source).await.unwrap();
    store.delete_document(later_source.id).await.unwrap();
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(first_expiry + chrono::Duration::milliseconds(500)),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(later_source.source_blob.id))
        .exec(&store.conn)
        .await
        .unwrap();
    let claim_at = first_expiry + chrono::Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(claim_at, claim_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.blob_id, later_source.source_blob.id);
    let exhausted = store
        .get_blob_retirement(exhausted_source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exhausted.status, BlobRetirementStatus::Failed);
    assert_eq!(exhausted.last_error_code.as_deref(), Some("lease_expired"));
}

#[tokio::test]
async fn expired_blob_retirement_leases_are_reclaimed_then_fail_at_the_attempt_limit() {
    let (_dir, store) = temp_store().await;
    let source = sample_raw_source(
        DocumentId::new(),
        "file:///retire-reclaim.bin",
        DocumentBlob::from_digest([0x74; 32], 74),
    );
    store.accept_document_source(&source).await.unwrap();
    store.delete_document(source.id).await.unwrap();
    entities::blob_retirement::Entity::update_many()
        .col_expr(
            entities::blob_retirement::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2_i32),
        )
        .filter(entities::blob_retirement::Column::BlobId.eq(source.source_blob.id))
        .exec(&store.conn)
        .await
        .unwrap();

    let queued = store
        .get_blob_retirement(source.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let first_at = queued.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_at + chrono::Duration::minutes(1);
    let first = store
        .claim_blob_retirement(first_at, first_expiry)
        .await
        .unwrap()
        .unwrap();
    let second_expiry = first_expiry + chrono::Duration::minutes(1);
    let second = store
        .claim_blob_retirement(first_expiry, second_expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.blob_id, first.blob_id);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.started_at, first.started_at);
    assert_ne!(second.lease_token, first.lease_token);
    assert_eq!(second.last_error_code.as_deref(), Some("lease_expired"));
    assert!(!store
        .heartbeat_blob_retirement(
            first.blob_id,
            first.lease_token.unwrap(),
            first_expiry,
            second_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());

    let final_at = second_expiry + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_blob_retirement(final_at, final_at + chrono::Duration::minutes(1))
            .await
            .unwrap(),
        None
    );
    let failed = store
        .get_blob_retirement(first.blob_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, BlobRetirementStatus::Failed);
    assert_eq!(failed.attempt_count, 2);
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
    assert_eq!(failed.finished_at, Some(final_at));
    assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
}

#[tokio::test]
async fn concurrent_blob_retirement_claimers_never_share_a_blob() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let mut latest_available_at = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    for index in 0..6 {
        let source = sample_raw_source(
            DocumentId::new(),
            &format!("file:///retire-concurrent-{index}.bin"),
            DocumentBlob::from_digest([0x80 + index as u8; 32], 80 + index),
        );
        store.accept_document_source(&source).await.unwrap();
        store.delete_document(source.id).await.unwrap();
        latest_available_at = latest_available_at.max(
            store
                .get_blob_retirement(source.source_blob.id)
                .await
                .unwrap()
                .unwrap()
                .available_at,
        );
    }
    let now = latest_available_at + chrono::Duration::seconds(1);
    let claims = (0..12).map(|_| {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .claim_blob_retirement(now, now + chrono::Duration::minutes(5))
                .await
        })
    });

    let mut claimed_ids = Vec::new();
    for result in futures::future::join_all(claims).await {
        if let Some(retirement) = result.unwrap().unwrap() {
            claimed_ids.push(retirement.blob_id);
        }
    }
    assert_eq!(claimed_ids.len(), 6);
    claimed_ids.sort();
    claimed_ids.dedup();
    assert_eq!(claimed_ids.len(), 6);
}

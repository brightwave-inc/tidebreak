#![cfg(feature = "postgres")]

use chrono::{Duration, Utc};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use tidebreak_core::{
    AcceptTurnOutcome, BlobRetirementStatus, Chat, DbStore, DeleteChatOutcome, DocumentBlob,
    DocumentId, DocumentScope, DocumentSourceUpsert, ImageMediaType, ImageRef, SessionId, Store,
    TurnId,
};

static POSTGRES_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn postgres_document_sources_match_the_sqlite_lifecycle() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((_url, store)) = postgres_test_store().await else {
        return;
    };

    let original_blob = DocumentBlob::from_bytes(b"original source bytes");
    let source = source_for(
        DocumentId::new(),
        None,
        "file:///original.txt",
        "Original",
        original_blob.clone(),
        "original text",
    );
    let accepted = store.accept_document_source(&source).await.unwrap();
    assert_eq!(accepted.source_blob.as_ref(), Some(&original_blob));
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(accepted.clone())
    );
    assert_eq!(
        store.list_documents(DocumentScope::Unscoped).await.unwrap(),
        vec![accepted.clone()]
    );

    let replacement_blob = DocumentBlob::from_bytes(b"replacement source bytes");
    let replacement = DocumentSourceUpsert {
        source_blob: replacement_blob.clone(),
        canonical_text: "replacement text".into(),
        updated_at: accepted.updated_at + Duration::seconds(1),
        ..source.clone()
    };
    let replaced = store.accept_document_source(&replacement).await.unwrap();
    assert_eq!(replaced.created_at, accepted.created_at);
    assert_eq!(replaced.updated_at, replacement.updated_at);
    assert_eq!(replaced.source_blob.as_ref(), Some(&replacement_blob));
    assert_eq!(
        store
            .get_blob_retirement(original_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );

    let keeper = source_for(
        DocumentId::new(),
        None,
        "file:///keeper.txt",
        "Keeper",
        original_blob.clone(),
        "keep the original blob live",
    );
    store.accept_document_source(&keeper).await.unwrap();
    assert_eq!(
        store
            .get_blob_retirement(original_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );

    store.delete_document(keeper.id).await.unwrap();
    let queued = store
        .get_blob_retirement(original_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued.status, BlobRetirementStatus::Queued);
    let claimed_at = queued.available_at + Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(claimed_at, claimed_at + Duration::minutes(5))
        .await
        .unwrap()
        .expect("the final document reference should release the original blob");
    assert_eq!(claimed.blob_id, original_blob.id);
    assert!(store
        .complete_blob_retirement(
            claimed.blob_id,
            claimed.lease_token.unwrap(),
            claimed_at + Duration::seconds(1),
        )
        .await
        .unwrap());

    store.delete_document(replacement.id).await.unwrap();
    assert_eq!(
        store
            .get_blob_retirement(replacement_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
}

#[tokio::test]
async fn postgres_persists_image_and_file_attachments_with_turn_identity() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((_url, store)) = postgres_test_store().await else {
        return;
    };

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let image_blob = DocumentBlob::from_bytes(b"postgres image bytes");
    let image = image_for(&image_blob, 800, 600);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());

    let first = source_for(
        DocumentId::new(),
        Some(chat.id),
        "file:///brief.pdf",
        "brief.pdf",
        DocumentBlob::from_bytes(b"pdf bytes"),
        "parsed PDF",
    );
    let second = source_for(
        DocumentId::new(),
        Some(chat.id),
        "file:///notes.txt",
        "notes.txt",
        DocumentBlob::from_bytes(b"notes"),
        "plain notes",
    );
    store.accept_document_source(&first).await.unwrap();
    store.accept_document_source(&second).await.unwrap();

    let turn_id = TurnId::new();
    let documents = [first.id, second.id];
    let turn = match store
        .accept_turn_with_attachments(
            turn_id,
            chat.id,
            "gpt-5",
            "compare these",
            &[image],
            &documents,
            &[],
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance outcome: {outcome:?}"),
    };

    let image_attachments = store.list_message_attachments(chat.id).await.unwrap();
    assert_eq!(image_attachments.len(), 1);
    assert_eq!(image_attachments[0].message_id, turn.input_message_id);
    assert_eq!(image_attachments[0].ordinal, 0);
    assert_eq!(image_attachments[0].image, image);

    let file_attachments = store
        .list_message_document_attachments(chat.id)
        .await
        .unwrap();
    assert_eq!(file_attachments.len(), 2);
    assert_eq!(
        file_attachments
            .iter()
            .map(|attachment| (
                attachment.ordinal,
                attachment.document_id,
                attachment.title.as_deref(),
                attachment.source_blob.as_ref().map(|blob| blob.id),
                attachment.readable,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                0,
                first.id,
                Some("brief.pdf"),
                Some(first.source_blob.id),
                true
            ),
            (
                1,
                second.id,
                Some("notes.txt"),
                Some(second.source_blob.id),
                true
            ),
        ]
    );

    assert!(matches!(
        store
            .accept_turn_with_attachments(
                turn_id,
                chat.id,
                "gpt-5",
                "compare these",
                &[image],
                &documents,
                &[],
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::Existing(_)
    ));
    assert!(matches!(
        store
            .accept_turn_with_attachments(
                turn_id,
                chat.id,
                "gpt-5",
                "compare these",
                &[image],
                &[second.id, first.id],
                &[],
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
}

#[tokio::test]
async fn postgres_shared_blob_survives_until_document_and_chat_release_it() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((_url, store)) = postgres_test_store().await else {
        return;
    };

    let shared_blob = DocumentBlob::from_bytes(b"shared document and image bytes");
    let source = source_for(
        DocumentId::new(),
        None,
        "file:///shared.png",
        "shared.png",
        shared_blob.clone(),
        "",
    );
    store.accept_document_source(&source).await.unwrap();

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let image = image_for(&shared_blob, 320, 240);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    let turn = match store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "same bytes",
            &[image],
            &[],
            &[],
        )
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance outcome: {outcome:?}"),
    };
    store
        .request_turn_cancellation_and_append_event(turn.id, turn.updated_at + Duration::seconds(1))
        .await
        .unwrap()
        .expect("the queued turn should be cancellable");

    store.delete_document(source.id).await.unwrap();
    let queued = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    let first_claim_at = queued.available_at + Duration::seconds(1);
    assert!(store
        .claim_blob_retirement(first_claim_at, first_claim_at + Duration::minutes(5))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .get_blob_retirement(shared_blob.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );

    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    let requeued = store
        .get_blob_retirement(shared_blob.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requeued.status, BlobRetirementStatus::Queued);
    let final_claim_at = requeued.available_at + Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(final_claim_at, final_claim_at + Duration::minutes(5))
        .await
        .unwrap()
        .expect("the final chat reference should release the shared blob");
    assert_eq!(claimed.blob_id, shared_blob.id);
}

#[tokio::test]
async fn postgres_blob_retirement_recovers_expiry_and_retries_exactly() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((_url, store)) = postgres_test_store().await else {
        return;
    };

    let blob_id = uuid::Uuid::new_v4();
    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    assert!(!store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let queued = store.get_blob_retirement(blob_id).await.unwrap().unwrap();
    let first_claim_at = queued.available_at + Duration::seconds(1);
    let first_expiry = first_claim_at + Duration::minutes(1);
    let first = store
        .claim_blob_retirement(first_claim_at, first_expiry)
        .await
        .unwrap()
        .expect("the queued blob should be claimable");
    assert_eq!(first.status, BlobRetirementStatus::Running);
    assert_eq!(first.attempt_count, 1);

    let reclaimed_at = first_expiry;
    let second_expiry = reclaimed_at + Duration::minutes(1);
    let second = store
        .claim_blob_retirement(reclaimed_at, second_expiry)
        .await
        .unwrap()
        .expect("the expired lease should be reclaimed");
    assert_eq!(second.blob_id, blob_id);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.started_at, first.started_at);
    assert_eq!(second.last_error_code.as_deref(), Some("lease_expired"));
    assert!(!store
        .heartbeat_blob_retirement(
            blob_id,
            first.lease_token.unwrap(),
            reclaimed_at + Duration::seconds(1),
            second_expiry + Duration::minutes(1),
        )
        .await
        .unwrap());

    let heartbeat_at = reclaimed_at + Duration::seconds(1);
    let extended_expiry = second_expiry + Duration::minutes(1);
    let second_token = second.lease_token.unwrap();
    assert!(store
        .heartbeat_blob_retirement(blob_id, second_token, heartbeat_at, extended_expiry)
        .await
        .unwrap());
    let failed_at = heartbeat_at + Duration::seconds(1);
    let retry_at = failed_at + Duration::seconds(2);
    assert_eq!(
        store
            .record_blob_retirement_failure(
                blob_id,
                second_token,
                failed_at,
                Some(retry_at),
                "temporary_store_error",
                Some("retry the object deletion"),
            )
            .await
            .unwrap(),
        Some(BlobRetirementStatus::RetryWait)
    );
    assert!(store
        .claim_blob_retirement(
            retry_at - Duration::microseconds(1),
            retry_at + Duration::minutes(1),
        )
        .await
        .unwrap()
        .is_none());

    let third = store
        .claim_blob_retirement(retry_at, retry_at + Duration::minutes(1))
        .await
        .unwrap()
        .expect("the retry should become claimable at its scheduled time");
    assert_eq!(third.attempt_count, 3);
    let completed_at = retry_at + Duration::seconds(1);
    assert!(!store
        .complete_blob_retirement(blob_id, uuid::Uuid::new_v4(), completed_at)
        .await
        .unwrap());
    assert!(store
        .complete_blob_retirement(blob_id, third.lease_token.unwrap(), completed_at)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_blob_retirement(blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Succeeded
    );

    assert!(store.ensure_orphan_blob_retirement(blob_id).await.unwrap());
    let requeued = store.get_blob_retirement(blob_id).await.unwrap().unwrap();
    assert_eq!(requeued.status, BlobRetirementStatus::Queued);
    assert_eq!(requeued.attempt_count, 0);
    assert_eq!(requeued.started_at, None);
    assert_eq!(requeued.finished_at, None);
}

#[tokio::test]
async fn postgres_blob_lease_validation_rejects_a_late_document_reference() {
    let _guard = POSTGRES_TEST_LOCK.lock().await;
    let Some((url, store)) = postgres_test_store().await else {
        return;
    };

    let retired = source_for(
        DocumentId::new(),
        None,
        "file:///retired.bin",
        "retired.bin",
        DocumentBlob::from_bytes(b"retired bytes"),
        "",
    );
    store.accept_document_source(&retired).await.unwrap();
    store.delete_document(retired.id).await.unwrap();
    let queued = store
        .get_blob_retirement(retired.source_blob.id)
        .await
        .unwrap()
        .unwrap();
    let claimed_at = queued.available_at + Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(claimed_at, claimed_at + Duration::minutes(5))
        .await
        .unwrap()
        .expect("the orphaned blob should be claimable");

    let live = source_for(
        DocumentId::new(),
        None,
        "file:///late-reference.bin",
        "late-reference.bin",
        DocumentBlob::from_bytes(b"different live bytes"),
        "",
    );
    store.accept_document_source(&live).await.unwrap();
    let connection = Database::connect(&url).await.unwrap();
    let updated = connection
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE document SET source_blob_id = $1, source_sha256 = $2, source_byte_len = $3 WHERE id = $4",
            [
                retired.source_blob.id.into(),
                retired.source_blob.sha256.to_vec().into(),
                i64::try_from(retired.source_blob.byte_len).unwrap().into(),
                live.id.0.into(),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);

    assert!(!store
        .validate_blob_retirement_lease(
            claimed.blob_id,
            claimed.lease_token.unwrap(),
            claimed_at + Duration::seconds(1),
        )
        .await
        .unwrap());
    assert_eq!(
        store
            .get_blob_retirement(claimed.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );
}

async fn postgres_test_store() -> Option<(String, DbStore)> {
    let source_url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return None,
    };
    let url = private_postgres_database(&source_url, "document_blob").await;
    let store = DbStore::connect(&url).await.unwrap();
    empty_postgres_tables(&url).await;
    Some((url, store))
}

async fn private_postgres_database(url: &str, suffix: &str) -> String {
    let (prefix, database, query) = split_postgres_url(url);
    let head: String = database
        .chars()
        .take(62usize.saturating_sub(suffix.len()))
        .collect();
    let name = format!("{head}_{suffix}");
    let _ = Database::connect(url)
        .await
        .unwrap()
        .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
        .await;
    format!("{prefix}{name}{query}")
}

fn split_postgres_url(url: &str) -> (&str, &str, &str) {
    let (base, query) = match url.find('?') {
        Some(at) => url.split_at(at),
        None => (url, ""),
    };
    let at = base
        .rfind('/')
        .expect("TIDEBREAK_POSTGRES_TEST_URL names a database");
    (&base[..=at], &base[at + 1..], query)
}

async fn empty_postgres_tables(url: &str) {
    Database::connect(url)
        .await
        .unwrap()
        .execute_unprepared(
            "DO $$ \
             DECLARE tables text; \
             BEGIN \
               SELECT string_agg(format('%I.%I', schemaname, tablename), ', ') INTO tables \
                 FROM pg_tables \
                WHERE schemaname = 'public' \
                  AND tablename NOT IN ('seaql_migrations', 'advisory_lock'); \
               IF tables IS NOT NULL THEN \
                 EXECUTE 'TRUNCATE TABLE ' || tables || ' CASCADE'; \
               END IF; \
             END $$;",
        )
        .await
        .unwrap();
}

fn source_for(
    id: DocumentId,
    chat_id: Option<SessionId>,
    origin_uri: &str,
    title: &str,
    source_blob: DocumentBlob,
    canonical_text: &str,
) -> DocumentSourceUpsert {
    DocumentSourceUpsert {
        id,
        chat_id,
        project_id: None,
        origin_uri: Some(origin_uri.into()),
        media_type: "application/octet-stream".into(),
        title: Some(title.into()),
        source_blob,
        canonical_text: canonical_text.into(),
        updated_at: postgres_now(),
    }
}

fn image_for(blob: &DocumentBlob, width: u32, height: u32) -> ImageRef {
    ImageRef {
        blob_id: blob.id,
        media_type: ImageMediaType::Png,
        width,
        height,
        byte_len: blob.byte_len,
    }
}

fn sample_chat() -> Chat {
    Chat {
        id: SessionId::new(),
        project_id: None,
        title: None,
        model: None,
        reasoning_effort: None,
        permission_mode: None,
        network_policy: Default::default(),
        memory_incognito: false,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: postgres_now(),
    }
}

fn postgres_now() -> chrono::DateTime<Utc> {
    chrono::DateTime::<Utc>::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap()
}

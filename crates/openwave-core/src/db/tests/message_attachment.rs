//! Image attachment persistence and its effect on blob retention.

use super::*;
use crate::image::{ImageMediaType, ImageRef};
use crate::model::{DocumentSourceBlob, MAX_MESSAGE_ATTACHMENTS};

fn image_for(bytes: &[u8], width: u32, height: u32) -> ImageRef {
    let blob = DocumentSourceBlob::from_bytes(bytes);
    ImageRef {
        blob_id: blob.id,
        media_type: ImageMediaType::Png,
        width,
        height,
        byte_len: blob.byte_len,
    }
}

async fn accept_turn_with_images(
    store: &DbStore,
    chat_id: ChatId,
    content: &str,
    images: &[ImageRef],
) -> TurnRun {
    match store
        .accept_turn_with_attachments(TurnId::new(), chat_id, "gpt-5", content, images)
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    }
}

/// Accept a turn carrying `images` and drive it to a terminal state.
///
/// Conversation deletion is fail-closed against live work, so any test that
/// deletes a chat has to leave its turns quiesced first.
async fn accept_quiesced_turn_with_images(
    store: &DbStore,
    chat_id: ChatId,
    content: &str,
    images: &[ImageRef],
) -> TurnRun {
    let turn = accept_turn_with_images(store, chat_id, content, images).await;
    store
        .request_turn_cancellation_and_append_event(
            turn.id,
            turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    turn
}

#[tokio::test]
async fn m0014_upgrades_an_existing_store_and_orders_deletion_behind_its_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("attachment-upgrade.db").display()
    );
    let conn = Database::connect(&url).await.unwrap();
    // Foreign keys are advisory on SQLite unless asked for. Turning them on is
    // what makes this test stand in for PostgreSQL, where `message_attachment`
    // restricts deletion of the message and chat it points at — so conversation
    // deletion has to remove attachments before either.
    conn.execute_unprepared("PRAGMA foreign_keys=ON;")
        .await
        .unwrap();
    migration::Migrator::up(&conn, Some(13)).await.unwrap();
    let store = DbStore { conn: conn.clone() };
    let chat = sample_chat();
    super::create_chat_before_agent_run_split(&store, &chat).await;
    // The fixture starts before attachment persistence (and therefore before
    // standing-grant and agent-run model persistence). Upgrade before calling
    // current lifecycle code: its durable projections legitimately expect
    // columns that did not exist in this historical schema.
    migration::Migrator::up(&conn, None).await.unwrap();
    let pre_attachment_turn = TurnId::new();
    store
        .accept_turn(pre_attachment_turn, chat.id, "gpt-5", "before any image")
        .await
        .unwrap();
    store
        .request_turn_cancellation_and_append_event(pre_attachment_turn, Utc::now())
        .await
        .unwrap();

    // A conversation created before attachment persistence keeps its history and
    // simply has no images.
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    assert!(store
        .list_message_attachments(chat.id)
        .await
        .unwrap()
        .is_empty());

    let image = image_for(b"attached after the upgrade", 200, 100);
    accept_quiesced_turn_with_images(&store, chat.id, "and now an image", &[image]).await;
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        1
    );

    // Deletion succeeds only if attachments are removed ahead of the message
    // and chat rows they reference.
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
}

#[tokio::test]
async fn accepted_attachments_persist_identity_in_submission_order() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let first = image_for(b"first attachment bytes", 800, 600);
    let second = ImageRef {
        media_type: ImageMediaType::Jpeg,
        ..image_for(b"second attachment bytes", 1024, 768)
    };
    let turn =
        accept_turn_with_images(&store, chat.id, "what is in these?", &[first, second]).await;

    let attachments = store.list_message_attachments(chat.id).await.unwrap();
    assert_eq!(attachments.len(), 2);
    assert_eq!(
        attachments
            .iter()
            .map(|attachment| (attachment.ordinal, attachment.image))
            .collect::<Vec<_>>(),
        vec![(0, first), (1, second)]
    );
    for attachment in &attachments {
        assert_eq!(attachment.message_id, turn.input_message_id);
        assert_eq!(attachment.chat_id, chat.id);
        assert_eq!(attachment.validate(), Ok(()));
    }

    // Another conversation's attachments never leak into this one.
    let other = sample_chat();
    store.create_chat(&other).await.unwrap();
    accept_turn_with_images(
        &store,
        other.id,
        "unrelated",
        &[image_for(b"unrelated bytes", 32, 32)],
    )
    .await;
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        2
    );
}

#[tokio::test]
async fn attachments_join_the_turn_idempotency_proof() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let image = image_for(b"idempotent attachment bytes", 640, 480);
    let other = image_for(b"a different attachment", 640, 480);

    let accepted = match store
        .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[image])
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected first acceptance outcome: {outcome:?}"),
    };

    // A byte-identical retry re-references the same blob instead of recording
    // the attachment twice.
    let existing = match store
        .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[image])
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Existing(turn) => turn,
        outcome => panic!("unexpected retry outcome: {outcome:?}"),
    };
    assert_eq!(existing, accepted);
    assert_eq!(
        store.list_message_attachments(chat.id).await.unwrap().len(),
        1
    );

    // Reusing the identity with different images is a conflict, not a silent
    // acceptance of the first submission's attachments.
    assert!(matches!(
        store
            .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[other])
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[image, other])
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn(turn_id, chat.id, "gpt-5", "describe")
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
    let attachments = store.list_message_attachments(chat.id).await.unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].image, image);
}

#[tokio::test]
async fn attachment_bounds_are_rejected_before_any_row_is_written() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let oversized: Vec<ImageRef> = (0..=MAX_MESSAGE_ATTACHMENTS)
        .map(|index| image_for(format!("attachment {index}").as_bytes(), 16, 16))
        .collect();
    assert!(store
        .accept_turn_with_attachments(TurnId::new(), chat.id, "gpt-5", "too many", &oversized)
        .await
        .is_err());

    let degenerate = ImageRef {
        width: 0,
        ..image_for(b"degenerate", 800, 600)
    };
    assert!(store
        .accept_turn_with_attachments(TurnId::new(), chat.id, "gpt-5", "degenerate", &[degenerate])
        .await
        .is_err());

    let nil_blob = ImageRef {
        blob_id: uuid::Uuid::nil(),
        ..image_for(b"nil blob", 800, 600)
    };
    assert!(store
        .accept_turn_with_attachments(TurnId::new(), chat.id, "gpt-5", "nil blob", &[nil_blob])
        .await
        .is_err());

    assert!(store
        .list_message_attachments(chat.id)
        .await
        .unwrap()
        .is_empty());
    assert!(store.list_turn_runs(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_reloaded_conversation_rebuilds_the_same_image_blocks_in_order() {
    use crate::provider::ContentBlock;

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let first = image_for(b"reloaded first image", 800, 600);
    let second = ImageRef {
        media_type: ImageMediaType::Webp,
        ..image_for(b"reloaded second image", 400, 300)
    };
    accept_quiesced_turn_with_images(&store, chat.id, "compare these", &[first, second]).await;
    accept_quiesced_turn_with_images(&store, chat.id, "and this one?", &[]).await;

    let rebuild = || async {
        let messages = store.list_messages(chat.id).await.unwrap();
        let tool_calls = store.list_tool_calls(chat.id).await.unwrap();
        let attachments = store.list_message_attachments(chat.id).await.unwrap();
        crate::agent::rebuild_transcript_for_test(&messages, &tool_calls, &attachments)
    };

    let transcript = rebuild().await;
    assert_eq!(transcript.len(), 2);
    assert_eq!(
        transcript[0].content,
        vec![
            ContentBlock::Image { image: first },
            ContentBlock::Image { image: second },
            ContentBlock::Text {
                text: "compare these".into()
            },
        ]
    );
    assert_eq!(
        transcript[1].content,
        vec![ContentBlock::Text {
            text: "and this one?".into()
        }]
    );
    // The block transcript is never stored, so "reloading" means running the
    // same reconstruction again; it must land on the identical sequence.
    assert_eq!(rebuild().await, transcript);
}

/// Claim every due retirement until the queue is empty, newest lease last.
///
/// One scan returns at most one lease, so a test that wants to know *which*
/// blobs the auditor would actually delete has to drain the queue rather than
/// inspect a single claim.
async fn drain_blob_retirement_claims(
    store: &DbStore,
    now: chrono::DateTime<Utc>,
) -> Vec<uuid::Uuid> {
    let mut claimed = Vec::new();
    while let Some(retirement) = store
        .claim_blob_retirement(now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
    {
        claimed.push(retirement.blob_id);
    }
    claimed
}

/// The kinds of row that keep a blob alive.
///
/// Blob liveness is the union across every one of these. A new referring table
/// must gain a variant here, and [`every_reference_class_blocks_every_retirement_decision`]
/// then exercises it against every decision site — so widening only some of the
/// sites fails the suite instead of silently deleting bytes in production.
#[derive(Clone, Copy, Debug)]
enum ReferenceClass {
    Document,
    MessageAttachment,
}

impl ReferenceClass {
    const ALL: [Self; 2] = [Self::Document, Self::MessageAttachment];

    /// Create one live reference of this class to `blob`.
    async fn establish(self, store: &DbStore, blob: &DocumentSourceBlob) {
        match self {
            Self::Document => {
                let source = sample_raw_source(
                    DocumentId::new(),
                    "file:///reference-class.bin",
                    blob.clone(),
                );
                store.accept_document_source(&source).await.unwrap();
            }
            Self::MessageAttachment => {
                let chat = sample_chat();
                store.create_chat(&chat).await.unwrap();
                let image = ImageRef {
                    blob_id: blob.id,
                    media_type: ImageMediaType::Png,
                    width: 800,
                    height: 600,
                    byte_len: blob.byte_len,
                };
                accept_turn_with_images(store, chat.id, "keep this blob", &[image]).await;
            }
        }
    }
}

/// Force a queued retirement for a blob that is already referenced.
///
/// This is what conversation and document deletion do when *one* referrer is
/// dropped while others remain: blob ids are content-derived, so a candidate can
/// legitimately exist for a blob that is still in use. Whether those bytes
/// survive is decided entirely by the reference checks under test.
async fn enqueue_retirement_despite_reference(store: &DbStore, blob_id: uuid::Uuid) {
    ops::blob::enqueue_on(&store.conn, blob_id).await.unwrap();
    assert_eq!(
        store
            .get_blob_retirement(blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
}

#[tokio::test]
async fn every_reference_class_blocks_every_retirement_decision() {
    for class in ReferenceClass::ALL {
        let blob = DocumentSourceBlob::from_bytes(b"bytes shared across reference classes");

        // Site 1: the orphan audit must never queue a referenced blob.
        {
            let (_dir, store) = temp_store().await;
            class.establish(&store, &blob).await;
            assert!(
                !store.ensure_orphan_blob_retirement(blob.id).await.unwrap(),
                "{class:?}: orphan audit queued a referenced blob"
            );
            assert_eq!(
                store.get_blob_retirement(blob.id).await.unwrap(),
                None,
                "{class:?}: orphan audit left a retirement candidate behind"
            );

            // And it must retract a candidate that a concurrent drop queued.
            enqueue_retirement_despite_reference(&store, blob.id).await;
            assert!(
                !store.ensure_orphan_blob_retirement(blob.id).await.unwrap(),
                "{class:?}: orphan audit re-queued a referenced blob"
            );
            assert_eq!(
                store
                    .get_blob_retirement(blob.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                BlobRetirementStatus::Cancelled,
                "{class:?}: orphan audit left a live blob queued for deletion"
            );
        }

        // Site 2: the retirement claim scan must cancel, never claim.
        {
            let (_dir, store) = temp_store().await;
            class.establish(&store, &blob).await;
            enqueue_retirement_despite_reference(&store, blob.id).await;
            let now = Utc::now() + chrono::Duration::seconds(1);
            assert!(
                store
                    .claim_blob_retirement(now, now + chrono::Duration::minutes(5))
                    .await
                    .unwrap()
                    .is_none(),
                "{class:?}: claim scan handed out a lease on a referenced blob"
            );
            assert_eq!(
                store
                    .get_blob_retirement(blob.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                BlobRetirementStatus::Cancelled,
                "{class:?}: claim scan left a live blob queued for deletion"
            );
        }

        // Site 3: the lease-validated delete must refuse. A worker can hold a
        // lease taken before the reference appeared; this is the last check
        // between a live blob and an irreversible delete.
        {
            let (_dir, store) = temp_store().await;
            class.establish(&store, &blob).await;
            let now = Utc::now() + chrono::Duration::seconds(1);
            let lease_token = uuid::Uuid::new_v4();
            let running = entities::blob_retirement::ActiveModel {
                blob_id: Set(blob.id),
                status: Set(BlobRetirementStatus::Running.as_str().into()),
                attempt_count: Set(1),
                max_attempts: Set(BlobRetirement::DEFAULT_MAX_ATTEMPTS),
                available_at: Set(now),
                lease_token: Set(Some(lease_token)),
                lease_expires_at: Set(Some(now + chrono::Duration::minutes(5))),
                started_at: Set(Some(now)),
                finished_at: Set(None),
                last_error_code: Set(None),
                last_error_detail: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            };
            running.insert(&store.conn).await.unwrap();

            let checked_at = now + chrono::Duration::seconds(1);
            assert!(
                !store
                    .validate_blob_retirement_lease(blob.id, lease_token, checked_at)
                    .await
                    .unwrap(),
                "{class:?}: lease validation approved deleting a referenced blob"
            );
            assert_eq!(
                store
                    .get_blob_retirement(blob.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                BlobRetirementStatus::Cancelled,
                "{class:?}: lease validation left a live blob claimable"
            );
        }
    }
}

#[tokio::test]
async fn deleting_a_chat_retires_only_the_attachment_blobs_it_still_owns() {
    let (_dir, store) = temp_store().await;
    let shared = image_for(b"bytes attached by two conversations", 800, 600);
    let private = image_for(b"bytes attached by one conversation", 640, 480);

    let kept = sample_chat();
    store.create_chat(&kept).await.unwrap();
    accept_quiesced_turn_with_images(&store, kept.id, "keep sharing", &[shared]).await;

    let doomed = sample_chat();
    store.create_chat(&doomed).await.unwrap();
    accept_quiesced_turn_with_images(&store, doomed.id, "about to go", &[shared, private]).await;

    assert_eq!(
        store.delete_chat(doomed.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert!(store
        .list_message_attachments(doomed.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store.list_message_attachments(kept.id).await.unwrap().len(),
        1
    );

    // Both blobs become retirement candidates: deletion cannot tell from the
    // rows it removed which bytes are still shared.
    for blob_id in [shared.blob_id, private.blob_id] {
        assert_eq!(
            store
                .get_blob_retirement(blob_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            BlobRetirementStatus::Queued
        );
    }

    // The claim scan resolves that: the blob the surviving conversation still
    // attaches is retracted, and only the genuinely orphaned one is leased for
    // deletion.
    let now = Utc::now() + chrono::Duration::seconds(1);
    let claimed = drain_blob_retirement_claims(&store, now).await;
    assert_eq!(
        claimed,
        vec![private.blob_id],
        "only the orphaned attachment blob may be leased for deletion"
    );
    assert_eq!(
        store
            .get_blob_retirement(shared.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled,
        "a blob another conversation still attaches must survive"
    );

    // Deleting the last conversation holding it finally frees the shared blob.
    assert_eq!(
        store.delete_chat(kept.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    let later = now + chrono::Duration::seconds(1);
    assert_eq!(
        drain_blob_retirement_claims(&store, later).await,
        vec![shared.blob_id],
        "the last reference dropping must free the shared blob"
    );
}

#[tokio::test]
async fn an_attachment_blob_shared_with_a_document_survives_either_owner() {
    let (_dir, store) = temp_store().await;
    let blob = DocumentSourceBlob::from_bytes(b"bytes both a source and an attachment reference");
    let image = ImageRef {
        blob_id: blob.id,
        media_type: ImageMediaType::Png,
        width: 320,
        height: 240,
        byte_len: blob.byte_len,
    };

    let source = sample_raw_source(DocumentId::new(), "file:///also-a-source.png", blob.clone());
    store.accept_document_source(&source).await.unwrap();

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    accept_quiesced_turn_with_images(&store, chat.id, "same bytes", &[image]).await;

    // Dropping the document leaves the attachment holding the blob.
    store.delete_document(source.id).await.unwrap();
    let now = Utc::now() + chrono::Duration::seconds(1);
    assert!(store
        .claim_blob_retirement(now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .is_none());

    // Dropping the attachment too finally releases it.
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    let later = now + chrono::Duration::seconds(1);
    let claimed = store
        .claim_blob_retirement(later, later + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .expect("the last reference dropping must free the blob");
    assert_eq!(claimed.blob_id, blob.id);
}

#[tokio::test]
async fn recording_an_attachment_retracts_a_pending_retirement_for_its_blob() {
    let (_dir, store) = temp_store().await;
    let image = image_for(b"bytes queued for retirement then re-attached", 128, 128);
    assert!(store
        .ensure_orphan_blob_retirement(image.blob_id)
        .await
        .unwrap());

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    accept_turn_with_images(&store, chat.id, "re-attached", &[image]).await;

    assert_eq!(
        store
            .get_blob_retirement(image.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );
}

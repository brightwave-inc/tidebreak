//! Image attachment persistence and its effect on blob retention.

use super::*;
use crate::image::{ImageMediaType, ImageRef};
use crate::model::{DocumentBlob, MAX_MESSAGE_ATTACHMENTS};

fn image_for(bytes: &[u8], width: u32, height: u32) -> ImageRef {
    let blob = DocumentBlob::from_bytes(bytes);
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
    for image in images {
        assert!(store.publish_chat_image(chat_id, image).await.unwrap());
    }
    match store
        .accept_turn_with_attachments(TurnId::new(), chat_id, "gpt-5", content, images, &[], &[])
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
async fn published_image_authority_is_chat_scoped_and_retry_idempotent() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    let image = image_for(b"same published image bytes", 480, 320);

    assert!(store
        .publish_chat_image(first_chat.id, &image)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_published_chat_image(first_chat.id, image.blob_id)
            .await
            .unwrap(),
        Some(image)
    );
    assert_eq!(
        store
            .get_published_chat_image(second_chat.id, image.blob_id)
            .await
            .unwrap(),
        None
    );

    // Retrying the same publication recovers one reservation rather than
    // creating another row or changing its descriptor.
    assert!(store
        .publish_chat_image(first_chat.id, &image)
        .await
        .unwrap());
    assert_eq!(
        entities::chat_image_publication::Entity::find()
            .all(&store.conn)
            .await
            .unwrap()
            .len(),
        1
    );

    // The same content id can be published independently to another chat.
    assert!(store
        .publish_chat_image(second_chat.id, &image)
        .await
        .unwrap());
    assert_eq!(
        entities::chat_image_publication::Entity::find()
            .all(&store.conn)
            .await
            .unwrap()
            .len(),
        2
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
async fn message_context_is_persisted_for_the_model_without_changing_visible_content() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let image = image_for(b"voice image", 640, 480);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    let mut document = sample_document(None);
    document.chat_id = Some(chat.id);
    document.title = Some("meeting-notes.pdf".into());
    document.media_type = "application/pdf".into();
    document.source_blob = Some(DocumentBlob::from_bytes(b"opaque pdf"));
    document.canonical_text.clear();
    store.create_document(&document).await.unwrap();

    let turn_id = TurnId::new();
    let invoked = vec!["pdf-documents".to_owned()];
    let accepted = store
        .accept_turn_with_message_context(
            turn_id,
            chat.id,
            "gpt-5",
            "Summarize the meting notes",
            &[image],
            &[document.id],
            &invoked,
            true,
        )
        .await
        .unwrap();
    let AcceptTurnOutcome::Accepted(turn) = accepted else {
        panic!("unexpected acceptance outcome: {accepted:?}");
    };
    assert!(turn.voice_input_used);

    let messages = store.list_messages(chat.id).await.unwrap();
    let [message] = messages.as_slice() else {
        panic!("accepted turn should persist one input message");
    };
    assert_eq!(message.content, "Summarize the meting notes");
    let llm_content = message
        .llm_content
        .as_deref()
        .expect("message-scoped context should create a model projection");
    assert!(llm_content.contains("transcribed from speech"));
    assert!(llm_content.contains("pdf-documents"));
    assert!(llm_content.contains(&image.blob_id.to_string()));
    assert!(llm_content.contains(&document.id.to_string()));
    assert!(llm_content.ends_with("# User message\n\nSummarize the meting notes"));

    assert!(matches!(
        store
            .accept_turn_with_message_context(
                turn_id,
                chat.id,
                "gpt-5",
                "Summarize the meting notes",
                &[image],
                &[document.id],
                &invoked,
                true,
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::Existing(_)
    ));
    assert!(matches!(
        store
            .accept_turn_with_message_context(
                turn_id,
                chat.id,
                "gpt-5",
                "Summarize the meting notes",
                &[image],
                &[document.id],
                &invoked,
                false,
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
}

#[tokio::test]
async fn file_attachments_persist_with_the_message_and_join_its_idempotency_proof() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut first = sample_document(None);
    first.chat_id = Some(chat.id);
    first.title = Some("brief.pdf".into());
    first.media_type = "application/pdf".into();
    first.source_blob = Some(DocumentBlob::from_bytes(b"pdf bytes"));
    let mut second = sample_document(None);
    second.chat_id = Some(chat.id);
    second.title = Some("notes.txt".into());
    second.media_type = "text/plain".into();
    second.source_blob = Some(DocumentBlob::from_bytes(b"notes"));
    store.create_document(&first).await.unwrap();
    store.create_document(&second).await.unwrap();

    let turn_id = TurnId::new();
    let documents = [first.id, second.id];
    let accepted = store
        .accept_turn_with_attachments(
            turn_id,
            chat.id,
            "gpt-5",
            "compare these",
            &[],
            &documents,
            &[],
        )
        .await
        .unwrap();
    assert!(matches!(accepted, AcceptTurnOutcome::Accepted(_)));

    let attachments = store
        .list_message_document_attachments(chat.id)
        .await
        .unwrap();
    assert_eq!(
        attachments
            .iter()
            .map(|attachment| (
                attachment.ordinal,
                attachment.document_id,
                attachment.title.as_deref(),
                attachment.media_type.as_str(),
                attachment.source_blob.as_ref().map(|blob| blob.byte_len),
                attachment.readable,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                0,
                first.id,
                Some("brief.pdf"),
                "application/pdf",
                Some(9),
                true,
            ),
            (1, second.id, Some("notes.txt"), "text/plain", Some(5), true,),
        ]
    );
    assert!(attachments
        .iter()
        .all(|attachment| attachment.validate().is_ok()));

    assert!(matches!(
        store
            .accept_turn_with_attachments(
                turn_id,
                chat.id,
                "gpt-5",
                "compare these",
                &[],
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
                &[],
                &[second.id, first.id],
                &[],
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
}

#[tokio::test]
async fn attachments_join_the_turn_idempotency_proof() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let image = image_for(b"idempotent attachment bytes", 640, 480);
    let other = image_for(b"a different attachment", 640, 480);
    assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
    assert!(store.publish_chat_image(chat.id, &other).await.unwrap());

    let accepted = match store
        .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[image], &[], &[])
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected first acceptance outcome: {outcome:?}"),
    };

    // A byte-identical retry re-references the same blob instead of recording
    // the attachment twice.
    let existing = match store
        .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[image], &[], &[])
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
            .accept_turn_with_attachments(turn_id, chat.id, "gpt-5", "describe", &[other], &[], &[])
            .await
            .unwrap(),
        AcceptTurnOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn_with_attachments(
                turn_id,
                chat.id,
                "gpt-5",
                "describe",
                &[image, other],
                &[],
                &[],
            )
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

async fn assert_rejected_attachment_left_no_turn_rows(store: &DbStore, chat_id: ChatId) {
    assert!(store.list_messages(chat_id).await.unwrap().is_empty());
    assert!(store
        .list_message_attachments(chat_id)
        .await
        .unwrap()
        .is_empty());
    assert!(store.list_turn_runs(chat_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_never_published_image_cannot_be_accepted() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let published = image_for(b"published first", 640, 480);
    let unpublished = image_for(b"never published", 320, 240);
    assert!(store.publish_chat_image(chat.id, &published).await.unwrap());

    let error = store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "describe",
            &[published, unpublished],
            &[],
            &[],
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("is not published for chat"));
    assert_rejected_attachment_left_no_turn_rows(&store, chat.id).await;
}

#[tokio::test]
async fn another_chats_publication_cannot_be_accepted() {
    let (_dir, store) = temp_store().await;
    let source = sample_chat();
    let target = sample_chat();
    store.create_chat(&source).await.unwrap();
    store.create_chat(&target).await.unwrap();
    let image = image_for(b"published elsewhere", 640, 480);
    assert!(store.publish_chat_image(source.id, &image).await.unwrap());

    let error = store
        .accept_turn_with_attachments(
            TurnId::new(),
            target.id,
            "gpt-5",
            "describe",
            &[image],
            &[],
            &[],
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("is not published for chat"));
    assert_rejected_attachment_left_no_turn_rows(&store, target.id).await;
}

#[tokio::test]
async fn a_conflicting_published_descriptor_cannot_be_accepted() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let published = image_for(b"descriptor conflict", 640, 480);
    assert!(store.publish_chat_image(chat.id, &published).await.unwrap());
    let conflicting = ImageRef {
        width: published.width + 1,
        ..published
    };

    let error = store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "describe",
            &[conflicting],
            &[],
            &[],
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("conflicting metadata"));
    assert_rejected_attachment_left_no_turn_rows(&store, chat.id).await;
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
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "too many",
            &oversized,
            &[],
            &[]
        )
        .await
        .is_err());

    let degenerate = ImageRef {
        width: 0,
        ..image_for(b"degenerate", 800, 600)
    };
    assert!(store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "degenerate",
            &[degenerate],
            &[],
            &[],
        )
        .await
        .is_err());

    let nil_blob = ImageRef {
        blob_id: uuid::Uuid::nil(),
        ..image_for(b"nil blob", 800, 600)
    };
    assert!(store
        .accept_turn_with_attachments(
            TurnId::new(),
            chat.id,
            "gpt-5",
            "nil blob",
            &[nil_blob],
            &[],
            &[],
        )
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
                text: format!(
                    "# Important context\n\n<attachments>\n\
                     image_1: id={}; media_type=image/png; byte_size=20; this is image content block 1\n\
                     image_2: id={}; media_type=image/webp; byte_size=21; this is image content block 2\n\
                     </attachments>\n\n# User message\n\ncompare these",
                    first.blob_id, second.blob_id
                )
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
    ChatImagePublication,
    MessageAttachment,
    CodeTurnAttachment,
}

impl ReferenceClass {
    const ALL: [Self; 4] = [
        Self::Document,
        Self::ChatImagePublication,
        Self::MessageAttachment,
        Self::CodeTurnAttachment,
    ];

    /// Create one live reference of this class to `blob`.
    async fn establish(self, store: &DbStore, blob: &DocumentBlob) {
        match self {
            Self::Document => {
                let source = sample_raw_source(
                    DocumentId::new(),
                    "file:///reference-class.bin",
                    blob.clone(),
                );
                store.accept_document_source(&source).await.unwrap();
            }
            Self::ChatImagePublication => {
                let chat = sample_chat();
                store.create_chat(&chat).await.unwrap();
                let image = ImageRef {
                    blob_id: blob.id,
                    media_type: ImageMediaType::Png,
                    width: 800,
                    height: 600,
                    byte_len: blob.byte_len,
                };
                assert!(store.publish_chat_image(chat.id, &image).await.unwrap());
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
            Self::CodeTurnAttachment => {
                pin_code_turn_attachment(store, blob).await;
            }
        }
    }
}

async fn pin_code_turn_attachment(store: &DbStore, blob: &DocumentBlob) {
    use crate::code::{
        Attention, AttentionSource, CodePermissionMode, CodeRepo, CodeSession, CodeSessionId,
        CodeSessionKind, CodeSessionLifecycle, CodeTurn, CodeTurnAttachment, CodeTurnId,
        CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus, HarnessKind, RepoId, WorkspaceId,
    };
    use crate::db::code::{insert_repo, insert_session, insert_turn, insert_workspace};

    let now = Utc::now();
    let repo_id = RepoId::new();
    insert_repo(
        store,
        &CodeRepo {
            id: repo_id,
            owner: crate::OwnerId::local(),
            root_path: format!("/tmp/code-turn-{}", blob.id),
            display_name: "example".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: now,
            removed_at: None,
            cloned_from: None,
        },
    )
    .await
    .unwrap();
    let workspace_id = WorkspaceId::new();
    insert_workspace(
        store,
        &CodeWorkspace {
            id: workspace_id,
            owner: crate::OwnerId::local(),
            repo_id,
            title: "first".into(),
            worktree_path: format!("/tmp/code-turn-wt-{}", blob.id),
            branch_name: "tidebreak/first".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: now,
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        },
    )
    .await
    .unwrap();
    let session_id = CodeSessionId::new();
    insert_session(
        store,
        &CodeSession {
            id: session_id,
            owner: crate::OwnerId::local(),
            workspace_id,
            kind: CodeSessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: Some("scripted".into()),
            harness_resume_ref: None,
            permission_mode: CodePermissionMode::Plan,
            model: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 0,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: now,
        },
    )
    .await
    .unwrap();
    insert_turn(
        store,
        &crate::OwnerId::local(),
        &CodeTurn {
            id: CodeTurnId::new(),
            session_id,
            ordinal: 1,
            status: CodeTurnStatus::Completed,
            user_input: "look".into(),
            user_input_blob_id: None,
            attachments: vec![CodeTurnAttachment {
                blob_id: blob.id,
                media_type: ImageMediaType::Png,
                byte_len: blob.byte_len,
            }],
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: now,
            ended_at: Some(now),
        },
    )
    .await
    .unwrap();
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
async fn empty_blob_retirement_poll_does_not_wait_for_sqlite_writer() {
    let (_dir, store) = temp_store_with_max_connections(2).await;

    // Hold SQLite's single writer with an unrelated transaction. An empty
    // retirement scan is only a hint that no work exists, so it must remain a
    // read and return without joining the writer queue.
    let writer = store.conn.begin().await.unwrap();
    writer
        .execute_unprepared("UPDATE advisory_lock SET name = name WHERE name = 'turn_claim'")
        .await
        .unwrap();

    let now = Utc::now();
    let claimed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.claim_blob_retirement(now, now + chrono::Duration::minutes(5)),
    )
    .await
    .expect("an empty retirement scan waited for SQLite's writer")
    .unwrap();
    assert!(claimed.is_none());

    writer.rollback().await.unwrap();
}

#[tokio::test]
async fn every_reference_class_blocks_every_retirement_decision() {
    for class in ReferenceClass::ALL {
        let blob = DocumentBlob::from_bytes(b"bytes shared across reference classes");

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

    assert!(matches!(
        store.delete_chat(doomed.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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
    assert!(matches!(
        store.delete_chat(kept.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    let later = now + chrono::Duration::seconds(1);
    assert_eq!(
        drain_blob_retirement_claims(&store, later).await,
        vec![shared.blob_id],
        "the last reference dropping must free the shared blob"
    );
}

#[tokio::test]
async fn deleting_a_chat_releases_only_its_published_image_reservations() {
    let (_dir, store) = temp_store().await;
    let shared = image_for(b"published by two conversations", 800, 600);
    let private = image_for(b"published by one conversation", 640, 480);

    let kept = sample_chat();
    store.create_chat(&kept).await.unwrap();
    assert!(store.publish_chat_image(kept.id, &shared).await.unwrap());

    let doomed = sample_chat();
    store.create_chat(&doomed).await.unwrap();
    assert!(store.publish_chat_image(doomed.id, &shared).await.unwrap());
    assert!(store.publish_chat_image(doomed.id, &private).await.unwrap());

    assert!(matches!(
        store.delete_chat(doomed.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(
        store
            .get_published_chat_image(doomed.id, shared.blob_id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .get_published_chat_image(kept.id, shared.blob_id)
            .await
            .unwrap(),
        Some(shared)
    );

    let now = Utc::now() + chrono::Duration::seconds(1);
    assert_eq!(
        drain_blob_retirement_claims(&store, now).await,
        vec![private.blob_id],
        "the surviving chat's reservation must keep the shared blob live"
    );
    assert_eq!(
        store
            .get_blob_retirement(shared.blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Cancelled
    );

    assert!(matches!(
        store.delete_chat(kept.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(
        drain_blob_retirement_claims(&store, now + chrono::Duration::seconds(1)).await,
        vec![shared.blob_id]
    );
}

#[tokio::test]
async fn an_attachment_blob_shared_with_a_document_survives_either_owner() {
    let (_dir, store) = temp_store().await;
    let blob = DocumentBlob::from_bytes(b"bytes both a source and an attachment reference");
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
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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

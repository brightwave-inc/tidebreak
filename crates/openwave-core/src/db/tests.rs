use super::*;
use crate::model::{
    ByteSpan, ChatRootAttachment, DocumentParseOutput, DocumentSourceBlob, DocumentSourceUpsert,
    RetrievalEvidenceInput, RetrievalEvidenceSource, RootAttachmentChangeAction,
    RootAttachmentChangeFailure, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    SourceLocation, SourceRegion, ToolCallExecution, ToolCallResolution, ToolCallStatus,
    MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};
use crate::storage::ApplyTurnSteerOutcome;
use crate::{ApprovalClass, ChunkId, ToolApprovalStatus};
use chrono::{DateTime, Utc};

mod agent_run;
mod context_checkpoint;
mod delegated_file_read;
mod message_attachment;
mod multi_agent_wait;
mod output;
mod parent_terminal_guard;
mod root_attachment;
mod sandbox_spawn_checkpoint;
mod turn_steer;

async fn temp_store() -> (tempfile::TempDir, DbStore) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let store = DbStore::connect(&url).await.unwrap();
    (dir, store)
}

#[tokio::test]
async fn bundled_sqlite_supports_fts5() {
    let (_dir, store) = temp_store().await;
    store
        .conn
        .execute_unprepared("CREATE VIRTUAL TABLE fts_probe USING fts5(content)")
        .await
        .unwrap();
    store
        .conn
        .execute_unprepared("INSERT INTO fts_probe(content) VALUES ('hybrid retrieval')")
        .await
        .unwrap();
    let row = store
        .conn
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT content FROM fts_probe WHERE fts_probe MATCH 'hybrid'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<String>("", "content").unwrap(),
        "hybrid retrieval"
    );
}

async fn set_turn_max_attempts(store: &DbStore, turn_id: TurnId, max_attempts: i32) {
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(max_attempts),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
}

fn sample_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("hello".into()),
        model: None,
        reasoning_effort: None,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn test_checkpoint_progress() -> crate::model::TurnCheckpointProgress {
    crate::model::TurnCheckpointProgress {
        model_steps: 1,
        usage: crate::provider::Usage {
            input_tokens: 13,
            output_tokens: 8,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 3,
        },
    }
}

fn sample_project() -> Project {
    Project {
        id: ProjectId::new(),
        title: Some("proj".into()),
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn sample_raw_source(
    id: DocumentId,
    uri: &str,
    source_blob: DocumentSourceBlob,
) -> DocumentSourceUpsert {
    DocumentSourceUpsert {
        id,
        chat_id: None,
        project_id: None,
        source_uri: Some(uri.into()),
        media_type: "application/octet-stream".into(),
        title: None,
        source_blob,
        updated_at: Utc::now(),
    }
}

fn sample_document(project_id: Option<ProjectId>) -> DocumentRecord {
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_100, 0).unwrap();
    DocumentRecord {
        chat_id: None,
        id: DocumentId::new(),
        project_id,
        source_uri: Some("file:///資料/notes.md".into()),
        media_type: "text/markdown".into(),
        title: Some("Résumé 📈".into()),
        source_blob: None,
        canonical_text: "# Résumé\n\n売上 grew by 10%.".into(),
        canonical_fingerprint: None,
        source_regions: Vec::new(),
        content_revision: 1,
        revision_token: uuid::Uuid::new_v4(),
        processing_status: DocumentProcessingStatus::Queued,
        indexed_revision: None,
        index_fingerprint: None,
        created_at,
        updated_at: created_at,
        indexed_at: None,
    }
}

#[tokio::test]
async fn projects_roundtrip_and_a_chat_can_belong_to_one() {
    let (_dir, store) = temp_store().await;
    let project = sample_project();
    store.create_project(&project).await.unwrap();

    assert_eq!(
        store.get_project(project.id).await.unwrap().as_ref(),
        Some(&project)
    );
    assert_eq!(store.list_projects().await.unwrap(), vec![project.clone()]);
    assert_eq!(store.get_project(ProjectId::new()).await.unwrap(), None);

    // A chat carrying the project link round-trips it; a loose chat stays None.
    let mut in_project = sample_chat();
    in_project.project_id = Some(project.id);
    store.create_chat(&in_project).await.unwrap();
    assert_eq!(
        store
            .get_chat(in_project.id)
            .await
            .unwrap()
            .unwrap()
            .project_id,
        Some(project.id)
    );

    let loose = sample_chat();
    store.create_chat(&loose).await.unwrap();
    assert_eq!(
        store.get_chat(loose.id).await.unwrap().unwrap().project_id,
        None
    );

    // The project link survives a list, not just a by-id fetch.
    let listed = store.list_chats().await.unwrap();
    let listed_link = |id| {
        listed
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.project_id)
    };
    assert_eq!(listed_link(in_project.id), Some(project.id));
    assert_eq!(listed_link(loose.id), None);
}

#[tokio::test]
async fn ordered_root_projections_roundtrip_and_project_defaults_are_snapshotted() {
    let (_dir, store) = temp_store().await;
    let root_a = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let root_b = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let root_c = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut project = sample_project();
    project.attachment_revision = 7;
    project.root_attachments = vec![root_b, root_a];
    store.create_project(&project).await.unwrap();
    assert_eq!(
        store.get_project(project.id).await.unwrap(),
        Some(project.clone())
    );

    let mut chat = sample_chat();
    chat.project_id = Some(project.id);
    chat.attachment_revision = 1;
    chat.root_attachments = vec![
        ChatRootAttachment {
            root_id: root_b,
            origin: RootAttachmentOrigin::ProjectDefault,
        },
        ChatRootAttachment {
            root_id: root_a,
            origin: RootAttachmentOrigin::ProjectDefault,
        },
        ChatRootAttachment {
            root_id: root_c,
            origin: RootAttachmentOrigin::Conversation,
        },
    ];
    store.create_chat(&chat).await.unwrap();
    assert_eq!(store.get_chat(chat.id).await.unwrap(), Some(chat));
}

#[tokio::test]
async fn root_projection_validation_fails_closed() {
    let (_dir, store) = temp_store().await;
    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();

    let mut duplicate_project = sample_project();
    duplicate_project.root_attachments = vec![root, root];
    assert!(store.create_project(&duplicate_project).await.is_err());

    let mut oversized_project = sample_project();
    oversized_project.root_attachments = (0..=MAX_ROOT_ATTACHMENTS)
        .map(|_| HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap())
        .collect();
    assert!(store.create_project(&oversized_project).await.is_err());

    let project_root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut project = sample_project();
    project.attachment_revision = 1;
    project.root_attachments = vec![project_root];
    store.create_project(&project).await.unwrap();

    let mut stale_snapshot = sample_chat();
    stale_snapshot.project_id = Some(project.id);
    stale_snapshot.attachment_revision = 1;
    stale_snapshot.root_attachments = vec![ChatRootAttachment {
        root_id: root,
        origin: RootAttachmentOrigin::ProjectDefault,
    }];
    assert!(store.create_chat(&stale_snapshot).await.is_err());
    assert!(store.get_chat(stale_snapshot.id).await.unwrap().is_none());

    let mut standalone = sample_chat();
    standalone.attachment_revision = 1;
    standalone.root_attachments = vec![ChatRootAttachment {
        root_id: root,
        origin: RootAttachmentOrigin::ProjectDefault,
    }];
    assert!(store.create_chat(&standalone).await.is_err());

    let mut zero_revision = sample_chat();
    zero_revision.root_attachments = vec![ChatRootAttachment {
        root_id: root,
        origin: RootAttachmentOrigin::Conversation,
    }];
    assert!(store.create_chat(&zero_revision).await.is_err());

    let mut orphan = sample_chat();
    orphan.project_id = Some(ProjectId::new());
    assert!(store.create_chat(&orphan).await.is_err());
    assert!(store.get_chat(orphan.id).await.unwrap().is_none());
}

#[tokio::test]
async fn corrupted_root_projection_rows_fail_closed_on_read() {
    let (_dir, store) = temp_store().await;
    let standalone = sample_chat();
    store.create_chat(&standalone).await.unwrap();
    entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .filter(entities::chat::Column::Id.eq(standalone.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    entities::chat_root_attachment::ActiveModel {
        chat_id: Set(standalone.id.0),
        root_id: Set(uuid::Uuid::new_v4()),
        position: Set(0),
        origin: Set("project_default".into()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(store.get_chat(standalone.id).await.is_err());

    let gapped = sample_chat();
    store.create_chat(&gapped).await.unwrap();
    entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .filter(entities::chat::Column::Id.eq(gapped.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    entities::chat_root_attachment::ActiveModel {
        chat_id: Set(gapped.id.0),
        root_id: Set(uuid::Uuid::new_v4()),
        position: Set(1),
        origin: Set("conversation".into()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(store.get_chat(gapped.id).await.is_err());

    let project = sample_project();
    store.create_project(&project).await.unwrap();
    let mut mixed = sample_chat();
    mixed.project_id = Some(project.id);
    store.create_chat(&mixed).await.unwrap();
    entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(2_i64),
        )
        .filter(entities::chat::Column::Id.eq(mixed.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    for (position, origin) in [(0, "conversation"), (1, "project_default")] {
        entities::chat_root_attachment::ActiveModel {
            chat_id: Set(mixed.id.0),
            root_id: Set(uuid::Uuid::new_v4()),
            position: Set(position),
            origin: Set(origin.into()),
        }
        .insert(&store.conn)
        .await
        .unwrap();
    }
    assert!(store.get_chat(mixed.id).await.is_err());

    let corrupted_project = sample_project();
    store.create_project(&corrupted_project).await.unwrap();
    entities::project_root_attachment::ActiveModel {
        project_id: Set(corrupted_project.id.0),
        root_id: Set(uuid::Uuid::new_v4()),
        position: Set(0),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(store.get_project(corrupted_project.id).await.is_err());
    let mut base = sample_chat();
    base.project_id = Some(corrupted_project.id);
    assert!(store
        .create_chat_with_project_defaults(&base)
        .await
        .is_err());
    assert!(store.get_chat(base.id).await.unwrap().is_none());
}

#[tokio::test]
async fn project_membership_fk_and_attachment_insertions_are_atomic() {
    let (_dir, store) = temp_store().await;
    let orphan = entities::chat::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        project_id: Set(Some(uuid::Uuid::new_v4())),
        title: Set(None),
        model: Set(None),
        reasoning_effort: Set(None),
        attachment_revision: Set(0),
        created_at: Set(Utc::now()),
    };
    assert!(orphan.insert(&store.conn).await.is_err());

    let project = sample_project();
    store.create_project(&project).await.unwrap();
    let mut chat = sample_chat();
    chat.project_id = Some(project.id);
    store.create_chat(&chat).await.unwrap();
    assert!(entities::project::Entity::delete_by_id(project.id.0)
        .exec(&store.conn)
        .await
        .is_err());

    let mut max_revision = sample_project();
    max_revision.attachment_revision = MAX_ATTACHMENT_REVISION;
    store.create_project(&max_revision).await.unwrap();
    let mut excessive_revision = sample_project();
    excessive_revision.attachment_revision = MAX_ATTACHMENT_REVISION + 1;
    assert!(store.create_project(&excessive_revision).await.is_err());
    let direct_excessive = entities::project::ActiveModel {
        id: Set(uuid::Uuid::new_v4()),
        title: Set(None),
        attachment_revision: Set(MAX_ATTACHMENT_REVISION + 1),
        created_at: Set(Utc::now()),
    };
    assert!(direct_excessive.insert(&store.conn).await.is_err());

    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut rooted_project = sample_project();
    rooted_project.attachment_revision = 1;
    rooted_project.root_attachments = vec![root];
    store.create_project(&rooted_project).await.unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_chat_root_insert
             BEFORE INSERT ON chat_root_attachment
             BEGIN SELECT RAISE(FAIL, 'forced chat root failure'); END;",
        )
        .await
        .unwrap();
    let mut rejected_chat = sample_chat();
    rejected_chat.project_id = Some(rooted_project.id);
    assert!(store
        .create_chat_with_project_defaults(&rejected_chat)
        .await
        .is_err());
    assert!(store.get_chat(rejected_chat.id).await.unwrap().is_none());
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_chat_root_insert")
        .await
        .unwrap();

    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_project_root_insert
             BEFORE INSERT ON project_root_attachment
             BEGIN SELECT RAISE(FAIL, 'forced project root failure'); END;",
        )
        .await
        .unwrap();
    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut rejected = sample_project();
    rejected.attachment_revision = 1;
    rejected.root_attachments = vec![root];
    assert!(store.create_project(&rejected).await.is_err());
    assert!(store.get_project(rejected.id).await.unwrap().is_none());
}

#[tokio::test]
async fn set_chat_model_updates_then_clears() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);

    store
        .set_chat_model(chat.id, Some("claude-x".into()))
        .await
        .unwrap();
    assert_eq!(
        store
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("claude-x")
    );

    store.set_chat_model(chat.id, None).await.unwrap();
    assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);
}

#[tokio::test]
async fn chats_stored_before_the_effort_scale_widened_still_load() {
    let (_dir, store) = temp_store().await;
    // Written the way a release before `none`/`xhigh`/`max` existed wrote them:
    // straight into the column, with no chance to migrate the token.
    for (stored, expected) in [
        ("low", Some(ReasoningEffort::Low)),
        ("medium", Some(ReasoningEffort::Medium)),
        ("high", Some(ReasoningEffort::High)),
        // A token this build does not recognize is dropped, not fatal — the
        // chat still opens on the provider default.
        ("aggressive", None),
    ] {
        let chat = sample_chat();
        entities::chat::ActiveModel {
            id: Set(chat.id.0),
            project_id: Set(None),
            title: Set(chat.title.clone()),
            model: Set(None),
            reasoning_effort: Set(Some(stored.to_owned())),
            attachment_revision: Set(0),
            created_at: Set(chat.created_at),
        }
        .insert(&store.conn)
        .await
        .unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .reasoning_effort,
            expected,
            "stored effort {stored} no longer loads"
        );
    }

    // Every level this build can write reads back as itself.
    for effort in ReasoningEffort::ALL {
        let mut chat = sample_chat();
        chat.reasoning_effort = Some(*effort);
        store.create_chat(&chat).await.unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .reasoning_effort,
            Some(*effort)
        );
    }
}

#[tokio::test]
async fn list_projects_is_newest_first() {
    let (_dir, store) = temp_store().await;
    let mut older = sample_project();
    older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut newer = sample_project();
    newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    store.create_project(&older).await.unwrap();
    store.create_project(&newer).await.unwrap();
    assert_eq!(store.list_projects().await.unwrap(), vec![newer, older]);
}

#[tokio::test]
async fn project_title_update_sets_clears_and_reports_missing_identity() {
    let (_dir, store) = temp_store().await;
    let project = sample_project();
    store.create_project(&project).await.unwrap();

    assert!(store
        .update_project_title(project.id, Some("Renamed".into()))
        .await
        .unwrap());
    assert_eq!(
        store
            .get_project(project.id)
            .await
            .unwrap()
            .unwrap()
            .title
            .as_deref(),
        Some("Renamed")
    );

    assert!(store.update_project_title(project.id, None).await.unwrap());
    assert_eq!(
        store.get_project(project.id).await.unwrap().unwrap().title,
        None
    );
    assert!(!store
        .update_project_title(ProjectId::new(), Some("missing".into()))
        .await
        .unwrap());
}

#[tokio::test]
async fn project_deletion_requires_an_empty_project_and_reports_missing_identity() {
    let (_dir, store) = temp_store().await;

    let empty = sample_project();
    store.create_project(&empty).await.unwrap();
    assert_eq!(
        store.delete_project(empty.id).await.unwrap(),
        DeleteProjectOutcome::Deleted
    );
    assert_eq!(
        store.delete_project(empty.id).await.unwrap(),
        DeleteProjectOutcome::NotFound
    );

    let with_chat = sample_project();
    store.create_project(&with_chat).await.unwrap();
    let mut chat = sample_chat();
    chat.project_id = Some(with_chat.id);
    store.create_chat(&chat).await.unwrap();
    assert_eq!(
        store.delete_project(with_chat.id).await.unwrap(),
        DeleteProjectOutcome::NotEmpty
    );
    assert!(store.get_project(with_chat.id).await.unwrap().is_some());
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert_eq!(
        store.delete_project(with_chat.id).await.unwrap(),
        DeleteProjectOutcome::Deleted
    );

    let with_document = sample_project();
    store.create_project(&with_document).await.unwrap();
    let document = sample_document(Some(with_document.id));
    store.create_document(&document).await.unwrap();
    assert_eq!(
        store.delete_project(with_document.id).await.unwrap(),
        DeleteProjectOutcome::NotEmpty
    );
    store.delete_document(document.id).await.unwrap();
    assert_eq!(
        store.delete_project(with_document.id).await.unwrap(),
        DeleteProjectOutcome::Deleted
    );

    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut with_root = sample_project();
    with_root.attachment_revision = 1;
    with_root.root_attachments = vec![root];
    store.create_project(&with_root).await.unwrap();
    assert_eq!(
        store.delete_project(with_root.id).await.unwrap(),
        DeleteProjectOutcome::NotEmpty
    );
    entities::project_root_attachment::Entity::delete_many()
        .filter(entities::project_root_attachment::Column::ProjectId.eq(with_root.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert_eq!(
        store.delete_project(with_root.id).await.unwrap(),
        DeleteProjectOutcome::Deleted
    );
}

#[tokio::test]
async fn project_deletion_serializes_with_staged_source_ingestion() {
    let (_dir, store) = temp_store().await;

    for attempt in 0..16_u8 {
        let project = sample_project();
        store.create_project(&project).await.unwrap();
        let mut source = sample_raw_source(
            DocumentId::new(),
            &format!("file:///project-race-{attempt}.bin"),
            DocumentSourceBlob::from_digest([attempt.saturating_add(1); 32], 1),
        );
        source.project_id = Some(project.id);

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let delete_store = store.clone();
        let delete_barrier = barrier.clone();
        let delete = tokio::spawn(async move {
            delete_barrier.wait().await;
            delete_store.delete_project(project.id).await
        });
        let ingest_store = store.clone();
        let ingest_barrier = barrier.clone();
        let ingest_source = source.clone();
        let ingest = tokio::spawn(async move {
            ingest_barrier.wait().await;
            ingest_store
                .accept_document_source_and_enqueue_parse(&ingest_source, "parser-v1", 3)
                .await
        });
        barrier.wait().await;

        let deleted = delete.await.unwrap().unwrap();
        let ingested = ingest.await.unwrap();
        match (deleted, ingested) {
            (DeleteProjectOutcome::Deleted, Err(AgentError::ProjectNotFound(missing_project))) => {
                assert_eq!(missing_project, project.id)
            }
            (DeleteProjectOutcome::NotEmpty, Ok((record, _))) => {
                assert_eq!(record.project_id, Some(project.id));
                store.delete_document(record.id).await.unwrap();
                assert_eq!(
                    store.delete_project(project.id).await.unwrap(),
                    DeleteProjectOutcome::Deleted
                );
            }
            (outcome, result) => {
                panic!("unexpected deletion/ingestion race result: {outcome:?}, {result:?}")
            }
        }
    }
}

#[tokio::test]
async fn every_project_scoped_first_write_reports_a_typed_missing_project() {
    let (_dir, store) = temp_store().await;
    let missing = ProjectId::new();

    let legacy = sample_document(Some(missing));
    assert!(matches!(
        store.create_document(&legacy).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));

    let canonical = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(missing),
        source_uri: Some("file:///missing-project.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "missing project".into(),
        source_regions: Vec::new(),
        updated_at: Utc::now(),
    };
    assert!(matches!(
        store.upsert_document(&canonical).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));
    assert!(matches!(
        store
            .upsert_document_and_enqueue_index(&canonical, "pipeline-v1", 3)
            .await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));

    let mut staged = sample_raw_source(
        DocumentId::new(),
        "file:///missing-project.bin",
        DocumentSourceBlob::from_digest([0x41; 32], 1),
    );
    staged.project_id = Some(missing);
    assert!(matches!(
        store
            .accept_document_source_and_enqueue_parse(&staged, "parser-v1", 3)
            .await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));

    let mut chat = sample_chat();
    chat.project_id = Some(missing);
    assert!(matches!(
        store.create_chat_with_project_defaults(&chat).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));
}

#[tokio::test]
async fn documents_roundtrip_and_list_by_corpus_scope() {
    let (_dir, store) = temp_store().await;
    let project_a = sample_project();
    let mut project_b = sample_project();
    project_b.id = ProjectId::new();
    store.create_project(&project_a).await.unwrap();
    store.create_project(&project_b).await.unwrap();

    let mut unscoped = sample_document(None);
    unscoped.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut in_a = sample_document(Some(project_a.id));
    in_a.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    in_a.processing_status = DocumentProcessingStatus::Ready;
    in_a.indexed_revision = Some(1);
    in_a.index_fingerprint = Some("parser=v1;chunker=v1;embed=test".into());
    in_a.indexed_at = Some(DateTime::<Utc>::from_timestamp(2_001, 0).unwrap());
    let mut in_b = sample_document(Some(project_b.id));
    in_b.created_at = DateTime::<Utc>::from_timestamp(3_000, 0).unwrap();
    in_b.source_blob = Some(DocumentSourceBlob::from_digest([0x5a; 32], 8_192));
    in_b.canonical_fingerprint = Some("parser=markdown-v1".into());

    for document in [&unscoped, &in_a, &in_b] {
        store.create_document(document).await.unwrap();
    }
    unscoped = store.get_document(unscoped.id).await.unwrap().unwrap();
    in_a = store.get_document(in_a.id).await.unwrap().unwrap();
    in_b = store.get_document(in_b.id).await.unwrap().unwrap();

    let legacy_replacement = DocumentUpsert {
        chat_id: None,
        id: in_b.id,
        project_id: in_b.project_id,
        source_uri: in_b.source_uri.clone(),
        media_type: in_b.media_type.clone(),
        title: in_b.title.clone(),
        canonical_text: in_b.canonical_text.clone(),
        source_regions: in_b.source_regions.clone(),
        updated_at: in_b.updated_at,
    };
    assert!(store
        .upsert_document_and_enqueue_index(&legacy_replacement, "pipeline-v1", 3)
        .await
        .is_err());
    assert_eq!(
        store.get_document(in_b.id).await.unwrap(),
        Some(in_b.clone())
    );

    assert_eq!(
        store.get_document(in_a.id).await.unwrap().as_ref(),
        Some(&in_a)
    );
    assert_eq!(store.get_document(DocumentId::new()).await.unwrap(), None);
    assert_eq!(
        store
            .list_documents(DocumentScope::Project(project_a.id))
            .await
            .unwrap(),
        vec![in_a.clone()]
    );
    assert_eq!(
        store
            .list_documents(DocumentScope::Project(project_b.id))
            .await
            .unwrap(),
        vec![in_b.clone()]
    );
    assert_eq!(
        store.list_documents(DocumentScope::Unscoped).await.unwrap(),
        vec![unscoped.clone()]
    );
    assert_eq!(
        store.list_documents(DocumentScope::All).await.unwrap(),
        vec![in_b, in_a, unscoped.clone()]
    );

    store.delete_document(unscoped.id).await.unwrap();
    store.delete_document(unscoped.id).await.unwrap();
    assert_eq!(store.get_document(unscoped.id).await.unwrap(), None);
}

#[tokio::test]
async fn document_summaries_page_by_created_at_then_id_without_gaps() {
    let (_dir, store) = temp_store().await;
    // Keep both groups inside one microsecond so cursor implementations
    // that truncate sub-microsecond precision would skip the older group.
    let newer = DateTime::<Utc>::from_timestamp(2_000, 900).unwrap();
    let older = DateTime::<Utc>::from_timestamp(2_000, 700).unwrap();
    let fixtures = [
        (3_u128, newer, "newest tie"),
        (2, newer, "middle tie"),
        (1, newer, "last tie"),
        (5, older, "older high id"),
        (4, older, "older low id"),
    ];
    for (raw_id, created_at, title) in fixtures {
        let mut document = sample_document(None);
        document.id = DocumentId(uuid::Uuid::from_u128(raw_id));
        document.title = Some(title.into());
        document.canonical_text = format!("content that listings must not load: {title}");
        document.created_at = created_at;
        document.updated_at = created_at;
        store.create_document(&document).await.unwrap();
    }

    let first = store
        .list_document_summaries(DocumentScope::All, None, 2)
        .await
        .unwrap();
    assert_eq!(
        first
            .iter()
            .map(|document| document.id.0)
            .collect::<Vec<_>>(),
        vec![uuid::Uuid::from_u128(3), uuid::Uuid::from_u128(2)]
    );
    let second = store
        .list_document_summaries(
            DocumentScope::All,
            Some(DocumentListCursor {
                created_at: first[1].created_at,
                id: first[1].id,
            }),
            2,
        )
        .await
        .unwrap();
    assert_eq!(
        second
            .iter()
            .map(|document| document.id.0)
            .collect::<Vec<_>>(),
        vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(5)]
    );
    let third = store
        .list_document_summaries(
            DocumentScope::All,
            Some(DocumentListCursor {
                created_at: second[1].created_at,
                id: second[1].id,
            }),
            2,
        )
        .await
        .unwrap();
    assert_eq!(
        third
            .iter()
            .map(|document| document.id.0)
            .collect::<Vec<_>>(),
        vec![uuid::Uuid::from_u128(4)]
    );
    assert!(store
        .list_document_summaries(DocumentScope::All, None, 0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn ready_summaries_report_whether_anything_is_actually_searchable() {
    let (_dir, store) = temp_store().await;
    let indexed_at = DateTime::<Utc>::from_timestamp(1_700_000_200, 0).unwrap();
    let ready = |raw_id: u128, canonical_text: &str| {
        let mut document = sample_document(None);
        document.id = DocumentId(uuid::Uuid::from_u128(raw_id));
        document.canonical_text = canonical_text.to_owned();
        document.processing_status = DocumentProcessingStatus::Ready;
        document.indexed_revision = Some(document.content_revision);
        document.index_fingerprint = Some("chunker=test;embedder=test".to_owned());
        document.indexed_at = Some(indexed_at);
        document
    };
    // A parser can succeed and still produce nothing to index: an image with
    // no OCR, or a format whose parser is not installed on this host.
    let with_text = ready(2, "売上 grew by 10%.");
    let without_text = ready(1, "");
    let mut still_queued = sample_document(None);
    still_queued.id = DocumentId(uuid::Uuid::from_u128(3));
    for document in [&with_text, &without_text, &still_queued] {
        store.create_document(document).await.unwrap();
    }

    let searchable = store
        .list_document_summaries(DocumentScope::All, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|document| (document.id, document.processing_status, document.searchable))
        .collect::<Vec<_>>();
    assert_eq!(
        searchable,
        vec![
            (
                still_queued.id,
                DocumentProcessingStatus::Queued,
                // Not yet searchable, but for a reason the caller must not
                // confuse with a parser that found nothing.
                false
            ),
            (with_text.id, DocumentProcessingStatus::Ready, true),
            (without_text.id, DocumentProcessingStatus::Ready, false),
        ]
    );
    // The listing decides emptiness in the database, so it must still never
    // carry the text itself.
    assert!(with_text.is_searchable());
    assert!(!without_text.is_searchable());
    assert!(!still_queued.is_searchable());
}

#[tokio::test]
async fn document_project_fk_rejects_orphans_and_direct_project_deletion() {
    let (_dir, store) = temp_store().await;
    let orphan = sample_document(Some(ProjectId::new()));
    assert!(store.create_document(&orphan).await.is_err());

    let project = sample_project();
    store.create_project(&project).await.unwrap();
    let document = sample_document(Some(project.id));
    store.create_document(&document).await.unwrap();
    assert!(entities::project::Entity::delete_by_id(project.id.0)
        .exec(&store.conn)
        .await
        .is_err());
    assert!(store.get_project(project.id).await.unwrap().is_some());
    assert_eq!(
        store
            .get_document(document.id)
            .await
            .unwrap()
            .unwrap()
            .generation(),
        store
            .get_document_generation(document.id)
            .await
            .unwrap()
            .unwrap()
    );
}

#[tokio::test]
async fn live_document_cannot_move_between_project_corpora() {
    let (_dir, store) = temp_store().await;
    let project_a = sample_project();
    let mut project_b = sample_project();
    project_b.id = ProjectId::new();
    store.create_project(&project_a).await.unwrap();
    store.create_project(&project_b).await.unwrap();
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(project_a.id),
        source_uri: Some("file:///scoped.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "project A source".into(),
        source_regions: Vec::new(),
        updated_at: Utc::now(),
    };
    let first = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let moved = DocumentUpsert {
        project_id: Some(project_b.id),
        canonical_text: "must not move".into(),
        source_regions: Vec::new(),
        ..source
    };
    assert!(store
        .upsert_document_and_enqueue_index(&moved, "pipeline-v1", 3)
        .await
        .is_err());
    assert_eq!(store.get_document(moved.id).await.unwrap(), Some(first.0));
}

#[tokio::test]
async fn document_constraints_reject_invalid_catalog_state() {
    let (_dir, store) = temp_store().await;

    let mut empty_media_type = sample_document(None);
    empty_media_type.media_type.clear();
    assert!(store.create_document(&empty_media_type).await.is_err());

    let mut empty_source_uri = sample_document(None);
    empty_source_uri.source_uri = Some(String::new());
    assert!(store.create_document(&empty_source_uri).await.is_err());

    let mut invalid_revision = sample_document(None);
    invalid_revision.content_revision = 0;
    assert!(store.create_document(&invalid_revision).await.is_err());

    let mut future_index = sample_document(None);
    future_index.indexed_revision = Some(2);
    future_index.index_fingerprint = Some("v1".into());
    future_index.indexed_at = Some(Utc::now());
    assert!(store.create_document(&future_index).await.is_err());

    let mut partial_watermark = sample_document(None);
    partial_watermark.indexed_revision = Some(1);
    assert!(store.create_document(&partial_watermark).await.is_err());

    let mut empty_fingerprint = sample_document(None);
    empty_fingerprint.processing_status = DocumentProcessingStatus::Ready;
    empty_fingerprint.indexed_revision = Some(1);
    empty_fingerprint.index_fingerprint = Some(String::new());
    empty_fingerprint.indexed_at = Some(Utc::now());
    assert!(store.create_document(&empty_fingerprint).await.is_err());

    let mut oversized_fingerprint = sample_document(None);
    oversized_fingerprint.processing_status = DocumentProcessingStatus::Ready;
    oversized_fingerprint.indexed_revision = Some(1);
    oversized_fingerprint.index_fingerprint =
        Some("x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1));
    oversized_fingerprint.indexed_at = Some(Utc::now());
    assert!(store.create_document(&oversized_fingerprint).await.is_err());

    let mut invalid_regions = sample_document(None);
    invalid_regions.source_regions = vec![SourceRegion {
        span: ByteSpan::new(4, 5),
        location: SourceLocation::Page {
            number: std::num::NonZeroU32::new(1).unwrap(),
        },
    }];
    assert!(store.create_document(&invalid_regions).await.is_err());

    let mut oversized_source = sample_document(None);
    oversized_source.source_blob = Some(DocumentSourceBlob::from_digest([0; 32], u64::MAX));
    assert!(store.create_document(&oversized_source).await.is_err());

    let mut invalid_source_id = sample_document(None);
    let mut invalid_blob = DocumentSourceBlob::from_digest([0x11; 32], 512);
    invalid_blob.id = uuid::Uuid::new_v4();
    invalid_source_id.source_blob = Some(invalid_blob);
    assert!(store.create_document(&invalid_source_id).await.is_err());
    assert_eq!(
        store.get_document(invalid_source_id.id).await.unwrap(),
        None
    );

    let mut empty_parser_fingerprint = sample_document(None);
    empty_parser_fingerprint.canonical_fingerprint = Some(String::new());
    assert!(store
        .create_document(&empty_parser_fingerprint)
        .await
        .is_err());

    let mut valid_source = sample_document(None);
    valid_source.source_blob = Some(DocumentSourceBlob::from_digest([0x22; 32], 512));
    store.create_document(&valid_source).await.unwrap();
    assert!(entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::SourceSha256,
            sea_orm::sea_query::Expr::value(sea_orm::sea_query::Value::Bytes(
                Some(vec![0x22; 31],)
            )),
        )
        .filter(entities::document::Column::Id.eq(valid_source.id.0))
        .exec(&store.conn)
        .await
        .is_err());
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::SourceBlobId,
            sea_orm::sea_query::Expr::value(Some(uuid::Uuid::new_v4())),
        )
        .filter(entities::document::Column::Id.eq(valid_source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(store.get_document(valid_source.id).await.is_err());
}

#[tokio::test]
async fn source_regions_roundtrip_and_provenance_changes_advance_revision() {
    let (_dir, store) = temp_store().await;
    let page = |number| SourceRegion {
        span: ByteSpan::new(0, 9),
        location: SourceLocation::Page {
            number: std::num::NonZeroU32::new(number).unwrap(),
        },
    };
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///report.pdf".into()),
        media_type: "application/pdf".into(),
        title: Some("Report".into()),
        canonical_text: "page text".into(),
        source_regions: vec![page(1)],
        updated_at: Utc::now(),
    };

    let (first, first_job) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    assert_eq!(first.content_revision, 1);
    assert_eq!(first.source_regions, source.source_regions);
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(first.clone())
    );

    let (same, same_job) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    assert_eq!(same.generation(), first.generation());
    assert_eq!(same_job.id, first_job.id);

    let changed = DocumentUpsert {
        source_regions: vec![page(2)],
        updated_at: Utc::now(),
        ..source
    };
    let (second, _) = store
        .upsert_document_and_enqueue_index(&changed, "pipeline-v1", 3)
        .await
        .unwrap();
    assert_eq!(second.content_revision, 2);
    assert_eq!(second.source_regions, changed.source_regions);
}

#[tokio::test]
async fn raw_source_parse_completion_atomically_enqueues_index() {
    let (_dir, store) = temp_store().await;
    let source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///report.pdf".into()),
        media_type: "application/pdf".into(),
        title: Some("Report".into()),
        source_blob: DocumentSourceBlob::from_digest([0x33; 32], 4_096),
        updated_at: Utc::now(),
    };

    let mut invalid = source.clone();
    invalid.source_blob.id = uuid::Uuid::new_v4();
    let error = store
        .accept_document_source_and_enqueue_parse(&invalid, "parser=pdf-v1", 3)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("blob id does not match its SHA-256 digest"));
    assert_eq!(store.get_document(source.id).await.unwrap(), None);

    let (accepted, parse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "parser=pdf-v1", 3)
        .await
        .unwrap();
    assert_eq!(accepted.source_blob.as_ref(), Some(&source.source_blob));
    assert!(accepted.canonical_text.is_empty());
    assert_eq!(accepted.canonical_fingerprint, None);
    assert_eq!(parse_job.kind, DocumentJobKind::Parse);
    assert_eq!(parse_job.status, DocumentJobStatus::Queued);

    let repeated = store
        .accept_document_source_and_enqueue_parse(&source, "parser=pdf-v1", 9)
        .await
        .unwrap();
    assert_eq!(repeated, (accepted.clone(), parse_job.clone()));
    assert_eq!(
        store
            .ensure_document_index_job(
                source.id,
                accepted.generation(),
                "chunk=v1;embed=test",
                5,
                DocumentIndexJobReason::PipelineChanged,
            )
            .await
            .unwrap(),
        EnsureDocumentIndexJobOutcome::Parsing(parse_job.clone())
    );
    assert_eq!(store.list_document_jobs(source.id).await.unwrap().len(), 1);

    let claim_at = Utc::now() + chrono::Duration::seconds(1);
    let lease_expires_at = claim_at + chrono::Duration::minutes(1);
    let claimed = store
        .claim_document_job(claim_at, lease_expires_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, parse_job.id);
    let lease_token = claimed.lease_token.unwrap();
    assert!(store
        .complete_document_index_job(
            claimed.id,
            lease_token,
            claim_at + chrono::Duration::milliseconds(500),
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get_document_job(claimed.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Running
    );
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some("transient_parse".to_owned())),
        )
        .col_expr(
            entities::document_job::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some("prior attempt".to_owned())),
        )
        .filter(entities::document_job::Column::Id.eq(claimed.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let output = DocumentParseOutput {
        canonical_text: "Page one".into(),
        source_regions: vec![SourceRegion {
            span: ByteSpan::new(0, 8),
            location: SourceLocation::Page {
                number: std::num::NonZeroU32::new(1).unwrap(),
            },
        }],
    };
    let completed_at = claim_at + chrono::Duration::seconds(1);
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_parse_index_insert
             BEFORE INSERT ON document_job
             WHEN NEW.kind = 'index'
             BEGIN SELECT RAISE(FAIL, 'injected index insert failure'); END",
        )
        .await
        .unwrap();
    assert!(store
        .complete_document_parse_job_and_enqueue_index(
            claimed.id,
            lease_token,
            completed_at,
            &output,
            "chunk=v1;embed=test",
            5,
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get_document_job(claimed.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Running
    );
    let still_processing = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(
        still_processing.processing_status,
        DocumentProcessingStatus::Processing
    );
    assert!(still_processing.canonical_text.is_empty());
    assert_eq!(still_processing.canonical_fingerprint, None);
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_parse_index_insert")
        .await
        .unwrap();

    let (parsed, index_job) = store
        .complete_document_parse_job_and_enqueue_index(
            claimed.id,
            lease_token,
            completed_at,
            &output,
            "chunk=v1;embed=test",
            5,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parsed.generation(), accepted.generation());
    assert_eq!(parsed.canonical_text, output.canonical_text);
    assert_eq!(parsed.source_regions, output.source_regions);
    assert_eq!(
        parsed.canonical_fingerprint.as_deref(),
        Some("parser=pdf-v1")
    );
    assert_eq!(parsed.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(index_job.kind, DocumentJobKind::Index);
    assert_eq!(index_job.status, DocumentJobStatus::Queued);
    assert_eq!(index_job.document_id, source.id);
    assert_eq!(index_job.content_revision, parsed.content_revision);
    assert_eq!(index_job.revision_token, parsed.revision_token);
    assert!(store
        .complete_document_parse_job_and_enqueue_index(
            claimed.id,
            lease_token,
            completed_at,
            &output,
            "chunk=v1;embed=test",
            5,
        )
        .await
        .unwrap()
        .is_none());
    let jobs = store.list_document_jobs(source.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, DocumentJobStatus::Succeeded);
    assert_eq!(jobs[0].last_error_code, None);
    assert_eq!(jobs[0].last_error_detail, None);
    assert_eq!(jobs[1], index_job);

    let (reparse, reparse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "parser=pdf-v2", 3)
        .await
        .unwrap();
    assert_eq!(reparse.content_revision, parsed.content_revision + 1);
    assert_ne!(reparse.revision_token, parsed.revision_token);
    assert!(reparse.canonical_text.is_empty());
    assert_eq!(reparse.canonical_fingerprint, None);
    assert_eq!(reparse_job.kind, DocumentJobKind::Parse);
    assert_eq!(
        store
            .get_document_job(index_job.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
}

#[tokio::test]
async fn blob_retirement_coalesces_candidates_and_live_writes_cancel_episodes() {
    let (_dir, store) = temp_store().await;
    let shared_blob = DocumentSourceBlob::from_digest([0x51; 32], 51);
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
    let (_, job_a) = store
        .accept_document_source_and_enqueue_parse(&source_a, "parser-v1", 3)
        .await
        .unwrap();
    store
        .accept_document_source_and_enqueue_parse(&source_b, "parser-v1", 3)
        .await
        .unwrap();
    assert_eq!(
        store.get_blob_retirement(shared_blob.id).await.unwrap(),
        None
    );

    let replacement_b = sample_raw_source(
        source_b.id,
        source_b.source_uri.as_deref().unwrap(),
        DocumentSourceBlob::from_digest([0x52; 32], 52),
    );
    store
        .accept_document_source_and_enqueue_parse(&replacement_b, "parser-v1", 3)
        .await
        .unwrap();
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

    let repeated = store
        .accept_document_source_and_enqueue_parse(&source_a, "parser-v1", 9)
        .await
        .unwrap();
    assert_eq!(repeated.1, job_a);
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
        source_a.source_uri.as_deref().unwrap(),
        DocumentSourceBlob::from_digest([0x53; 32], 53),
    );
    store
        .accept_document_source_and_enqueue_parse(&replacement_a, "parser-v1", 3)
        .await
        .unwrap();
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
    store
        .accept_document_source_and_enqueue_parse(&source_c, "parser-v1", 3)
        .await
        .unwrap();
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
async fn source_replacement_rolls_back_blob_retirement_with_job_insert_failure() {
    let (_dir, store) = temp_store().await;
    let original = sample_raw_source(
        DocumentId::new(),
        "file:///rollback-source.bin",
        DocumentSourceBlob::from_digest([0x61; 32], 61),
    );
    let (original_record, original_job) = store
        .accept_document_source_and_enqueue_parse(&original, "parser-v1", 3)
        .await
        .unwrap();
    let replacement = sample_raw_source(
        original.id,
        original.source_uri.as_deref().unwrap(),
        DocumentSourceBlob::from_digest([0x62; 32], 62),
    );
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_replacement_parse_insert
             BEFORE INSERT ON document_job
             WHEN NEW.kind = 'parse'
             BEGIN SELECT RAISE(FAIL, 'injected replacement failure'); END",
        )
        .await
        .unwrap();
    assert!(store
        .accept_document_source_and_enqueue_parse(&replacement, "parser-v1", 3)
        .await
        .is_err());
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_replacement_parse_insert")
        .await
        .unwrap();

    assert_eq!(
        store.get_document(original.id).await.unwrap(),
        Some(original_record)
    );
    assert_eq!(
        store.get_document_job(original_job.id).await.unwrap(),
        Some(original_job)
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
        DocumentSourceBlob::from_digest([0x63; 32], 63),
    );
    let (record, _) = store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x7b; 32], 81),
    );
    store
        .accept_document_source_and_enqueue_parse(&referenced, "parser-v1", 3)
        .await
        .unwrap();
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
    let shared_blob = DocumentSourceBlob::from_digest([0x71; 32], 71);
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
    store
        .accept_document_source_and_enqueue_parse(&source_a, "parser-v1", 3)
        .await
        .unwrap();
    store
        .accept_document_source_and_enqueue_parse(&source_b, "parser-v1", 3)
        .await
        .unwrap();
    let replacement = sample_raw_source(
        source_a.id,
        source_a.source_uri.as_deref().unwrap(),
        DocumentSourceBlob::from_digest([0x72; 32], 72),
    );
    store
        .accept_document_source_and_enqueue_parse(&replacement, "parser-v1", 3)
        .await
        .unwrap();

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
        DocumentSourceBlob::from_digest([0x73; 32], 73),
    );
    store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x75; 32], 75),
    );
    store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x79; 32], 79),
    );
    store
        .accept_document_source_and_enqueue_parse(&retired_source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x7a; 32], 80),
    );
    store
        .accept_document_source_and_enqueue_parse(&live_source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x76; 32], 76),
    );
    store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x77; 32], 77),
    );
    store
        .accept_document_source_and_enqueue_parse(&exhausted_source, "parser-v1", 3)
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
        DocumentSourceBlob::from_digest([0x78; 32], 78),
    );
    store
        .accept_document_source_and_enqueue_parse(&later_source, "parser-v1", 3)
        .await
        .unwrap();
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
        DocumentSourceBlob::from_digest([0x74; 32], 74),
    );
    store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
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
            DocumentSourceBlob::from_digest([0x80 + index as u8; 32], 80 + index),
        );
        store
            .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
            .await
            .unwrap();
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

#[tokio::test]
async fn ensure_parse_job_advances_parser_changes_and_reuses_the_transition() {
    let (_dir, store) = temp_store().await;
    let source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///parser-upgrade.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        source_blob: DocumentSourceBlob::from_digest([0x22; 32], 128),
        updated_at: Utc::now(),
    };
    let (accepted, parse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
    assert_eq!(
        store
            .ensure_document_parse_job(source.id, accepted.generation(), "parser-v1", 9)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::Existing(parse_job.clone())
    );

    let repair_source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        source_uri: Some("file:///repair-missing-parse.txt".into()),
        source_blob: DocumentSourceBlob::from_digest([0x33; 32], 64),
        ..source.clone()
    };
    let (pending, missing_parse_job) = store
        .accept_document_source_and_enqueue_parse(&repair_source, "parser-v1", 3)
        .await
        .unwrap();
    assert_eq!(pending.canonical_fingerprint, None);
    assert_eq!(
        entities::document_job::Entity::delete_by_id(missing_parse_job.id.0)
            .exec(&store.conn)
            .await
            .unwrap()
            .rows_affected,
        1
    );
    let repaired = store
        .ensure_document_parse_job(repair_source.id, pending.generation(), "parser-v1", 7)
        .await
        .unwrap();
    let EnsureDocumentParseJobOutcome::Enqueued(repaired) = repaired else {
        panic!("expected missing Parse work to be repaired, got {repaired:?}");
    };
    assert_eq!(repaired.generation(), pending.generation());
    assert_eq!(repaired.pipeline_fingerprint, "parser-v1");
    assert_eq!(repaired.max_attempts, 7);

    let claim_at = parse_job.available_at + chrono::Duration::seconds(1);
    let running = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    let (parsed, index_job) = store
        .complete_document_parse_job_and_enqueue_index(
            running.id,
            running.lease_token.unwrap(),
            claim_at + chrono::Duration::seconds(1),
            &DocumentParseOutput {
                canonical_text: "canonical v1".into(),
                source_regions: Vec::new(),
            },
            "index-v1",
            5,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .ensure_document_parse_job(source.id, parsed.generation(), "parser-v1", 3)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::CanonicalCurrent
    );

    let outcome = store
        .ensure_document_parse_job(source.id, parsed.generation(), "parser-v2", 4)
        .await
        .unwrap();
    let EnsureDocumentParseJobOutcome::Enqueued(reparse_job) = outcome else {
        panic!("expected a parser-change reparse job, got {outcome:?}");
    };
    assert_eq!(reparse_job.kind, DocumentJobKind::Parse);
    assert_eq!(reparse_job.pipeline_fingerprint, "parser-v2");
    assert_eq!(reparse_job.max_attempts, 4);
    assert_eq!(reparse_job.content_revision, parsed.content_revision + 1);
    assert_ne!(reparse_job.revision_token, parsed.revision_token);
    let reparsing = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(reparsing.generation(), reparse_job.generation());
    assert_eq!(reparsing.source_blob, parsed.source_blob);
    assert!(reparsing.canonical_text.is_empty());
    assert_eq!(reparsing.canonical_fingerprint, None);
    assert!(reparsing.source_regions.is_empty());
    assert_eq!(
        reparsing.processing_status,
        DocumentProcessingStatus::Queued
    );
    assert_eq!(reparsing.indexed_revision, None);
    assert_eq!(reparsing.index_fingerprint, None);
    assert_eq!(reparsing.indexed_at, None);
    assert_eq!(
        store
            .get_document_job(index_job.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
    assert_eq!(
        store
            .ensure_document_parse_job(source.id, parsed.generation(), "parser-v2", 8)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::GenerationChanged(reparse_job.generation())
    );
    assert_eq!(
        store
            .ensure_document_parse_job(source.id, reparse_job.generation(), "parser-v2", 8)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::Existing(reparse_job)
    );

    let canonical = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "inline canonical".into(),
        source_regions: Vec::new(),
        updated_at: Utc::now(),
    };
    let (canonical, _) = store
        .upsert_document_and_enqueue_index(&canonical, "index-v1", 3)
        .await
        .unwrap();
    assert_eq!(
        store
            .ensure_document_parse_job(canonical.id, canonical.generation(), "parser-v2", 3)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::SourceUnavailable
    );

    let before_failure = store.get_document(source.id).await.unwrap().unwrap();
    let jobs_before_failure = store.list_document_jobs(source.id).await.unwrap();
    let clock_before_failure = store
        .get_document_generation(source.id)
        .await
        .unwrap()
        .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_ensure_parse_job_insert
                 BEFORE INSERT ON document_job
                 BEGIN
                     SELECT RAISE(FAIL, 'injected ensure Parse job failure');
                 END;",
        )
        .await
        .unwrap();
    assert!(store
        .ensure_document_parse_job(source.id, before_failure.generation(), "parser-v3", 3)
        .await
        .is_err());
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(before_failure)
    );
    assert_eq!(
        store.list_document_jobs(source.id).await.unwrap(),
        jobs_before_failure
    );
    assert_eq!(
        store.get_document_generation(source.id).await.unwrap(),
        Some(clock_before_failure)
    );
}

#[tokio::test]
async fn concurrent_ensure_parse_advances_once_and_fences_stale_callers() {
    let (_dir, store) = temp_store().await;
    let source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///concurrent-parser-upgrade.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        source_blob: DocumentSourceBlob::from_digest([0x44; 32], 96),
        updated_at: Utc::now(),
    };
    let (accepted, parse_job) = store
        .accept_document_source_and_enqueue_parse(&source, "parser-v1", 3)
        .await
        .unwrap();
    let claim_at = parse_job.available_at + chrono::Duration::seconds(1);
    let running = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    let (parsed, _) = store
        .complete_document_parse_job_and_enqueue_index(
            running.id,
            running.lease_token.unwrap(),
            claim_at + chrono::Duration::seconds(1),
            &DocumentParseOutput {
                canonical_text: "canonical v1".into(),
                source_regions: Vec::new(),
            },
            "index-v1",
            3,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parsed.generation(), accepted.generation());

    let document_id = source.id;
    let observed_generation = parsed.generation();
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .ensure_document_parse_job(document_id, observed_generation, "parser-v2", 5)
                .await
                .unwrap()
        }));
    }

    let mut enqueued = None;
    let mut fenced = 0;
    for task in tasks {
        match task.await.unwrap() {
            EnsureDocumentParseJobOutcome::Enqueued(job) => {
                assert!(enqueued.replace(job).is_none());
            }
            EnsureDocumentParseJobOutcome::GenerationChanged(generation) => {
                assert_eq!(
                    generation.content_revision,
                    observed_generation.content_revision + 1
                );
                fenced += 1;
            }
            outcome => panic!("unexpected concurrent ensure outcome: {outcome:?}"),
        }
    }
    assert_eq!(fenced, 7);
    let enqueued = enqueued.expect("one caller must enqueue the parser upgrade");
    let current = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(current.generation(), enqueued.generation());
    assert_eq!(
        current.content_revision,
        observed_generation.content_revision + 1
    );
    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(
        jobs.iter()
            .filter(|job| {
                job.generation() == current.generation()
                    && job.kind == DocumentJobKind::Parse
                    && job.pipeline_fingerprint == "parser-v2"
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn document_job_schema_enforces_delivery_and_idempotency_invariants() {
    let (_dir, store) = temp_store().await;
    let document = sample_document(None);
    store.create_document(&document).await.unwrap();
    let document = store.get_document(document.id).await.unwrap().unwrap();
    let now = DateTime::<Utc>::from_timestamp(1_752_148_800, 0).unwrap();
    let make_job =
        |document: &DocumentRecord, fingerprint: &str| entities::document_job::ActiveModel {
            id: Set(uuid::Uuid::new_v4()),
            document_id: Set(document.id.0),
            content_revision: Set(document.content_revision),
            revision_token: Set(document.revision_token),
            kind: Set(DocumentJobKind::Index.as_str().into()),
            status: Set(DocumentJobStatus::Queued.as_str().into()),
            pipeline_fingerprint: Set(fingerprint.into()),
            attempt_count: Set(0),
            max_attempts: Set(5),
            available_at: Set(now),
            lease_token: Set(None),
            lease_expires_at: Set(None),
            started_at: Set(None),
            finished_at: Set(None),
            last_error_code: Set(None),
            last_error_detail: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
    let first = make_job(&document, "pipeline-v1")
        .insert(&store.conn)
        .await
        .unwrap();

    let mut parse_document = sample_document(None);
    parse_document.source_blob = Some(DocumentSourceBlob::from_digest([0x11; 32], 1_024));
    store.create_document(&parse_document).await.unwrap();
    let parse_document = store
        .get_document(parse_document.id)
        .await
        .unwrap()
        .unwrap();
    let mut parse_job = make_job(&parse_document, "parser-v1");
    parse_job.kind = Set(DocumentJobKind::Parse.as_str().into());
    let parse_job = parse_job.insert(&store.conn).await.unwrap();
    assert_eq!(
        store
            .get_document_job(DocumentJobId(parse_job.id))
            .await
            .unwrap()
            .unwrap()
            .kind,
        DocumentJobKind::Parse
    );

    // A document has only one nonterminal pipeline stage at a time.
    assert!(make_job(&document, "pipeline-v2")
        .insert(&store.conn)
        .await
        .is_err());

    // State-dependent attempt, lease, and timestamp rules are independent.
    let another_document = sample_document(None);
    store.create_document(&another_document).await.unwrap();
    let another_document = store
        .get_document(another_document.id)
        .await
        .unwrap()
        .unwrap();
    let mut running_without_lease = make_job(&another_document, "pipeline-v1");
    running_without_lease.status = Set(DocumentJobStatus::Running.as_str().into());
    running_without_lease.attempt_count = Set(1);
    running_without_lease.started_at = Set(Some(now));
    assert!(running_without_lease.insert(&store.conn).await.is_err());

    let mut running_without_attempt = make_job(&another_document, "pipeline-v1");
    running_without_attempt.status = Set(DocumentJobStatus::Running.as_str().into());
    running_without_attempt.lease_token = Set(Some(uuid::Uuid::new_v4()));
    running_without_attempt.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
    assert!(running_without_attempt.insert(&store.conn).await.is_err());
    let mut exhausted_retry = make_job(&another_document, "pipeline-v1");
    exhausted_retry.status = Set(DocumentJobStatus::RetryWait.as_str().into());
    exhausted_retry.attempt_count = Set(5);
    exhausted_retry.started_at = Set(Some(now));
    assert!(exhausted_retry.insert(&store.conn).await.is_err());
    let mut terminal_without_finish = make_job(&another_document, "pipeline-v1");
    terminal_without_finish.status = Set(DocumentJobStatus::Failed.as_str().into());
    terminal_without_finish.attempt_count = Set(5);
    terminal_without_finish.started_at = Set(Some(now));
    assert!(terminal_without_finish.insert(&store.conn).await.is_err());
    let mut terminal_without_attempt = make_job(&another_document, "pipeline-v1");
    terminal_without_attempt.status = Set(DocumentJobStatus::Succeeded.as_str().into());
    terminal_without_attempt.finished_at = Set(Some(now));
    assert!(terminal_without_attempt.insert(&store.conn).await.is_err());

    let mut unknown_kind = make_job(&another_document, "pipeline-v1");
    unknown_kind.kind = Set("unknown".into());
    assert!(unknown_kind.insert(&store.conn).await.is_err());
    assert!(make_job(&another_document, "")
        .insert(&store.conn)
        .await
        .is_err());
    assert!(make_job(&another_document, &"x".repeat(513))
        .insert(&store.conn)
        .await
        .is_err());
    let mut oversized_error = make_job(&another_document, "pipeline-v1");
    oversized_error.last_error_code = Set(Some("e".repeat(129)));
    assert!(oversized_error.insert(&store.conn).await.is_err());
    let mut empty_error = make_job(&another_document, "pipeline-v1");
    empty_error.last_error_code = Set(Some(String::new()));
    assert!(empty_error.insert(&store.conn).await.is_err());
    let mut empty_detail = make_job(&another_document, "pipeline-v1");
    empty_detail.last_error_detail = Set(Some(String::new()));
    assert!(empty_detail.insert(&store.conn).await.is_err());
    let mut oversized_detail = make_job(&another_document, "pipeline-v1");
    oversized_detail.last_error_detail = Set(Some("d".repeat(4097)));
    assert!(oversized_detail.insert(&store.conn).await.is_err());

    let valid_running_document = sample_document(None);
    store
        .create_document(&valid_running_document)
        .await
        .unwrap();
    let valid_running_document = store
        .get_document(valid_running_document.id)
        .await
        .unwrap()
        .unwrap();
    let mut valid_running = make_job(&valid_running_document, "pipeline-v1");
    valid_running.status = Set(DocumentJobStatus::Running.as_str().into());
    valid_running.attempt_count = Set(1);
    valid_running.started_at = Set(Some(now));
    valid_running.lease_token = Set(Some(uuid::Uuid::new_v4()));
    valid_running.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
    valid_running.insert(&store.conn).await.unwrap();

    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Succeeded.as_str()),
        )
        .col_expr(
            entities::document_job::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::document_job::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::document_job::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::document_job::Column::Id.eq(first.id))
        .exec(&store.conn)
        .await
        .unwrap();

    // Terminal history frees the active slot, but the same semantic job is
    // still deduplicated by exact revision, kind, and pipeline fingerprint.
    assert!(make_job(&document, "pipeline-v1")
        .insert(&store.conn)
        .await
        .is_err());
    make_job(&document, "pipeline-v2")
        .insert(&store.conn)
        .await
        .unwrap();

    store.delete_document(document.id).await.unwrap();
    let remaining = entities::document_job::Entity::find()
        .filter(entities::document_job::Column::DocumentId.eq(document.id.0))
        .all(&store.conn)
        .await
        .unwrap();
    assert!(remaining.is_empty());
}

async fn ready_document(store: &DbStore, source: &DocumentUpsert) -> DocumentRecord {
    let (document, job) = store
        .upsert_document_and_enqueue_index(source, "pipeline-v1", 3)
        .await
        .unwrap();
    let claim_at = job.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert!(store
        .complete_document_index_job(
            claimed.id,
            claimed.lease_token.unwrap(),
            claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    store.get_document(document.id).await.unwrap().unwrap()
}

#[tokio::test]
async fn missing_derived_state_requeues_succeeded_job_without_advancing_generation() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///missing-derived.txt".into()),
        media_type: "text/plain".into(),
        title: Some("missing derived".into()),
        canonical_text: "same authoritative source".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(9_000, 0).unwrap(),
    };
    let ready = ready_document(&store, &source).await;
    let original_job = store.list_document_jobs(source.id).await.unwrap().remove(0);

    let outcome = store
        .ensure_document_index_job(
            source.id,
            ready.generation(),
            "pipeline-v1",
            7,
            DocumentIndexJobReason::DerivedStateMissing,
        )
        .await
        .unwrap();
    let EnsureDocumentIndexJobOutcome::Enqueued(job) = outcome else {
        panic!("expected requeued job")
    };
    assert_eq!(job.id, original_job.id);
    assert_eq!(job.generation(), ready.generation());
    assert_eq!(job.status, DocumentJobStatus::Queued);
    assert_eq!(job.max_attempts, 7);
    let current = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(current.generation(), ready.generation());
    assert_eq!(current.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(current.indexed_revision, None);
    assert_eq!(current.index_fingerprint, None);
}

#[tokio::test]
async fn index_maintenance_does_not_implicitly_revive_failed_job() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///failed-maintenance.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "requires an explicit retry".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(9_050, 0).unwrap(),
    };
    let (document, job) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 1)
        .await
        .unwrap();
    let claim_at = job.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "embed_failed",
                None,
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::Failed)
    );

    let outcome = store
        .ensure_document_index_job(
            source.id,
            document.generation(),
            "pipeline-v1",
            5,
            DocumentIndexJobReason::DerivedStateMissing,
        )
        .await
        .unwrap();
    let EnsureDocumentIndexJobOutcome::Failed(failed) = outcome else {
        panic!("failed maintenance job must remain a stable no-op")
    };
    assert_eq!(failed.id, job.id);
    assert_eq!(failed.max_attempts, 1);
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Failed
    );
}

#[tokio::test]
async fn incomplete_succeeded_generation_advances_once_and_reuses_the_new_job() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///incomplete-derived.txt".into()),
        media_type: "text/plain".into(),
        title: Some("incomplete derived".into()),
        canonical_text: "source remains stable".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(9_075, 0).unwrap(),
    };
    let ready = ready_document(&store, &source).await;
    let succeeded = store.list_document_jobs(source.id).await.unwrap().remove(0);
    assert_eq!(succeeded.status, DocumentJobStatus::Succeeded);

    let first = store
        .ensure_document_index_job(
            source.id,
            ready.generation(),
            "pipeline-v1",
            4,
            DocumentIndexJobReason::DerivedStateIncomplete,
        )
        .await
        .unwrap();
    let EnsureDocumentIndexJobOutcome::Enqueued(job) = first else {
        panic!("incomplete succeeded state must advance and enqueue")
    };
    assert_ne!(job.id, succeeded.id);
    assert_eq!(job.content_revision, ready.content_revision + 1);

    assert_eq!(
        store
            .ensure_document_index_job(
                source.id,
                ready.generation(),
                "pipeline-v1",
                4,
                DocumentIndexJobReason::DerivedStateIncomplete,
            )
            .await
            .unwrap(),
        EnsureDocumentIndexJobOutcome::Existing(job.clone())
    );
    let current = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(current.generation(), job.generation());
    assert_eq!(current.canonical_text, ready.canonical_text);
    assert_eq!(current.created_at, ready.created_at);
    assert_eq!(current.updated_at, ready.updated_at);
    assert_eq!(store.list_document_jobs(source.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_pipeline_change_advances_once_and_preserves_source() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///pipeline-change.txt".into()),
        media_type: "text/plain".into(),
        title: Some("pipeline change".into()),
        canonical_text: "same authoritative source".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(9_100, 0).unwrap(),
    };
    let ready = ready_document(&store, &source).await;
    let document_id = source.id;
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let generation = ready.generation();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .ensure_document_index_job(
                    document_id,
                    generation,
                    "pipeline-v2",
                    5,
                    DocumentIndexJobReason::PipelineChanged,
                )
                .await
                .unwrap()
        }));
    }
    let mut job_ids = std::collections::HashSet::new();
    for task in tasks {
        match task.await.unwrap() {
            EnsureDocumentIndexJobOutcome::Enqueued(job)
            | EnsureDocumentIndexJobOutcome::Existing(job) => {
                job_ids.insert(job.id);
            }
            outcome => panic!("unexpected outcome: {outcome:?}"),
        }
    }
    assert_eq!(job_ids.len(), 1);

    let current = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(current.content_revision, ready.content_revision + 1);
    assert_ne!(current.revision_token, ready.revision_token);
    assert_eq!(current.project_id, ready.project_id);
    assert_eq!(current.source_uri, ready.source_uri);
    assert_eq!(current.media_type, ready.media_type);
    assert_eq!(current.title, ready.title);
    assert_eq!(current.canonical_text, ready.canonical_text);
    assert_eq!(current.created_at, ready.created_at);
    assert_eq!(current.updated_at, ready.updated_at);
    assert_eq!(current.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(current.indexed_revision, None);
    assert_eq!(current.index_fingerprint, None);
    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].status, DocumentJobStatus::Succeeded);
    assert_eq!(jobs[1].pipeline_fingerprint, "pipeline-v2");
}

#[tokio::test]
async fn source_revision_and_index_job_commit_and_supersede_together() {
    let (_dir, store) = temp_store().await;
    let document_id = DocumentId::new();
    let first_at = DateTime::<Utc>::from_timestamp(10_000, 0).unwrap();
    let first_source = DocumentUpsert {
        chat_id: None,
        id: document_id,
        project_id: None,
        source_uri: Some("file:///async.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "first".into(),
        source_regions: Vec::new(),
        updated_at: first_at,
    };

    let (first_revision, first_job) = store
        .upsert_document_and_enqueue_index(&first_source, "pipeline-v1", 5)
        .await
        .unwrap();
    assert_eq!(first_revision.content_revision, 1);
    assert_eq!(
        first_revision.processing_status,
        DocumentProcessingStatus::Queued
    );
    assert_eq!(first_job.document_id, first_revision.id);
    assert_eq!(first_job.content_revision, first_revision.content_revision);
    assert_eq!(first_job.revision_token, first_revision.revision_token);
    assert_eq!(
        store.get_document_job(first_job.id).await.unwrap(),
        Some(first_job.clone())
    );

    // A request retry after an ambiguous response must return the exact
    // committed revision/job even when the source timestamp was refreshed.
    let retry_source = DocumentUpsert {
        updated_at: first_at + chrono::Duration::seconds(1),
        ..first_source.clone()
    };
    let retried = store
        .upsert_document_and_enqueue_index(&retry_source, "pipeline-v1", 5)
        .await
        .unwrap();
    assert_eq!(retried, (first_revision.clone(), first_job.clone()));
    assert_eq!(
        store.list_document_jobs(document_id).await.unwrap().len(),
        1
    );

    // Simulate a claimed first job; a new source revision must fence and
    // terminally cancel it before the replacement queued job is inserted.
    let lease = uuid::Uuid::new_v4();
    let claimed_at = first_job.created_at;
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Running.as_str()),
        )
        .col_expr(
            entities::document_job::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::document_job::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease)),
        )
        .col_expr(
            entities::document_job::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(claimed_at + chrono::Duration::minutes(5))),
        )
        .col_expr(
            entities::document_job::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(claimed_at)),
        )
        .filter(entities::document_job::Column::Id.eq(first_job.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let second_at = DateTime::<Utc>::from_timestamp(20_000, 0).unwrap();
    let second_source = DocumentUpsert {
        canonical_text: "second".into(),
        source_regions: Vec::new(),
        updated_at: second_at,
        ..first_source
    };
    let (second_revision, second_job) = store
        .upsert_document_and_enqueue_index(&second_source, "pipeline-v1", 3)
        .await
        .unwrap();
    assert_eq!(second_revision.content_revision, 2);
    assert_eq!(second_job.content_revision, 2);
    assert_eq!(second_job.max_attempts, 3);
    assert_eq!(second_job.revision_token, second_revision.revision_token);

    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].id, first_job.id);
    assert_eq!(jobs[0].status, DocumentJobStatus::Cancelled);
    assert_eq!(jobs[0].lease_token, None);
    assert_eq!(jobs[0].lease_expires_at, None);
    assert!(jobs[0].finished_at.is_some_and(|at| at >= claimed_at));
    assert_eq!(jobs[0].finished_at, Some(second_job.created_at));
    assert_eq!(jobs[1], second_job);

    let unknown = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        ..second_source
    };
    assert!(store
        .upsert_document_and_enqueue_index(&unknown, "", 5)
        .await
        .is_err());
    assert_eq!(store.get_document(unknown.id).await.unwrap(), None);
    assert!(store
        .upsert_document_and_enqueue_index(&unknown, "pipeline-v1", 0)
        .await
        .is_err());
    assert_eq!(store.list_document_jobs(unknown.id).await.unwrap(), vec![]);

    let orphan = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(ProjectId::new()),
        ..unknown
    };
    assert!(store
        .upsert_document_and_enqueue_index(&orphan, "pipeline-v1", 5)
        .await
        .is_err());
    assert_eq!(store.get_document(orphan.id).await.unwrap(), None);
    assert_eq!(store.list_document_jobs(orphan.id).await.unwrap(), vec![]);
}

#[tokio::test]
async fn enqueue_rolls_back_source_when_job_insert_fails() {
    let (_dir, store) = temp_store().await;
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_document_job_insert
                 BEFORE INSERT ON document_job
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document job failure');
                 END;",
        )
        .await
        .unwrap();

    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///rollback.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "must roll back".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(30_000, 0).unwrap(),
    };
    assert!(store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
        .await
        .is_err());
    assert_eq!(store.get_document(source.id).await.unwrap(), None);
    assert_eq!(
        store.get_document_generation(source.id).await.unwrap(),
        None
    );
    assert_eq!(store.list_document_jobs(source.id).await.unwrap(), vec![]);
}

#[tokio::test]
async fn replacement_enqueue_failure_restores_source_and_live_job() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///replacement-rollback.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "original".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(40_000, 0).unwrap(),
    };
    let (_, job) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
        .await
        .unwrap();
    let claimed_at = job.created_at;
    let lease_token = uuid::Uuid::new_v4();
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::Status,
            sea_orm::sea_query::Expr::value(DocumentJobStatus::Running.as_str()),
        )
        .col_expr(
            entities::document_job::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::document_job::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::document_job::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(claimed_at + chrono::Duration::minutes(5))),
        )
        .col_expr(
            entities::document_job::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(claimed_at)),
        )
        .filter(entities::document_job::Column::Id.eq(job.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::ProcessingStatus,
            sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Processing.as_str()),
        )
        .filter(entities::document::Column::Id.eq(source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let original_document = store.get_document(source.id).await.unwrap().unwrap();
    let original_job = store.get_document_job(job.id).await.unwrap().unwrap();
    let original_generation = store
        .get_document_generation(source.id)
        .await
        .unwrap()
        .unwrap();

    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_replacement_document_job_insert
                 BEFORE INSERT ON document_job
                 BEGIN
                     SELECT RAISE(FAIL, 'injected replacement job failure');
                 END;",
        )
        .await
        .unwrap();
    let replacement = DocumentUpsert {
        canonical_text: "replacement".into(),
        source_regions: Vec::new(),
        updated_at: source.updated_at + chrono::Duration::seconds(1),
        ..source
    };
    assert!(store
        .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 5)
        .await
        .is_err());

    assert_eq!(
        store.get_document(replacement.id).await.unwrap(),
        Some(original_document)
    );
    assert_eq!(
        store.get_document_job(job.id).await.unwrap(),
        Some(original_job.clone())
    );
    assert_eq!(
        store.list_document_jobs(replacement.id).await.unwrap(),
        vec![original_job]
    );
    assert_eq!(
        store.get_document_generation(replacement.id).await.unwrap(),
        Some(original_generation)
    );
}

#[tokio::test]
async fn document_delete_failure_rolls_back_tombstone_source_and_jobs() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///delete-rollback.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "retain on failure".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(45_000, 0).unwrap(),
    };
    let (document, job) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 5)
        .await
        .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_document_delete
                 BEFORE DELETE ON document
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document delete failure');
                 END;",
        )
        .await
        .unwrap();

    assert!(store.delete_document(source.id).await.is_err());
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(document.clone())
    );
    assert_eq!(
        store.get_document_generation(source.id).await.unwrap(),
        Some(document.generation())
    );
    assert_eq!(store.get_document_job(job.id).await.unwrap(), Some(job));
}

#[tokio::test]
async fn concurrent_source_enqueues_leave_one_current_revision_and_job() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let document_id = DocumentId::new();
    let writes = (1..=8).map(|revision| {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .upsert_document_and_enqueue_index(
                    &DocumentUpsert {
                        chat_id: None,
                        id: document_id,
                        project_id: None,
                        source_uri: Some("file:///concurrent-async.txt".into()),
                        media_type: "text/plain".into(),
                        title: None,
                        canonical_text: format!("writer {revision}"),
                        source_regions: Vec::new(),
                        updated_at: DateTime::<Utc>::from_timestamp(revision, 0).unwrap(),
                    },
                    "pipeline-v1",
                    5,
                )
                .await
        })
    });

    let mut revisions = Vec::new();
    for result in futures::future::join_all(writes).await {
        let (document, job) = result.unwrap().unwrap();
        assert_eq!(job.content_revision, document.content_revision);
        assert_eq!(job.revision_token, document.revision_token);
        revisions.push(document.content_revision);
    }
    revisions.sort_unstable();
    assert_eq!(revisions, (1..=8).collect::<Vec<_>>());

    let current = store.get_document(document_id).await.unwrap().unwrap();
    let jobs = store.list_document_jobs(document_id).await.unwrap();
    assert_eq!(jobs.len(), 8);
    let active: Vec<_> = jobs
        .iter()
        .filter(|job| job.status == DocumentJobStatus::Queued)
        .collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].content_revision, current.content_revision);
    assert_eq!(active[0].revision_token, current.revision_token);
    assert_eq!(
        jobs.iter()
            .filter(|job| job.status == DocumentJobStatus::Cancelled)
            .count(),
        7
    );
}

#[tokio::test]
async fn concurrent_identical_first_enqueues_reuse_one_revision_and_job() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let document_id = DocumentId::new();
    let writes = (1..=8).map(|request| {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .upsert_document_and_enqueue_index(
                    &DocumentUpsert {
                        chat_id: None,
                        id: document_id,
                        project_id: None,
                        source_uri: Some("file:///identical-concurrent.txt".into()),
                        media_type: "text/plain".into(),
                        title: None,
                        canonical_text: "same source".into(),
                        source_regions: Vec::new(),
                        // Source observation time is deliberately not part of
                        // semantic request identity.
                        updated_at: DateTime::<Utc>::from_timestamp(request, 0).unwrap(),
                    },
                    "pipeline-v1",
                    5,
                )
                .await
        })
    });

    let results = futures::future::join_all(writes).await;
    let first = results[0].as_ref().unwrap().as_ref().unwrap().clone();
    for result in results {
        assert_eq!(result.unwrap().unwrap(), first);
    }
    assert_eq!(first.0.content_revision, 1);
    assert_eq!(
        store.list_document_jobs(document_id).await.unwrap(),
        vec![first.1]
    );
}

#[tokio::test]
async fn document_job_claim_and_heartbeat_require_the_live_lease() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///claim.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "claim me".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(50_000, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let now = queued.available_at + chrono::Duration::seconds(1);
    assert!(store.claim_document_job(now, now).await.is_err());

    let lease_expires_at = now + chrono::Duration::minutes(5);
    let claimed = store
        .claim_document_job(now, lease_expires_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, queued.id);
    assert_eq!(claimed.status, DocumentJobStatus::Running);
    assert_eq!(claimed.attempt_count, 1);
    assert_eq!(claimed.started_at, Some(now));
    assert_eq!(claimed.lease_expires_at, Some(lease_expires_at));
    let lease_token = claimed.lease_token.unwrap();
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Processing
    );
    assert_eq!(
        store
            .claim_document_job(now, lease_expires_at)
            .await
            .unwrap(),
        None
    );

    let heartbeat_at = now + chrono::Duration::minutes(1);
    assert!(!store
        .heartbeat_document_job(
            claimed.id,
            uuid::Uuid::new_v4(),
            heartbeat_at,
            lease_expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert!(!store
        .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, lease_expires_at)
        .await
        .unwrap());
    assert!(!store
        .heartbeat_document_job(
            claimed.id,
            lease_token,
            now - chrono::Duration::seconds(1),
            lease_expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert!(store
        .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, heartbeat_at)
        .await
        .is_err());

    let extended = lease_expires_at + chrono::Duration::minutes(5);
    assert!(store
        .heartbeat_document_job(claimed.id, lease_token, heartbeat_at, extended)
        .await
        .unwrap());
    let heartbeated = store.get_document_job(claimed.id).await.unwrap().unwrap();
    assert_eq!(heartbeated.lease_expires_at, Some(extended));
    assert_eq!(heartbeated.updated_at, heartbeat_at);
    assert!(!store
        .heartbeat_document_job(
            claimed.id,
            lease_token,
            extended,
            extended + chrono::Duration::minutes(5),
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn live_document_job_completion_atomically_publishes_ready_watermark() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///complete-job.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "complete me".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(55_000, 0).unwrap(),
    };
    let (revision, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(5);
    let claimed = store
        .claim_document_job(claimed_at, lease_expires_at)
        .await
        .unwrap()
        .unwrap();
    let completed_at = claimed_at + chrono::Duration::minutes(1);
    assert!(!store
        .complete_document_index_job(claimed.id, uuid::Uuid::new_v4(), completed_at)
        .await
        .unwrap());
    assert!(!store
        .complete_document_index_job(
            claimed.id,
            claimed.lease_token.unwrap(),
            claimed_at - chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    assert!(store
        .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
        .await
        .unwrap());
    assert!(!store
        .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
        .await
        .unwrap());

    let succeeded = store.get_document_job(claimed.id).await.unwrap().unwrap();
    assert_eq!(succeeded.status, DocumentJobStatus::Succeeded);
    assert_eq!(succeeded.lease_token, None);
    assert_eq!(succeeded.lease_expires_at, None);
    assert_eq!(succeeded.finished_at, Some(completed_at));
    assert_eq!(succeeded.last_error_code, None);
    assert_eq!(succeeded.last_error_detail, None);
    let ready = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(ready.processing_status, DocumentProcessingStatus::Ready);
    assert_eq!(ready.indexed_revision, Some(revision.content_revision));
    assert_eq!(ready.index_fingerprint.as_deref(), Some("pipeline-v1"));
    assert_eq!(ready.indexed_at, Some(completed_at));
}

#[tokio::test]
async fn live_document_job_failure_retries_then_fails_permanently() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///fail-job.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "fail and retry".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(56_000, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let first_at = queued.available_at + chrono::Duration::seconds(1);
    let first = store
        .claim_document_job(first_at, first_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    let failed_at = first_at + chrono::Duration::minutes(1);
    let retry_at = failed_at + chrono::Duration::minutes(2);
    assert_eq!(
        store
            .record_document_job_failure(
                first.id,
                first.lease_token.unwrap(),
                failed_at,
                Some(retry_at),
                "embed_timeout",
                Some("provider timed out"),
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::RetryWait)
    );
    let waiting = store.get_document_job(first.id).await.unwrap().unwrap();
    assert_eq!(waiting.status, DocumentJobStatus::RetryWait);
    assert_eq!(waiting.attempt_count, 1);
    assert_eq!(waiting.available_at, retry_at);
    assert_eq!(waiting.finished_at, None);
    assert_eq!(waiting.lease_token, None);
    assert_eq!(waiting.last_error_code.as_deref(), Some("embed_timeout"));
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Queued
    );
    assert_eq!(
        store
            .claim_document_job(failed_at, failed_at + chrono::Duration::minutes(5))
            .await
            .unwrap(),
        None
    );

    let second = store
        .claim_document_job(retry_at, retry_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.attempt_count, 2);
    let permanent_at = retry_at + chrono::Duration::minutes(1);
    assert_eq!(
        store
            .record_document_job_failure(
                second.id,
                second.lease_token.unwrap(),
                permanent_at,
                None,
                "invalid_source",
                None,
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::Failed)
    );
    let failed = store.get_document_job(second.id).await.unwrap().unwrap();
    assert_eq!(failed.status, DocumentJobStatus::Failed);
    assert_eq!(failed.finished_at, Some(permanent_at));
    assert_eq!(failed.last_error_code.as_deref(), Some("invalid_source"));
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Failed
    );
}

#[tokio::test]
async fn document_job_failure_validates_details_and_exhausts_retry_budget() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///exhaust-failure.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "one attempt".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(56_500, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 1)
        .await
        .unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claimed_at, claimed_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    let failed_at = claimed_at + chrono::Duration::minutes(1);
    assert!(store
        .record_document_job_failure(
            claimed.id,
            claimed.lease_token.unwrap(),
            failed_at,
            Some(failed_at),
            "timeout",
            None,
        )
        .await
        .is_err());
    assert!(store
        .record_document_job_failure(
            claimed.id,
            claimed.lease_token.unwrap(),
            failed_at,
            None,
            "",
            None,
        )
        .await
        .is_err());
    assert!(store
        .record_document_job_failure(
            claimed.id,
            claimed.lease_token.unwrap(),
            failed_at,
            None,
            "timeout",
            Some(""),
        )
        .await
        .is_err());
    assert_eq!(
        store
            .record_document_job_failure(
                claimed.id,
                uuid::Uuid::new_v4(),
                failed_at,
                None,
                "timeout",
                None,
            )
            .await
            .unwrap(),
        None
    );

    assert_eq!(
        store
            .record_document_job_failure(
                claimed.id,
                claimed.lease_token.unwrap(),
                failed_at,
                Some(failed_at + chrono::Duration::minutes(1)),
                "timeout",
                Some("retry budget is exhausted"),
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::Failed)
    );
    let failed = store.get_document_job(claimed.id).await.unwrap().unwrap();
    assert_eq!(failed.status, DocumentJobStatus::Failed);
    assert_eq!(failed.finished_at, Some(failed_at));
}

#[tokio::test]
async fn explicit_retry_only_revives_current_failed_index_job() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "retry me".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let (document, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)
        .await
        .unwrap();
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "pipeline-v1",
                9,
            )
            .await
            .unwrap(),
        Some(queued.clone())
    );

    let claim_at = queued.available_at + chrono::Duration::seconds(1);
    let running = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "pipeline-v1",
                9,
            )
            .await
            .unwrap(),
        Some(running.clone())
    );
    assert_eq!(
        store
            .record_document_job_failure(
                running.id,
                running.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "embedding_failed",
                Some("service unavailable"),
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::Failed)
    );
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "other-pipeline",
                4,
            )
            .await
            .unwrap(),
        None
    );

    let retried = store
        .retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Index,
            "pipeline-v1",
            4,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried.id, queued.id);
    assert_eq!(retried.status, DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, 4);
    assert_eq!(retried.lease_token, None);
    assert_eq!(retried.lease_expires_at, None);
    assert_eq!(retried.started_at, None);
    assert_eq!(retried.finished_at, None);
    assert_eq!(retried.last_error_code, None);
    assert_eq!(retried.last_error_detail, None);
    let document = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(document.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(document.indexed_revision, None);
    assert_eq!(document.index_fingerprint, None);
    assert_eq!(document.indexed_at, None);
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "pipeline-v1",
                8,
            )
            .await
            .unwrap(),
        Some(retried.clone())
    );

    let retry_claim_at = retried.available_at + chrono::Duration::seconds(1);
    let retry_running = store
        .claim_document_job(
            retry_claim_at,
            retry_claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(store
        .complete_document_index_job(
            retry_running.id,
            retry_running.lease_token.unwrap(),
            retry_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap());
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "pipeline-v1",
                4,
            )
            .await
            .unwrap(),
        None
    );

    let replacement = DocumentUpsert {
        canonical_text: "replacement".into(),
        source_regions: Vec::new(),
        updated_at: source.updated_at + chrono::Duration::seconds(1),
        ..source.clone()
    };
    let (replacement_document, cancelled) = store
        .upsert_document_and_enqueue_index(&replacement, "pipeline-v2", 2)
        .await
        .unwrap();
    assert_eq!(
        store
            .retry_document_job(
                replacement.id,
                replacement_document.generation(),
                DocumentJobKind::Index,
                "pipeline-v1",
                4,
            )
            .await
            .unwrap(),
        None
    );
    let (newer_document, _) = store
        .upsert_document_and_enqueue_index(
            &DocumentUpsert {
                canonical_text: "newer replacement".into(),
                source_regions: Vec::new(),
                updated_at: source.updated_at + chrono::Duration::seconds(2),
                ..replacement
            },
            "pipeline-v3",
            2,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .get_document_job(cancelled.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                newer_document.generation(),
                DocumentJobKind::Index,
                "pipeline-v2",
                4,
            )
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn explicit_retry_revives_only_the_pending_parse_stage() {
    let (_dir, store) = temp_store().await;
    let source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///retry-report.pdf".into()),
        media_type: "application/pdf".into(),
        title: Some("Retry report".into()),
        source_blob: DocumentSourceBlob::from_digest([0x44; 32], 4_096),
        updated_at: Utc::now(),
    };
    let (document, queued) = store
        .accept_document_source_and_enqueue_parse(&source, "parser=pdf-v1", 1)
        .await
        .unwrap();
    let claim_at = queued.available_at + chrono::Duration::seconds(1);
    let running = store
        .claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .record_document_job_failure(
                running.id,
                running.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "parse_failed",
                Some("malformed page"),
            )
            .await
            .unwrap(),
        Some(DocumentJobStatus::Failed)
    );
    assert_eq!(
        store
            .ensure_document_parse_job(source.id, document.generation(), "parser=pdf-v1", 5,)
            .await
            .unwrap(),
        EnsureDocumentParseJobOutcome::Failed(
            store.get_document_job(queued.id).await.unwrap().unwrap()
        )
    );

    assert_eq!(
        store
            .retry_document_job(
                source.id,
                document.generation(),
                DocumentJobKind::Index,
                "parser=pdf-v1",
                5,
            )
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .retry_document_job(
                source.id,
                DocumentGeneration {
                    content_revision: document.content_revision + 1,
                    revision_token: document.revision_token,
                },
                DocumentJobKind::Parse,
                "parser=pdf-v1",
                5,
            )
            .await
            .unwrap(),
        None
    );
    let retried = store
        .retry_document_job(
            source.id,
            document.generation(),
            DocumentJobKind::Parse,
            "parser=pdf-v1",
            5,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retried.id, queued.id);
    assert_eq!(retried.kind, DocumentJobKind::Parse);
    assert_eq!(retried.status, DocumentJobStatus::Queued);
    assert_eq!(retried.attempt_count, 0);
    assert_eq!(retried.max_attempts, 5);
    assert_eq!(retried.started_at, None);
    assert_eq!(retried.finished_at, None);
    assert_eq!(retried.last_error_code, None);
    assert_eq!(retried.last_error_detail, None);
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Queued
    );
}

#[tokio::test]
async fn completion_document_failure_rolls_back_the_job_transition() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///complete-rollback.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "roll back completion".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(57_000, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let claimed_at = queued.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(claimed_at, claimed_at + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_document_ready
                 BEFORE UPDATE OF processing_status ON document
                 WHEN NEW.processing_status = 'ready'
                 BEGIN
                     SELECT RAISE(FAIL, 'injected document completion failure');
                 END;",
        )
        .await
        .unwrap();

    let completed_at = claimed_at + chrono::Duration::minutes(1);
    assert!(store
        .complete_document_index_job(claimed.id, claimed.lease_token.unwrap(), completed_at,)
        .await
        .is_err());
    assert_eq!(
        store.get_document_job(claimed.id).await.unwrap(),
        Some(claimed)
    );
    let document = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(
        document.processing_status,
        DocumentProcessingStatus::Processing
    );
    assert_eq!(document.indexed_revision, None);
}

#[tokio::test]
async fn expired_document_job_leases_are_reclaimed_then_fail_at_the_attempt_limit() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///lease-recovery.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "recover me".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(60_000, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)
        .await
        .unwrap();
    let first_at = queued.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_at + chrono::Duration::minutes(1);
    let first = store
        .claim_document_job(first_at, first_expiry)
        .await
        .unwrap()
        .unwrap();

    let second_expiry = first_expiry + chrono::Duration::minutes(1);
    let second = store
        .claim_document_job(first_expiry, second_expiry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.started_at, first.started_at);
    assert_ne!(second.lease_token, first.lease_token);
    assert_eq!(second.lease_expires_at, Some(second_expiry));
    assert!(!store
        .heartbeat_document_job(
            first.id,
            first.lease_token.unwrap(),
            first_expiry,
            second_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());

    let fallback_source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        source_uri: Some("file:///after-exhausted-lease.txt".into()),
        canonical_text: "claim after cleanup".into(),
        source_regions: Vec::new(),
        ..source.clone()
    };
    let (_, fallback) = store
        .upsert_document_and_enqueue_index(&fallback_source, "pipeline-v1", 2)
        .await
        .unwrap();
    let fallback_due = second_expiry + chrono::Duration::seconds(1);
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(fallback_due),
        )
        .filter(entities::document_job::Column::Id.eq(fallback.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let final_claim_at = fallback_due + chrono::Duration::seconds(1);
    let claimed_after_cleanup = store
        .claim_document_job(
            final_claim_at,
            final_claim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed_after_cleanup.id, fallback.id);

    let failed = store.get_document_job(first.id).await.unwrap().unwrap();
    assert_eq!(failed.status, DocumentJobStatus::Failed);
    assert_eq!(failed.attempt_count, 2);
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
    assert_eq!(failed.finished_at, Some(final_claim_at));
    assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
    assert_eq!(
        store
            .get_document(source.id)
            .await
            .unwrap()
            .unwrap()
            .processing_status,
        DocumentProcessingStatus::Failed
    );
}

#[tokio::test]
async fn claim_cancels_a_superseded_candidate_then_claims_the_next_job() {
    let (_dir, store) = temp_store().await;
    let first_source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///stale-claim.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "stale".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(70_000, 0).unwrap(),
    };
    let (_, stale_job) = store
        .upsert_document_and_enqueue_index(&first_source, "pipeline-v1", 3)
        .await
        .unwrap();
    let second_source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        source_uri: Some("file:///next-claim.txt".into()),
        canonical_text: "next".into(),
        source_regions: Vec::new(),
        ..first_source.clone()
    };
    let (_, next_job) = store
        .upsert_document_and_enqueue_index(&second_source, "pipeline-v1", 3)
        .await
        .unwrap();

    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::ContentRevision,
            sea_orm::sea_query::Expr::value(2_i64),
        )
        .col_expr(
            entities::document::Column::RevisionToken,
            sea_orm::sea_query::Expr::value(uuid::Uuid::new_v4()),
        )
        .filter(entities::document::Column::Id.eq(first_source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let now = next_job.available_at + chrono::Duration::seconds(1);
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, next_job.id);
    assert_eq!(
        store
            .get_document_job(stale_job.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        DocumentJobStatus::Cancelled
    );
}

#[tokio::test]
async fn claim_reports_exact_identity_status_corruption_without_cancelling() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///claim-corruption.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "exact but inconsistent".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(75_000, 0).unwrap(),
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::ProcessingStatus,
            sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Processing.as_str()),
        )
        .filter(entities::document::Column::Id.eq(source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let now = queued.available_at + chrono::Duration::seconds(1);
    assert!(store
        .claim_document_job(now, now + chrono::Duration::minutes(5))
        .await
        .is_err());
    assert_eq!(
        store.get_document_job(queued.id).await.unwrap(),
        Some(queued)
    );
}

#[tokio::test]
async fn claim_orders_expired_and_queued_jobs_by_effective_due_time() {
    let (_dir, store) = temp_store().await;
    let running_source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///expired-first.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "expired first".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(76_000, 0).unwrap(),
    };
    let (_, running_queued) = store
        .upsert_document_and_enqueue_index(&running_source, "pipeline-v1", 3)
        .await
        .unwrap();
    let first_claim_at = running_queued.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    let running = store
        .claim_document_job(first_claim_at, first_expiry)
        .await
        .unwrap()
        .unwrap();

    let queued_source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        source_uri: Some("file:///queued-second.txt".into()),
        canonical_text: "queued second".into(),
        source_regions: Vec::new(),
        ..running_source
    };
    let (_, queued) = store
        .upsert_document_and_enqueue_index(&queued_source, "pipeline-v1", 3)
        .await
        .unwrap();
    // The running job's original `available_at` is older, but its effective
    // due time is the later lease expiry. The queued job must win.
    let queued_due = first_expiry - chrono::Duration::seconds(30);
    entities::document_job::Entity::update_many()
        .col_expr(
            entities::document_job::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(queued_due),
        )
        .filter(entities::document_job::Column::Id.eq(queued.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let now = first_expiry + chrono::Duration::minutes(1);
    let claimed = store
        .claim_document_job(now, now + chrono::Duration::minutes(5))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, queued.id);
    assert_eq!(claimed.attempt_count, 1);
    assert_eq!(
        store
            .get_document_job(running.id)
            .await
            .unwrap()
            .unwrap()
            .attempt_count,
        1
    );
}

#[tokio::test]
async fn concurrent_document_job_claimers_never_share_a_job() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let mut latest_available_at = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    for index in 0..6 {
        let (_, job) = store
            .upsert_document_and_enqueue_index(
                &DocumentUpsert {
                    chat_id: None,
                    id: DocumentId::new(),
                    project_id: None,
                    source_uri: Some(format!("file:///claim-{index}.txt")),
                    media_type: "text/plain".into(),
                    title: None,
                    canonical_text: format!("document {index}"),
                    source_regions: Vec::new(),
                    updated_at: DateTime::<Utc>::from_timestamp(80_000 + index, 0).unwrap(),
                },
                "pipeline-v1",
                3,
            )
            .await
            .unwrap();
        latest_available_at = latest_available_at.max(job.available_at);
    }
    let now = latest_available_at + chrono::Duration::seconds(1);
    let claims = (0..12).map(|_| {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .claim_document_job(now, now + chrono::Duration::minutes(5))
                .await
        })
    });

    let mut claimed_ids = Vec::new();
    for result in futures::future::join_all(claims).await {
        if let Some(job) = result.unwrap().unwrap() {
            claimed_ids.push(job.id);
        }
    }
    assert_eq!(claimed_ids.len(), 6);
    claimed_ids.sort_by_key(|id| id.0);
    claimed_ids.dedup();
    assert_eq!(claimed_ids.len(), 6);
}

#[tokio::test]
async fn concurrent_claim_and_replacement_enqueue_preserve_one_current_job() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    for iteration in 0..8 {
        let source = DocumentUpsert {
            chat_id: None,
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some(format!("file:///claim-enqueue-{iteration}.txt")),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "first".into(),
            source_regions: Vec::new(),
            updated_at: DateTime::<Utc>::from_timestamp(90_000 + iteration * 2, 0).unwrap(),
        };
        let (_, queued) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        let now = queued.available_at + chrono::Duration::minutes(1);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

        let claim_store = store.clone();
        let claim_barrier = barrier.clone();
        let claim = tokio::spawn(async move {
            claim_barrier.wait().await;
            claim_store
                .claim_document_job(now, now + chrono::Duration::minutes(5))
                .await
        });
        let enqueue_store = store.clone();
        let enqueue_barrier = barrier.clone();
        let replacement = DocumentUpsert {
            canonical_text: "replacement".into(),
            source_regions: Vec::new(),
            updated_at: source.updated_at + chrono::Duration::seconds(1),
            ..source.clone()
        };
        let enqueue = tokio::spawn(async move {
            enqueue_barrier.wait().await;
            enqueue_store
                .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 3)
                .await
        });
        barrier.wait().await;

        claim.await.unwrap().unwrap().unwrap();
        enqueue.await.unwrap().unwrap();
        let current = store.get_document(source.id).await.unwrap().unwrap();
        let jobs = store.list_document_jobs(source.id).await.unwrap();
        let active: Vec<_> = jobs
            .iter()
            .filter(|job| !job.status.is_terminal())
            .collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].content_revision, current.content_revision);
        assert_eq!(active[0].revision_token, current.revision_token);
    }
}

#[tokio::test]
async fn concurrent_delete_and_enqueue_leave_one_coherent_generation() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    for iteration in 0..8 {
        let source = DocumentUpsert {
            chat_id: None,
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some(format!("file:///delete-enqueue-{iteration}.txt")),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "first".into(),
            source_regions: Vec::new(),
            updated_at: DateTime::<Utc>::from_timestamp(110_000 + iteration * 2, 0).unwrap(),
        };
        let (first, _) = store
            .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
            .await
            .unwrap();
        assert_eq!(first.content_revision, 1);
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));

        let delete_store = store.clone();
        let delete_barrier = barrier.clone();
        let id = source.id;
        let deletion = tokio::spawn(async move {
            delete_barrier.wait().await;
            delete_store.delete_document(id).await
        });
        let enqueue_store = store.clone();
        let enqueue_barrier = barrier.clone();
        let replacement = DocumentUpsert {
            canonical_text: "replacement".into(),
            source_regions: Vec::new(),
            updated_at: source.updated_at + chrono::Duration::seconds(1),
            ..source.clone()
        };
        let enqueue = tokio::spawn(async move {
            enqueue_barrier.wait().await;
            enqueue_store
                .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 3)
                .await
        });
        barrier.wait().await;

        let tombstone = deletion.await.unwrap().unwrap();
        let (enqueued, _) = enqueue.await.unwrap().unwrap();
        let retained = store
            .get_document_generation(source.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.content_revision, 3);
        assert_eq!(
            retained.content_revision,
            tombstone.content_revision.max(enqueued.content_revision)
        );
        match store.get_document(source.id).await.unwrap() {
            Some(current) => {
                assert_eq!(current.generation(), retained);
                let jobs = store.list_document_jobs(source.id).await.unwrap();
                assert_eq!(jobs.len(), 1);
                assert_eq!(jobs[0].generation(), retained);
            }
            None => {
                assert_eq!(retained, tombstone);
                assert!(store
                    .list_document_jobs(source.id)
                    .await
                    .unwrap()
                    .is_empty());
            }
        }
    }
}

#[tokio::test]
async fn document_upsert_revisions_and_index_watermark_are_compare_and_set() {
    let (_dir, store) = temp_store().await;
    let id = DocumentId::derive("file:///report.txt");
    let first_at = DateTime::<Utc>::from_timestamp(10_000, 0).unwrap();
    let first = DocumentUpsert {
        id,
        chat_id: None,
        project_id: None,
        source_uri: Some("file:///report.txt".into()),
        media_type: "text/plain".into(),
        title: Some("Report".into()),
        canonical_text: "first version".into(),
        source_regions: Vec::new(),
        updated_at: first_at,
    };

    let revision_one = store.upsert_document(&first).await.unwrap();
    assert_eq!(revision_one.content_revision, 1);
    assert_eq!(revision_one.created_at, first_at);
    assert_eq!(revision_one.indexed_revision, None);
    assert_eq!(
        revision_one.processing_status,
        DocumentProcessingStatus::Queued
    );
    assert!(store
        .mark_document_indexed(id, 1, revision_one.revision_token, "", first_at)
        .await
        .is_err());
    assert!(store
        .mark_document_indexed(
            id,
            1,
            revision_one.revision_token,
            &"x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1),
            first_at,
        )
        .await
        .is_err());
    assert!(store
        .mark_document_indexed(id, 1, revision_one.revision_token, "index-v1", first_at,)
        .await
        .unwrap());

    let second_at = DateTime::<Utc>::from_timestamp(20_000, 0).unwrap();
    let second = DocumentUpsert {
        canonical_text: "second version".into(),
        source_regions: Vec::new(),
        updated_at: second_at,
        ..first
    };
    let revision_two = store.upsert_document(&second).await.unwrap();
    assert_eq!(revision_two.content_revision, 2);
    assert_eq!(revision_two.created_at, first_at);
    assert_eq!(revision_two.updated_at, second_at);
    assert_ne!(revision_two.revision_token, revision_one.revision_token);
    assert_eq!(revision_two.indexed_revision, None);
    assert_eq!(
        revision_two.processing_status,
        DocumentProcessingStatus::Queued
    );
    assert_eq!(revision_two.index_fingerprint, None);
    assert_eq!(revision_two.indexed_at, None);

    // A late indexer for revision one cannot mark revision two current.
    assert!(!store
        .mark_document_indexed(id, 1, revision_one.revision_token, "stale", second_at)
        .await
        .unwrap());
    assert!(store
        .mark_document_indexed(id, 2, revision_two.revision_token, "index-v2", second_at,)
        .await
        .unwrap());
    let indexed = store.get_document(id).await.unwrap().unwrap();
    assert_eq!(indexed.indexed_revision, Some(2));
    assert_eq!(indexed.processing_status, DocumentProcessingStatus::Ready);
    assert_eq!(indexed.index_fingerprint.as_deref(), Some("index-v2"));
    assert_eq!(indexed.indexed_at, Some(second_at));
    assert!(!store
        .clear_document_index(id, 2, revision_one.revision_token)
        .await
        .unwrap());
    assert!(store
        .clear_document_index(id, 2, revision_two.revision_token)
        .await
        .unwrap());
    let cleared = store.get_document(id).await.unwrap().unwrap();
    assert_eq!(cleared.indexed_revision, None);
    assert_eq!(cleared.processing_status, DocumentProcessingStatus::Queued);
    assert_eq!(cleared.index_fingerprint, None);
    assert_eq!(cleared.indexed_at, None);

    assert!(!store
        .mark_document_indexed(
            DocumentId::new(),
            1,
            uuid::Uuid::new_v4(),
            "missing",
            second_at,
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn stale_revision_token_cannot_mark_a_recreated_document_indexed() {
    let (_dir, store) = temp_store().await;
    let first = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "old lifecycle".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let old = store.upsert_document(&first).await.unwrap();
    store.delete_document(first.id).await.unwrap();
    let recreated_at = DateTime::<Utc>::from_timestamp(2, 0).unwrap();
    let recreated = store
        .upsert_document(&DocumentUpsert {
            chat_id: None,
            id: old.id,
            project_id: old.project_id,
            source_uri: old.source_uri.clone(),
            media_type: old.media_type.clone(),
            title: old.title.clone(),
            canonical_text: "new lifecycle".into(),
            source_regions: Vec::new(),
            updated_at: recreated_at,
        })
        .await
        .unwrap();

    assert_eq!(recreated.content_revision, 3);
    assert_ne!(recreated.revision_token, old.revision_token);
    assert!(!store
        .mark_document_indexed(
            recreated.id,
            old.content_revision,
            old.revision_token,
            "stale",
            recreated.updated_at,
        )
        .await
        .unwrap());
    assert!(store
        .mark_document_indexed(
            recreated.id,
            recreated.content_revision,
            recreated.revision_token,
            "current",
            recreated.updated_at,
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn document_generation_clock_survives_unknown_delete_and_recreation() {
    let (_dir, store) = temp_store().await;
    let id = DocumentId::new();

    let unknown_tombstone = store.delete_document(id).await.unwrap();
    assert_eq!(unknown_tombstone.content_revision, 1);
    assert_eq!(store.delete_document(id).await.unwrap(), unknown_tombstone);
    assert_eq!(
        store.get_document_generation(id).await.unwrap(),
        Some(unknown_tombstone)
    );
    assert_eq!(store.get_document(id).await.unwrap(), None);

    let source = DocumentUpsert {
        id,
        chat_id: None,
        project_id: None,
        source_uri: Some("file:///generation-clock.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "first live source".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(100_000, 0).unwrap(),
    };
    let first = store.upsert_document(&source).await.unwrap();
    assert_eq!(first.content_revision, 2);
    assert_ne!(first.revision_token, unknown_tombstone.revision_token);
    let second = store
        .upsert_document(&DocumentUpsert {
            canonical_text: "second live source".into(),
            source_regions: Vec::new(),
            ..source.clone()
        })
        .await
        .unwrap();
    assert_eq!(second.content_revision, 3);

    let tombstone = store.delete_document(id).await.unwrap();
    assert_eq!(tombstone.content_revision, 4);
    assert_eq!(store.delete_document(id).await.unwrap(), tombstone);
    let recreated = store.upsert_document(&source).await.unwrap();
    assert_eq!(recreated.content_revision, 5);
    assert_ne!(recreated.revision_token, tombstone.revision_token);
    assert_eq!(
        recreated.generation(),
        store.get_document_generation(id).await.unwrap().unwrap()
    );
}

#[tokio::test]
async fn pending_document_retirement_survives_reopen_and_uses_exact_cas() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("retirement.db").display()
    );
    let id = DocumentId::new();
    let source = DocumentUpsert {
        id,
        chat_id: None,
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "retire me".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let store = DbStore::connect(&url).await.unwrap();
    store.upsert_document(&source).await.unwrap();
    let tombstone = store.delete_document(id).await.unwrap();
    assert_eq!(store.delete_document(id).await.unwrap(), tombstone);
    assert_eq!(
        store
            .list_pending_document_retirements(None, 0)
            .await
            .unwrap(),
        vec![]
    );
    drop(store);

    let store = DbStore::connect(&url).await.unwrap();
    assert_eq!(
        store
            .list_pending_document_retirements(None, 10)
            .await
            .unwrap(),
        vec![(id, tombstone)]
    );

    let recreated = store
        .upsert_document(&DocumentUpsert {
            canonical_text: "new lifecycle".into(),
            source_regions: Vec::new(),
            updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
            ..source
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .list_pending_document_retirements(None, 10)
            .await
            .unwrap(),
        vec![(id, tombstone)]
    );
    assert_eq!(
        store.get_pending_document_retirement(id).await.unwrap(),
        Some(tombstone)
    );
    assert!(store
        .complete_document_retirement(id, tombstone)
        .await
        .unwrap());
    assert!(!store
        .complete_document_retirement(id, tombstone)
        .await
        .unwrap());

    let current_tombstone = store.delete_document(id).await.unwrap();
    assert_eq!(
        current_tombstone.content_revision,
        recreated.content_revision + 1
    );
    assert!(!store
        .complete_document_retirement(id, tombstone)
        .await
        .unwrap());
    assert!(store
        .complete_document_retirement(id, current_tombstone)
        .await
        .unwrap());
    assert!(!store
        .complete_document_retirement(id, current_tombstone)
        .await
        .unwrap());
    assert!(store
        .list_pending_document_retirements(None, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pending_document_retirement_cursor_advances_and_can_wrap() {
    let (_dir, store) = temp_store().await;
    let ids = [1_u128, 2, 3].map(|value| DocumentId(uuid::Uuid::from_u128(value)));
    let mut generations = Vec::new();
    for id in ids {
        generations.push(store.delete_document(id).await.unwrap());
    }

    assert_eq!(
        store
            .list_pending_document_retirements(None, 2)
            .await
            .unwrap(),
        vec![(ids[0], generations[0]), (ids[1], generations[1])]
    );
    assert_eq!(
        store
            .list_pending_document_retirements(Some(ids[1]), 2)
            .await
            .unwrap(),
        vec![(ids[2], generations[2])]
    );
    assert!(store
        .list_pending_document_retirements(Some(ids[2]), 2)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_pending_document_retirements(None, 1)
            .await
            .unwrap(),
        vec![(ids[0], generations[0])]
    );
}

#[tokio::test]
async fn document_generation_overflow_leaves_source_and_clock_unchanged() {
    let (_dir, store) = temp_store().await;
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "maximum generation".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(101_000, 0).unwrap(),
    };
    let first = store.upsert_document(&source).await.unwrap();
    entities::document_generation::Entity::update_many()
        .col_expr(
            entities::document_generation::Column::ContentRevision,
            sea_orm::sea_query::Expr::value(i64::MAX),
        )
        .filter(entities::document_generation::Column::DocumentId.eq(source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::ContentRevision,
            sea_orm::sea_query::Expr::value(i64::MAX),
        )
        .filter(entities::document::Column::Id.eq(source.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let before = store.get_document(source.id).await.unwrap().unwrap();
    assert_eq!(before.content_revision, i64::MAX);
    assert_eq!(before.revision_token, first.revision_token);

    assert!(store.upsert_document(&source).await.is_err());
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        store.get_document_generation(source.id).await.unwrap(),
        Some(before.generation())
    );

    assert!(store
        .ensure_document_index_job(
            source.id,
            before.generation(),
            "pipeline-v1",
            3,
            DocumentIndexJobReason::DerivedStateIncomplete,
        )
        .await
        .is_err());
    assert_eq!(
        store.get_document(source.id).await.unwrap(),
        Some(before.clone())
    );
    assert_eq!(
        store.get_document_generation(source.id).await.unwrap(),
        Some(before.generation())
    );
    assert!(store
        .list_document_jobs(source.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn concurrent_first_document_upserts_allocate_distinct_revisions() {
    let (_dir, store) = temp_store().await;
    let first = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "a".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let second = DocumentUpsert {
        canonical_text: "b".into(),
        source_regions: Vec::new(),
        ..first.clone()
    };

    let (first, second) = tokio::join!(
        store.upsert_document(&first),
        store.upsert_document(&second)
    );
    let mut revisions = [
        first.unwrap().content_revision,
        second.unwrap().content_revision,
    ];
    revisions.sort_unstable();
    assert_eq!(revisions, [1, 2]);
}

#[tokio::test]
async fn document_upsert_rolls_back_when_project_is_unknown() {
    let (_dir, store) = temp_store().await;
    let upsert = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(ProjectId::new()),
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "content".into(),
        source_regions: Vec::new(),
        updated_at: Utc::now(),
    };
    assert!(store.upsert_document(&upsert).await.is_err());
    assert_eq!(store.get_document(upsert.id).await.unwrap(), None);
    assert_eq!(
        store.get_document_generation(upsert.id).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn concurrent_document_upserts_allocate_distinct_revisions() {
    let (_dir, store) = temp_store().await;
    let base = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "base".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let id = base.id;
    assert_eq!(
        store.upsert_document(&base).await.unwrap().content_revision,
        1
    );
    let a = DocumentUpsert {
        canonical_text: "a".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
        ..base.clone()
    };
    let b = DocumentUpsert {
        canonical_text: "b".into(),
        source_regions: Vec::new(),
        updated_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
        ..base
    };

    let (a, b) = tokio::join!(store.upsert_document(&a), store.upsert_document(&b));
    let mut revisions = [a.unwrap().content_revision, b.unwrap().content_revision];
    revisions.sort_unstable();
    assert_eq!(revisions, [2, 3]);
    let current = store.get_document(id).await.unwrap().unwrap();
    assert_eq!(current.content_revision, 3);
    assert!(matches!(current.canonical_text.as_str(), "a" | "b"));
    assert_eq!(current.indexed_revision, None);
}

#[tokio::test]
async fn high_contention_document_upserts_do_not_drop_writers() {
    let (_dir, store) = temp_store().await;
    let id = DocumentId::new();
    let writes = (0..64).map(|i| {
        let store = store.clone();
        async move {
            store
                .upsert_document(&DocumentUpsert {
                    id,
                    chat_id: None,
                    project_id: None,
                    source_uri: None,
                    media_type: "text/plain".into(),
                    title: None,
                    canonical_text: format!("writer {i}"),
                    source_regions: Vec::new(),
                    updated_at: DateTime::<Utc>::from_timestamp(i, 0).unwrap(),
                })
                .await
                .unwrap()
                .content_revision
        }
    });

    let mut revisions = futures::future::join_all(writes).await;
    revisions.sort_unstable();
    assert_eq!(revisions, (1..=64).collect::<Vec<_>>());
    assert_eq!(
        store
            .get_document(id)
            .await
            .unwrap()
            .unwrap()
            .content_revision,
        64
    );
}

#[tokio::test]
async fn m0006_upgrades_an_existing_store_without_losing_records() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("upgrade.db").display()
    );
    let conn = Database::connect(&url).await.unwrap();
    conn.execute_unprepared("PRAGMA foreign_keys=ON;")
        .await
        .unwrap();
    migration::Migrator::up(&conn, Some(5)).await.unwrap();
    let store = DbStore { conn: conn.clone() };
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    migration::Migrator::up(&conn, None).await.unwrap();

    assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
    let mut document = sample_document(None);
    let supplied_token = document.revision_token;
    store.create_document(&document).await.unwrap();
    let stored = store.get_document(document.id).await.unwrap().unwrap();
    assert_ne!(stored.revision_token, supplied_token);
    document.revision_token = stored.revision_token;
    assert_eq!(stored, document);
}

#[tokio::test]
async fn m0011_preserves_legacy_documents_as_conversationless() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("document-chat-upgrade.db").display()
    );
    let conn = Database::connect(&url).await.unwrap();
    migration::Migrator::up(&conn, Some(10)).await.unwrap();
    let document_id = DocumentId::new();
    let revision_token = uuid::Uuid::new_v4();
    conn.execute_unprepared(&format!(
        "INSERT INTO document_generation \
         (document_id, content_revision, revision_token, tombstone, retirement_pending) \
         VALUES (X'{}', 1, X'{}', 0, 0)",
        document_id.0.simple(),
        revision_token.simple()
    ))
    .await
    .unwrap();
    conn.execute_unprepared(&format!(
        "INSERT INTO document \
         (id, project_id, source_uri, media_type, title, source_blob_id, source_sha256, \
          source_byte_len, canonical_text, canonical_fingerprint, source_regions, \
         content_revision, revision_token, processing_status, indexed_revision, \
          index_fingerprint, created_at, updated_at, indexed_at) \
         VALUES (X'{}', NULL, 'file:///legacy.txt', 'text/plain', NULL, NULL, \
          NULL, NULL, 'legacy source', NULL, '[]', 1, X'{}', 'queued', \
          NULL, NULL, '2026-07-22 00:00:00+00:00', '2026-07-22 00:00:00+00:00', NULL)",
        document_id.0.simple(),
        revision_token.simple()
    ))
    .await
    .unwrap();

    migration::Migrator::up(&conn, None).await.unwrap();
    let store = DbStore { conn: conn.clone() };
    let legacy = store.get_document(document_id).await.unwrap().unwrap();
    assert_eq!(legacy.chat_id, None);
    assert_eq!(
        store.list_documents(DocumentScope::Unscoped).await.unwrap(),
        vec![legacy]
    );
    let columns = conn
        .query_all_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA table_info(document)".to_owned(),
        ))
        .await
        .unwrap();
    assert!(columns.iter().any(|row| {
        row.try_get::<String>("", "name")
            .is_ok_and(|name| name == "chat_id")
    }));
}

#[tokio::test]
async fn m0013_adds_outputs_to_an_existing_conversation_store() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("output-upgrade.db").display()
    );
    let conn = Database::connect(&url).await.unwrap();
    conn.execute_unprepared("PRAGMA foreign_keys=ON;")
        .await
        .unwrap();
    migration::Migrator::up(&conn, Some(12)).await.unwrap();
    let store = DbStore { conn: conn.clone() };
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    migration::Migrator::up(&conn, None).await.unwrap();

    // The conversation survives, and it can now own outputs.
    assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
    assert!(store.list_outputs(chat.id, 10).await.unwrap().is_empty());
    let request = crate::deliverable::CreateOutput {
        id: crate::id::OutputId::new(),
        chat_id: chat.id,
        filename: "brief.md".into(),
        revision: crate::deliverable::NewOutputRevision {
            id: crate::id::OutputRevisionId::new(),
            byte_len: 7,
            sha256: [3; 32],
            turn_id: None,
            citations: Vec::new(),
            created_at: DateTime::<Utc>::from_timestamp(1_710_000_000, 0).unwrap(),
        },
    };
    let created = store.create_output(&request).await.unwrap();
    assert_eq!(store.list_outputs(chat.id, 10).await.unwrap(), [created]);
}

#[tokio::test]
async fn m0015_adds_output_citations_without_changing_existing_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("output-citation-upgrade.db").display()
    );
    let conn = Database::connect(&url).await.unwrap();
    conn.execute_unprepared("PRAGMA foreign_keys=ON;")
        .await
        .unwrap();
    migration::Migrator::up(&conn, Some(14)).await.unwrap();
    let store = DbStore { conn: conn.clone() };
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let request = crate::deliverable::CreateOutput {
        id: crate::id::OutputId::new(),
        chat_id: chat.id,
        filename: "brief.md".into(),
        revision: crate::deliverable::NewOutputRevision {
            id: crate::id::OutputRevisionId::new(),
            byte_len: 7,
            sha256: [3; 32],
            turn_id: None,
            citations: Vec::new(),
            created_at: DateTime::<Utc>::from_timestamp(1_710_000_000, 0).unwrap(),
        },
    };
    let created = store.create_output(&request).await.unwrap();

    migration::Migrator::up(&conn, None).await.unwrap();

    assert_eq!(store.get_output(created.id).await.unwrap(), Some(created));
    assert!(store
        .list_output_revision_citations(request.revision.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn chats_and_messages_roundtrip() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
    assert_eq!(store.list_chats().await.unwrap(), vec![chat.clone()]);
    assert_eq!(store.get_chat(ChatId::new()).await.unwrap(), None);

    let msg = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        content: "hi there".into(),
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
    };
    store.append_message(&msg).await.unwrap();
    assert_eq!(store.list_messages(chat.id).await.unwrap(), vec![msg]);
}

async fn make_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    model: &str,
    now: DateTime<Utc>,
) -> entities::turn_run::ActiveModel {
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
        content: Set("turn input".into()),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    entities::turn_run::ActiveModel {
        id: Set(turn_id.0),
        chat_id: Set(chat_id.0),
        agent_run_id: Set(crate::id::AgentRunId::foreground_for_chat(chat_id).0),
        agent_run_depth: Set(0),
        input_message_id: Set(input_message_id.0),
        output_message_id: Set(None),
        model: Set(model.into()),
        status: Set(TurnRunStatus::Queued.as_str().into()),
        attempt_count: Set(0),
        max_attempts: Set(crate::model::TurnRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        model_steps: Set(0),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        available_at: Set(now),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        steer_revision: Set(0),
        last_steer_applied_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
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
    let stored = store.get_turn_run(TurnId(first.id)).await.unwrap().unwrap();
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
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap(), vec![stored]);

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
        content: Set("done".into()),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::turn_run::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(1),
        )
        .col_expr(
            entities::turn_run::Column::OutputMessageId,
            sea_orm::sea_query::Expr::value(Some(first_output_id.0)),
        )
        .col_expr(
            entities::turn_run::Column::StartedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::turn_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::turn_run::Column::Id.eq(first.id))
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

    let mut cross_chat_coordinator = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    cross_chat_coordinator.agent_run_id =
        Set(crate::id::AgentRunId::foreground_for_chat(chat.id).0);
    assert!(cross_chat_coordinator.insert(&store.conn).await.is_err());

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
    running_without_lease.started_at = Set(Some(now));
    assert!(running_without_lease.insert(&store.conn).await.is_err());

    let mut cancelling_without_lease =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    cancelling_without_lease.status = Set(TurnRunStatus::Cancelling.as_str().into());
    cancelling_without_lease.attempt_count = Set(1);
    cancelling_without_lease.claim_count = Set(1);
    cancelling_without_lease.started_at = Set(Some(now));
    assert!(cancelling_without_lease.insert(&store.conn).await.is_err());

    let mut retry_without_error = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    retry_without_error.status = Set(TurnRunStatus::RetryWait.as_str().into());
    retry_without_error.attempt_count = Set(1);
    retry_without_error.claim_count = Set(1);
    retry_without_error.max_attempts = Set(2);
    retry_without_error.started_at = Set(Some(now));
    assert!(retry_without_error.insert(&store.conn).await.is_err());

    let mut failed_without_finish = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    failed_without_finish.status = Set(TurnRunStatus::Failed.as_str().into());
    failed_without_finish.attempt_count = Set(1);
    failed_without_finish.claim_count = Set(1);
    failed_without_finish.started_at = Set(Some(now));
    failed_without_finish.last_error_code = Set(Some("provider_error".into()));
    assert!(failed_without_finish.insert(&store.conn).await.is_err());

    let mut completed_without_output =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    completed_without_output.status = Set(TurnRunStatus::Completed.as_str().into());
    completed_without_output.attempt_count = Set(1);
    completed_without_output.claim_count = Set(1);
    completed_without_output.started_at = Set(Some(now));
    completed_without_output.finished_at = Set(Some(now));
    assert!(completed_without_output.insert(&store.conn).await.is_err());

    let mut queued_with_output = make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    queued_with_output.output_message_id = Set(Some(first_output_id.0));
    assert!(queued_with_output.insert(&store.conn).await.is_err());

    let mut completed_with_wrong_output =
        make_queued_turn(&store, invalid_chat.id, "gpt-5", now).await;
    completed_with_wrong_output.status = Set(TurnRunStatus::Completed.as_str().into());
    completed_with_wrong_output.attempt_count = Set(1);
    completed_with_wrong_output.claim_count = Set(1);
    completed_with_wrong_output.started_at = Set(Some(now));
    completed_with_wrong_output.finished_at = Set(Some(now));
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
    oversized_error.started_at = Set(Some(now));
    oversized_error.finished_at = Set(Some(now));
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
    running.started_at = Set(Some(now));
    let running_token = uuid::Uuid::new_v4();
    running.lease_token = Set(Some(running_token));
    running.lease_expires_at = Set(Some(now + chrono::Duration::minutes(1)));
    entities::turn_claim::ActiveModel {
        token: Set(running_token),
        turn_id: Set(running_turn_id),
        attempt_count: Set(1),
        claim_count: Set(1),
        claimed_at: Set(now),
        lease_expires_at: Set(now + chrono::Duration::minutes(1)),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    running.insert(&store.conn).await.unwrap();
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Cancelling.as_str()),
        )
        .filter(entities::turn_run::Column::Id.eq(running_turn_id))
        .exec(&store.conn)
        .await
        .unwrap();
    assert!(make_queued_turn(&store, running_chat.id, "gpt-5", now)
        .await
        .insert(&store.conn)
        .await
        .is_err());

    let valid_failure = entities::turn_failure::ActiveModel {
        lease_token: Set(running_token),
        turn_id: Set(running_turn_id),
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
            "INSERT INTO turn_failure (
                lease_token, turn_id, attempt_count, model_steps,
                input_tokens, output_tokens, cache_read_input_tokens,
                cache_creation_input_tokens, requested_retry_at, error_code,
                error_detail, resolved_at, result_status
            ) VALUES (
                '{running_token}', '{running_turn_id}', 1, {},
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

    assert!(entities::turn_claim::ActiveModel {
        token: Set(uuid::Uuid::new_v4()),
        turn_id: Set(running_turn_id),
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
    duplicate_lease.started_at = Set(Some(now));
    duplicate_lease.lease_token = Set(Some(running_token));
    duplicate_lease.lease_expires_at = Set(Some(now + chrono::Duration::minutes(1)));
    assert!(duplicate_lease.insert(&store.conn).await.is_err());

    let mismatched_receipt_chat = sample_chat();
    store.create_chat(&mismatched_receipt_chat).await.unwrap();
    let mut mismatched_receipt =
        make_queued_turn(&store, mismatched_receipt_chat.id, "gpt-5", now).await;
    let mismatched_turn_id = mismatched_receipt.id.clone().unwrap();
    let mismatched_token = uuid::Uuid::new_v4();
    entities::turn_claim::ActiveModel {
        token: Set(mismatched_token),
        turn_id: Set(mismatched_turn_id),
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
    mismatched_receipt.started_at = Set(Some(now));
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
    retry_wait.started_at = Set(Some(now));
    retry_wait.last_error_code = Set(Some("provider_unavailable".into()));
    retry_wait.insert(&store.conn).await.unwrap();

    let failed_chat = sample_chat();
    store.create_chat(&failed_chat).await.unwrap();
    let mut failed = make_queued_turn(&store, failed_chat.id, "gpt-5", now).await;
    failed.status = Set(TurnRunStatus::Failed.as_str().into());
    failed.attempt_count = Set(1);
    failed.claim_count = Set(1);
    failed.started_at = Set(Some(now));
    failed.finished_at = Set(Some(now));
    failed.last_error_code = Set(Some("unsafe_to_retry".into()));
    failed.last_error_detail = Set(Some("tool outcome is ambiguous".into()));
    failed.insert(&store.conn).await.unwrap();

    for started in [false, true] {
        let cancelled_chat = sample_chat();
        store.create_chat(&cancelled_chat).await.unwrap();
        let mut cancelled = make_queued_turn(&store, cancelled_chat.id, "gpt-5", now).await;
        cancelled.status = Set(TurnRunStatus::Cancelled.as_str().into());
        cancelled.finished_at = Set(Some(now));
        if started {
            cancelled.attempt_count = Set(1);
            cancelled.claim_count = Set(1);
            cancelled.started_at = Set(Some(now));
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
    missing.input_message_id = Set(MessageId::new().0);
    assert!(missing.insert(&store.conn).await.is_err());

    let mut wrong_chat = make_queued_turn(&store, first_chat.id, "gpt-5", now).await;
    wrong_chat.chat_id = Set(second_chat.id.0);
    assert!(wrong_chat.insert(&store.conn).await.is_err());

    let mut wrong_turn = make_queued_turn(&store, first_chat.id, "gpt-5", now).await;
    wrong_turn.id = Set(TurnId::new().0);
    assert!(wrong_turn.insert(&store.conn).await.is_err());
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
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap().len(), 1);
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
    assert!(store.list_turn_runs(other.id).await.unwrap().is_empty());
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
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap().len(), 1);
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
    assert_eq!(
        store.list_turn_runs(competing_chat.id).await.unwrap().len(),
        1
    );
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
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().id,
        turn_id
    );
    assert_eq!(
        store.list_messages(first_chat.id).await.unwrap().len()
            + store.list_messages(second_chat.id).await.unwrap().len(),
        1
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
        .claim_turn_run(token, claimed_at, expiry)
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
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
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
        .claim_turn_run(
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    assert!(store
        .claim_turn_run(
            uuid::Uuid::nil(),
            claimed_at,
            claimed_at + chrono::Duration::seconds(1)
        )
        .await
        .is_err());
    let first_token = uuid::Uuid::new_v4();
    assert!(store
        .claim_turn_run(first_token, claimed_at, claimed_at)
        .await
        .is_err());
    let first_expiry = claimed_at + chrono::Duration::minutes(1);
    let first = store
        .claim_turn_run(first_token, claimed_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(first.lease_token, Some(first_token));
    assert_eq!(
        store
            .claim_turn_run(first_token, claimed_at, first_expiry)
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
        .heartbeat_turn_run(
            turn_id,
            uuid::Uuid::new_v4(),
            heartbeat_at,
            first_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert!(!store
        .heartbeat_turn_run(turn_id, first_token, heartbeat_at, first_expiry)
        .await
        .unwrap());
    assert!(store
        .heartbeat_turn_run(turn_id, first_token, heartbeat_at, heartbeat_at)
        .await
        .is_err());

    let extended = first_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn_run(turn_id, first_token, heartbeat_at, extended)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_turn_run(turn_id)
            .await
            .unwrap()
            .unwrap()
            .updated_at,
        canonical_heartbeat_at
    );
    assert!(!store
        .heartbeat_turn_run(
            turn_id,
            first_token,
            heartbeat_at - chrono::Duration::seconds(1),
            extended + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    assert_eq!(
        store
            .claim_turn_run(
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
        .claim_turn_run(second_token, extended, second_expiry)
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
        .heartbeat_turn_run(
            turn_id,
            first_token,
            extended + chrono::Duration::seconds(1),
            second_expiry + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());

    let exhausted = store
        .claim_turn_run(
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
    let failed = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.attempt_count, 2);
    assert_eq!(failed.lease_token, None);
    assert_eq!(failed.lease_expires_at, None);
    assert_eq!(failed.finished_at, Some(second_expiry));
    assert_eq!(failed.last_error_code.as_deref(), Some("lease_expired"));
    let recovered = store
        .record_turn_run_failure_and_append_event(
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
            .claim_turn_run(
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
        store.get_turn_run(next.id).await.unwrap().unwrap().status,
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
        .claim_turn_run(
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
    let parked = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Resuming.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(resume_at),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(resume_at),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .filter(entities::turn_run::Column::LeaseToken.eq(first.lease_token))
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
        .claim_turn_run(
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
        content: "resumed answer".into(),
        created_at: resume_at + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn_run(turn_id, first_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        None,
        "the earlier lease segment must not complete the resumed turn"
    );
    assert!(matches!(
        store
            .complete_turn_run(turn_id, resumed_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert!(store
        .complete_turn_run(turn_id, first_token, 0, output.created_at, &output)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn client_wait_parks_resolves_and_recovers_exactly() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let _accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "connect a folder")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = Utc::now();
    let turn_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            turn_lease,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({"suggested_name": "Documents"}),
    };
    let progress = crate::model::TurnCheckpointProgress {
        model_steps: 3,
        usage: crate::provider::Usage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 2,
        },
    };
    assert!(store
        .park_turn_for_client_tool_call(
            turn_id,
            turn_lease,
            0,
            crate::model::TurnCheckpointProgress {
                model_steps: 0,
                usage: crate::provider::Usage::default(),
            },
            Utc::now(),
            &request,
        )
        .await
        .is_err());
    let stale_steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                stale_steer_id,
                turn_id,
                chat.id,
                "apply before native checkpoint",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Accepted(_)
    ));
    let first_steer_at = Utc::now();
    assert!(matches!(
        store
            .apply_turn_steer(
                turn_id,
                turn_lease,
                stale_steer_id,
                1,
                None,
                &[],
                first_steer_at,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ApplyTurnSteerOutcome::Applied(_)
    ));
    let checkpoint_at = Utc::now();
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                0,
                progress,
                checkpoint_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::OutputSuperseded(turn)
            if turn.steer_revision == 1
    ));

    let pending_steer_id = TurnSteerId::new();
    assert!(matches!(
        store
            .accept_turn_steer(
                pending_steer_id,
                turn_id,
                chat.id,
                "pending before native checkpoint",
                false,
            )
            .await
            .unwrap(),
        AcceptTurnSteerOutcome::Accepted(_)
    ));
    let pending_checkpoint_at = Utc::now();
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                1,
                progress,
                pending_checkpoint_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::SteerPending(turn)
            if turn.steer_revision == 1
    ));
    let second_steer_at = Utc::now();
    assert!(matches!(
        store
            .apply_turn_steer(
                turn_id,
                turn_lease,
                pending_steer_id,
                2,
                None,
                &[],
                second_steer_at,
            )
            .await
            .unwrap()
            .unwrap()
            .outcome,
        ApplyTurnSteerOutcome::Applied(_)
    ));

    let parked_at = Utc::now();
    assert_eq!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                uuid::Uuid::new_v4(),
                2,
                progress,
                parked_at,
                &request,
            )
            .await
            .unwrap(),
        None
    );
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    let (parked_turn, parked_call, parked_wait) = match store
        .park_turn_for_client_tool_call(turn_id, turn_lease, 2, progress, parked_at, &request)
        .await
        .unwrap()
        .unwrap()
    {
        ParkTurnForClientCallOutcome::Parked {
            turn, call, wait, ..
        } => (turn, call, wait),
        outcome => panic!("unexpected park outcome: {outcome:?}"),
    };
    assert_eq!(parked_turn.status, TurnRunStatus::WaitingForClient);
    assert_eq!(parked_turn.lease_token, None);
    assert_eq!(parked_turn.model_steps, progress.model_steps);
    assert_eq!(parked_turn.usage, progress.usage);
    assert_eq!(parked_call.status, ToolCallStatus::Pending);
    assert_eq!(
        parked_wait.status,
        crate::model::TurnClientWaitStatus::Waiting
    );
    assert_eq!((parked_wait.attempt_count, parked_wait.claim_count), (1, 1));
    assert_eq!(parked_wait.progress, progress);
    let conflicting_progress = crate::model::TurnCheckpointProgress {
        model_steps: progress.model_steps + 1,
        ..progress
    };
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                2,
                conflicting_progress,
                parked_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::IdentityConflict
    ));
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "must stay occupied")
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(turn) if turn.id == turn_id
    ));

    let executor_id = uuid::Uuid::new_v4();
    let client_lease = uuid::Uuid::new_v4();
    let client_claimed_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .claim_client_tool_call(
                request.id,
                chat.id,
                executor_id,
                client_lease,
                client_claimed_at,
                client_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Claimed(_)
    ));
    let resolved_at = client_claimed_at + chrono::Duration::seconds(1);
    let resolution = ToolCallResolution::Completed {
        result: "root-1".into(),
    };
    let journaled = store
        .resolve_client_tool_call_and_append_event(
            request.id,
            chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(journaled.outcome, ResolveToolCallOutcome::Resolved);
    assert_eq!(journaled.terminal_event, None);
    assert_eq!(
        journaled.turn.as_ref().map(|turn| turn.status),
        Some(TurnRunStatus::Resuming)
    );
    let resumable = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(resumable.status, TurnRunStatus::Resuming);
    assert_eq!((resumable.attempt_count, resumable.claim_count), (1, 1));
    assert_eq!(resumable.model_steps, progress.model_steps);
    assert_eq!(resumable.usage, progress.usage);
    let wait = entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(entities::turn_client_wait::Entity::update_many()
        .col_expr(
            entities::turn_client_wait::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(0),
        )
        .filter(entities::turn_client_wait::Column::CallId.eq(request.id.0))
        .exec(&store.conn)
        .await
        .is_err());
    assert!(entities::turn_client_wait::Entity::update_many()
        .col_expr(
            entities::turn_client_wait::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(u32::MAX) + 1),
        )
        .filter(entities::turn_client_wait::Column::CallId.eq(request.id.0))
        .exec(&store.conn)
        .await
        .is_err());
    assert_eq!(
        wait.status,
        crate::model::TurnClientWaitStatus::Resumed.as_str()
    );
    assert_eq!(wait.closed_at, Some(resumable.updated_at));

    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                2,
                progress,
                Utc::now(),
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Existing { turn, wait, .. }
            if turn.status == TurnRunStatus::Resuming
                && wait.status == crate::model::TurnClientWaitStatus::Resumed
    ));
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed = store
        .claim_turn_run(
            resumed_lease,
            resolved_at,
            resolved_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    assert_eq!(resumed.model_steps, progress.model_steps);
    assert_eq!(resumed.usage, progress.usage);
    let regressing_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "must not commit".into(),
        created_at: resolved_at + chrono::Duration::microseconds(1),
    };
    assert_eq!(
        store
            .complete_turn_run_and_append_event(
                turn_id,
                turn_lease,
                2,
                regressing_output.created_at,
                &regressing_output,
                crate::provider::Usage::default(),
                crate::provider::StopReason::EndTurn,
            )
            .await
            .unwrap(),
        None,
        "a stale claim remains lease-lost even when its proposed usage is lower"
    );
    assert!(store
        .complete_turn_run_and_append_event(
            turn_id,
            resumed_lease,
            2,
            regressing_output.created_at,
            &regressing_output,
            crate::provider::Usage::default(),
            crate::provider::StopReason::EndTurn,
        )
        .await
        .is_err());
    assert!(store
        .list_messages(chat.id)
        .await
        .unwrap()
        .iter()
        .all(|message| message.id != regressing_output.id));

    let second_progress = crate::model::TurnCheckpointProgress {
        model_steps: 2,
        usage: crate::provider::Usage {
            input_tokens: 17,
            output_tokens: 9,
            cache_read_input_tokens: 4,
            cache_creation_input_tokens: 1,
        },
    };
    let expected_total_usage = crate::provider::Usage {
        input_tokens: progress.usage.input_tokens + second_progress.usage.input_tokens,
        output_tokens: progress.usage.output_tokens + second_progress.usage.output_tokens,
        cache_read_input_tokens: progress.usage.cache_read_input_tokens
            + second_progress.usage.cache_read_input_tokens,
        cache_creation_input_tokens: progress.usage.cache_creation_input_tokens
            + second_progress.usage.cache_creation_input_tokens,
    };
    let second_request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        provider_id: "native-second".into(),
        name: "open_file".into(),
        arguments: serde_json::json!({"root_id": "root-1"}),
        ..request.clone()
    };
    let second_parked_at = resolved_at + chrono::Duration::seconds(1);
    let second_parked = store
        .park_turn_for_client_tool_call(
            turn_id,
            resumed_lease,
            2,
            second_progress,
            second_parked_at,
            &second_request,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        second_parked,
        ParkTurnForClientCallOutcome::Parked { ref turn, ref wait, .. }
            if turn.model_steps == progress.model_steps + second_progress.model_steps
                && turn.usage == expected_total_usage
                && wait.progress == second_progress
    ));
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                resumed_lease,
                2,
                second_progress,
                Utc::now(),
                &second_request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Existing { turn, wait, .. }
            if turn.model_steps == progress.model_steps + second_progress.model_steps
                && turn.usage == expected_total_usage
                && wait.progress == second_progress
    ));
    let twice_parked = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(
        twice_parked.model_steps,
        progress.model_steps + second_progress.model_steps
    );
    assert_eq!(twice_parked.usage, expected_total_usage);
}

#[tokio::test]
async fn client_wait_accounting_overflow_rolls_back_the_checkpoint() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "overflow")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease_token = uuid::Uuid::new_v4();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .claim_turn_run(
            lease_token,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(i64::from(u32::MAX)),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    assert!(store
        .park_turn_for_client_tool_call(
            turn_id,
            lease_token,
            0,
            crate::model::TurnCheckpointProgress {
                model_steps: 1,
                usage: crate::provider::Usage {
                    input_tokens: 1,
                    ..crate::provider::Usage::default()
                },
            },
            claimed_at + chrono::Duration::seconds(1),
            &request,
        )
        .await
        .is_err());
    assert!(store.list_tool_calls(chat.id).await.unwrap().is_empty());
    assert!(entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    let still_running = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.lease_token, Some(lease_token));
    assert_eq!(still_running.usage.input_tokens, u32::MAX);
}

async fn park_test_client_wait(
    store: &DbStore,
    chat_id: ChatId,
) -> (TurnId, crate::model::ClientToolCallRequest, DateTime<Utc>) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat_id, "gpt-5", "native action")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let turn_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            turn_lease,
            claimed_at,
            claimed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "native".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
    };
    let parked_at = claimed_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .park_turn_for_client_tool_call(
                turn_id,
                turn_lease,
                0,
                test_checkpoint_progress(),
                parked_at,
                &request,
            )
            .await
            .unwrap()
            .unwrap(),
        ParkTurnForClientCallOutcome::Parked { .. }
    ));
    (turn_id, request, parked_at)
}

async fn park_test_user_questions(
    store: &DbStore,
    chat_id: ChatId,
) -> (TurnId, crate::model::ClientToolCallRequest, DateTime<Utc>) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat_id, "gpt-5", "ask me")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected question turn acceptance: {outcome:?}"),
    };
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let request = crate::model::ClientToolCallRequest {
        id: CallId::new(),
        chat_id,
        turn_id,
        provider_id: "provider-question".into(),
        name: crate::ASK_USER_QUESTIONS_TOOL.into(),
        arguments: serde_json::json!({
            "questions": [
                {
                    "id": "target",
                    "header": "Target",
                    "question": "Where should I deploy?",
                    "options": [
                        {"id": "staging", "label": "Staging", "description": "Deploy for internal verification."},
                        {"id": "production", "label": "Production", "description": "Deploy to customers."}
                    ],
                    "allow_free_form": false
                },
                {
                    "id": "note",
                    "header": "Note",
                    "question": "Anything else I should know?",
                    "allow_free_form": true
                }
            ]
        }),
    };
    let parked_at = claimed_at + chrono::Duration::seconds(1);
    let parked = store
        .park_turn_for_client_tool_call(
            turn_id,
            lease,
            0,
            test_checkpoint_progress(),
            parked_at,
            &request,
        )
        .await
        .unwrap()
        .unwrap();
    match parked {
        ParkTurnForClientCallOutcome::Parked {
            renderer_event:
                Some(SequencedEvent {
                    event:
                        AgentEvent::UserQuestionsAsked {
                            call_id,
                            turn_id: event_turn_id,
                        },
                    ..
                }),
            ..
        } => {
            assert_eq!(call_id, request.id);
            assert_eq!(event_turn_id, turn_id);
        }
        outcome => panic!("unexpected question checkpoint: {outcome:?}"),
    }
    (turn_id, request, parked_at)
}

fn sample_user_answers() -> crate::AnswerUserQuestions {
    crate::AnswerUserQuestions {
        answers: vec![
            crate::UserQuestionAnswer {
                question_id: "target".into(),
                option_id: Some("staging".into()),
                free_form: None,
            },
            crate::UserQuestionAnswer {
                question_id: "note".into(),
                option_id: None,
                free_form: Some("Keep the rollout reversible.".into()),
            },
        ],
    }
}

#[tokio::test]
async fn user_questions_survive_reconnect_and_answer_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("questions.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    drop(store);

    let restarted = DbStore::connect(&url).await.unwrap();
    let pending = restarted
        .list_pending_user_questions(chat.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, request.id);
    assert_eq!(pending[0].turn_id, turn_id);
    assert_eq!(
        pending[0]
            .questions
            .iter()
            .map(|question| question.id.as_str())
            .collect::<Vec<_>>(),
        vec!["target", "note"]
    );
    assert!(matches!(
        restarted
            .claim_client_tool_call(
                request.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                parked_at + chrono::Duration::seconds(1),
                parked_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    ));

    let answer_request = crate::AnswerUserQuestionsRequest {
        chat_id: chat.id,
        call_id: request.id,
        answers: sample_user_answers(),
    };
    let answered_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        restarted
            .answer_user_questions(&answer_request, answered_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Answered(turn)
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    assert!(restarted
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed_at = answered_at + chrono::Duration::seconds(1);
    let resumed = restarted
        .claim_turn_run(
            resumed_lease,
            resumed_at,
            resumed_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("the exact parked turn must be reclaimable");
    assert_eq!(resumed.id, turn_id);
    assert_eq!((resumed.attempt_count, resumed.claim_count), (1, 2));
    let transcript = restarted
        .get_chat_transcript(chat.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1,
        "answering must not create a second user turn"
    );
    assert!(matches!(
        restarted
            .answer_user_questions(&answer_request, answered_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Existing(turn)
            if turn.id == turn_id
    ));
    let contradictory = crate::AnswerUserQuestionsRequest {
        answers: crate::AnswerUserQuestions {
            answers: vec![
                crate::UserQuestionAnswer {
                    question_id: "target".into(),
                    option_id: Some("production".into()),
                    free_form: None,
                },
                crate::UserQuestionAnswer {
                    question_id: "note".into(),
                    option_id: None,
                    free_form: Some("Keep the rollout reversible.".into()),
                },
            ],
        },
        ..answer_request
    };
    assert_eq!(
        restarted
            .answer_user_questions(&contradictory, answered_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::AnswerConflict
    );
    let call = restarted
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == request.id)
        .unwrap();
    assert_eq!(call.status, ToolCallStatus::Completed);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(
            call.result.as_deref().expect("answer result")
        )
        .unwrap(),
        sample_user_answers()
    );
    assert!(matches!(
        restarted
            .request_turn_cancellation(turn_id, resumed_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Requested(_)
    ));
    assert!(matches!(
        restarted
            .finish_turn_cancellation(
                turn_id,
                resumed_lease,
                resumed_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap()
            .unwrap(),
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
    assert_eq!(
        restarted.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted
    );
}

#[tokio::test]
async fn user_question_answer_validation_and_cancellation_are_closed() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let invalid = crate::AnswerUserQuestionsRequest {
        chat_id: chat.id,
        call_id: request.id,
        answers: crate::AnswerUserQuestions {
            answers: vec![crate::UserQuestionAnswer {
                question_id: "target".into(),
                option_id: Some("not-an-option".into()),
                free_form: None,
            }],
        },
    };
    assert_eq!(
        store
            .answer_user_questions(&invalid, parked_at)
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::InvalidAnswer
    );
    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    assert_eq!(
        store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: other_chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at,
            )
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Unavailable
    );
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, parked_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Cancelled(turn)
            if turn.status == TurnRunStatus::Cancelled
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap(),
        crate::AnswerUserQuestionsOutcome::Unavailable
    );
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted
    );
}

#[tokio::test]
async fn user_question_answer_and_cancel_race_has_one_serial_outcome() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let answer = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap()
    });
    let cancel_store = store.clone();
    let cancel = tokio::spawn(async move {
        barrier.wait().await;
        cancel_store
            .request_turn_cancellation(turn_id, parked_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap()
    });
    let answer = answer.await.unwrap();
    let cancel = cancel.await.unwrap();
    assert!(matches!(
        answer,
        crate::AnswerUserQuestionsOutcome::Answered(_)
            | crate::AnswerUserQuestionsOutcome::Unavailable
    ));
    assert!(matches!(
        cancel,
        RequestTurnCancellationOutcome::Requested(_)
            | RequestTurnCancellationOutcome::Existing(_)
            | RequestTurnCancellationOutcome::Cancelled(_)
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelled
    );
}

#[tokio::test]
async fn pending_question_projection_serializes_with_answer() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let call_id = request.id;
    let request_turn_id = request.turn_id;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let list_store = store.clone();
    let list_barrier = barrier.clone();
    let list = tokio::spawn(async move {
        list_barrier.wait().await;
        list_store.list_pending_user_questions(chat.id).await
    });
    let answer_store = store.clone();
    let answer = tokio::spawn(async move {
        barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
    });
    let listed = list
        .await
        .unwrap()
        .expect("projection must not observe drift");
    assert!(
        listed.is_empty()
            || (listed.len() == 1
                && listed[0].call_id == call_id
                && listed[0].turn_id == request_turn_id)
    );
    assert!(matches!(
        answer.await.unwrap().unwrap(),
        crate::AnswerUserQuestionsOutcome::Answered(_)
    ));
    assert!(store
        .list_pending_user_questions(chat.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn user_question_answer_and_worker_claim_race_is_recoverable() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_user_questions(&store, chat.id).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let answer_store = store.clone();
    let answer_barrier = barrier.clone();
    let answer = tokio::spawn(async move {
        answer_barrier.wait().await;
        answer_store
            .answer_user_questions(
                &crate::AnswerUserQuestionsRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    answers: sample_user_answers(),
                },
                parked_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap()
    });
    let claim_store = store.clone();
    let first_lease = uuid::Uuid::new_v4();
    let claim_at = parked_at + chrono::Duration::seconds(2);
    let claim = tokio::spawn(async move {
        barrier.wait().await;
        claim_store
            .claim_turn_run(
                first_lease,
                claim_at,
                claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
    });
    assert!(matches!(
        answer.await.unwrap(),
        crate::AnswerUserQuestionsOutcome::Answered(turn)
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    let first_claim = claim.await.unwrap();
    let (claimed, lease) = if let Some(turn) = first_claim.turn {
        (turn, first_lease)
    } else {
        let lease = uuid::Uuid::new_v4();
        let turn = store
            .claim_turn_run(
                lease,
                claim_at + chrono::Duration::seconds(1),
                claim_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("answer wake remains durably claimable after an early scan");
        (turn, lease)
    };
    assert_eq!(claimed.id, turn_id);
    assert_eq!((claimed.attempt_count, claimed.claim_count), (1, 2));
    let cancel_at = claim_at + chrono::Duration::seconds(2);
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, cancel_at)
            .await
            .unwrap()
            .unwrap(),
        RequestTurnCancellationOutcome::Requested(_)
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(turn_id, lease, cancel_at + chrono::Duration::seconds(1))
            .await
            .unwrap()
            .unwrap(),
        crate::FinishTurnCancellationOutcome::Cancelled(_)
    ));
}

#[tokio::test]
async fn client_wait_schema_rejects_invalid_scope_claim_and_lifecycle() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_client_wait(&store, chat.id).await;
    let wait = entities::turn_client_wait::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();

    let second_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "native-second".into(),
        name: "connect_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: parked_at + chrono::Duration::microseconds(1),
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&second_call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));

    let valid_second = entities::turn_client_wait::ActiveModel {
        call_id: Set(second_call.id.0),
        turn_id: Set(turn_id.0),
        chat_id: Set(chat.id.0),
        park_lease_token: Set(wait.park_lease_token),
        attempt_count: Set(wait.attempt_count),
        claim_count: Set(wait.claim_count),
        model_steps: Set(1),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        status: Set(crate::model::TurnClientWaitStatus::Waiting.as_str().into()),
        parked_at: Set(second_call.created_at),
        closed_at: Set(None),
    };
    assert!(valid_second.clone().insert(&store.conn).await.is_err());

    let mut wrong_claim = valid_second.clone();
    wrong_claim.claim_count = Set(wait.claim_count + 1);
    wrong_claim.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    wrong_claim.closed_at = Set(Some(second_call.created_at));
    assert!(wrong_claim.insert(&store.conn).await.is_err());

    let mut wrong_scope = valid_second.clone();
    wrong_scope.chat_id = Set(ChatId::new().0);
    wrong_scope.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    wrong_scope.closed_at = Set(Some(second_call.created_at));
    assert!(wrong_scope.insert(&store.conn).await.is_err());

    let mut missing_close = valid_second.clone();
    missing_close.status = Set(crate::model::TurnClientWaitStatus::Resumed.as_str().into());
    assert!(missing_close.insert(&store.conn).await.is_err());

    let mut close_before_park = valid_second;
    close_before_park.status = Set(crate::model::TurnClientWaitStatus::Cancelled
        .as_str()
        .into());
    close_before_park.closed_at = Set(Some(
        second_call.created_at - chrono::Duration::microseconds(1),
    ));
    assert!(close_before_park.insert(&store.conn).await.is_err());
}

#[tokio::test]
async fn client_wait_cancellation_fences_unclaimed_and_claimed_native_work() {
    let (_dir, store) = temp_store().await;

    let unclaimed_chat = sample_chat();
    store.create_chat(&unclaimed_chat).await.unwrap();
    let (unclaimed_turn, unclaimed_call, unclaimed_parked_at) =
        park_test_client_wait(&store, unclaimed_chat.id).await;
    let cancelled_at = unclaimed_parked_at + chrono::Duration::seconds(1);
    let cancelled = store
        .request_turn_cancellation_and_append_event(unclaimed_turn, cancelled_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        cancelled.outcome,
        RequestTurnCancellationOutcome::Cancelled(ref turn)
            if turn.status == TurnRunStatus::Cancelled
                && turn.usage == test_checkpoint_progress().usage
    ));
    assert!(matches!(
        cancelled.terminal_event,
        Some(SequencedEvent {
            event: AgentEvent::TurnCancelled { usage },
            ..
        }) if usage == test_checkpoint_progress().usage
    ));
    assert_eq!(
        store.list_tool_calls(unclaimed_chat.id).await.unwrap()[0].status,
        ToolCallStatus::Cancelled
    );
    let unclaimed_wait = entities::turn_client_wait::Entity::find_by_id(unclaimed_call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unclaimed_wait.status,
        crate::model::TurnClientWaitStatus::Cancelled.as_str()
    );

    let claimed_chat = Chat {
        id: ChatId::new(),
        ..sample_chat()
    };
    store.create_chat(&claimed_chat).await.unwrap();
    let (claimed_turn, claimed_call, claimed_parked_at) =
        park_test_client_wait(&store, claimed_chat.id).await;
    let client_claimed_at = claimed_parked_at + chrono::Duration::seconds(1);
    let client_lease = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .claim_client_tool_call(
                claimed_call.id,
                claimed_chat.id,
                uuid::Uuid::new_v4(),
                client_lease,
                client_claimed_at,
                client_claimed_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Claimed(_)
    ));
    let requested_at = client_claimed_at + chrono::Duration::seconds(1);
    let requested = store
        .request_turn_cancellation_and_append_event(claimed_turn, requested_at)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        requested.outcome,
        RequestTurnCancellationOutcome::Requested(ref turn)
            if turn.status == TurnRunStatus::CancellingClient
    ));
    assert_eq!(requested.terminal_event, None);
    assert!(matches!(
        store
            .accept_turn(
                TurnId::new(),
                claimed_chat.id,
                "gpt-5",
                "must remain occupied",
            )
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(turn) if turn.id == claimed_turn
    ));

    let resolved_at = client_claimed_at + chrono::Duration::minutes(1);
    let resolution = ToolCallResolution::Cancelled {
        result: "cancelled by user".into(),
    };
    let live = store
        .resolve_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(live.outcome, ResolveToolCallOutcome::LeaseLost);
    assert_eq!(live.turn, None);
    assert_eq!(live.terminal_event, None);
    let journaled = store
        .resolve_expired_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(journaled.outcome, ResolveToolCallOutcome::Resolved);
    assert_eq!(
        journaled.turn.as_ref().map(|turn| turn.status),
        Some(TurnRunStatus::Cancelled)
    );
    let terminal_event = journaled.terminal_event.clone().unwrap();
    assert!(matches!(
        terminal_event.event,
        AgentEvent::TurnCancelled { usage } if usage == test_checkpoint_progress().usage
    ));
    assert_eq!(
        store
            .get_turn_run(claimed_turn)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Cancelled
    );
    let claimed_wait = entities::turn_client_wait::Entity::find_by_id(claimed_call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        claimed_wait.status,
        crate::model::TurnClientWaitStatus::Cancelled.as_str()
    );
    let events = store.list_events(claimed_chat.id, 0).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, AgentEvent::TurnCancelled { .. }));
    let recovered = store
        .resolve_expired_client_tool_call_and_append_event(
            claimed_call.id,
            claimed_chat.id,
            client_lease,
            resolved_at,
            &resolution,
            resolved_at,
        )
        .await
        .unwrap();
    assert_eq!(recovered.outcome, ResolveToolCallOutcome::Existing);
    assert_eq!(recovered.terminal_event, Some(terminal_event));
}

#[tokio::test]
async fn concurrent_client_resolution_and_cancellation_do_not_invert_locks() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);

    for _ in 0..8 {
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        let (turn_id, call, parked_at) = park_test_client_wait(&store, chat.id).await;
        let client_token = uuid::Uuid::new_v4();
        let claimed_at = parked_at + chrono::Duration::seconds(1);
        assert!(matches!(
            store
                .claim_client_tool_call(
                    call.id,
                    chat.id,
                    uuid::Uuid::new_v4(),
                    client_token,
                    claimed_at,
                    claimed_at + chrono::Duration::minutes(1),
                )
                .await
                .unwrap(),
            ClaimClientToolCallOutcome::Claimed(_)
        ));

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let cancel_store = store.clone();
        let cancel_barrier = barrier.clone();
        let cancel_at = claimed_at + chrono::Duration::seconds(1);
        let cancellation = tokio::spawn(async move {
            cancel_barrier.wait().await;
            cancel_store
                .request_turn_cancellation(turn_id, cancel_at)
                .await
        });
        let resolve_store = store.clone();
        let resolution = tokio::spawn(async move {
            barrier.wait().await;
            resolve_store
                .resolve_client_tool_call(
                    call.id,
                    chat.id,
                    client_token,
                    cancel_at,
                    &ToolCallResolution::Completed {
                        result: "connected".into(),
                    },
                    cancel_at,
                )
                .await
        });
        let (cancellation, resolution) =
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(cancellation, resolution)
            })
            .await
            .expect("client resolution and cancellation must not deadlock");
        assert!(matches!(
            cancellation.unwrap().unwrap().unwrap(),
            RequestTurnCancellationOutcome::Requested(_)
                | RequestTurnCancellationOutcome::Cancelled(_)
        ));
        assert_eq!(
            resolution.unwrap().unwrap(),
            ResolveToolCallOutcome::Resolved
        );
        assert_eq!(
            store.get_turn_run(turn_id).await.unwrap().unwrap().status,
            TurnRunStatus::Cancelled
        );
    }
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
    entities::turn_claim::ActiveModel {
        token: Set(uuid::Uuid::new_v4()),
        turn_id: Set(accepted.id.0),
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
        store.claim_turn_run(
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
        store
            .get_turn_run(accepted.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Queued
    );
}

#[tokio::test]
async fn turn_completion_atomically_persists_exact_output_and_recovers_retries() {
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
    let lease_expires_at = claimed_at + chrono::Duration::minutes(1);
    let lease_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease_token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "final answer".into(),
        created_at: claimed_at + chrono::Duration::nanoseconds(1_234_567),
    };

    assert_eq!(
        store
            .complete_turn_run(turn_id, uuid::Uuid::new_v4(), 0, output.created_at, &output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    let mut invalid = output.clone();
    invalid.role = Role::User;
    assert!(store
        .complete_turn_run(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    invalid = output.clone();
    invalid.turn_id = TurnId::new();
    assert!(store
        .complete_turn_run(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    invalid = output.clone();
    invalid.chat_id = ChatId::new();
    assert!(store
        .complete_turn_run(turn_id, lease_token, 0, output.created_at, &invalid)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);

    let CompleteTurnRunOutcome::Completed(completed) = store
        .complete_turn_run(turn_id, lease_token, 0, output.created_at, &output)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("first exact completion must commit")
    };
    let canonical_completed_at =
        DateTime::<Utc>::from_timestamp_micros(output.created_at.timestamp_micros()).unwrap();
    assert_eq!(completed.status, TurnRunStatus::Completed);
    assert_eq!(completed.output_message_id, Some(output.id));
    assert_eq!(completed.lease_token, None);
    assert_eq!(completed.lease_expires_at, None);
    assert_eq!(completed.finished_at, Some(canonical_completed_at));
    assert_eq!(completed.updated_at, canonical_completed_at);
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].id, output.id);
    assert_eq!(messages[1].content, output.content);
    assert_eq!(messages[1].created_at, canonical_completed_at);

    assert_eq!(
        store
            .complete_turn_run(
                turn_id,
                lease_token,
                0,
                lease_expires_at + chrono::Duration::seconds(1),
                &output,
            )
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Existing(completed.clone()))
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);

    let mut mismatched = output.clone();
    mismatched.content = "different answer".into();
    assert!(store
        .complete_turn_run(turn_id, lease_token, 0, lease_expires_at, &mismatched)
        .await
        .is_err());
    mismatched = output.clone();
    mismatched.id = MessageId::new();
    assert!(store
        .complete_turn_run(turn_id, lease_token, 0, lease_expires_at, &mismatched)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_failure_receipt_recovers_exact_retries_after_the_turn_advances() {
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let lease_expires_at = claimed_at + chrono::Duration::minutes(2);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let resolved_at =
        claimed_at + chrono::Duration::seconds(1) + chrono::Duration::nanoseconds(999);
    let retry_at = resolved_at + chrono::Duration::minutes(1) + chrono::Duration::nanoseconds(777);
    let canonical_resolved_at =
        DateTime::<Utc>::from_timestamp_micros(resolved_at.timestamp_micros()).unwrap();
    let canonical_retry_at =
        DateTime::<Utc>::from_timestamp_micros(retry_at.timestamp_micros()).unwrap();
    let progress_steps = 2;
    let progress_usage = Usage {
        input_tokens: 13,
        output_tokens: 5,
        cache_read_input_tokens: 3,
        cache_creation_input_tokens: 2,
    };

    assert!(store
        .record_turn_run_failure(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(canonical_resolved_at + chrono::Duration::nanoseconds(999)),
            0,
            Usage::default(),
            "provider_unavailable",
            None,
        )
        .await
        .is_err());
    assert_eq!(
        store
            .record_turn_run_failure(
                turn_id,
                uuid::Uuid::new_v4(),
                resolved_at,
                TurnFailureRetry::RetryAt(retry_at),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        None
    );

    let journaled = store
        .record_turn_run_failure_and_append_event(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(retry_at),
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .unwrap()
        .unwrap();
    let RecordTurnFailureOutcome::Recorded(receipt) = journaled.outcome else {
        panic!("first exact failure must commit")
    };
    assert_eq!(journaled.terminal_event, None);
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
    assert_eq!(receipt.lease_token, token);
    assert_eq!(receipt.turn_id, turn_id);
    assert_eq!(receipt.attempt_count, 1);
    assert_eq!(receipt.requested_retry_at, Some(canonical_retry_at));
    assert_eq!(receipt.resolved_at, canonical_resolved_at);
    assert_eq!(receipt.result_status, TurnRunStatus::RetryWait);
    assert_eq!(receipt.model_steps, progress_steps);
    assert_eq!(receipt.usage, progress_usage);
    assert_eq!(receipt.error_code, "provider_unavailable");
    assert_eq!(receipt.error_detail.as_deref(), Some("temporary outage"));
    let waiting = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(waiting.status, TurnRunStatus::RetryWait);
    assert_eq!(waiting.available_at, canonical_retry_at);
    assert_eq!(waiting.finished_at, None);
    assert_eq!(waiting.lease_token, None);
    assert_eq!(waiting.model_steps, progress_steps);
    assert_eq!(waiting.usage, progress_usage);
    assert_eq!(
        waiting.last_error_code.as_deref(),
        Some("provider_unavailable")
    );

    assert_eq!(
        store
            .record_turn_run_failure(
                turn_id,
                token,
                canonical_retry_at + chrono::Duration::hours(1),
                TurnFailureRetry::RetryAt(retry_at),
                progress_steps,
                progress_usage,
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt.clone()))
    );
    assert!(store
        .record_turn_run_failure_and_append_event(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::Permanent,
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .is_err());
    assert!(store
        .record_turn_run_failure(
            turn_id,
            token,
            resolved_at,
            TurnFailureRetry::RetryAt(retry_at),
            progress_steps,
            progress_usage,
            "provider_unavailable",
            Some("different outage"),
        )
        .await
        .is_err());

    let second_token = uuid::Uuid::new_v4();
    let second_expiry = canonical_retry_at + chrono::Duration::minutes(2);
    let second = store
        .claim_turn_run(second_token, canonical_retry_at, second_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.last_error_code, None);
    assert_eq!(second.model_steps, progress_steps);
    assert_eq!(second.usage, progress_usage);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "recovered".into(),
        created_at: canonical_retry_at + chrono::Duration::seconds(1),
    };
    assert!(matches!(
        store
            .complete_turn_run(turn_id, second_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(
        store
            .record_turn_run_failure(
                turn_id,
                token,
                second_expiry + chrono::Duration::hours(1),
                TurnFailureRetry::RetryAt(retry_at),
                progress_steps,
                progress_usage,
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt))
    );
}

#[tokio::test]
async fn turn_failure_exhaustion_retains_retry_intent_and_rolls_back_atomically() {
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
    set_turn_max_attempts(&store, turn_id, 1).await;
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let failed_at = claimed_at + chrono::Duration::seconds(1);
    let retry_at = failed_at + chrono::Duration::minutes(1);
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_failure
             BEFORE UPDATE OF status ON turn_run
             WHEN NEW.status = 'failed'
             BEGIN SELECT RAISE(FAIL, 'forced turn failure rollback'); END",
        )
        .await
        .unwrap();
    assert!(store
        .record_turn_run_failure(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_error",
            None,
        )
        .await
        .is_err());
    assert!(entities::turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
    store
        .conn
        .execute_unprepared("DROP TRIGGER fail_turn_failure")
        .await
        .unwrap();

    let journaled = store
        .record_turn_run_failure_and_append_event(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_error",
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let RecordTurnFailureOutcome::Recorded(receipt) = journaled.outcome else {
        panic!("failure after rollback must commit")
    };
    assert_eq!(receipt.result_status, TurnRunStatus::Failed);
    assert_eq!(receipt.requested_retry_at, Some(retry_at));
    let failed = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.finished_at, Some(failed_at));
    assert_eq!(failed.available_at, accepted.available_at);
    assert_eq!(failed.last_error_code.as_deref(), Some("provider_error"));
    let terminal = journaled
        .terminal_event
        .expect("terminal failure must append an event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnFailed {
            error: crate::error::AgentErrorInfo {
                kind: "provider_error".into(),
                message: "provider_error".into(),
            }
        }
    );
    assert_eq!(store.list_events(chat.id, 0).await.unwrap(), vec![terminal]);
}

#[tokio::test]
async fn turn_failure_rolls_back_receipt_and_state_when_terminal_event_fails() {
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
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(1),
        turn_id: Set(Some(turn_id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        })
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .record_turn_run_failure_and_append_event(
            turn_id,
            token,
            claimed_at + chrono::Duration::seconds(1),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "provider_error",
            Some("terminal insert must fail"),
        )
        .await
        .is_err());
    assert!(entities::turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    let still_running = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.finished_at, None);
    assert_eq!(still_running.last_error_code, None);
    assert!(entities::turn_failure::Entity::find_by_id(token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn permanent_turn_failure_uses_the_heartbeated_lease_and_rejects_expiry() {
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
    let original_expiry = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, original_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let heartbeat_at = claimed_at + chrono::Duration::seconds(30);
    let extended_expiry = original_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn_run(turn_id, token, heartbeat_at, extended_expiry)
        .await
        .unwrap());
    let failed_at = original_expiry + chrono::Duration::seconds(1);
    let failure_usage = Usage {
        input_tokens: 7,
        output_tokens: 3,
        cache_read_input_tokens: 2,
        cache_creation_input_tokens: 1,
    };
    let RecordTurnFailureOutcome::Recorded(receipt) = store
        .record_turn_run_failure(
            turn_id,
            token,
            failed_at,
            TurnFailureRetry::Permanent,
            2,
            failure_usage,
            "unsafe_to_retry",
            Some("tool outcome is ambiguous"),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("live heartbeated failure must commit")
    };
    assert_eq!(receipt.requested_retry_at, None);
    assert_eq!(receipt.resolved_at, failed_at);
    assert_eq!(receipt.result_status, TurnRunStatus::Failed);
    assert_eq!(receipt.model_steps, 2);
    assert_eq!(receipt.usage, failure_usage);
    let failed = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(failed.status, TurnRunStatus::Failed);
    assert_eq!(failed.finished_at, Some(failed_at));
    assert_eq!(failed.last_error_code.as_deref(), Some("unsafe_to_retry"));
    assert_eq!(failed.model_steps, 2);
    assert_eq!(failed.usage, failure_usage);
    assert_eq!(
        store
            .record_turn_run_failure(
                turn_id,
                token,
                failed_at + chrono::Duration::hours(1),
                TurnFailureRetry::Permanent,
                2,
                failure_usage,
                "unsafe_to_retry",
                Some("tool outcome is ambiguous"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt.clone()))
    );
    assert!(store
        .record_turn_run_failure(
            turn_id,
            token,
            failed_at + chrono::Duration::hours(1),
            TurnFailureRetry::Permanent,
            3,
            failure_usage,
            "unsafe_to_retry",
            Some("tool outcome is ambiguous"),
        )
        .await
        .is_err());

    let expired_chat = sample_chat();
    store.create_chat(&expired_chat).await.unwrap();
    let expired_turn = match store
        .accept_turn(TurnId::new(), expired_chat.id, "gpt-5", "expired")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let expired_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let expired_at = expired_claim_at + chrono::Duration::minutes(1);
    let expired_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(expired_token, expired_claim_at, expired_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(
        store
            .record_turn_run_failure(
                expired_turn.id,
                expired_token,
                expired_at,
                TurnFailureRetry::Permanent,
                0,
                Usage::default(),
                "too_late",
                None,
            )
            .await
            .unwrap(),
        None
    );
    assert!(entities::turn_failure::Entity::find_by_id(expired_token)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .get_turn_run(expired_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Running
    );
}

#[tokio::test]
async fn queued_and_retry_wait_turns_cancel_immediately_and_idempotently() {
    let (_dir, store) = temp_store().await;
    let queued_chat = sample_chat();
    store.create_chat(&queued_chat).await.unwrap();
    let queued = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "queued")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    assert_eq!(
        store
            .request_turn_cancellation_and_append_event(
                queued.id,
                queued.updated_at - chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        None
    );
    let cancelled_at = queued.updated_at + chrono::Duration::seconds(1);
    let journaled = store
        .request_turn_cancellation_and_append_event(queued.id, cancelled_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("queued cancellation must be immediate")
    };
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.attempt_count, 0);
    assert_eq!(cancelled.finished_at, Some(cancelled_at));
    let terminal = journaled
        .terminal_event
        .expect("queued cancellation must append a terminal event");
    assert_eq!(
        terminal.event,
        AgentEvent::TurnCancelled {
            usage: Usage::default()
        }
    );
    let recovered = store
        .request_turn_cancellation_and_append_event(queued.id, queued.updated_at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        RequestTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal));
    let after_cancel = match store
        .accept_turn(TurnId::new(), queued_chat.id, "gpt-5", "after cancel")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    assert!(matches!(
        store
            .request_turn_cancellation(
                after_cancel.id,
                after_cancel.updated_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Cancelled(_))
    ));

    let retry_chat = sample_chat();
    store.create_chat(&retry_chat).await.unwrap();
    let retry_turn = match store
        .accept_turn(TurnId::new(), retry_chat.id, "gpt-5", "retry")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
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
    let claimed_at = retry_turn.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let failed_at = claimed_at + chrono::Duration::seconds(1);
    let retry_at = failed_at + chrono::Duration::minutes(1);
    let RecordTurnFailureOutcome::Recorded(receipt) = store
        .record_turn_run_failure(
            retry_turn.id,
            token,
            failed_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            Usage::default(),
            "provider_unavailable",
            Some("temporary outage"),
        )
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("retryable failure must commit")
    };
    let retry_cancelled_at = failed_at + chrono::Duration::seconds(1);
    let journaled = store
        .request_turn_cancellation_and_append_event(retry_turn.id, retry_cancelled_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("retry-wait cancellation must be immediate")
    };
    assert!(matches!(
        journaled.terminal_event,
        Some(SequencedEvent {
            event: AgentEvent::TurnCancelled { .. },
            ..
        })
    ));
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.finished_at, Some(retry_cancelled_at));
    assert_eq!(cancelled.last_error_code, None);
    assert_eq!(cancelled.last_error_detail, None);
    assert_eq!(
        store
            .record_turn_run_failure(
                retry_turn.id,
                token,
                retry_cancelled_at,
                TurnFailureRetry::RetryAt(retry_at),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        Some(RecordTurnFailureOutcome::Existing(receipt))
    );
}

#[tokio::test]
async fn immediate_cancellation_rolls_back_when_terminal_event_fails() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel before claim")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(1),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        })
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .request_turn_cancellation_and_append_event(
            turn.id,
            turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .is_err());
    let still_queued = store.get_turn_run(turn.id).await.unwrap().unwrap();
    assert_eq!(still_queued.status, TurnRunStatus::Queued);
    assert_eq!(still_queued.finished_at, None);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn running_turn_cancellation_holds_the_chat_until_exact_worker_acknowledgement() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "running")
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
        .claim_turn_run(token, claimed_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let requested_at = claimed_at + chrono::Duration::seconds(1);
    let requested = store
        .request_turn_cancellation_and_append_event(turn.id, requested_at)
        .await
        .unwrap()
        .unwrap();
    let RequestTurnCancellationOutcome::Requested(cancelling) = requested.outcome else {
        panic!("running cancellation must await worker acknowledgement")
    };
    assert_eq!(requested.terminal_event, None);
    assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
    assert_eq!(cancelling.lease_token, Some(token));
    assert_eq!(cancelling.lease_expires_at, Some(expires_at));
    assert_eq!(cancelling.finished_at, None);
    assert!(!store
        .heartbeat_turn_run(
            turn.id,
            token,
            requested_at + chrono::Duration::seconds(1),
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap());
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        content: "too late".into(),
        created_at: requested_at + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn_run(turn.id, token, 0, output.created_at, &output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .record_turn_run_failure(
                turn.id,
                token,
                output.created_at,
                TurnFailureRetry::Permanent,
                0,
                Usage::default(),
                "too_late",
                None,
            )
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "must remain busy")
            .await
            .unwrap(),
        AcceptTurnOutcome::ChatBusy(active) if active.status == TurnRunStatus::Cancelling
    ));
    assert_eq!(
        store
            .request_turn_cancellation(turn.id, claimed_at)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Existing(cancelling))
    );
    assert!(store
        .finish_turn_cancellation(turn.id, uuid::Uuid::nil(), requested_at)
        .await
        .is_err());
    assert_eq!(
        store
            .finish_turn_cancellation(turn.id, uuid::Uuid::new_v4(), requested_at)
            .await
            .unwrap(),
        None
    );

    let acknowledged_at = expires_at + chrono::Duration::seconds(1);
    let usage = Usage {
        input_tokens: 13,
        output_tokens: 8,
        ..Usage::default()
    };
    let journaled = store
        .finish_turn_cancellation_and_append_event(turn.id, token, acknowledged_at, usage)
        .await
        .unwrap()
        .unwrap();
    let FinishTurnCancellationOutcome::Cancelled(cancelled) = journaled.outcome else {
        panic!("exact worker acknowledgement must cancel")
    };
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.lease_token, None);
    assert_eq!(cancelled.lease_expires_at, None);
    assert_eq!(cancelled.finished_at, Some(acknowledged_at));
    let terminal = journaled
        .terminal_event
        .expect("worker acknowledgement must append a terminal event");
    assert_eq!(terminal.event, AgentEvent::TurnCancelled { usage });
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            usage,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.outcome,
        FinishTurnCancellationOutcome::Existing(cancelled)
    );
    assert_eq!(recovered.terminal_event, Some(terminal));
    assert!(store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            Usage::default(),
        )
        .await
        .is_err());
    assert!(matches!(
        store
            .accept_turn(TurnId::new(), chat.id, "gpt-5", "after acknowledgement")
            .await
            .unwrap(),
        AcceptTurnOutcome::Accepted(_)
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
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
        .claim_turn_run(token, claimed_at, expires_at)
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
        .claim_turn_run(
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
    let cancelled = store.get_turn_run(turn.id).await.unwrap().unwrap();
    assert_eq!(cancelled.status, TurnRunStatus::Cancelled);
    assert_eq!(cancelled.finished_at, Some(expires_at));
    assert_eq!(cancelled.lease_token, None);
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            expires_at + chrono::Duration::hours(1),
            Usage::default(),
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
        .claim_turn_run(token, claimed_at, expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(1),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(AgentEvent::TurnFailed {
            error: crate::error::AgentErrorInfo {
                kind: "forced".into(),
                message: "occupy terminal slot".into(),
            },
        })
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();

    assert!(store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .is_err());
    let still_running = store.get_turn_run(turn.id).await.unwrap().unwrap();
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
            .claim_turn_run(uuid::Uuid::new_v4(), claimed_at, expires_at)
            .await
            .unwrap()
            .turn
            .expect("both turns must be claimable");
    }

    let scan_token = uuid::Uuid::new_v4();
    let first_action = store
        .claim_turn_run(
            scan_token,
            expires_at,
            expires_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(first_action.turn, None);
    assert_eq!(
        store
            .claim_turn_run(
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
        .claim_turn_run(
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
async fn concurrent_cancellation_requests_converge_on_one_running_turn() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel concurrently")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let requested_at = claimed_at + chrono::Duration::seconds(1);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.request_turn_cancellation(turn.id, requested_at).await
        }));
    }
    let mut requested = 0;
    let mut existing = 0;
    for task in tasks {
        match task.await.unwrap().unwrap().unwrap() {
            RequestTurnCancellationOutcome::Requested(cancelling) => {
                requested += 1;
                assert_eq!(cancelling.lease_token, Some(token));
            }
            RequestTurnCancellationOutcome::Existing(cancelling) => {
                existing += 1;
                assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
                assert_eq!(cancelling.lease_token, Some(token));
            }
            outcome => panic!("unexpected concurrent cancellation outcome: {outcome:?}"),
        }
    }
    assert_eq!((requested, existing), (1, 7));
    assert_eq!(
        store.get_turn_run(turn.id).await.unwrap().unwrap().status,
        TurnRunStatus::Cancelling
    );
}

#[tokio::test]
async fn turn_completion_and_cancellation_serialize_to_one_decision() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "race cancellation")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let claimed_at = turn.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let decided_at = claimed_at + chrono::Duration::seconds(1);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: turn.id,
        role: Role::Assistant,
        content: "race answer".into(),
        created_at: decided_at,
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let completion = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn_run(turn.id, token, 0, decided_at, &output)
                .await
        })
    };
    let cancellation = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store.request_turn_cancellation(turn.id, decided_at).await
        })
    };
    let completion = completion.await.unwrap().unwrap();
    let cancellation = cancellation.await.unwrap().unwrap().unwrap();
    match (completion, cancellation) {
        (
            Some(CompleteTurnRunOutcome::Completed(completed)),
            RequestTurnCancellationOutcome::AlreadyTerminal(observed),
        ) => {
            assert_eq!(observed, completed);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
        }
        (None, RequestTurnCancellationOutcome::Requested(cancelling)) => {
            assert_eq!(cancelling.status, TurnRunStatus::Cancelling);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
        }
        outcomes => panic!("unexpected completion/cancellation race: {outcomes:?}"),
    }
}

#[tokio::test]
async fn turn_completion_and_failure_serialize_to_one_terminal_decision() {
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(2))
        .await
        .unwrap()
        .turn
        .unwrap();
    let resolved_at = claimed_at + chrono::Duration::seconds(1);
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "race winner".into(),
        created_at: resolved_at,
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let completion = {
        let store = store.clone();
        let barrier = barrier.clone();
        let output = output.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn_run(turn_id, token, 0, resolved_at, &output)
                .await
        })
    };
    let failure = {
        let store = store.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            store
                .record_turn_run_failure(
                    turn_id,
                    token,
                    resolved_at,
                    TurnFailureRetry::RetryAt(resolved_at + chrono::Duration::minutes(1)),
                    0,
                    Usage::default(),
                    "provider_error",
                    None,
                )
                .await
        })
    };
    let completion = completion.await.unwrap().unwrap();
    let failure = failure.await.unwrap().unwrap();
    let turn = store.get_turn_run(turn_id).await.unwrap().unwrap();
    match (completion, failure) {
        (Some(CompleteTurnRunOutcome::Completed(_)), None) => {
            assert_eq!(turn.status, TurnRunStatus::Completed);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
            assert!(entities::turn_failure::Entity::find_by_id(token)
                .one(&store.conn)
                .await
                .unwrap()
                .is_none());
        }
        (None, Some(RecordTurnFailureOutcome::Recorded(receipt))) => {
            assert_eq!(receipt.result_status, TurnRunStatus::RetryWait);
            assert_eq!(turn.status, TurnRunStatus::RetryWait);
            assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
        }
        outcomes => panic!("unexpected completion/failure race: {outcomes:?}"),
    }
}

#[tokio::test]
async fn turn_completion_uses_the_heartbeated_lease_and_fences_operation_time() {
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
    let original_expiry = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, original_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let heartbeat_at = claimed_at + chrono::Duration::seconds(30);
    let extended_expiry = original_expiry + chrono::Duration::minutes(1);
    assert!(store
        .heartbeat_turn_run(turn_id, token, heartbeat_at, extended_expiry)
        .await
        .unwrap());

    let prepared_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "prepared before retry".into(),
        created_at: heartbeat_at - chrono::Duration::microseconds(1),
    };
    let future_output = Message {
        id: MessageId::new(),
        content: "future output".into(),
        created_at: heartbeat_at + chrono::Duration::seconds(2),
        ..prepared_output.clone()
    };
    assert!(store
        .complete_turn_run(
            turn_id,
            token,
            0,
            heartbeat_at + chrono::Duration::seconds(1),
            &future_output,
        )
        .await
        .is_err());
    let output = Message {
        id: MessageId::new(),
        content: "after the original lease".into(),
        created_at: original_expiry + chrono::Duration::seconds(1),
        ..prepared_output
    };
    assert!(matches!(
        store
            .complete_turn_run(turn_id, token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_completion_rejects_prepared_output_retried_after_expiry() {
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
    let lease_expires_at = claimed_at + chrono::Duration::minutes(1);
    let token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();
    let prepared_output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "prepared while live".into(),
        created_at: lease_expires_at - chrono::Duration::seconds(1),
    };

    assert_eq!(
        store
            .complete_turn_run(turn_id, token, 0, lease_expires_at, &prepared_output)
            .await
            .unwrap(),
        None
    );
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::Running
    );
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = accepted.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    let first_token = uuid::Uuid::new_v4();
    store
        .claim_turn_run(first_token, first_claim_at, first_expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    let second_token = uuid::Uuid::new_v4();
    let second_expiry = first_expiry + chrono::Duration::minutes(1);
    let reclaimed = store
        .claim_turn_run(second_token, first_expiry, second_expiry)
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
        content: "stale answer".into(),
        created_at: first_expiry + chrono::Duration::seconds(1),
    };
    assert_eq!(
        store
            .complete_turn_run(
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
        ..stale_output
    };
    assert!(matches!(
        store
            .complete_turn_run(turn_id, second_token, 0, output.created_at, &output)
            .await
            .unwrap(),
        Some(CompleteTurnRunOutcome::Completed(_))
    ));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_different_turn_completions_commit_one_output_once() {
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
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "one answer".into(),
        created_at: claimed_at + chrono::Duration::seconds(1),
    };
    let competing_output = Message {
        id: MessageId::new(),
        content: "different answer".into(),
        ..output.clone()
    };
    let store = std::sync::Arc::new(store);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for output in [output, competing_output] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .complete_turn_run(turn_id, token, 0, output.created_at, &output)
                .await
        }));
    }
    let mut committed = 0;
    let mut conflicted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(Some(CompleteTurnRunOutcome::Completed(_))) => committed += 1,
            Err(AgentError::Store(message))
                if message.contains("already completed with different output") =>
            {
                conflicted += 1;
            }
            outcome => panic!("unexpected concurrent completion outcome: {outcome:?}"),
        }
    }
    assert_eq!((committed, conflicted), (1, 1));
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 2);
}

#[tokio::test]
async fn turn_completion_rolls_back_output_when_state_update_fails() {
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
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    store
        .conn
        .execute_unprepared(
            "CREATE TRIGGER fail_turn_completion
             BEFORE UPDATE OF status ON turn_run
             WHEN NEW.status = 'completed'
             BEGIN SELECT RAISE(FAIL, 'forced turn completion failure'); END",
        )
        .await
        .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "must roll back".into(),
        created_at: claimed_at + chrono::Duration::seconds(1),
    };
    assert!(store
        .complete_turn_run(turn_id, token, 0, output.created_at, &output)
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    let still_running = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.output_message_id, None);
}

#[tokio::test]
async fn turn_completion_rolls_back_state_and_output_when_terminal_event_fails() {
    use crate::error::AgentErrorInfo;
    use crate::event::AgentEvent;
    use crate::provider::{StopReason, Usage};

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
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .unwrap();
    entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(1),
        turn_id: Set(Some(turn_id.0)),
        lease_token: Set(Some(token)),
        attempt_event_ordinal: Set(Some(i32::MAX)),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(AgentEvent::TurnFailed {
            error: AgentErrorInfo {
                kind: "forced".into(),
                message: "occupy terminal slot".into(),
            },
        })
        .unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "must roll back".into(),
        created_at: claimed_at + chrono::Duration::seconds(1),
    };

    assert!(store
        .complete_turn_run_and_append_event(
            turn_id,
            token,
            0,
            output.created_at,
            &output,
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .is_err());
    assert_eq!(store.list_messages(chat.id).await.unwrap().len(), 1);
    let still_running = store.get_turn_run(turn_id).await.unwrap().unwrap();
    assert_eq!(still_running.status, TurnRunStatus::Running);
    assert_eq!(still_running.output_message_id, None);
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
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
                .claim_turn_run(lease_token, claim_at, lease_expires_at)
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(expired_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    store
        .claim_turn_run(uuid::Uuid::new_v4(), first_claim_at, first_expiry)
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .filter(entities::turn_run::Column::Id.eq(queued_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let claimed_queued = store
        .claim_turn_run(
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
        .claim_turn_run(
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(expired_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let first_claim_at = expired_turn.available_at + chrono::Duration::seconds(1);
    let first_expiry = first_claim_at + chrono::Duration::minutes(1);
    store
        .claim_turn_run(uuid::Uuid::new_v4(), first_claim_at, first_expiry)
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
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(queued_due_at),
        )
        .filter(entities::turn_run::Column::Id.eq(queued_turn.id.0))
        .exec(&store.conn)
        .await
        .unwrap();

    let reclaimed = store
        .claim_turn_run(
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
            .get_turn_run(queued_turn.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TurnRunStatus::Queued
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
    assert_eq!(store.get_turn_run(turn_id).await.unwrap(), None);
    assert!(store.list_turn_runs(chat.id).await.unwrap().is_empty());
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
             BEFORE INSERT ON turn_run
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
    assert_eq!(store.get_turn_run(turn_id).await.unwrap(), None);
    assert!(store.list_turn_runs(chat.id).await.unwrap().is_empty());
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn settings_roundtrip_and_overwrite() {
    let (_dir, store) = temp_store().await;
    assert_eq!(store.get_setting("model").await.unwrap(), None);
    store
        .set_setting("model", &serde_json::json!("claude"))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting("model").await.unwrap(),
        Some(serde_json::json!("claude"))
    );
    store
        .set_setting("model", &serde_json::json!("gpt"))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting("model").await.unwrap(),
        Some(serde_json::json!("gpt"))
    );
}

#[tokio::test]
async fn list_chats_is_newest_first_and_messages_follow_commit_sequence() {
    let (_dir, store) = temp_store().await;
    let mut older = sample_chat();
    older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
    let mut newer = sample_chat();
    newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
    store.create_chat(&older).await.unwrap();
    store.create_chat(&newer).await.unwrap();
    // list_chats is newest-first.
    assert_eq!(
        store.list_chats().await.unwrap(),
        vec![newer.clone(), older.clone()]
    );

    // Transcript order follows the durable per-chat commit sequence, even when
    // caller-provided timestamps move backwards.
    let msg = |ts: i64| Message {
        id: MessageId::new(),
        chat_id: newer.id,
        turn_id: TurnId::new(),
        role: Role::User,
        content: format!("t{ts}"),
        created_at: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
    };
    let (m1, m2) = (msg(20), msg(10));
    store.append_message(&m1).await.unwrap();
    store.append_message(&m2).await.unwrap();
    let listed = store.list_messages(newer.id).await.unwrap();
    assert_eq!(listed, vec![m1, m2]);
}

#[tokio::test]
async fn delete_chat_erases_quiesced_history_and_fails_closed_for_live_work_or_roots() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let message = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::User,
        content: "delete this history".into(),
        created_at: Utc::now(),
    };
    store.append_message(&message).await.unwrap();
    store
        .append_event(
            chat.id,
            &AgentEvent::TextDelta {
                text: "live".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert_eq!(store.get_chat(chat.id).await.unwrap(), None);
    assert!(store.list_messages(chat.id).await.unwrap().is_empty());
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::NotFound
    );

    let active = sample_chat();
    store.create_chat(&active).await.unwrap();
    let active_turn_id = TurnId::new();
    store
        .accept_turn(active_turn_id, active.id, "test", "still working")
        .await
        .unwrap();
    assert_eq!(
        store.delete_chat(active.id).await.unwrap(),
        DeleteChatOutcome::ActiveWork
    );
    assert!(store.get_chat(active.id).await.unwrap().is_some());
    let active_turn = store.get_turn_run(active_turn_id).await.unwrap().unwrap();
    store
        .request_turn_cancellation_and_append_event(
            active_turn_id,
            active_turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    assert_eq!(
        store.delete_chat(active.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );

    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut rooted = sample_chat();
    rooted.attachment_revision = 1;
    rooted.root_attachments = vec![ChatRootAttachment {
        root_id: root,
        origin: RootAttachmentOrigin::Conversation,
    }];
    store.create_chat(&rooted).await.unwrap();
    assert_eq!(
        store.delete_chat(rooted.id).await.unwrap(),
        DeleteChatOutcome::RootsAttached
    );
    assert!(store.get_chat(rooted.id).await.unwrap().is_some());

    let ambiguous = sample_chat();
    store.create_chat(&ambiguous).await.unwrap();
    let change = BeginRootAttachmentChange {
        id: RootAttachmentChangeId::new(),
        chat_id: ambiguous.id,
        executor_id: uuid::Uuid::new_v4(),
        root_id: HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap(),
        action: RootAttachmentChangeAction::Attach,
        expected_attachment_revision: 0,
        created_at: Utc::now(),
    };
    assert!(matches!(
        store.begin_root_attachment_change(&change).await.unwrap(),
        BeginRootAttachmentChangeOutcome::Begun(_)
    ));
    assert!(matches!(
        store
            .finish_root_attachment_change(
                change.id,
                change.executor_id,
                &RootAttachmentChangeTerminal::Failed {
                    broker_changed: None,
                    broker_currently_attached: None,
                    failure: RootAttachmentChangeFailure {
                        code: "broker_unavailable".into(),
                        message: "could not verify the folder attachment".into(),
                        retryable: true,
                    },
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        FinishRootAttachmentChangeOutcome::Finished(_)
    ));
    assert_eq!(
        store.delete_chat(ambiguous.id).await.unwrap(),
        DeleteChatOutcome::RootAttachmentStateUnresolved
    );
    assert!(store.get_chat(ambiguous.id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_chat_atomically_retires_only_its_owned_sources() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut owned = sample_document(None);
    owned.chat_id = Some(chat.id);
    owned.source_blob = Some(DocumentSourceBlob::from_bytes(b"owned source bytes"));
    let owned_id = owned.id;
    let blob_id = owned.source_blob.as_ref().unwrap().id;
    store.create_document(&owned).await.unwrap();

    let legacy = sample_document(None);
    let legacy_id = legacy.id;
    store.create_document(&legacy).await.unwrap();

    assert_eq!(
        store
            .list_document_ids(DocumentScope::Chat(chat.id))
            .await
            .unwrap(),
        vec![owned_id]
    );
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert_eq!(store.get_chat(chat.id).await.unwrap(), None);
    assert_eq!(store.get_document(owned_id).await.unwrap(), None);
    let tombstone = store
        .get_pending_document_retirement(owned_id)
        .await
        .unwrap()
        .expect("chat deletion must publish a pending vector tombstone");
    assert_eq!(tombstone.content_revision, 2);
    assert_eq!(
        store
            .get_blob_retirement(blob_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        BlobRetirementStatus::Queued
    );
    assert!(store.get_document(legacy_id).await.unwrap().is_some());
}

#[tokio::test]
async fn document_chat_scope_is_isolated_and_mutually_exclusive_with_project_scope() {
    let (_dir, store) = temp_store().await;
    let first_chat = sample_chat();
    let second_chat = sample_chat();
    let project = sample_project();
    store.create_chat(&first_chat).await.unwrap();
    store.create_chat(&second_chat).await.unwrap();
    store.create_project(&project).await.unwrap();

    let uri = "file:///shared-name.txt";
    let first_id = DocumentId::derive_for_chat(first_chat.id, uri);
    let second_id = DocumentId::derive_for_chat(second_chat.id, uri);
    assert_ne!(first_id, second_id);
    for (chat_id, id, text) in [
        (first_chat.id, first_id, "first"),
        (second_chat.id, second_id, "second"),
    ] {
        store
            .upsert_document(&DocumentUpsert {
                id,
                chat_id: Some(chat_id),
                project_id: None,
                source_uri: Some(uri.into()),
                media_type: "text/plain".into(),
                title: None,
                canonical_text: text.into(),
                source_regions: Vec::new(),
                updated_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .list_document_ids(DocumentScope::Chat(first_chat.id))
            .await
            .unwrap(),
        vec![first_id]
    );
    assert_eq!(
        store
            .list_document_ids(DocumentScope::Chat(second_chat.id))
            .await
            .unwrap(),
        vec![second_id]
    );

    let error = store
        .upsert_document(&DocumentUpsert {
            id: DocumentId::new(),
            chat_id: Some(first_chat.id),
            project_id: Some(project.id),
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "invalid double scope".into(),
            source_regions: Vec::new(),
            updated_at: Utc::now(),
        })
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("both a conversation and a project"));
}

#[tokio::test]
async fn refused_turn_metadata_hydrates_with_its_exact_durable_output() {
    use crate::event::AgentEvent;
    use crate::provider::{RefusalDetails, RefusalOutcome, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut expected = Vec::new();
    for (content, category, partial_output) in [
        ("", "cyber", false),
        ("Visible partial", "general_harms", true),
    ] {
        let turn_id = TurnId::new();
        let accepted = match store
            .accept_turn(turn_id, chat.id, "claude", "question")
            .await
            .unwrap()
        {
            AcceptTurnOutcome::Accepted(turn) => turn,
            outcome => panic!("unexpected acceptance: {outcome:?}"),
        };
        let lease_token = uuid::Uuid::new_v4();
        store
            .claim_turn_run(
                lease_token,
                accepted.available_at,
                accepted.available_at + chrono::Duration::minutes(1),
            )
            .await
            .unwrap()
            .turn
            .expect("accepted turn is claimable");
        let output = Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id,
            role: Role::Assistant,
            content: content.into(),
            created_at: accepted.available_at,
        };
        let refusal = RefusalOutcome::new(
            RefusalDetails::from_category(Some(category)),
            partial_output,
        );
        let completed = store
            .complete_refused_turn_run_with_citations_and_append_event(
                turn_id,
                lease_token,
                0,
                output.created_at,
                &output,
                &[],
                Usage::default(),
                refusal.clone(),
            )
            .await
            .unwrap()
            .expect("live refusal completion");
        assert!(matches!(
            completed.terminal_event.as_ref().map(|event| &event.event),
            Some(AgentEvent::TurnRefused {
                refusal: journaled,
                ..
            }) if journaled == &refusal
        ));
        expected.push((output, refusal));
    }

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(transcript.refusals.len(), 2);
    for (output, refusal) in expected {
        let stored = transcript
            .messages
            .iter()
            .find(|message| message.id == output.id)
            .expect("refused output remains a normal durable assistant message");
        assert_eq!(stored.content, output.content);
        assert!(transcript
            .refusals
            .iter()
            .any(|snapshot| { snapshot.message_id == output.id && snapshot.refusal == refusal }));
    }
}

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
    let stored = entities::event::Entity::find_by_id((chat.id.0, 1))
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_id, Some(turn.id.0));
    assert_eq!(stored.lease_token, Some(lease_token));
    assert_eq!(stored.attempt_event_ordinal, Some(1));
    assert!(!stored.terminal);
    assert_eq!(
        serde_json::from_value::<AgentEvent>(stored.payload).unwrap(),
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

    entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(2),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(lease_token)),
        attempt_event_ordinal: Set(Some(2)),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(&terminal).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .unwrap();
    assert!(entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(3),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(Some(lease_token)),
        attempt_event_ordinal: Set(Some(3)),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(&terminal).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .is_err());
    assert!(entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(4),
        turn_id: Set(None),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(true),
        payload: Set(serde_json::to_value(&terminal).unwrap()),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .is_err());
    assert!(entities::event::ActiveModel {
        chat_id: Set(chat.id.0),
        seq: Set(5),
        turn_id: Set(Some(turn.id.0)),
        lease_token: Set(None),
        attempt_event_ordinal: Set(None),
        scan_token: Set(None),
        terminal: Set(false),
        payload: Set(serde_json::to_value(&started).unwrap()),
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

#[tokio::test]
async fn all_roles_round_trip() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
    for (i, role) in roles.iter().enumerate() {
        store
            .append_message(&Message {
                id: MessageId::new(),
                chat_id: chat.id,
                turn_id: TurnId::new(),
                role: *role,
                content: String::new(),
                created_at: DateTime::<Utc>::from_timestamp(i as i64, 0).unwrap(),
            })
            .await
            .unwrap();
    }
    let got: Vec<Role> = store
        .list_messages(chat.id)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.role)
        .collect();
    assert_eq!(got, roles);
}

#[tokio::test]
async fn server_tool_call_lifecycle_is_atomic_and_idempotent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let created = DateTime::<Utc>::from_timestamp(1_700_000_010, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({"path": "note.txt"}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: created,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Existing(_)
    ));
    assert_eq!(
        store
            .accept_tool_call(&ToolCallRecord {
                arguments: serde_json::json!({"path": "other.txt"}),
                ..call.clone()
            })
            .await
            .unwrap(),
        AcceptToolCallOutcome::IdentityConflict
    );

    let completed = DateTime::<Utc>::from_timestamp(1_700_000_011, 0).unwrap();
    let resolution = ToolCallResolution::Completed {
        result: "hello".into(),
    };
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, completed)
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, completed)
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
    match store.accept_tool_call(&call).await.unwrap() {
        AcceptToolCallOutcome::Existing(existing) => {
            assert_eq!(existing.status, ToolCallStatus::Completed);
            assert_eq!(existing.result.as_deref(), Some("hello"));
        }
        outcome => panic!("unexpected terminal acceptance retry: {outcome:?}"),
    }
    assert_eq!(
        store
            .resolve_server_tool_call(
                call.id,
                &ToolCallResolution::Completed {
                    result: "different".into(),
                },
                completed,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::AlreadyTerminal
    );

    let listed = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].created_at, created);
    assert_eq!(listed[0].resolved_at, Some(completed));
    assert_eq!(listed[0].status, ToolCallStatus::Completed);
    assert_eq!(listed[0].result.as_deref(), Some("hello"));
    assert_eq!(listed[0].arguments, serde_json::json!({"path": "note.txt"}));
}

#[tokio::test]
async fn claimed_tool_results_are_co_committed_with_the_turn_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "run a tool")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let first_claim_at = accepted.available_at;
    let first_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            first_lease,
            first_claim_at,
            first_claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "tu_claimed".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": "note.txt"}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: first_claim_at,
        resolved_at: None,
    };
    assert!(matches!(
        store
            .accept_claimed_tool_call(&call, first_lease, first_claim_at)
            .await
            .unwrap(),
        AcceptClaimedToolCallOutcome::Accepted(_)
    ));
    let stored = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.turn_lease_token, Some(first_lease));
    assert_eq!(stored.resolution_turn_lease_token, None);

    let resolution = ToolCallResolution::Completed {
        result: "written".into(),
    };
    assert_eq!(
        store
            .resolve_claimed_server_tool_call_with_evidence(
                call.id,
                chat.id,
                turn_id,
                uuid::Uuid::new_v4(),
                first_claim_at,
                &resolution,
                first_claim_at,
                &[],
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store.list_tool_calls(chat.id).await.unwrap()[0].status,
        ToolCallStatus::Pending
    );

    let retry_at = first_claim_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    let retried = store
        .claim_turn_run(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(retried.attempt_count, 2);
    assert_eq!(
        store
            .resolve_claimed_server_tool_call_with_evidence(
                call.id,
                chat.id,
                turn_id,
                first_lease,
                retry_at,
                &resolution,
                retry_at,
                &[],
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost,
        "the result and stale lease check must commit together"
    );

    let interrupted = ToolCallResolution::Failed {
        result: "not replayed".into(),
        error_code: "tool_execution_interrupted".into(),
        error_detail: None,
    };
    assert_eq!(
        store
            .abandon_inherited_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                retry_lease,
                retry_at,
                &interrupted,
                retry_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    let stored = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.resolution_turn_lease_token, Some(retry_lease));
    assert_eq!(stored.status, ToolCallStatus::Failed.as_str());
}

#[tokio::test]
async fn claimed_intermediate_message_is_co_committed_with_the_turn_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "draft")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let claimed_at = accepted.available_at;
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(lease, claimed_at, claimed_at + chrono::Duration::seconds(1))
        .await
        .unwrap();
    let message = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        content: "intermediate".into(),
        created_at: claimed_at,
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&message, &[], lease, claimed_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Appended
    );
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&message, &[], lease, claimed_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Existing
    );

    let retry_at = claimed_at + chrono::Duration::seconds(2);
    let retry_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            retry_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    let stale_message = Message {
        id: MessageId::new(),
        content: "stale".into(),
        created_at: retry_at,
        ..message
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(&stale_message, &[], lease, retry_at,)
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::LeaseLost
    );
    assert!(entities::message::Entity::find_by_id(stale_message.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn retrieval_evidence_is_atomic_generation_fenced_and_survives_source_changes() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let updated_at = DateTime::<Utc>::from_timestamp(1_700_001_000, 0).unwrap();
    let source = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///facts.txt".into()),
        media_type: "text/plain".into(),
        title: Some("Facts".into()),
        canonical_text: "old text".into(),
        source_regions: Vec::new(),
        updated_at,
    };
    let (document, _) = store
        .upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)
        .await
        .unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "search_1".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "old"}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: updated_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let span = ByteSpan::new(0, source.canonical_text.len());
    let evidence = RetrievalEvidenceInput {
        rank: 1,
        source_token: uuid::Uuid::new_v4(),
        document_id: source.id,
        generation: document.generation(),
        chunk_id: ChunkId::derive(source.id, span.start, span.end),
        span,
        snippet: source.canonical_text.clone(),
        heading_path: vec!["Archive".into()],
        source_regions: Vec::new(),
        source: RetrievalEvidenceSource::Uri {
            uri: source.source_uri.clone().unwrap(),
        },
    };
    let resolution = ToolCallResolution::Completed {
        result: "Found 1 passage".into(),
    };
    let resolved_at = updated_at + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &resolution,
                resolved_at,
                std::slice::from_ref(&evidence),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &resolution,
                resolved_at,
                std::slice::from_ref(&evidence),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
    let stored = store.list_retrieval_evidence(call.id).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].call_id, call.id);
    assert_eq!(stored[0].chat_id, call.chat_id);
    assert_eq!(stored[0].turn_id, call.turn_id);
    assert_eq!(stored[0].evidence, evidence);

    // Direct source reads create the same durable citation evidence as semantic
    // search. No other server tool may attach this private sidecar.
    let read_call = ToolCallRecord {
        id: CallId::new(),
        provider_id: "read_source_1".into(),
        name: "read_source".into(),
        arguments: serde_json::json!({"document_id": source.id}),
        created_at: updated_at + chrono::Duration::microseconds(500),
        ..call.clone()
    };
    store.accept_tool_call(&read_call).await.unwrap();
    let read_evidence = RetrievalEvidenceInput {
        source_token: uuid::Uuid::new_v4(),
        ..evidence.clone()
    };
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                read_call.id,
                &ToolCallResolution::Completed {
                    result: "Read source range".into(),
                },
                resolved_at,
                std::slice::from_ref(&read_evidence),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store.list_retrieval_evidence(read_call.id).await.unwrap()[0].evidence,
        read_evidence
    );

    let second_call = ToolCallRecord {
        id: CallId::new(),
        provider_id: "search_2".into(),
        created_at: updated_at + chrono::Duration::milliseconds(1),
        ..call.clone()
    };
    store.accept_tool_call(&second_call).await.unwrap();
    let private_tail = "PRIVATE_RENDERER_TAIL";
    let long_snippet = format!("{}{private_tail}", "x".repeat(700));
    let long_span = ByteSpan::new(0, long_snippet.len());
    let second_evidence = RetrievalEvidenceInput {
        source_token: uuid::Uuid::new_v4(),
        chunk_id: ChunkId::derive(source.id, long_span.start, long_span.end),
        span: long_span,
        snippet: long_snippet,
        heading_path: vec!["H".repeat(200)],
        source_regions: (0..9)
            .map(|index| SourceRegion {
                span: ByteSpan::new(index, index + 1),
                location: SourceLocation::Page {
                    number: std::num::NonZeroU32::new(
                        u32::try_from(index + 1).expect("test page fits u32"),
                    )
                    .unwrap(),
                },
            })
            .collect(),
        ..evidence.clone()
    };
    store
        .resolve_server_tool_call_with_evidence(
            second_call.id,
            &resolution,
            resolved_at,
            std::slice::from_ref(&second_evidence),
        )
        .await
        .unwrap();

    let assistant = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: call.turn_id,
        role: Role::Assistant,
        content: "Grounded answer".into(),
        created_at: resolved_at + chrono::Duration::seconds(1),
    };
    let reference = crate::AssistantCitationReference {
        source_token: evidence.source_token,
    };
    let second_reference = crate::AssistantCitationReference {
        source_token: second_evidence.source_token,
    };
    let unknown_reference = crate::AssistantCitationReference {
        source_token: uuid::Uuid::new_v4(),
    };
    store
        .append_assistant_message_with_citations(
            &assistant,
            &[unknown_reference, second_reference, reference],
        )
        .await
        .unwrap();
    store
        .append_assistant_message_with_citations(
            &assistant,
            &[unknown_reference, second_reference, reference],
        )
        .await
        .unwrap();
    assert!(store
        .append_assistant_message_with_citations(&assistant, &[reference, second_reference])
        .await
        .is_err());
    assert!(store
        .append_assistant_message_with_citations(
            &Message {
                content: "different".into(),
                ..assistant.clone()
            },
            &[second_reference, reference],
        )
        .await
        .is_err());
    let snapshot = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(snapshot.citations.len(), 2);
    assert_eq!(snapshot.citations[0].message_id, assistant.id);
    assert_eq!(snapshot.citations[0].ordinal, 1);
    assert_eq!(snapshot.citations[1].ordinal, 2);
    assert_eq!(snapshot.citations[0].excerpt.chars().count(), 600);
    assert!(!snapshot.citations[0].excerpt.contains(private_tail));
    assert_eq!(
        snapshot.citations[0]
            .heading
            .as_ref()
            .unwrap()
            .chars()
            .count(),
        160
    );
    assert_eq!(snapshot.citations[0].pages, [1, 2, 3, 4, 5, 6, 7, 8]);

    let other_chat = sample_chat();
    store.create_chat(&other_chat).await.unwrap();
    let cross_chat = Message {
        id: MessageId::new(),
        chat_id: other_chat.id,
        turn_id: call.turn_id,
        role: Role::Assistant,
        content: "cross chat".into(),
        created_at: assistant.created_at,
    };
    store
        .append_assistant_message_with_citations(&cross_chat, &[reference])
        .await
        .unwrap();
    assert!(store
        .get_chat_transcript(other_chat.id)
        .await
        .unwrap()
        .unwrap()
        .citations
        .is_empty());
    let cross_turn = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        role: Role::Assistant,
        content: "cross turn".into(),
        created_at: assistant.created_at,
    };
    store
        .append_assistant_message_with_citations(&cross_turn, &[reference])
        .await
        .unwrap();
    assert_eq!(
        store
            .get_chat_transcript(chat.id)
            .await
            .unwrap()
            .unwrap()
            .citations
            .len(),
        2
    );

    let mut conflicting = evidence.clone();
    conflicting.generation.revision_token = uuid::Uuid::new_v4();
    assert_eq!(
        store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &resolution,
                resolved_at,
                &[conflicting],
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::AlreadyTerminal
    );

    let replacement = DocumentUpsert {
        canonical_text: "new text".into(),
        updated_at: updated_at + chrono::Duration::seconds(2),
        ..source.clone()
    };
    let (replacement, _) = store
        .upsert_document_and_enqueue_index(&replacement, "pipeline-v1", 3)
        .await
        .unwrap();
    assert_ne!(replacement.generation(), document.generation());
    assert_eq!(
        store.list_retrieval_evidence(call.id).await.unwrap(),
        stored
    );
    store.delete_document(source.id).await.unwrap();
    assert_eq!(
        store.list_retrieval_evidence(call.id).await.unwrap(),
        stored
    );
    let historical = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(historical.citations, snapshot.citations);
    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert!(entities::assistant_citation::Entity::find()
        .all(&store.conn)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn invalid_retrieval_identity_rolls_back_tool_completion() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = Utc::now();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "search_invalid".into(),
        name: "search".into(),
        arguments: serde_json::json!({"query": "facts"}),
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let document_id = DocumentId::new();
    let invalid = RetrievalEvidenceInput {
        rank: 1,
        source_token: uuid::Uuid::new_v4(),
        document_id,
        generation: DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
        },
        chunk_id: ChunkId::derive(DocumentId::new(), 0, 4),
        span: ByteSpan::new(0, 4),
        snippet: "fact".into(),
        heading_path: Vec::new(),
        source_regions: Vec::new(),
        source: RetrievalEvidenceSource::Inline,
    };
    let resolution = ToolCallResolution::Completed {
        result: "Found 1 passage".into(),
    };
    assert!(store
        .resolve_server_tool_call_with_evidence(
            call.id,
            &resolution,
            created_at + chrono::Duration::seconds(1),
            &[invalid],
        )
        .await
        .is_err());
    assert!(store
        .list_retrieval_evidence(call.id)
        .await
        .unwrap()
        .is_empty());
    let pending = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(pending[0].status, ToolCallStatus::Pending);

    let oversized_snippet = "x".repeat(RetrievalEvidenceInput::MAX_SNIPPET_BYTES + 1);
    let oversized_span = ByteSpan::new(0, oversized_snippet.len());
    let oversized = RetrievalEvidenceInput {
        rank: 1,
        source_token: uuid::Uuid::new_v4(),
        document_id,
        generation: DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
        },
        chunk_id: ChunkId::derive(document_id, oversized_span.start, oversized_span.end),
        span: oversized_span,
        snippet: oversized_snippet,
        heading_path: Vec::new(),
        source_regions: Vec::new(),
        source: RetrievalEvidenceSource::Inline,
    };
    assert!(store
        .resolve_server_tool_call_with_evidence(
            call.id,
            &resolution,
            created_at + chrono::Duration::seconds(1),
            &[oversized],
        )
        .await
        .is_err());
    let nul_span = ByteSpan::new(0, 4);
    let nul = RetrievalEvidenceInput {
        rank: 1,
        source_token: uuid::Uuid::new_v4(),
        document_id,
        generation: DocumentGeneration {
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
        },
        chunk_id: ChunkId::derive(document_id, nul_span.start, nul_span.end),
        span: nul_span,
        snippet: "a\0bc".into(),
        heading_path: Vec::new(),
        source_regions: Vec::new(),
        source: RetrievalEvidenceSource::Inline,
    };
    assert!(store
        .resolve_server_tool_call_with_evidence(
            call.id,
            &resolution,
            created_at + chrono::Duration::seconds(1),
            &[nul],
        )
        .await
        .is_err());
    if usize::BITS > 63 {
        let start = (i64::MAX as usize) + 1;
        let span = ByteSpan::new(start, start + 4);
        let outside_storage_range = RetrievalEvidenceInput {
            rank: 1,
            source_token: uuid::Uuid::new_v4(),
            document_id,
            generation: DocumentGeneration {
                content_revision: 1,
                revision_token: uuid::Uuid::new_v4(),
            },
            chunk_id: ChunkId::derive(document_id, span.start, span.end),
            span,
            snippet: "fact".into(),
            heading_path: Vec::new(),
            source_regions: Vec::new(),
            source: RetrievalEvidenceSource::Inline,
        };
        assert!(store
            .resolve_server_tool_call_with_evidence(
                call.id,
                &resolution,
                created_at + chrono::Duration::seconds(1),
                &[outside_storage_range],
            )
            .await
            .is_err());
    }
    assert_eq!(
        store.list_tool_calls(chat.id).await.unwrap()[0].status,
        ToolCallStatus::Pending
    );
}

async fn claimed_sensitive_call(
    store: &DbStore,
    chat: &Chat,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    claimed_sensitive_call_named(store, chat, "search").await
}

async fn claimed_sensitive_call_named(
    store: &DbStore,
    chat: &Chat,
    tool_name: &str,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    claimed_sensitive_call_with(
        store,
        chat,
        tool_name,
        serde_json::json!({"query": "private"}),
    )
    .await
}

async fn claimed_sensitive_call_with(
    store: &DbStore,
    chat: &Chat,
    tool_name: &str,
    arguments: serde_json::Value,
) -> (TurnId, uuid::Uuid, ToolCallRecord, ApprovalRequest) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "search")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::MaxAttempts,
            sea_orm::sea_query::Expr::value(2),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(&store.conn)
        .await
        .unwrap();
    let claimed_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(accepted.available_at);
    let lease_token = uuid::Uuid::new_v4();
    let expiry = claimed_at + chrono::Duration::minutes(5);
    let claimed = store
        .claim_turn_run(lease_token, claimed_at, expiry)
        .await
        .unwrap()
        .turn
        .expect("turn should be claimable");
    assert_eq!(claimed.id, turn_id);
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: "approval-call".into(),
        name: tool_name.into(),
        arguments,
        execution: ToolCallExecution::Server,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    let request = ApprovalRequest {
        call_id: call.id,
        chat_id: chat.id,
        turn_id,
        tool_name: call.name.clone(),
        class: ApprovalClass::Sensitive,
        kind: crate::ToolApprovalKind::for_tool_name(&call.name),
        preview: None,
        summary: "search requires approval".into(),
    };
    (turn_id, lease_token, call, request)
}

#[tokio::test]
async fn recovered_exec_approval_still_names_the_command_it_will_run() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, _call, request) = claimed_sensitive_call_with(
        &store,
        &chat,
        "exec",
        serde_json::json!({ "command": "cargo", "args": ["build"], "cwd": "checkout" }),
    )
    .await;

    store
        .request_tool_call_approval_and_append_event(
            &request,
            lease_token,
            1,
            DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        )
        .await
        .unwrap();

    // The preview is rebuilt from the arguments the call is parked on, so a
    // card recovered after a restart describes the command that will actually
    // run rather than whatever was in flight when the process died.
    let recovered = store
        .get_tool_call_approval(request.call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.preview,
        Some(crate::ToolActionPreview::Exec {
            command: "cargo".into(),
            args: vec!["build".into()],
            cwd: "checkout".into(),
        })
    );
}

#[tokio::test]
async fn external_mcp_approval_roundtrips_as_one_shot_consent() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, _call, request) =
        claimed_sensitive_call_named(&store, &chat, "mcp__documents__search").await;

    let registered = store
        .request_tool_call_approval_and_append_event(
            &request,
            lease_token,
            1,
            DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        )
        .await
        .unwrap();
    let approval = match registered.outcome {
        RequestToolApprovalOutcome::Requested(approval) => approval,
        outcome => panic!("unexpected approval request outcome: {outcome:?}"),
    };
    assert_eq!(
        approval.kind,
        crate::ToolApprovalKind::ExternalMcpMayCallServer
    );
    assert!(approval.kind.is_approvable());
    assert!(!approval.kind.is_standing_grantable());

    let recovered = store
        .get_tool_call_approval(request.call_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.kind, approval.kind);
}

#[tokio::test]
async fn approval_registration_journals_once_and_decision_is_exact() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, _call, request) = claimed_sensitive_call(&store, &chat).await;

    // Deliberately stale caller time: the claimed operation must use the DB
    // statement clock for both request ordering and lease freshness.
    let stale_clock = DateTime::<Utc>::from_timestamp(1, 0).unwrap();
    let first = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, stale_clock)
        .await
        .unwrap();
    let requested = match first.outcome {
        RequestToolApprovalOutcome::Requested(approval) => approval,
        outcome => panic!("unexpected approval request outcome: {outcome:?}"),
    };
    assert!(requested.requested_at > stale_clock);
    let first_event = first.required_event.expect("request event must commit");
    assert_eq!(first_event.seq, 1);
    assert!(matches!(
        first_event.event,
        AgentEvent::ApprovalRequired { call_id, .. } if call_id == request.call_id
    ));

    let retry = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        retry.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert_eq!(
        retry.required_event.as_ref().map(|event| event.seq),
        Some(1)
    );
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);

    // A later attempt recovers the pending request without appending a second
    // ApprovalRequired event under its new claim identity.
    let failure_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    let retry_at = failure_at + chrono::Duration::seconds(1);
    assert!(store
        .record_turn_run_failure(
            turn_id,
            lease_token,
            failure_at,
            TurnFailureRetry::RetryAt(retry_at),
            0,
            crate::provider::Usage::default(),
            "worker_restarted",
            None,
        )
        .await
        .unwrap()
        .is_some());
    let after_lease_release = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        after_lease_release.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert_eq!(
        after_lease_release
            .required_event
            .as_ref()
            .map(|event| event.seq),
        Some(1)
    );
    let resumed_lease = uuid::Uuid::new_v4();
    let resumed_claim = store
        .claim_turn_run(
            resumed_lease,
            retry_at,
            retry_at + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();
    assert!(
        resumed_claim.turn.is_some(),
        "retry should be claimable: {resumed_claim:?}"
    );
    let resumed = store
        .request_tool_call_approval_and_append_event(&request, resumed_lease, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        resumed.outcome,
        RequestToolApprovalOutcome::Existing(ref approval) if approval == &requested
    ));
    assert!(resumed.required_event.is_none());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);

    let decided = store
        .decide_tool_call_approval(
            chat.id,
            request.call_id,
            &ApprovalDecision::Approve,
            stale_clock,
        )
        .await
        .unwrap();
    let approved = match decided {
        DecideToolApprovalOutcome::Decided(approval) => approval,
        outcome => panic!("unexpected approval decision outcome: {outcome:?}"),
    };
    assert_eq!(approved.status, ToolApprovalStatus::Approved);
    assert!(approved.decided_at.is_some_and(|decided_at| {
        decided_at >= requested.requested_at && decided_at > stale_clock
    }));
    let terminal_lost_ack = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        terminal_lost_ack.outcome,
        RequestToolApprovalOutcome::Existing(ref existing) if existing == &approved
    ));
    assert_eq!(
        terminal_lost_ack
            .required_event
            .as_ref()
            .map(|event| event.seq),
        Some(1)
    );
    assert!(matches!(
        store
            .decide_tool_call_approval(
                chat.id,
                request.call_id,
                &ApprovalDecision::Approve,
                Utc::now(),
            )
            .await
            .unwrap(),
        DecideToolApprovalOutcome::Existing(existing) if existing == approved
    ));
    assert_eq!(
        store
            .decide_tool_call_approval(
                chat.id,
                request.call_id,
                &ApprovalDecision::Reject {
                    reason: "changed".into(),
                },
                Utc::now(),
            )
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );

    let terminal_retry = store
        .request_tool_call_approval_and_append_event(&request, resumed_lease, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        terminal_retry.outcome,
        RequestToolApprovalOutcome::Existing(existing) if existing == approved
    ));
    assert!(terminal_retry.required_event.is_none());
    assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 1);
}

#[tokio::test]
async fn delete_chat_erases_a_terminal_approval_receipt_before_its_event() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, _call, request) = claimed_sensitive_call(&store, &chat).await;

    let registered = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(registered.required_event.is_some());

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .request_turn_cancellation(turn_id, cancel_at)
        .await
        .unwrap()
        .is_some());
    let finish_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .finish_turn_cancellation(turn_id, lease_token, finish_at)
        .await
        .unwrap()
        .is_some());

    assert_eq!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted
    );
    assert!(store.get_chat(chat.id).await.unwrap().is_none());
    assert!(store.list_events(chat.id, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_closes_pending_approval_and_tool_atomically() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    let pending = store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(matches!(
        pending.outcome,
        RequestToolApprovalOutcome::Requested(_)
    ));
    assert_eq!(
        store
            .list_pending_tool_call_approvals(chat.id, 100)
            .await
            .unwrap()
            .len(),
        1
    );

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .request_turn_cancellation(turn_id, cancel_at)
        .await
        .unwrap()
        .is_some());
    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now(),)
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
    let finish_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .finish_turn_cancellation(turn_id, lease_token, finish_at)
        .await
        .unwrap()
        .is_some());

    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    let approval = store
        .get_tool_call_approval(call.id)
        .await
        .unwrap()
        .expect("terminal approval receipt must remain");
    assert_eq!(approval.status, ToolApprovalStatus::Rejected);
    assert_eq!(
        approval.reason.as_deref(),
        Some("turn cancellation revoked approval")
    );
    let stored_call = store
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|stored| stored.id == call.id)
        .unwrap();
    assert_eq!(stored_call.status, ToolCallStatus::Cancelled);
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now(),)
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
}

#[tokio::test]
async fn retry_wait_cancellation_terminalizes_pending_call_and_approval() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();

    let failure_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(store
        .record_turn_run_failure(
            turn_id,
            lease_token,
            failure_at,
            TurnFailureRetry::RetryAt(failure_at + chrono::Duration::minutes(1)),
            0,
            crate::provider::Usage::default(),
            "retry_later",
            None,
        )
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().status,
        TurnRunStatus::RetryWait
    );

    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    assert!(matches!(
        store
            .request_turn_cancellation(turn_id, cancel_at)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Cancelled(turn))
            if turn.status == TurnRunStatus::Cancelled
    ));
    assert_eq!(
        store
            .list_tool_calls(chat.id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call.id)
            .unwrap()
            .status,
        ToolCallStatus::Cancelled
    );
    assert_eq!(
        store
            .get_tool_call_approval(call.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Rejected
    );
    assert!(store
        .list_pending_tool_call_approvals(chat.id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .decide_tool_call_approval(chat.id, call.id, &ApprovalDecision::Approve, Utc::now())
            .await
            .unwrap(),
        DecideToolApprovalOutcome::DecisionConflict
    );
}

#[tokio::test]
async fn cancellation_and_approval_decision_serialize_without_pending_state() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (turn_id, _lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();
    let cancel_at = super::ops::agent_run::database_now(&store.conn)
        .await
        .unwrap()
        .max(
            store
                .get_turn_run(turn_id)
                .await
                .unwrap()
                .unwrap()
                .updated_at,
        );
    let chat_id = chat.id;
    let call_id = call.id;
    let cancelling = store.clone();
    let deciding = store.clone();
    let (cancelled, decided) = tokio::join!(
        async move {
            cancelling
                .request_turn_cancellation(turn_id, cancel_at)
                .await
        },
        async move {
            deciding
                .decide_tool_call_approval(chat_id, call_id, &ApprovalDecision::Approve, Utc::now())
                .await
        }
    );
    assert!(cancelled.unwrap().is_some());
    assert!(matches!(
        decided.unwrap(),
        DecideToolApprovalOutcome::Decided(_) | DecideToolApprovalOutcome::DecisionConflict
    ));
    assert!(store
        .list_pending_tool_call_approvals(chat_id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_ne!(
        store
            .get_tool_call_approval(call_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Pending
    );
}

#[tokio::test]
async fn failed_tool_resolution_and_approval_decision_serialize_to_one_terminal_receipt() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, _lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();

    let chat_id = chat.id;
    let call_id = call.id;
    let resolving = store.clone();
    let deciding = store.clone();
    let resolution = ToolCallResolution::Failed {
        result: "tool implementation is unavailable".into(),
        error_code: "tool_error".into(),
        error_detail: None,
    };
    let (resolved, decided) = tokio::join!(
        async move {
            resolving
                .resolve_server_tool_call_with_evidence(call_id, &resolution, Utc::now(), &[])
                .await
        },
        async move {
            deciding
                .decide_tool_call_approval(chat_id, call_id, &ApprovalDecision::Approve, Utc::now())
                .await
        }
    );
    assert!(matches!(
        resolved.unwrap(),
        ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
    ));
    assert!(matches!(
        decided.unwrap(),
        DecideToolApprovalOutcome::Decided(_) | DecideToolApprovalOutcome::DecisionConflict
    ));
    let approval = store
        .get_tool_call_approval(call_id)
        .await
        .unwrap()
        .expect("approval receipt must remain");
    assert_ne!(approval.status, ToolApprovalStatus::Pending);
    assert!(store
        .list_pending_tool_call_approvals(chat_id, 100)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .list_tool_calls(chat_id)
            .await
            .unwrap()
            .into_iter()
            .find(|stored| stored.id == call_id)
            .unwrap()
            .status,
        ToolCallStatus::Failed
    );
}

#[tokio::test]
async fn approval_reject_reason_rejects_controls_before_commit() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, lease_token, call, request) = claimed_sensitive_call(&store, &chat).await;
    store
        .request_tool_call_approval_and_append_event(&request, lease_token, 1, Utc::now())
        .await
        .unwrap();
    assert!(store
        .decide_tool_call_approval(
            chat.id,
            call.id,
            &ApprovalDecision::Reject {
                reason: "bad\0reason".into(),
            },
            Utc::now(),
        )
        .await
        .is_err());
    assert_eq!(
        store
            .get_tool_call_approval(call.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        ToolApprovalStatus::Pending
    );
}

#[tokio::test]
async fn client_tool_call_is_fenced_by_its_exact_lease() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_020, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_client".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({"hint": "Documents"}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    assert_eq!(
        store
            .resolve_server_tool_call(
                call.id,
                &ToolCallResolution::Cancelled {
                    result: "not selected".into(),
                },
                created_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store.list_pending_client_tool_calls(chat.id).await.unwrap(),
        vec![call.clone()]
    );

    let executor = uuid::Uuid::new_v4();
    let requested_lease_token = uuid::Uuid::new_v4();
    let claimed_at = created_at + chrono::Duration::seconds(1);
    let first_expiry = claimed_at + chrono::Duration::minutes(1);
    let claimed = match store
        .claim_client_tool_call(
            call.id,
            chat.id,
            executor,
            requested_lease_token,
            claimed_at,
            first_expiry,
        )
        .await
        .unwrap()
    {
        ClaimClientToolCallOutcome::Claimed(claim) => claim,
        outcome => panic!("unexpected claim outcome: {outcome:?}"),
    };
    assert_eq!(claimed.call.client_executor_id, Some(executor));
    let lease_token = claimed.lease_token;
    assert_eq!(lease_token, requested_lease_token);
    assert!(!serde_json::to_string(&claimed.call)
        .unwrap()
        .contains(&lease_token.to_string()));
    assert!(!format!("{claimed:?}").contains(&lease_token.to_string()));
    assert_eq!(claimed.call.client_lease_expires_at, Some(first_expiry));
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                executor,
                lease_token,
                claimed_at + chrono::Duration::milliseconds(1),
                first_expiry + chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Existing(_)
    ));
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                executor,
                uuid::Uuid::new_v4(),
                claimed_at,
                first_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                claimed_at,
                first_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );

    let extended_expiry = first_expiry + chrono::Duration::minutes(1);
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Extended
    );
    assert_eq!(
        store
            .heartbeat_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                claimed_at + chrono::Duration::seconds(1),
                extended_expiry,
            )
            .await
            .unwrap(),
        HeartbeatClientToolCallOutcome::Existing
    );
    let resolution = ToolCallResolution::Failed {
        result: "folder picker failed".into(),
        error_code: "picker_failed".into(),
        error_detail: Some("native dialog closed unexpectedly".into()),
    };
    let resolved_at = claimed_at + chrono::Duration::seconds(2);
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at + chrono::Duration::milliseconds(1),
                &resolution,
                resolved_at + chrono::Duration::milliseconds(1),
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                ChatId::new(),
                lease_token,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert_eq!(
        store
            .resolve_server_tool_call(call.id, &resolution, resolved_at)
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    assert!(store
        .list_pending_client_tool_calls(chat.id)
        .await
        .unwrap()
        .is_empty());
    let stored = store.list_tool_calls(chat.id).await.unwrap().pop().unwrap();
    assert_eq!(stored.status, ToolCallStatus::Failed);
    assert_eq!(stored.error_code.as_deref(), Some("picker_failed"));
    assert_eq!(stored.client_executor_id, Some(executor));
    assert_eq!(stored.client_lease_expires_at, None);
}

#[tokio::test]
async fn expired_client_lease_is_not_transferred_implicitly() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_030, 0).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_picker".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Existing(_)
    ));
    let first = uuid::Uuid::new_v4();
    let requested_lease_token = uuid::Uuid::new_v4();
    let claimed_at = created_at + chrono::Duration::seconds(1);
    let expiry = claimed_at + chrono::Duration::seconds(5);
    let lease_token = match store
        .claim_client_tool_call(
            call.id,
            chat.id,
            first,
            requested_lease_token,
            claimed_at,
            expiry,
        )
        .await
        .unwrap()
    {
        ClaimClientToolCallOutcome::Claimed(claim) => claim.lease_token,
        outcome => panic!("unexpected claim outcome: {outcome:?}"),
    };
    let after_expiry = expiry + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                after_expiry,
                after_expiry + chrono::Duration::minutes(1),
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Unavailable
    );
    assert_eq!(
        store
            .resolve_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_expiry,
                &ToolCallResolution::Cancelled {
                    result: "cancelled".into(),
                },
                after_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::LeaseLost
    );
    let recovered = ToolCallResolution::Cancelled {
        result: "cancelled after native receipt recovery".into(),
    };
    assert_eq!(
        store
            .resolve_expired_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_expiry,
                &recovered,
                after_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Resolved
    );
    assert_eq!(
        store
            .resolve_expired_client_tool_call(
                call.id,
                chat.id,
                lease_token,
                after_expiry,
                &recovered,
                after_expiry,
            )
            .await
            .unwrap(),
        ResolveToolCallOutcome::Existing
    );
}

#[tokio::test]
async fn concurrent_client_claim_has_one_sqlite_winner() {
    let (_dir, store) = temp_store().await;
    let store = std::sync::Arc::new(store);
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_040, 123_456_789).unwrap();
    let call = ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id: TurnId::new(),
        provider_id: "tu_race".into(),
        name: "select_folder".into(),
        arguments: serde_json::json!({}),
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        error_code: None,
        error_detail: None,
        client_executor_id: None,
        client_lease_expires_at: None,
        created_at,
        resolved_at: None,
    };
    store.accept_tool_call(&call).await.unwrap();
    let claim_at = created_at + chrono::Duration::seconds(1);
    let lease_expires_at = claim_at + chrono::Duration::minutes(1);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let executor_id = uuid::Uuid::new_v4();
        let lease_token = uuid::Uuid::new_v4();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store
                .claim_client_tool_call(
                    call.id,
                    chat.id,
                    executor_id,
                    lease_token,
                    claim_at,
                    lease_expires_at,
                )
                .await
                .unwrap()
        }));
    }
    let mut claimed = 0;
    let mut unavailable = 0;
    for task in tasks {
        match task.await.unwrap() {
            ClaimClientToolCallOutcome::Claimed(_) => claimed += 1,
            ClaimClientToolCallOutcome::Unavailable => unavailable += 1,
            outcome => panic!("unexpected concurrent claim outcome: {outcome:?}"),
        }
    }
    assert_eq!(claimed, 1);
    assert_eq!(unavailable, 7);
}

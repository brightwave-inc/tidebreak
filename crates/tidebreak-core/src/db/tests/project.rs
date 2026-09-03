use super::*;

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
    entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .filter(entities::session::Column::Id.eq(standalone.id.0))
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
    entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(1_i64),
        )
        .filter(entities::session::Column::Id.eq(gapped.id.0))
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
    entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::AttachmentRevision,
            sea_orm::sea_query::Expr::value(2_i64),
        )
        .filter(entities::session::Column::Id.eq(mixed.id.0))
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
    let orphan = store
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO \"session\" (
                id, project_id, harness_kind, permission_mode, lifecycle,
                attention_state, attention_source, created_at
             ) VALUES (
                '{}', '{}', 'internal', 'ask', 'idle', '{{\"type\":\"idle\"}}',
                'lifecycle', '2026-09-01T00:00:00Z'
             )",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ))
        .await;
    assert!(
        orphan.is_err(),
        "a session cannot name a project that does not exist"
    );

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
        owner: sea_orm::ActiveValue::NotSet,
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
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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

/// Moving a conversation rewrites which identity holds its folder authority,
/// so the move has to snapshot the destination's defaults and has to refuse
/// whenever grants exist that the new identity would not carry.
#[tokio::test]
async fn moving_a_chat_snapshots_project_defaults_and_refuses_over_connected_folders() {
    let (_dir, store) = temp_store().await;
    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let mut project = sample_project();
    project.attachment_revision = 3;
    project.root_attachments = vec![root];
    store.create_project(&project).await.unwrap();

    let loose = sample_chat();
    store.create_chat(&loose).await.unwrap();
    assert_eq!(
        store
            .move_chat_to_project(loose.id, Some(project.id))
            .await
            .unwrap(),
        MoveChatOutcome::Moved
    );
    let filed = store.get_chat(loose.id).await.unwrap().unwrap();
    assert_eq!(filed.project_id, Some(project.id));
    // The destination's defaults arrive as the conversation's own ordered
    // snapshot, exactly as creating a chat inside the project would seed it.
    assert_eq!(
        filed.root_attachments,
        vec![ChatRootAttachment {
            root_id: root,
            origin: RootAttachmentOrigin::ProjectDefault,
        }]
    );
    assert_eq!(filed.attachment_revision, 1);

    // Now that it holds a folder, it cannot leave: the grants the broker issued
    // are keyed to the project it would stop being part of.
    assert_eq!(
        store.move_chat_to_project(loose.id, None).await.unwrap(),
        MoveChatOutcome::HasConnectedFolders
    );
    assert_eq!(
        store.get_chat(loose.id).await.unwrap().unwrap().project_id,
        Some(project.id)
    );

    // A conversation with nothing attached moves back out and keeps its
    // revision, there being no projection change to count.
    let empty_project = sample_project();
    store.create_project(&empty_project).await.unwrap();
    let mut second = sample_chat();
    second.id = SessionId::new();
    second.project_id = Some(empty_project.id);
    store.create_chat(&second).await.unwrap();
    assert_eq!(
        store.move_chat_to_project(second.id, None).await.unwrap(),
        MoveChatOutcome::Moved
    );
    let unfiled = store.get_chat(second.id).await.unwrap().unwrap();
    assert_eq!(unfiled.project_id, None);
    assert_eq!(unfiled.attachment_revision, second.attachment_revision);

    assert_eq!(
        store
            .move_chat_to_project(second.id, Some(ProjectId::new()))
            .await
            .unwrap(),
        MoveChatOutcome::ProjectNotFound
    );
    assert_eq!(
        store
            .move_chat_to_project(SessionId::new(), Some(project.id))
            .await
            .unwrap(),
        MoveChatOutcome::ChatNotFound
    );
}

#[tokio::test]
async fn project_deletion_serializes_with_synchronous_source_ingestion() {
    let (_dir, store) = temp_store().await;

    for attempt in 0..16_u8 {
        let project = sample_project();
        store.create_project(&project).await.unwrap();
        let mut source = sample_raw_source(
            DocumentId::new(),
            &format!("file:///project-race-{attempt}.bin"),
            DocumentBlob::from_digest([attempt.saturating_add(1); 32], 1),
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
            ingest_store.accept_document_source(&ingest_source).await
        });
        barrier.wait().await;

        let deleted = delete.await.unwrap().unwrap();
        let ingested = ingest.await.unwrap();
        match (deleted, ingested) {
            (DeleteProjectOutcome::Deleted, Err(AgentError::ProjectNotFound(missing_project))) => {
                assert_eq!(missing_project, project.id)
            }
            (DeleteProjectOutcome::NotEmpty, Ok(record)) => {
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
        origin_uri: Some("file:///missing-project.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "missing project".into(),
        updated_at: Utc::now(),
    };
    assert!(matches!(
        store.upsert_document(&canonical).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));
    let mut staged = sample_raw_source(
        DocumentId::new(),
        "file:///missing-project.bin",
        DocumentBlob::from_digest([0x41; 32], 1),
    );
    staged.project_id = Some(missing);
    assert!(matches!(
        store.accept_document_source(&staged).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));

    let mut chat = sample_chat();
    chat.project_id = Some(missing);
    assert!(matches!(
        store.create_chat_with_project_defaults(&chat).await,
        Err(AgentError::ProjectNotFound(id)) if id == missing
    ));
}

use super::*;

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
    let mut in_b = sample_document(Some(project_b.id));
    in_b.created_at = DateTime::<Utc>::from_timestamp(3_000, 0).unwrap();
    in_b.source_blob = Some(DocumentBlob::from_digest([0x5a; 32], 8_192));

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
        origin_uri: in_b.origin_uri.clone(),
        media_type: in_b.media_type.clone(),
        title: in_b.title.clone(),
        canonical_text: in_b.canonical_text.clone(),
        updated_at: in_b.updated_at,
    };
    assert!(store.upsert_document(&legacy_replacement).await.is_err());
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
async fn summaries_derive_readiness_from_canonical_text() {
    let (_dir, store) = temp_store().await;
    let document_with = |raw_id: u128, canonical_text: &str| {
        let mut document = sample_document(None);
        document.id = DocumentId(uuid::Uuid::from_u128(raw_id));
        document.canonical_text = canonical_text.to_owned();
        document
    };
    let with_text = document_with(2, "売上 grew by 10%.");
    let without_text = document_with(1, "");
    for document in [&with_text, &without_text] {
        store.create_document(document).await.unwrap();
    }

    let readable = store
        .list_document_summaries(DocumentScope::All, None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|document| (document.id, document.readable, document.readiness()))
        .collect::<Vec<_>>();
    assert_eq!(
        readable,
        vec![
            (with_text.id, true, crate::DocumentReadiness::Readable),
            (
                without_text.id,
                false,
                crate::DocumentReadiness::StoredNoText,
            ),
        ]
    );
    // The listing decides emptiness in the database, so it must still never
    // carry the text itself.
    assert!(with_text.is_readable());
    assert!(!without_text.is_readable());
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
        store.get_document(document.id).await.unwrap(),
        Some(document)
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
        origin_uri: Some("file:///scoped.txt".into()),
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "project A source".into(),
        updated_at: Utc::now(),
    };
    let first = store.upsert_document(&source).await.unwrap();
    let moved = DocumentUpsert {
        project_id: Some(project_b.id),
        canonical_text: "must not move".into(),
        ..source
    };
    assert!(store.upsert_document(&moved).await.is_err());
    assert_eq!(store.get_document(moved.id).await.unwrap(), Some(first));
}

#[tokio::test]
async fn document_constraints_reject_invalid_catalog_state() {
    let (_dir, store) = temp_store().await;

    let mut empty_media_type = sample_document(None);
    empty_media_type.media_type.clear();
    assert!(store.create_document(&empty_media_type).await.is_err());

    let mut empty_source_uri = sample_document(None);
    empty_source_uri.origin_uri = Some(String::new());
    assert!(store.create_document(&empty_source_uri).await.is_err());

    let mut oversized_source = sample_document(None);
    oversized_source.source_blob = Some(DocumentBlob::from_digest([0; 32], u64::MAX));
    assert!(store.create_document(&oversized_source).await.is_err());

    let mut invalid_source_id = sample_document(None);
    let mut invalid_blob = DocumentBlob::from_digest([0x11; 32], 512);
    invalid_blob.id = uuid::Uuid::new_v4();
    invalid_source_id.source_blob = Some(invalid_blob);
    assert!(store.create_document(&invalid_source_id).await.is_err());
    assert_eq!(
        store.get_document(invalid_source_id.id).await.unwrap(),
        None
    );

    let mut valid_source = sample_document(None);
    valid_source.source_blob = Some(DocumentBlob::from_digest([0x22; 32], 512));
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
async fn synchronous_source_accept_publishes_blob_and_text_together() {
    let (_dir, store) = temp_store().await;
    let source = DocumentSourceUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        origin_uri: Some("file:///report.txt".into()),
        media_type: "text/plain".into(),
        title: Some("Report".into()),
        source_blob: DocumentBlob::from_digest([0x33; 32], 4_096),
        canonical_text: "Page one".into(),
        updated_at: Utc::now(),
    };

    let mut invalid = source.clone();
    invalid.source_blob.id = uuid::Uuid::new_v4();
    let error = store.accept_document_source(&invalid).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("blob id does not match its SHA-256 digest"));
    assert_eq!(store.get_document(source.id).await.unwrap(), None);

    let accepted = store.accept_document_source(&source).await.unwrap();
    assert_eq!(accepted.source_blob.as_ref(), Some(&source.source_blob));
    assert_eq!(accepted.canonical_text, source.canonical_text);
    assert!(accepted.is_readable());

    let mut replacement = source.clone();
    replacement.canonical_text.clear();
    replacement.updated_at += chrono::Duration::seconds(1);
    let replaced = store.accept_document_source(&replacement).await.unwrap();
    assert_eq!(replaced.created_at, accepted.created_at);
    assert_eq!(replaced.updated_at, replacement.updated_at);
    assert!(!replaced.is_readable());
}

#[tokio::test]
async fn document_upsert_rolls_back_when_project_is_unknown() {
    let (_dir, store) = temp_store().await;
    let upsert = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: Some(ProjectId::new()),
        origin_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "content".into(),
        updated_at: Utc::now(),
    };
    assert!(store.upsert_document(&upsert).await.is_err());
    assert_eq!(store.get_document(upsert.id).await.unwrap(), None);
}

#[tokio::test]
async fn repeated_document_upserts_are_last_write_wins_and_preserve_creation_time() {
    let (_dir, store) = temp_store().await;
    let first = DocumentUpsert {
        chat_id: None,
        id: DocumentId::new(),
        project_id: None,
        origin_uri: None,
        media_type: "text/plain".into(),
        title: None,
        canonical_text: "first".into(),
        updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
    };
    let created = store.upsert_document(&first).await.unwrap();
    let second = DocumentUpsert {
        canonical_text: "second".into(),
        updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
        ..first
    };
    let replaced = store.upsert_document(&second).await.unwrap();

    assert_eq!(replaced.created_at, created.created_at);
    assert_eq!(replaced.updated_at, second.updated_at);
    assert_eq!(replaced.canonical_text, "second");
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
                origin_uri: Some(uri.into()),
                media_type: "text/plain".into(),
                title: None,
                canonical_text: text.into(),
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
            origin_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "invalid double scope".into(),
            updated_at: Utc::now(),
        })
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("both a conversation and a project"));
}

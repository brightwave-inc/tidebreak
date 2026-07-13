use super::*;
use crate::model::{
    ByteSpan, DocumentParseOutput, DocumentSourceBlob, DocumentSourceUpsert, SourceLocation,
};
use chrono::{DateTime, Utc};

async fn temp_store() -> (tempfile::TempDir, DbStore) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
    let store = DbStore::connect(&url).await.unwrap();
    (dir, store)
}

fn sample_chat() -> Chat {
    Chat {
        id: ChatId::new(),
        project_id: None,
        title: Some("hello".into()),
        model: None,
        workspace_dir: PathBuf::from("/tmp/ws"),
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn sample_project() -> Project {
    Project {
        id: ProjectId::new(),
        title: Some("proj".into()),
        workspace_dir: PathBuf::from("/tmp/proj"),
        created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
    }
}

fn sample_document(project_id: Option<ProjectId>) -> DocumentRecord {
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_100, 0).unwrap();
    DocumentRecord {
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
    in_b.source_blob = Some(DocumentSourceBlob {
        id: uuid::Uuid::new_v4(),
        sha256: [0x5a; 32],
        byte_len: 8_192,
    });
    in_b.canonical_fingerprint = Some("parser=markdown-v1".into());

    for document in [&unscoped, &in_a, &in_b] {
        store.create_document(document).await.unwrap();
    }
    unscoped = store.get_document(unscoped.id).await.unwrap().unwrap();
    in_a = store.get_document(in_a.id).await.unwrap().unwrap();
    in_b = store.get_document(in_b.id).await.unwrap().unwrap();

    let legacy_replacement = DocumentUpsert {
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
    oversized_source.source_blob = Some(DocumentSourceBlob {
        id: uuid::Uuid::new_v4(),
        sha256: [0; 32],
        byte_len: u64::MAX,
    });
    assert!(store.create_document(&oversized_source).await.is_err());

    let mut empty_parser_fingerprint = sample_document(None);
    empty_parser_fingerprint.canonical_fingerprint = Some(String::new());
    assert!(store
        .create_document(&empty_parser_fingerprint)
        .await
        .is_err());

    let mut valid_source = sample_document(None);
    valid_source.source_blob = Some(DocumentSourceBlob {
        id: uuid::Uuid::new_v4(),
        sha256: [0x22; 32],
        byte_len: 512,
    });
    store.create_document(&valid_source).await.unwrap();
    assert!(entities::document::Entity::update_many()
        .col_expr(
            entities::document::Column::SourceSha256,
            sea_orm::sea_query::Expr::value(Some(vec![0x22; 31])),
        )
        .filter(entities::document::Column::Id.eq(valid_source.id.0))
        .exec(&store.conn)
        .await
        .is_err());
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
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///report.pdf".into()),
        media_type: "application/pdf".into(),
        title: Some("Report".into()),
        source_blob: DocumentSourceBlob {
            id: uuid::Uuid::new_v4(),
            sha256: [0x33; 32],
            byte_len: 4_096,
        },
        updated_at: Utc::now(),
    };

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
    parse_document.source_blob = Some(DocumentSourceBlob {
        id: uuid::Uuid::new_v4(),
        sha256: [0x11; 32],
        byte_len: 1_024,
    });
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
        id: DocumentId::new(),
        project_id: None,
        source_uri: Some("file:///retry-report.pdf".into()),
        media_type: "application/pdf".into(),
        title: Some("Retry report".into()),
        source_blob: DocumentSourceBlob {
            id: uuid::Uuid::new_v4(),
            sha256: [0x44; 32],
            byte_len: 4_096,
        },
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
async fn list_chats_is_newest_first_and_messages_oldest_first() {
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

    // Messages come back oldest-first regardless of insert order.
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
    assert_eq!(listed, vec![m2, m1]);
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
async fn tool_calls_roundtrip_and_upsert_preserves_created_at() {
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
        result: None,
        is_error: false,
        created_at: created,
        completed_at: None,
    };
    store.upsert_tool_call(&call).await.unwrap();

    let completed = DateTime::<Utc>::from_timestamp(1_700_000_011, 0).unwrap();
    store
        .upsert_tool_call(&ToolCallRecord {
            result: Some("hello".into()),
            is_error: false,
            created_at: Utc::now(), // must not overwrite the original
            completed_at: Some(completed),
            ..call.clone()
        })
        .await
        .unwrap();

    let listed = store.list_tool_calls(chat.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].created_at, created);
    assert_eq!(listed[0].completed_at, Some(completed));
    assert_eq!(listed[0].result.as_deref(), Some("hello"));
    assert_eq!(listed[0].arguments, serde_json::json!({"path": "note.txt"}));
}

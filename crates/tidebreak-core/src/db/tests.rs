use super::*;
use crate::model::{
    ChatRootAttachment, DocumentBlob, DocumentSourceUpsert, RootAttachmentChangeAction,
    RootAttachmentChangeFailure, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    ToolCallExecution, ToolCallResolution, ToolCallStatus, MAX_ATTACHMENT_REVISION,
    MAX_ROOT_ATTACHMENTS,
};
use crate::storage::ApplyTurnSteerOutcome;
use crate::{ApprovalClass, ToolApprovalStatus};
use chrono::{DateTime, Utc};

mod agent_run;
mod app;
mod code;
mod connected_app;
mod context_checkpoint;
mod delegated_file_read;
mod message_attachment;
mod multi_agent_wait;
mod notification;
mod operation_log;
mod output;
mod parent_terminal_guard;
mod root_attachment;
mod sandbox_spawn_checkpoint;
mod turn_steer;
mod turn_terminal_usage;

struct MigratedSqliteTemplate {
    _directory: tempfile::TempDir,
    database: std::path::PathBuf,
}

static MIGRATED_SQLITE_TEMPLATE: tokio::sync::OnceCell<MigratedSqliteTemplate> =
    tokio::sync::OnceCell::const_new();

/// Build the current empty schema once, then clone it for ordinary store tests.
///
/// Every caller still owns a distinct writable file. Tests that exercise the
/// migration chain itself create their historical/raw schemas directly and do
/// not use this helper.
async fn migrated_sqlite_template() -> &'static MigratedSqliteTemplate {
    MIGRATED_SQLITE_TEMPLATE
        .get_or_init(|| async {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("template.db");
            let url = format!("sqlite://{}?mode=rwc", database.display());
            let store = DbStore::connect(&url).await.unwrap();
            store
                .conn
                .execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE);")
                .await
                .unwrap();
            drop(store);
            MigratedSqliteTemplate {
                _directory: directory,
                database,
            }
        })
        .await
}

async fn temp_store() -> (tempfile::TempDir, DbStore) {
    let template = migrated_sqlite_template().await;
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("test.db");
    std::fs::copy(&template.database, &database).unwrap();
    let url = format!("sqlite://{}?mode=rw", database.display());
    let conn = Database::connect(&url).await.unwrap();
    conn.execute_unprepared("PRAGMA journal_mode=WAL;")
        .await
        .unwrap();
    let store = DbStore { conn };
    (dir, store)
}

async fn temp_store_with_max_connections(max_connections: u32) -> (tempfile::TempDir, DbStore) {
    let template = migrated_sqlite_template().await;
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("test.db");
    std::fs::copy(&template.database, &database).unwrap();
    let url = format!("sqlite://{}?mode=rw", database.display());
    let mut options = sea_orm::ConnectOptions::new(url);
    options.max_connections(max_connections);
    let store = DbStore::connect_with_options(options).await.unwrap();
    (dir, store)
}

/// A checkpoint the host expects an executor lane to run.
fn dispatchable(call: &crate::model::SandboxToolCallRequest) -> SandboxToolCallParkEntry {
    SandboxToolCallParkEntry {
        call: call.clone(),
        resolution: None,
    }
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
        permission_mode: None,
        network_policy: Default::default(),
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

fn sample_raw_source(id: DocumentId, uri: &str, source_blob: DocumentBlob) -> DocumentSourceUpsert {
    DocumentSourceUpsert {
        id,
        chat_id: None,
        project_id: None,
        origin_uri: Some(uri.into()),
        media_type: "application/octet-stream".into(),
        title: None,
        source_blob,
        canonical_text: String::new(),
        updated_at: Utc::now(),
    }
}

fn sample_document(project_id: Option<ProjectId>) -> DocumentRecord {
    let created_at = DateTime::<Utc>::from_timestamp(1_700_000_100, 0).unwrap();
    DocumentRecord {
        chat_id: None,
        id: DocumentId::new(),
        project_id,
        origin_uri: Some("file:///資料/notes.md".into()),
        media_type: "text/markdown".into(),
        title: Some("Résumé 📈".into()),
        source_blob: None,
        canonical_text: "# Résumé\n\n売上 grew by 10%.".into(),
        created_at,
        updated_at: created_at,
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
        permission_mode: Set(None),
        network_policy: Set(r#"{"mode":"off"}"#.into()),
        attachment_revision: Set(0),
        created_at: Set(Utc::now()),
        owner: sea_orm::ActiveValue::NotSet,
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
            permission_mode: Set(None),
            network_policy: Set(r#"{"mode":"off"}"#.into()),
            attachment_revision: Set(0),
            created_at: Set(chat.created_at),
            owner: sea_orm::ActiveValue::NotSet,
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
    second.id = ChatId::new();
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
            .move_chat_to_project(ChatId::new(), Some(project.id))
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

/// The undo window is a bounded number of turns, and the bound is what keeps
/// snapshot storage from growing with editing activity. Falling out of the
/// window has to release the retained bytes too: a row deleted without
/// enqueueing its blob leaves the file behind forever with nothing pointing at
/// it, which is the leak this bound exists to avoid.
#[tokio::test]
async fn the_file_change_journal_keeps_only_the_newest_retained_turns() {
    use crate::model::{
        ExecFileChange, ExecFileSnapshotRecord, ExecUndoState, EXEC_SNAPSHOT_RETAINED_TURNS,
    };

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    let mut turns = Vec::new();
    for index in 0..(EXEC_SNAPSHOT_RETAINED_TURNS + 1) {
        let blob = DocumentBlob::from_bytes(format!("revision {index}").as_bytes());
        let turn_id = TurnId::new();
        store
            .record_exec_file_snapshots(
                chat.id,
                turn_id,
                &[ExecFileSnapshotRecord {
                    folder_path: "/Users/someone/Documents".into(),
                    relative_path: "notes.md".into(),
                    change: ExecFileChange::Overwritten,
                    prior_blob_id: Some(blob.id),
                    prior_byte_len: Some(blob.byte_len),
                    new_sha256: None,
                    undo: ExecUndoState::Available,
                }],
            )
            .await
            .unwrap();
        turns.push((turn_id, blob.id));
    }

    let retained = store.list_exec_file_snapshots(chat.id).await.unwrap();
    assert_eq!(retained.len(), EXEC_SNAPSHOT_RETAINED_TURNS);
    let (oldest_turn, oldest_blob) = turns[0];
    assert!(
        retained.iter().all(|row| row.turn_id != oldest_turn),
        "the turn that fell out of the window is still journaled"
    );
    assert_eq!(
        store
            .get_blob_retirement(oldest_blob)
            .await
            .unwrap()
            .map(|retirement| retirement.status),
        Some(BlobRetirementStatus::Queued),
        "its retained bytes were dropped without being released"
    );
    let (newest_turn, newest_blob) = *turns.last().unwrap();
    assert_eq!(retained.first().unwrap().turn_id, newest_turn);
    assert_eq!(store.get_blob_retirement(newest_blob).await.unwrap(), None);
}

/// Applied and rejected changes share one journal but not one write: a turn
/// records each in its own transaction, so its rows carry different timestamps.
/// Retention is a window over turns, and a turn inside it keeps both halves —
/// pruning that cut on a timestamp instead would retract the earlier half of a
/// turn it was supposed to be retaining.
#[tokio::test]
async fn retention_keeps_both_halves_of_a_turn_recorded_in_two_writes() {
    use crate::model::{
        ExecFileChange, ExecFileRejectionReason, ExecFileRejectionRecord, ExecFileSnapshotRecord,
        ExecUndoState, EXEC_SNAPSHOT_RETAINED_TURNS,
    };

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();

    // The straddling turn: its rejection is journaled before its applied
    // change, so the turn's own rows bracket every later cutoff candidate.
    let straddling_turn = TurnId::new();
    store
        .record_exec_file_rejections(
            chat.id,
            straddling_turn,
            &[ExecFileRejectionRecord {
                folder_path: "/Users/someone/Documents".into(),
                relative_path: "locked.md".into(),
                reason: ExecFileRejectionReason::Stale,
            }],
        )
        .await
        .unwrap();
    store
        .record_exec_file_snapshots(
            chat.id,
            straddling_turn,
            &[ExecFileSnapshotRecord {
                folder_path: "/Users/someone/Documents".into(),
                relative_path: "notes.md".into(),
                change: ExecFileChange::Overwritten,
                prior_blob_id: None,
                prior_byte_len: None,
                new_sha256: None,
                undo: ExecUndoState::Available,
            }],
        )
        .await
        .unwrap();

    // Fill the window exactly, so pruning runs with that turn still inside it.
    for index in 1..EXEC_SNAPSHOT_RETAINED_TURNS {
        store
            .record_exec_file_snapshots(
                chat.id,
                TurnId::new(),
                &[ExecFileSnapshotRecord {
                    folder_path: "/Users/someone/Documents".into(),
                    relative_path: format!("later-{index}.md"),
                    change: ExecFileChange::Created,
                    prior_blob_id: None,
                    prior_byte_len: None,
                    new_sha256: None,
                    undo: ExecUndoState::Available,
                }],
            )
            .await
            .unwrap();
    }

    let rejections = store.list_exec_file_rejections(chat.id).await.unwrap();
    assert!(
        rejections.iter().any(|row| row.turn_id == straddling_turn),
        "a retained turn lost the half it journaled first"
    );
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
        reasoning: Default::default(),
        content: "hi there".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: Set("turn input".into()),
        llm_content: Set(None),
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
        invoked_skills: Set(serde_json::json!([])),
        voice_input_used: Set(false),
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
        reasoning: Default::default(),
        content: Set("done".into()),
        llm_content: Set(None),
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
    let expired_at =
        ops::agent_run::database_now(&store.conn).await.unwrap() - chrono::Duration::seconds(1);
    let mut reservation: entities::turn_admission::ActiveModel =
        entities::turn_admission::Entity::find_by_id(turn_id.0)
            .one(&store.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
    reservation.lease_expires_at = Set(Some(expired_at));
    reservation.update(&store.conn).await.unwrap();

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
    assert!(store.get_turn_run(deleted.id).await.unwrap().is_none());

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
async fn empty_turn_claim_does_not_wait_for_sqlite_writer() {
    let (_dir, store) = temp_store_with_max_connections(2).await;

    // Hold SQLite's single writer with an unrelated transaction. A fresh scan
    // with no turn to claim must remain read-only and leave the writer queue
    // available for real work.
    let writer = store.conn.begin().await.unwrap();
    writer
        .execute_unprepared("UPDATE advisory_lock SET name = name WHERE name = 'agent_run_claim'")
        .await
        .unwrap();

    let now = Utc::now();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        store.claim_turn_run(
            uuid::Uuid::new_v4(),
            now,
            now + chrono::Duration::minutes(1),
        ),
    )
    .await
    .expect("an empty turn scan waited for SQLite's writer")
    .unwrap();
    assert!(outcome.turn.is_none());
    assert!(outcome.terminal_event.is_none());

    writer.rollback().await.unwrap();
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
async fn an_expired_lease_handback_makes_the_turn_immediately_reclaimable() {
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
    let expiry = claimed_at + chrono::Duration::minutes(1);
    let claimed = store
        .claim_turn_run(token, claimed_at, expiry)
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(claimed.lease_token, Some(token));

    // A wrong token hands nothing back; the live lease stays untouched.
    let handback_at = claimed_at + chrono::Duration::seconds(5);
    assert!(!store
        .expire_turn_run_lease(turn_id, uuid::Uuid::new_v4(), handback_at)
        .await
        .unwrap());
    assert_eq!(
        store
            .get_turn_run(turn_id)
            .await
            .unwrap()
            .unwrap()
            .lease_expires_at,
        Some(expiry)
    );

    // The exact token hands the lease back: the expiry drops to now, and the
    // next scan — the relaunched process — reclaims without waiting it out.
    assert!(store
        .expire_turn_run_lease(turn_id, token, handback_at)
        .await
        .unwrap());
    let reclaim_at = handback_at + chrono::Duration::seconds(1);
    let second_token = uuid::Uuid::new_v4();
    let reclaimed = store
        .claim_turn_run(
            second_token,
            reclaim_at,
            reclaim_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    assert_eq!(reclaimed.id, turn_id);
    assert_eq!(reclaimed.status, TurnRunStatus::Running);
    assert_eq!(reclaimed.lease_token, Some(second_token));

    // Handing back an already-superseded lease is a no-op.
    assert!(!store
        .expire_turn_run_lease(turn_id, token, reclaim_at + chrono::Duration::seconds(1))
        .await
        .unwrap());
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
        reasoning: Default::default(),
        content: "resumed answer".into(),
        llm_content: None,
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
    // A client call is executed and resolved outside the agent loop, and the
    // resumed loop reads its result straight into the model transcript without
    // ever revisiting the call. Nothing else announces that it finished, so the
    // renderer showed the row running from `ToolCallStarted` until the chat was
    // reopened. It announces itself here instead.
    let completions = |events: Vec<crate::SequencedEvent>| {
        events
            .into_iter()
            .filter(|event| matches!(event.event, AgentEvent::ToolCallCompleted { .. }))
            .collect::<Vec<_>>()
    };
    let announced = completions(store.list_events(chat.id, 0).await.unwrap());
    assert_eq!(announced.len(), 1);
    let AgentEvent::ToolCallCompleted {
        call_id,
        ref output,
        ref action,
        ..
    } = announced[0].event
    else {
        unreachable!("filtered to completions")
    };
    assert_eq!(call_id, request.id);
    assert!(!output.is_error);
    // Projected from the call's own stored arguments, so a client card names
    // its action identically live and after a reload.
    assert_eq!(
        action.as_ref(),
        crate::ToolActionPreview::build(&request.name, &request.arguments).as_ref()
    );

    // An exact retry recovers the same outcome without announcing it twice.
    assert_eq!(
        store
            .resolve_client_tool_call_and_append_event(
                request.id,
                chat.id,
                client_lease,
                resolved_at,
                &resolution,
                resolved_at,
            )
            .await
            .unwrap()
            .outcome,
        ResolveToolCallOutcome::Existing
    );
    assert_eq!(
        completions(store.list_events(chat.id, 0).await.unwrap()).len(),
        1
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
        reasoning: Default::default(),
        content: "must not commit".into(),
        llm_content: None,
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
                0,
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
            0,
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
                    "question_type": "multi_select",
                    "allow_free_form": true
                },
                {
                    "id": "note",
                    "header": "Note",
                    "question": "Anything else I should know?",
                    "question_type": "single_select",
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
        answers: vec![crate::UserQuestionAnswer {
            question_id: "target".into(),
            selected_option_ids: vec!["staging".into(), "production".into()],
            custom_answer: Some("Start with a canary.".into()),
        }],
        additional_user_context: Some("Keep the rollout reversible.".into()),
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
    let answered = restarted
        .answer_user_questions(&answer_request, answered_at)
        .await
        .unwrap();
    let crate::AnswerUserQuestionsOutcome::Answered {
        turn,
        completion_event,
    } = answered
    else {
        panic!("unexpected answer outcome: {answered:?}");
    };
    assert_eq!(turn.id, turn_id);
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    // The answer announces the call's completion itself: the renderer settles
    // the card from this event rather than waiting for the turn to end.
    let crate::AgentEvent::ToolCallCompleted {
        call_id,
        output,
        result: Some(preview),
        ..
    } = &completion_event.event
    else {
        panic!("unexpected completion event: {completion_event:?}");
    };
    assert_eq!(*call_id, request.id);
    assert!(!output.is_error);
    assert_eq!(
        serde_json::from_str::<crate::AnswerUserQuestions>(&output.content).unwrap(),
        sample_user_answers()
    );
    // The recap the transcript shows once the card is gone: option *labels*
    // rather than ids, and a row for the question nobody answered so the card
    // can say it was skipped instead of quietly dropping it.
    let crate::ToolResultPreview::UserQuestions {
        answers,
        additional_context,
    } = preview
    else {
        panic!("unexpected question recap: {preview:?}");
    };
    assert_eq!(
        answers,
        &vec![
            crate::AnsweredUserQuestion {
                question: "Where should I deploy?".into(),
                selected: vec!["Staging".into(), "Production".into()],
                custom_answer: Some("Start with a canary.".into()),
            },
            crate::AnsweredUserQuestion {
                question: "Anything else I should know?".into(),
                selected: Vec::new(),
                custom_answer: None,
            },
        ]
    );
    assert_eq!(
        additional_context.as_deref(),
        Some("Keep the rollout reversible.")
    );
    let answered_call = restarted
        .list_tool_calls(chat.id)
        .await
        .unwrap()
        .into_iter()
        .find(|call| call.id == request.id)
        .unwrap();
    // Rehydration reads the stored column, not the event, so a reload has to
    // find the same recap there.
    assert_eq!(answered_call.result_preview.as_ref(), Some(preview));
    let journaled_completions = restarted
        .list_events(chat.id, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| matches!(event.event, crate::AgentEvent::ToolCallCompleted { .. }))
        .collect::<Vec<_>>();
    assert_eq!(journaled_completions, vec![*completion_event]);
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
    // The exact retry recovered the committed answers without announcing the
    // completion a second time.
    assert_eq!(
        restarted
            .list_events(chat.id, 0)
            .await
            .unwrap()
            .into_iter()
            .filter(|event| matches!(event.event, crate::AgentEvent::ToolCallCompleted { .. }))
            .count(),
        1
    );
    let contradictory = crate::AnswerUserQuestionsRequest {
        answers: crate::AnswerUserQuestions {
            answers: vec![crate::UserQuestionAnswer {
                question_id: "target".into(),
                selected_option_ids: vec!["production".into()],
                custom_answer: None,
            }],
            additional_user_context: Some("Keep the rollout reversible.".into()),
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
    assert!(matches!(
        restarted.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted { .. }
    ));
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
                selected_option_ids: vec!["not-an-option".into()],
                custom_answer: None,
            }],
            additional_user_context: None,
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
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        crate::DeleteChatOutcome::Deleted { .. }
    ));
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
        crate::AnswerUserQuestionsOutcome::Answered { .. }
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
        crate::AnswerUserQuestionsOutcome::Answered { .. }
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
        crate::AnswerUserQuestionsOutcome::Answered { turn, .. }
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

async fn park_test_plan(
    store: &DbStore,
    chat_id: ChatId,
) -> (TurnId, crate::model::ClientToolCallRequest, DateTime<Utc>) {
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat_id, "gpt-5", "plan this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected plan turn acceptance: {outcome:?}"),
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
        provider_id: "provider-plan".into(),
        name: crate::EXIT_PLAN_MODE_TOOL.into(),
        arguments: serde_json::json!({
            "title": "Add health checks",
            "plan": "## Steps\n1. Add a `/healthz` route to the server.\n2. Cover it with one lifecycle test.\n",
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
                        AgentEvent::PlanProposed {
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
        outcome => panic!("unexpected plan checkpoint: {outcome:?}"),
    }
    (turn_id, request, parked_at)
}

/// Accepting a plan is the whole hand-off in one transaction: the pending card
/// survives a restart, the decision completes the call exactly once, and the
/// chat leaves plan mode so the resumed turn re-freezes with execution tools.
#[tokio::test]
async fn accepted_plan_resumes_the_turn_and_leaves_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plans.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_plan(&store, chat.id).await;
    drop(store);

    let restarted = DbStore::connect(&url).await.unwrap();
    let pending = restarted
        .list_pending_plan_approvals(chat.id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id, request.id);
    assert_eq!(pending[0].turn_id, turn_id);
    assert_eq!(pending[0].title, "Add health checks");
    assert!(pending[0].plan.contains("/healthz"));

    let decide_request = crate::DecidePlanRequest {
        chat_id: chat.id,
        call_id: request.id,
        decision: crate::PlanDecision {
            decision: crate::PlanDecisionChoice::Accept,
            feedback: None,
            permission_mode: None,
        },
    };
    let decided_at = parked_at + chrono::Duration::seconds(1);
    let outcome = restarted
        .decide_plan(&decide_request, decided_at)
        .await
        .unwrap();
    let crate::storage::DecidePlanOutcome::Decided {
        turn,
        completion_event,
    } = outcome
    else {
        panic!("unexpected plan decision: {outcome:?}");
    };
    assert_eq!(turn.id, turn_id);
    assert_eq!(turn.status, TurnRunStatus::Resuming);
    // The call resolves outside the agent loop, so this event is the only
    // thing that tells a connected renderer the card settled — and it carries
    // the recap the transcript shows in the pending card's place.
    let crate::AgentEvent::ToolCallCompleted {
        call_id,
        result: Some(preview),
        ..
    } = &completion_event.event
    else {
        panic!("unexpected completion: {:?}", completion_event.event);
    };
    assert_eq!(*call_id, request.id);
    assert!(matches!(
        preview,
        crate::ToolResultPreview::PlanDecision {
            title,
            plan,
            accepted: true,
            feedback: None,
        } if title == "Add health checks" && plan.contains("/healthz")
    ));
    assert_eq!(
        restarted
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        Some(crate::PermissionMode::Auto),
        "accepting must move the chat out of plan mode"
    );
    assert!(restarted
        .list_pending_plan_approvals(chat.id)
        .await
        .unwrap()
        .is_empty());
    let calls = restarted.list_tool_calls(chat.id).await.unwrap();
    let call = calls.iter().find(|call| call.id == request.id).unwrap();
    let result: serde_json::Value = serde_json::from_str(call.result.as_deref().unwrap()).unwrap();
    assert_eq!(result["decision"], "accepted");
    assert!(result["note"].as_str().unwrap().contains("left plan mode"));
    // Rehydration reads the stored column, not the event, so a reload has to
    // find the same recap there.
    assert_eq!(call.result_preview.as_ref(), Some(preview));

    // An exact retry recovers; a different decision conflicts.
    assert!(matches!(
        restarted.decide_plan(&decide_request, decided_at).await.unwrap(),
        crate::storage::DecidePlanOutcome::Existing(turn) if turn.id == turn_id
    ));
    let contradictory = crate::DecidePlanRequest {
        decision: crate::PlanDecision {
            decision: crate::PlanDecisionChoice::Reject,
            feedback: Some("Different call.".into()),
            permission_mode: None,
        },
        ..decide_request
    };
    assert!(matches!(
        restarted
            .decide_plan(&contradictory, decided_at)
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::DecisionConflict
    ));
}

/// Rejecting keeps the chat in plan mode and hands the feedback to the model.
#[tokio::test]
async fn rejected_plan_keeps_plan_mode_and_carries_feedback() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plan-reject.db").display()
    );
    let store = DbStore::connect(&url).await.unwrap();
    let chat = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&chat).await.unwrap();
    let (turn_id, request, parked_at) = park_test_plan(&store, chat.id).await;

    let decided_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .decide_plan(
                &crate::DecidePlanRequest {
                    chat_id: chat.id,
                    call_id: request.id,
                    decision: crate::PlanDecision {
                        decision: crate::PlanDecisionChoice::Reject,
                        feedback: Some("Split step 2 into its own slice.".into()),
                        permission_mode: None,
                    },
                },
                decided_at,
            )
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::Decided { turn, .. }
            if turn.id == turn_id && turn.status == TurnRunStatus::Resuming
    ));
    assert_eq!(
        store
            .get_chat(chat.id)
            .await
            .unwrap()
            .unwrap()
            .permission_mode,
        Some(crate::PermissionMode::Plan),
        "rejecting must keep the chat in plan mode"
    );
    let calls = store.list_tool_calls(chat.id).await.unwrap();
    let call = calls.iter().find(|call| call.id == request.id).unwrap();
    let result: serde_json::Value = serde_json::from_str(call.result.as_deref().unwrap()).unwrap();
    assert_eq!(result["decision"], "rejected");
    assert_eq!(result["feedback"], "Split step 2 into its own slice.");
    assert!(matches!(
        call.result_preview.as_ref(),
        Some(crate::ToolResultPreview::PlanDecision {
            accepted: false,
            feedback: Some(feedback),
            ..
        }) if feedback == "Split step 2 into its own slice."
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
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
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
    // The call resolved before the turn was cancelled, and both are announced
    // in that order. Without the completion the renderer would keep showing the
    // native call running underneath a cancelled turn.
    let events = store.list_events(claimed_chat.id, 0).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0].event,
        AgentEvent::ToolCallCompleted { call_id, .. } if call_id == claimed_call.id
    ));
    assert!(matches!(events[1].event, AgentEvent::TurnCancelled { .. }));
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
        reasoning: Default::default(),
        content: "final answer".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "recovered".into(),
        llm_content: None,
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

/// CI-observed wedge: a worker that loses the failure-resolution race to the
/// lease scanner retries its exact request after the wall clock has passed its
/// requested retry time. That caller must learn the lease is no longer its to
/// resolve (`Ok(None)`), not receive the future-retry invariant error forever.
#[tokio::test]
async fn stale_lease_failure_with_passed_retry_time_reports_the_lost_race() {
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
    set_turn_max_attempts(&store, turn_id, 2).await;
    let claimed_at = accepted.available_at + chrono::Duration::seconds(1);
    let worker_token = uuid::Uuid::new_v4();
    let lease_expires_at = claimed_at + chrono::Duration::minutes(2);
    store
        .claim_turn_run(worker_token, claimed_at, lease_expires_at)
        .await
        .unwrap()
        .turn
        .unwrap();

    // The scanner claims past the worker's lease, which retries the turn on a
    // scan lease, then expires that lease too, terminalizing the turn out from
    // under the worker.
    let scan_at = lease_expires_at + chrono::Duration::microseconds(1);
    let retried = store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            scan_at,
            scan_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .unwrap();
    let second_scan_at = retried.lease_expires_at.unwrap() + chrono::Duration::microseconds(1);
    let scanned = store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            second_scan_at,
            second_scan_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert!(scanned.terminal_event.is_some());

    // The worker retries its exact failure request; its retry time is now in
    // the past relative to both the request and the database clock.
    assert_eq!(
        store
            .record_turn_run_failure(
                turn_id,
                worker_token,
                second_scan_at + chrono::Duration::seconds(1),
                TurnFailureRetry::RetryAt(claimed_at + chrono::Duration::seconds(2)),
                0,
                Usage::default(),
                "provider_unavailable",
                Some("temporary outage"),
            )
            .await
            .unwrap(),
        None
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
        reasoning: Default::default(),
        content: "too late".into(),
        llm_content: None,
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
    let final_model_steps = 2;
    let journaled = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at,
            final_model_steps,
            usage,
            None,
            &[],
        )
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
    assert_eq!(cancelled.model_steps, final_model_steps);
    let terminal = journaled
        .terminal_event
        .expect("worker acknowledgement must append a terminal event");
    assert_eq!(terminal.event, AgentEvent::TurnCancelled { usage });
    let recovered = store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            final_model_steps,
            usage,
            None,
            &[],
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
            final_model_steps + 1,
            usage,
            None,
            &[],
        )
        .await
        .is_err());
    assert!(store
        .finish_turn_cancellation_and_append_event(
            turn.id,
            token,
            acknowledged_at + chrono::Duration::hours(1),
            final_model_steps,
            Usage::default(),
            None,
            &[],
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
    // The stopped turn commits no assistant message, so the transcript carries
    // the terminal turn itself. Without it a reopened conversation cannot tell
    // a response that was stopped from one that finished.
    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    assert_eq!(
        transcript.terminal_turns,
        vec![crate::storage::ChatTerminalTurnSnapshot {
            turn_id: turn.id,
            message_id: None,
            status: crate::storage::ChatTerminalTurnStatus::Cancelled,
            partial_content: String::new(),
            reasoning: String::new(),
            refusal: None,
            failure_kind: None,
            failure_detail: None,
            model: "gpt-5".into(),
            invoked_skills: Vec::new(),
            usage,
            voice_input_used: false,
            finished_at: acknowledged_at,
        }]
    );
}

#[tokio::test]
async fn cancellation_acknowledgement_accepts_the_request_callers_clock() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn = match store
        .accept_turn(TurnId::new(), chat.id, "gpt-5", "cancel immediately")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let token = uuid::Uuid::new_v4();
    let claimed_at = Utc::now();
    store
        .claim_turn_run(token, claimed_at, claimed_at + chrono::Duration::minutes(1))
        .await
        .unwrap()
        .turn
        .expect("the queued turn is claimable");

    // Cancellation stamps its transition from the authoritative database
    // clock. That clock may be later than the caller's timestamp even though
    // both operations happen in the same application tick (SQLite explicitly
    // rounds it to the end of the current millisecond). The exact worker must
    // still be able to acknowledge with the timestamp it already captured.
    let caller_now = Utc::now();
    assert!(matches!(
        store
            .request_turn_cancellation(turn.id, caller_now)
            .await
            .unwrap(),
        Some(RequestTurnCancellationOutcome::Requested(_))
    ));
    assert!(matches!(
        store
            .finish_turn_cancellation(turn.id, token, caller_now)
            .await
            .unwrap(),
        Some(FinishTurnCancellationOutcome::Cancelled(_))
    ));
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
            0,
            Usage::default(),
            None,
            &[],
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
        reasoning: Default::default(),
        content: "race answer".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "race winner".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "prepared before retry".into(),
        llm_content: None,
        created_at: heartbeat_at - chrono::Duration::microseconds(1),
    };
    let future_output = Message {
        id: MessageId::new(),
        content: "future output".into(),
        llm_content: None,
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
        llm_content: None,
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
        reasoning: Default::default(),
        content: "prepared while live".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "stale answer".into(),
        llm_content: None,
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
        llm_content: None,
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
        reasoning: Default::default(),
        content: "one answer".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::seconds(1),
    };
    let competing_output = Message {
        id: MessageId::new(),
        content: "different answer".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "must roll back".into(),
        llm_content: None,
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
        reasoning: Default::default(),
        content: "must roll back".into(),
        llm_content: None,
        created_at: claimed_at + chrono::Duration::seconds(1),
    };

    assert!(store
        .complete_turn_run_and_append_event(
            turn_id,
            token,
            0,
            output.created_at,
            &output,
            0,
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
        reasoning: Default::default(),
        content: format!("t{ts}"),
        llm_content: None,
        created_at: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
    };
    let (m1, m2) = (msg(20), msg(10));
    store.append_message(&m1).await.unwrap();
    store.append_message(&m2).await.unwrap();
    let listed = store.list_messages(newer.id).await.unwrap();
    assert_eq!(listed, vec![m1, m2]);
}

/// A plan write is a live-turn journal write, so it takes the same lease fence
/// as every other one: a stalled attempt that lost its turn must not overwrite
/// the plan the current attempt is being judged by, and one call must record
/// its plan once however many times the loop replays it.
#[tokio::test]
async fn task_plan_writes_are_fenced_on_the_turn_lease_and_recorded_once() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "gpt-5", "plan the work")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected turn acceptance: {outcome:?}"),
    };
    let claim_at = accepted.available_at;
    let stale_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            stale_lease,
            claim_at,
            claim_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    let call = |provider_id: &str| ToolCallRecord {
        id: CallId::new(),
        chat_id: chat.id,
        turn_id,
        provider_id: provider_id.into(),
        name: crate::UPDATE_TASK_PLAN_TOOL.into(),
        arguments: serde_json::json!({"steps": []}),
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
        created_at: claim_at,
        resolved_at: None,
    };
    // Both calls are admitted while the first attempt still owns the turn.
    let first = call("tu_plan_1");
    let stalled = call("tu_plan_2");
    for record in [&first, &stalled] {
        assert!(matches!(
            store
                .accept_claimed_tool_call(record, stale_lease, claim_at)
                .await
                .unwrap(),
            AcceptClaimedToolCallOutcome::Accepted(_)
        ));
    }
    let step = |content: &str| crate::TaskPlanStep {
        content: content.into(),
        status: crate::TaskPlanStepStatus::InProgress,
    };

    let recorded = store
        .update_task_plan(chat.id, first.id, &[step("draft the change")], Utc::now())
        .await
        .unwrap()
        .expect("the owning attempt records its plan");
    assert_eq!(recorded.turn_id, turn_id);
    // The row and the projection agree on when it committed; the row's value
    // is clamped to what the database can hold and the caller sees that one.
    assert_eq!(
        store.get_task_plan(chat.id).await.unwrap(),
        Some(recorded.clone())
    );
    let hints = |events: Vec<crate::SequencedEvent>| {
        events
            .into_iter()
            .filter(|event| matches!(event.event, AgentEvent::TaskPlanUpdated { .. }))
            .count()
    };
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);

    // The loop re-executing a row it already admitted must not journal a
    // second hint for the same call.
    assert_eq!(
        store
            .update_task_plan(chat.id, first.id, &[step("draft the change")], Utc::now())
            .await
            .unwrap(),
        Some(recorded.clone())
    );
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);

    // The turn is reclaimed by a later attempt. The first attempt is still
    // running somewhere with an admitted call in hand.
    let retry_at = claim_at + chrono::Duration::seconds(2);
    store
        .claim_turn_run(
            uuid::Uuid::new_v4(),
            retry_at,
            retry_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .update_task_plan(chat.id, stalled.id, &[step("stale intent")], Utc::now())
            .await
            .unwrap(),
        None,
        "an attempt that lost its lease must not replace the live plan"
    );
    assert_eq!(store.get_task_plan(chat.id).await.unwrap(), Some(recorded));
    assert_eq!(hints(store.list_events(chat.id, 0).await.unwrap()), 1);
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
        reasoning: Default::default(),
        content: "delete this history".into(),
        llm_content: None,
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
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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
    // A recorded task plan restricts against the chat, its turn, and the call
    // that wrote it, so deletion has to erase it explicitly. Before it did,
    // any chat that ever made a plan became undeletable.
    let planning_call = ToolCallRecord {
        id: CallId::new(),
        chat_id: active.id,
        turn_id: active_turn_id,
        provider_id: "tu_plan".into(),
        name: crate::UPDATE_TASK_PLAN_TOOL.into(),
        arguments: serde_json::json!({"steps": []}),
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
        created_at: Utc::now(),
        resolved_at: None,
    };
    store.accept_tool_call(&planning_call).await.unwrap();
    store
        .update_task_plan(
            active.id,
            planning_call.id,
            &[crate::TaskPlanStep {
                content: "ship it".into(),
                status: crate::TaskPlanStepStatus::InProgress,
            }],
            Utc::now(),
        )
        .await
        .unwrap()
        .expect("an unclaimed turn has no lease to fence against");
    assert!(matches!(
        store.delete_chat(active.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(store.get_task_plan(active.id).await.unwrap(), None);

    // A plan request restricts against the chat, its turn, the proposing call,
    // and the journal row holding its renderer hint. Before deletion erased it,
    // any chat that ever left plan mode was undeletable.
    let planned = Chat {
        permission_mode: Some(crate::PermissionMode::Plan),
        ..sample_chat()
    };
    store.create_chat(&planned).await.unwrap();
    let (planned_turn_id, plan_call, parked_at) = park_test_plan(&store, planned.id).await;
    let decided_at = parked_at + chrono::Duration::seconds(1);
    assert!(matches!(
        store
            .decide_plan(
                &crate::DecidePlanRequest {
                    chat_id: planned.id,
                    call_id: plan_call.id,
                    decision: crate::PlanDecision {
                        decision: crate::PlanDecisionChoice::Accept,
                        feedback: None,
                        permission_mode: None,
                    },
                },
                decided_at,
            )
            .await
            .unwrap(),
        crate::storage::DecidePlanOutcome::Decided { .. }
    ));
    let planned_turn = store.get_turn_run(planned_turn_id).await.unwrap().unwrap();
    store
        .request_turn_cancellation_and_append_event(
            planned_turn_id,
            planned_turn.updated_at + chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    assert!(matches!(
        store.delete_chat(planned.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert!(store
        .list_pending_plan_approvals(planned.id)
        .await
        .unwrap()
        .is_empty());

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

    // The store still refuses an unknown broker observation, even though the
    // desktop no longer records one: a rejected mutation now settles on the
    // state the broker reports. This keeps the gate honest for a row written by
    // an older build, which nothing will ever re-drive.
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
    owned.source_blob = Some(DocumentBlob::from_bytes(b"owned source bytes"));
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
    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
    assert_eq!(store.get_chat(chat.id).await.unwrap(), None);
    assert_eq!(store.get_document(owned_id).await.unwrap(), None);
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
            reasoning: Default::default(),
            content: content.into(),
            llm_content: None,
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
                0,
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
    assert_eq!(transcript.terminal_turns.len(), 2);
    for (output, refusal) in expected {
        let stored = transcript
            .messages
            .iter()
            .find(|message| message.id == output.id)
            .expect("refused output remains a normal durable assistant message");
        assert_eq!(stored.content, output.content);
        assert!(transcript.terminal_turns.iter().any(|snapshot| {
            snapshot.message_id == Some(output.id)
                && snapshot.status == crate::storage::ChatTerminalTurnStatus::Completed
                && snapshot.refusal.as_ref() == Some(&refusal)
        }));
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
                reasoning: Default::default(),
                content: String::new(),
                llm_content: None,
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
                raw_arguments: None,
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
            .resolve_claimed_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                uuid::Uuid::new_v4(),
                first_claim_at,
                &resolution,
                first_claim_at,
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
            .resolve_claimed_server_tool_call(
                call.id,
                chat.id,
                turn_id,
                first_lease,
                retry_at,
                &resolution,
                retry_at,
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
        reasoning: Default::default(),
        content: "intermediate".into(),
        llm_content: None,
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
        llm_content: None,
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
        created_at: claimed_at,
        resolved_at: None,
    };
    assert!(matches!(
        store.accept_tool_call(&call).await.unwrap(),
        AcceptToolCallOutcome::Accepted(_)
    ));
    let request = ApprovalRequest {
        auto_judge: false,
        call_id: call.id,
        chat_id: chat.id,
        turn_id,
        tool_name: call.name.clone(),
        class: ApprovalClass::Sensitive,
        kind: crate::ToolApprovalKind::for_tool_name(&call.name),
        preview: None,
    };
    (turn_id, lease_token, call, request)
}

#[tokio::test]
async fn workspace_approval_folds_storage_but_recovers_class_and_kind() {
    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let (_turn_id, _lease_token, call, mut request) = claimed_sensitive_call_with(
        &store,
        &chat,
        "write_file",
        serde_json::json!({"path": "notes.md", "content": "x"}),
    )
    .await;
    request.class = ApprovalClass::Workspace;
    request.kind = crate::ToolApprovalKind::WorkspaceMayModifyFiles;
    store
        .request_tool_call_approval(&request, Utc::now())
        .await
        .unwrap();

    // The stored row keeps the legacy spellings the column constraints allow…
    let row = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&store.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.approval_class.as_deref(), Some("sensitive"));
    assert_eq!(row.approval_kind.as_deref(), Some("unsupported"));

    // …and the read model recovers the real class and kind from the tool
    // name, so a workspace card parked across a restart stays approvable.
    let approval = store
        .get_tool_call_approval(call.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(approval.class, ApprovalClass::Workspace);
    assert_eq!(
        approval.kind,
        crate::ToolApprovalKind::WorkspaceMayModifyFiles
    );
    assert!(approval.kind.is_approvable());
    // Grantable about a place, and nothing wider — the whole-tool rung is the
    // chat's Auto mode, not a standing grant.
    assert!(approval.kind.grantable_at(&crate::GrantScope::PathSubtree {
        prefix: "notes.md".into()
    }));
    assert!(!approval.kind.grantable_at(&crate::GrantScope::WholeTool));
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
            files: Vec::new(),
            summary: None,
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

    assert!(matches!(
        store.delete_chat(chat.id).await.unwrap(),
        DeleteChatOutcome::Deleted { .. }
    ));
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
                .resolve_server_tool_call(call_id, &resolution, Utc::now())
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
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
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
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
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
    let recovered_expiry = after_expiry + chrono::Duration::minutes(1);
    assert!(matches!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                first,
                lease_token,
                after_expiry,
                recovered_expiry,
            )
            .await
            .unwrap(),
        ClaimClientToolCallOutcome::Existing(claim)
            if claim.lease_token == lease_token
                && claim.call.client_executor_id == Some(first)
                && claim.call.client_lease_expires_at == Some(recovered_expiry)
    ));
    let after_recovered_expiry = recovered_expiry + chrono::Duration::seconds(1);
    assert_eq!(
        store
            .claim_client_tool_call(
                call.id,
                chat.id,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                after_recovered_expiry,
                after_recovered_expiry + chrono::Duration::minutes(1),
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
                after_recovered_expiry,
                &ToolCallResolution::Cancelled {
                    result: "cancelled".into(),
                },
                after_recovered_expiry,
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
                after_recovered_expiry,
                &recovered,
                after_recovered_expiry,
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
                after_recovered_expiry,
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
        raw_arguments: None,
        execution: ToolCallExecution::Client,
        status: ToolCallStatus::Pending,
        result: None,
        result_preview: None,

        provider_replay: None,
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

/// Reasoning is journaled as deltas and has no column of its own, so the
/// transcript rebuilds it by matching the payload's variant tag in SQL. That
/// makes the serialized tag a persisted shape rather than an implementation
/// detail: rename the variant and every historical chat silently loses its
/// reasoning while nothing fails to compile.
#[tokio::test]
async fn reasoning_deltas_rebuild_into_the_transcript() {
    use crate::event::AgentEvent;
    use crate::provider::{StopReason, Usage};

    let (_dir, store) = temp_store().await;
    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "question")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
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
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "weighing ".into(),
        },
        AgentEvent::TextDelta {
            text: "not reasoning".into(),
        },
        AgentEvent::ReasoningDelta {
            text: "two approaches".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                chat.id,
                turn_id,
                lease_token,
                i32::try_from(ordinal).unwrap() + 1,
                accepted.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its own deltas");
    }
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "the answer".into(),
        llm_content: None,
        created_at: accepted.available_at,
    };
    store
        .complete_turn_run_and_append_event(
            turn_id,
            lease_token,
            0,
            output.created_at,
            &output,
            0,
            Usage::default(),
            StopReason::EndTurn,
        )
        .await
        .unwrap()
        .expect("live completion");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let terminal = transcript
        .terminal_turns
        .first()
        .expect("completed turn remains in the transcript");
    assert_eq!(
        (
            terminal.turn_id,
            terminal.message_id,
            &terminal.status,
            terminal.partial_content.as_str(),
            terminal.reasoning.as_str(),
            terminal.refusal.as_ref(),
            terminal.failure_kind.as_ref(),
        ),
        (
            turn_id,
            Some(output.id),
            &crate::storage::ChatTerminalTurnStatus::Completed,
            "",
            "weighing two approaches",
            None,
            None,
        ),
        "one turn's deltas rebuild beside the message it produced"
    );
    assert!(terminal.finished_at >= output.created_at);
}

/// Regression for #1220: failed and cancelled turns have no assistant message,
/// but their visible stream is still durable journal data.
#[tokio::test]
async fn message_less_terminal_turns_rebuild_partial_text_and_reasoning() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;

    let (_dir, store) = temp_store().await;

    let cancelled_chat = sample_chat();
    store.create_chat(&cancelled_chat).await.unwrap();
    let cancelled_id = TurnId::new();
    let cancelled = match store
        .accept_turn(cancelled_id, cancelled_chat.id, "claude", "stop this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let cancelled_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            cancelled_lease,
            cancelled.available_at,
            cancelled.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "considering cancellation".into(),
        },
        AgentEvent::TextDelta {
            text: "partial cancelled answer".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                cancelled_chat.id,
                cancelled_id,
                cancelled_lease,
                i32::try_from(ordinal).unwrap() + 1,
                cancelled.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its visible stream");
    }
    let cancellation_requested_at = cancelled.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(cancelled_id, cancellation_requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            cancelled_id,
            cancelled_lease,
            cancellation_requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation");

    let failed_chat = sample_chat();
    store.create_chat(&failed_chat).await.unwrap();
    let failed_id = TurnId::new();
    let failed = match store
        .accept_turn(failed_id, failed_chat.id, "claude", "fail this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let failed_lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            failed_lease,
            failed.available_at,
            failed.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    for (ordinal, event) in [
        AgentEvent::ReasoningDelta {
            text: "considering failure".into(),
        },
        AgentEvent::TextDelta {
            text: "partial failed answer".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        store
            .append_turn_event(
                failed_chat.id,
                failed_id,
                failed_lease,
                i32::try_from(ordinal).unwrap() + 1,
                failed.available_at,
                &event,
            )
            .await
            .unwrap()
            .expect("a live attempt may journal its visible stream");
    }
    store
        .record_turn_run_failure_and_append_event(
            failed_id,
            failed_lease,
            failed.available_at + chrono::Duration::seconds(1),
            TurnFailureRetry::Permanent,
            0,
            Usage::default(),
            "provider",
            Some("internal detail must not cross the renderer boundary"),
        )
        .await
        .unwrap()
        .expect("live failure terminalizes");

    let cancelled_snapshot = store
        .get_chat_transcript(cancelled_chat.id)
        .await
        .unwrap()
        .unwrap()
        .terminal_turns
        .pop()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            cancelled_snapshot.status,
            cancelled_snapshot.message_id,
            cancelled_snapshot.partial_content,
            cancelled_snapshot.reasoning,
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            None,
            "partial cancelled answer".into(),
            "considering cancellation".into(),
        )
    );

    let failed_snapshot = store
        .get_chat_transcript(failed_chat.id)
        .await
        .unwrap()
        .unwrap()
        .terminal_turns
        .pop()
        .expect("failed turn remains in the transcript");
    assert_eq!(
        (
            failed_snapshot.status,
            failed_snapshot.message_id,
            failed_snapshot.partial_content,
            failed_snapshot.reasoning,
            failed_snapshot.failure_kind,
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Failed,
            None,
            "partial failed answer".into(),
            "considering failure".into(),
            Some("provider".into()),
        )
    );
}

/// A cancellation acknowledged with partial output commits it as the turn's
/// durable assistant message in the same transition (#1182): the transcript
/// serves the prose from the message row rather than rebuilding journal
/// deltas, and the next turn's context (built from message rows) includes it.
#[tokio::test]
async fn cancellation_with_partial_output_commits_a_durable_message() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;
    use crate::{AssistantCitationInput, CitationLocator};

    let (_dir, store) = temp_store().await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "stop this")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    store
        .append_turn_event(
            chat.id,
            turn_id,
            lease,
            1,
            accepted.available_at,
            &AgentEvent::TextDelta {
                text: "the answer so far".into(),
            },
        )
        .await
        .unwrap()
        .expect("a live attempt may journal its visible stream");
    let requested_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(turn_id, requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");

    let document_id = DocumentId::new();
    store
        .upsert_document(&DocumentUpsert {
            id: document_id,
            project_id: None,
            chat_id: Some(chat.id),
            origin_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "the cited answer".into(),
            updated_at: requested_at,
        })
        .await
        .unwrap();
    let citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 1 },
    };
    let output = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: crate::format_citation_directive(
            "the answer so far",
            document_id,
            &citation.locator,
        ),
        llm_content: None,
        created_at: requested_at,
    };
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            Some(&output),
            std::slice::from_ref(&citation),
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation with output");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let snapshot = transcript
        .terminal_turns
        .last()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            snapshot.status.clone(),
            snapshot.message_id,
            snapshot.partial_content.as_str(),
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            Some(output.id),
            "",
        ),
        "the committed message, not the journal rebuild, carries the prose"
    );
    let committed = transcript
        .messages
        .iter()
        .find(|message| message.id == output.id)
        .expect("partial output is a durable message");
    assert_eq!(
        (committed.role, committed.content.as_str()),
        (Role::Assistant, output.content.as_str())
    );

    // An exact retry of the same acknowledgement stays idempotent.
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(2),
            0,
            Usage::default(),
            Some(&output),
            std::slice::from_ref(&citation),
        )
        .await
        .unwrap()
        .expect("exact cancellation retry recovers");
    assert_eq!(
        store
            .list_messages(chat.id)
            .await
            .unwrap()
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .count(),
        1,
        "a retried acknowledgement must not duplicate the output message"
    );

    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(3),
                0,
                Usage::default(),
                None,
                &[],
            )
            .await
            .is_err(),
        "a retry may not drop the committed partial output"
    );

    let changed_id = Message {
        id: MessageId::new(),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(4),
                0,
                Usage::default(),
                Some(&changed_id),
                std::slice::from_ref(&citation),
            )
            .await
            .is_err(),
        "a retry may not substitute another output message identity"
    );

    let changed_content = Message {
        content: crate::format_citation_directive(
            "a different answer",
            document_id,
            &citation.locator,
        ),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(5),
                0,
                Usage::default(),
                Some(&changed_content),
                std::slice::from_ref(&citation),
            )
            .await
            .is_err(),
        "a retry may not change the committed partial output content"
    );

    let changed_citation = AssistantCitationInput {
        document_id,
        locator: CitationLocator::Lines { start: 1, end: 2 },
    };
    let changed_citations = Message {
        content: crate::format_citation_directive(
            "the answer so far",
            document_id,
            &changed_citation.locator,
        ),
        ..output.clone()
    };
    assert!(
        store
            .finish_turn_cancellation_and_append_event(
                turn_id,
                lease,
                requested_at + chrono::Duration::seconds(6),
                0,
                Usage::default(),
                Some(&changed_citations),
                std::slice::from_ref(&changed_citation),
            )
            .await
            .is_err(),
        "a retry may not change the committed partial-output citations"
    );
}

/// Regression for #1714: cancelling during a tool call can leave a committed
/// assistant step even though the turn itself has no output message. Associate
/// the cancellation with the last committed step so its journaled prose is not
/// rendered again as a message-less terminal partial.
#[tokio::test]
async fn cancellation_after_committed_step_does_not_rebuild_its_prose() {
    use crate::event::AgentEvent;
    use crate::provider::Usage;

    let (_dir, store) = temp_store().await;

    let chat = sample_chat();
    store.create_chat(&chat).await.unwrap();
    let turn_id = TurnId::new();
    let accepted = match store
        .accept_turn(turn_id, chat.id, "claude", "start then stop")
        .await
        .unwrap()
    {
        AcceptTurnOutcome::Accepted(turn) => turn,
        outcome => panic!("unexpected acceptance outcome: {outcome:?}"),
    };
    let lease = uuid::Uuid::new_v4();
    store
        .claim_turn_run(
            lease,
            accepted.available_at,
            accepted.available_at + chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
        .turn
        .expect("accepted turn is claimable");
    store
        .append_turn_event(
            chat.id,
            turn_id,
            lease,
            1,
            accepted.available_at,
            &AgentEvent::TextDelta {
                text: "I will check that now.".into(),
            },
        )
        .await
        .unwrap()
        .expect("a live attempt may journal its visible stream");

    let committed = Message {
        id: MessageId::new(),
        chat_id: chat.id,
        turn_id,
        role: Role::Assistant,
        reasoning: Default::default(),
        content: "I will check that now.".into(),
        llm_content: None,
        created_at: accepted.available_at,
    };
    assert_eq!(
        store
            .append_claimed_assistant_message_with_citations(
                &committed,
                &[],
                lease,
                accepted.available_at,
            )
            .await
            .unwrap(),
        AppendClaimedMessageOutcome::Appended
    );

    let requested_at = accepted.available_at + chrono::Duration::seconds(1);
    store
        .request_turn_cancellation(turn_id, requested_at)
        .await
        .unwrap()
        .expect("running cancellation is accepted");
    store
        .finish_turn_cancellation_and_append_event(
            turn_id,
            lease,
            requested_at + chrono::Duration::seconds(1),
            0,
            Usage::default(),
            None,
            &[],
        )
        .await
        .unwrap()
        .expect("worker acknowledges cancellation during the tool call");

    let transcript = store.get_chat_transcript(chat.id).await.unwrap().unwrap();
    let snapshot = transcript
        .terminal_turns
        .last()
        .expect("cancelled turn remains in the transcript");
    assert_eq!(
        (
            snapshot.status.clone(),
            snapshot.message_id,
            snapshot.partial_content.as_str(),
        ),
        (
            crate::storage::ChatTerminalTurnStatus::Cancelled,
            Some(committed.id),
            "",
        ),
        "the committed step owns the cancellation instead of being duplicated from journal deltas"
    );
}

/// The #853 scoping contract: two principals' root aggregates are disjoint
/// under the owner-scoped surface — reads, lists, mutations, and creation
/// against another owner's parent all behave as if the other owner's rows do
/// not exist — while the unscoped surface still sees everything and keeps
/// attributing new rows to the local owner.
#[tokio::test]
async fn owner_scoped_queries_partition_root_aggregates() {
    let (_dir, store) = temp_store().await;
    let alice = OwnerId::new("user:alice").unwrap();
    let bob = OwnerId::new("user:bob").unwrap();
    let local = OwnerId::local();

    let project = sample_project();
    store.create_project_scoped(&alice, &project).await.unwrap();
    let mut chat = sample_chat();
    chat.project_id = Some(project.id);
    store.create_chat_scoped(&alice, &chat).await.unwrap();

    // Chats: partitioned reads, lists, and mutations.
    assert_eq!(
        store
            .get_chat_scoped(&alice, chat.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&chat)
    );
    assert_eq!(store.get_chat_scoped(&bob, chat.id).await.unwrap(), None);
    assert_eq!(
        store.list_chats_scoped(&alice).await.unwrap(),
        vec![chat.clone()]
    );
    assert_eq!(store.list_chats_scoped(&bob).await.unwrap(), Vec::new());
    assert_eq!(
        store.delete_chat_scoped(&bob, chat.id).await.unwrap(),
        DeleteChatOutcome::NotFound
    );
    assert!(!store
        .update_chat_metadata_scoped(
            &bob,
            chat.id,
            Some(Some("stolen".into())),
            None,
            None,
            None,
            None
        )
        .await
        .unwrap());
    assert!(store
        .get_chat_transcript_scoped(&bob, chat.id)
        .await
        .unwrap()
        .is_none());
    assert!(store
        .get_chat_transcript_scoped(&alice, chat.id)
        .await
        .unwrap()
        .is_some());
    // The failed cross-owner mutation left the row untouched.
    assert_eq!(
        store
            .get_chat_scoped(&alice, chat.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&chat)
    );

    // Projects: partitioned, and unusable as another owner's chat parent.
    assert_eq!(
        store.get_project_scoped(&bob, project.id).await.unwrap(),
        None
    );
    assert_eq!(store.list_projects_scoped(&bob).await.unwrap(), Vec::new());
    assert!(!store
        .update_project_title_scoped(&bob, project.id, Some("stolen".into()))
        .await
        .unwrap());
    assert_eq!(
        store.delete_project_scoped(&bob, project.id).await.unwrap(),
        DeleteProjectOutcome::NotFound
    );
    let mut cross_owner_chat = sample_chat();
    cross_owner_chat.project_id = Some(project.id);
    assert!(store
        .create_chat_scoped(&bob, &cross_owner_chat)
        .await
        .is_err());
    assert!(store
        .create_chat_with_project_defaults_scoped(&bob, &cross_owner_chat)
        .await
        .is_err());

    // Documents: inherit the parent's owner and partition with it.
    let source = DocumentSourceUpsert {
        id: DocumentId::new(),
        chat_id: Some(chat.id),
        project_id: None,
        origin_uri: None,
        media_type: "text/plain".into(),
        title: None,
        source_blob: DocumentBlob::from_bytes(b"alice's notes"),
        canonical_text: "alice's notes".into(),
        updated_at: Utc::now(),
    };
    let document = store
        .accept_document_source_scoped(&alice, &source)
        .await
        .unwrap();
    assert!(store
        .get_document_scoped(&alice, document.id)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        store.get_document_scoped(&bob, document.id).await.unwrap(),
        None
    );
    assert_eq!(
        store
            .list_document_summaries_scoped(&bob, DocumentScope::Chat(chat.id), None, 10)
            .await
            .unwrap(),
        Vec::new()
    );
    assert_eq!(
        store
            .list_document_summaries_scoped(&alice, DocumentScope::Chat(chat.id), None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    // A cross-owner accept against alice's chat is indistinguishable from a
    // missing parent; a cross-owner delete leaves the row in place.
    assert!(store
        .accept_document_source_scoped(&bob, &source)
        .await
        .is_err());
    store
        .delete_document_scoped(&bob, document.id)
        .await
        .unwrap();
    assert!(store
        .get_document_scoped(&alice, document.id)
        .await
        .unwrap()
        .is_some());

    // The unscoped surface still sees everything, and unscoped creation
    // attributes to the local owner.
    assert_eq!(store.list_chats().await.unwrap().len(), 1);
    let loose = sample_chat();
    store.create_chat(&loose).await.unwrap();
    assert_eq!(
        store
            .get_chat_scoped(&local, loose.id)
            .await
            .unwrap()
            .as_ref(),
        Some(&loose)
    );
    assert_eq!(store.get_chat_scoped(&alice, loose.id).await.unwrap(), None);
}

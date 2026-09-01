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
mod approval;
mod blob_retirement;
mod chat;
mod client_wait;
mod code;
mod connected_app;
mod context_checkpoint;
mod delegated_file_read;
mod document;
mod event_journal;
mod file_change_journal;
mod message_attachment;
mod multi_agent_wait;
mod notification;
mod operation_log;
mod output;
mod parent_terminal_guard;
mod project;
mod root_attachment;
mod sandbox_spawn_checkpoint;
mod store;
mod task_plan;
mod tool_call;
mod transcript;
mod turn_admission;
mod turn_cancellation;
mod turn_claim;
mod turn_completion;
mod turn_event_batch;
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

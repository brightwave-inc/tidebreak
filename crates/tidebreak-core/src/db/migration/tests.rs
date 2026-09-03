use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::prelude::{PostgresQueryBuilder, SchemaManager, SqliteQueryBuilder};
use sea_orm_migration::MigratorTrait;

#[cfg(feature = "sqlite")]
use super::rebuild_sqlite_code_workspace_for_archiving_inner;
use super::Migrator;

/// Every `CREATE TABLE` a fresh database runs comes from `sqlite_master`,
/// which stores the statement verbatim and appends what `ALTER TABLE`
/// adds. Comparing two databases through it therefore sees columns a
/// migration added, not only tables it created.
async fn schema_of(db: &sea_orm::DatabaseConnection) -> String {
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL \
             ORDER BY type, name"
                .to_owned(),
        ))
        .await
        .unwrap();
    rows.iter()
        .map(|row| row.try_get::<String>("", "sql").unwrap())
        .collect::<Vec<_>>()
        .join(";\n")
}

#[tokio::test]
async fn a_fresh_database_records_the_whole_chain() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, None).await.unwrap();

    let versions = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT version FROM seaql_migrations ORDER BY version".to_owned(),
        ))
        .await
        .unwrap();
    let versions: Vec<String> = versions
        .iter()
        .map(|row| row.try_get::<String>("", "version").unwrap())
        .collect();
    assert_eq!(
        versions,
        [
            "m20260814_000001_baseline",
            "m20260814_000002_app_owner",
            "m20260814_000003_code_owner",
            "m20260822_000004_baseline_repair",
            "m20260822_000005_code_session_fast_mode",
            "m20260822_000006_code_session_images",
            "m20260822_000007_trigger_fire_outbox",
            "m20260822_000008_trigger_delivery_receipts",
            "m20260822_000009_code_pull_request_facts",
            "m20260823_000010_trigger_fact_conditions",
            "m20260824_000011_code_queued_turns",
            "m20260824_000012_code_pull_request_live_tier",
            "m20260824_000013_code_pull_request_etags",
            "m20260824_000014_code_turn_model_snapshot",
            "m20260825_000015_pre_pin_code_lifecycle_repair",
            "m20260825_000016_code_approval_binding",
            "m20260826_000017_code_session_process_identity",
            "m20260826_000018_code_workspace_archiving",
            "m20260826_000019_code_permission_mode_intent",
            "m20260826_000020_agent_notification",
            "m20260827_000021_code_workflow_runs",
            "m20260827_000022_code_session_incarnations",
            "m20260827_000023_incarnation_ingest",
            "m20260828_000024_code_external_bindings",
            "m20260828_000025_code_external_events",
            "m20260828_000026_code_turn_rewrite",
            "m20260828_000027_code_external_grants",
            "m20260828_000028_code_connect_handshakes",
            "m20260901_000001_code_turn_park",
            "m20260901_000002_internal_engine_sessions",
            "m20260902_000001_conversations_are_sessions",
            "m20260902_000002_one_journal",
            "m20260902_000003_one_approval_surface",
            "m20260902_000004_memory_records",
            "m20260902_000005_memory_sweep_state",
            "m20260902_000006_chat_memory_incognito",
            "m20260903_000001_one_turn_lane",
            "m20260903_000002_ready_agent_run_wait_index",
            "m20260903_000003_client_wait_vendor_web_search",
            "m20260903_000004_restore_turn_claim_indexes",
            "m20260903_000005_pending_prompt_indexes",
        ]
    );
    assert!(db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT owner FROM app LIMIT 1".to_owned(),
        ))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn turn_claim_indexes_reach_existing_sqlite_databases() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260903_000004_restore_turn_claim_indexes")),
    )
    .await
    .unwrap();

    for index in ["idx_code_turn_due", "idx_code_turn_stale_lease"] {
        let missing = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "SELECT 1 AS present FROM sqlite_master \
                     WHERE type = 'index' AND name = '{index}'"
                ),
            ))
            .await
            .unwrap();
        assert!(
            missing.is_none(),
            "the pre-repair schema already has {index}"
        );
    }

    Migrator::up(&db, None).await.unwrap();

    for (index, query) in [
        (
            "idx_code_turn_due",
            "SELECT id FROM code_turn \
             WHERE status = 'queued' AND available_at <= '2026-09-03T00:00:00Z' \
             ORDER BY available_at, started_at LIMIT 1",
        ),
        (
            "idx_code_turn_stale_lease",
            "SELECT id FROM code_turn \
             WHERE status = 'running' AND lease_expires_at <= '2026-09-03T00:00:00Z' \
             ORDER BY lease_expires_at, started_at LIMIT 1",
        ),
    ] {
        let plan = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("EXPLAIN QUERY PLAN {query}"),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plan.contains(index),
            "the turn claim scan must use {index}:\n{plan}"
        );
    }
}

#[tokio::test]
async fn pending_prompt_indexes_reach_existing_sqlite_databases() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260903_000005_pending_prompt_indexes")),
    )
    .await
    .unwrap();

    for index in [
        "idx_tool_call_pending_prompt",
        "idx_code_approval_pending_prompt",
    ] {
        let missing = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "SELECT 1 AS present FROM sqlite_master \
                     WHERE type = 'index' AND name = '{index}'"
                ),
            ))
            .await
            .unwrap();
        assert!(
            missing.is_none(),
            "the pre-repair schema already has {index}"
        );
    }

    Migrator::up(&db, None).await.unwrap();

    for (index, query) in [
        (
            "idx_tool_call_pending_prompt",
            "SELECT * FROM tool_call \
             WHERE name = 'ask_user_questions' \
               AND execution = 'orchestration' AND status = 'pending'",
        ),
        (
            "idx_tool_call_pending_prompt",
            "SELECT * FROM tool_call \
             WHERE name = 'request_folder_access' \
               AND execution = 'client' AND status = 'pending' \
             ORDER BY chat_id, history_order",
        ),
        (
            "idx_code_approval_pending_prompt",
            "SELECT * FROM code_approval WHERE state = 'pending' \
             ORDER BY session_id, requested_at, id",
        ),
    ] {
        let plan = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("EXPLAIN QUERY PLAN {query}"),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plan.contains(index),
            "the pending prompt projection must use {index}:\n{plan}"
        );
        assert!(
            !plan.contains("USE TEMP B-TREE"),
            "the pending prompt projection must preserve index order:\n{plan}"
        );
    }
}

#[tokio::test]
async fn connect_handshake_migration_rolls_back_its_ephemeral_table() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    assert!(db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' \
             AND name = 'code_connect_handshake'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .is_some());

    // Every migration above the handshake one, then the handshake one.
    let above = Migrator::migrations().len()
        - Migrator::migrations()
            .iter()
            .position(|migration| migration.name() == "m20260828_000028_code_connect_handshakes")
            .expect("the handshake migration is in the chain");
    Migrator::down(&db, Some(u32::try_from(above).unwrap()))
        .await
        .unwrap();
    assert!(db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' \
             AND name = 'code_connect_handshake'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .is_none());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_failed_workspace_rebuild_rolls_back_the_live_sqlite_table() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, Some(16)).await.unwrap();
    db.execute_unprepared(
        "INSERT INTO code_repo (
            id, owner, root_path, display_name, default_base_ref,
            branch_prefix, quick_actions, created_at
         ) VALUES (
            'repo-archive', 'local', '/tmp/archive', 'archive', 'main',
            'tidebreak/', '[]', '2026-08-26T00:00:00Z'
         );
         INSERT INTO code_workspace (
            id, owner, repo_id, title, worktree_path, branch_name, base_ref,
            status, created_at
         ) VALUES (
            'workspace-archive', 'local', 'repo-archive', 'archive',
            '/tmp/archive-worktree', 'tidebreak/archive', 'main', 'active',
            '2026-08-26T00:00:00Z'
         );
         INSERT INTO code_session (
            id, owner, workspace_id, kind, harness_kind, permission_mode,
            lifecycle, attention_state, attention_source, created_at
         ) VALUES (
            'session-archive', 'local', 'workspace-archive', 'interactive',
            'claude_code', 'plan', 'idle', '{}', 'lifecycle',
            '2026-08-26T00:00:00Z'
         )",
    )
    .await
    .unwrap();

    let manager = SchemaManager::new(&db);
    let error = rebuild_sqlite_code_workspace_for_archiving_inner(&manager, true)
        .await
        .expect_err("the injected statement fails after the live table is dropped");
    assert!(error
        .to_string()
        .contains("missing_workspace_rebuild_table"));

    let workspace = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT status FROM code_workspace WHERE id = 'workspace-archive'".to_owned(),
        ))
        .await
        .unwrap()
        .expect("the original workspace table and row survive");
    assert_eq!(workspace.try_get::<String>("", "status").unwrap(), "active");
    assert!(db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT 1 AS present FROM code_session WHERE id = 'session-archive'".to_owned(),
        ))
        .await
        .unwrap()
        .is_some());
    assert!(db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' \
             AND name = 'code_workspace_archiving'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .is_none());

    rebuild_sqlite_code_workspace_for_archiving_inner(&manager, false)
        .await
        .unwrap();
    db.execute_unprepared(
        "UPDATE code_workspace SET status = 'archiving' WHERE id = 'workspace-archive'",
    )
    .await
    .unwrap();
    assert!(db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await
        .unwrap()
        .is_empty());
    let foreign_keys = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_keys".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(foreign_keys.try_get::<i64>("", "foreign_keys").unwrap(), 1);
}

/// A database that stopped at the current baseline and later took the rest
/// of the chain must land on the schema a fresh database gets in one pass.
///
/// This is the property the chain exists for. The desktop profile can be
/// deleted and rebuilt; the self-host PostgreSQL store cannot, so it only
/// ever sees the appended migrations. If those disagree with the baseline
/// by even a default or a nullability, the two deployments run different
/// schemas against the same queries — and nothing else notices, because
/// each one is internally consistent.
///
/// With this short chain the check is nearly free. It stops being free the
/// first time an appended migration adds a column the baseline declares
/// differently, which is the mistake the folded-in `ensure_*` branches
/// were one edit away from making.
///
/// This checks the internal ordering contract. The versioned release tests
/// cover the older schema that users actually upgrade from, including the
/// backend-specific statements in later migrations.
/// The rows a pre-merge profile carries: a conversation with two
/// messages, a completed turn, its coordinator run, a terminal journal
/// row, and a tool call, plus the engine-private pair slice C kept for
/// one internal session.
/// The chat journal a seeded conversation carries: the shapes the
/// backfill has to carry across, ending on the turn's terminal row.
fn seeded_chat_events() -> Vec<crate::AgentEvent> {
    use crate::AgentEvent;
    vec![
        AgentEvent::TurnStarted {
            turn_id: crate::TurnId(uuid::Uuid::from_u128(0xa004)),
        },
        AgentEvent::TextDelta { text: "hel".into() },
        AgentEvent::TextDelta { text: "lo".into() },
        AgentEvent::ToolCallCompleted {
            call_id: crate::CallId(uuid::Uuid::from_u128(0xa006)),
            output: crate::ToolOutput::text("ok"),
            action: Some(crate::ToolActionPreview::Exec {
                command: "echo".into(),
                args: vec!["hi".into()],
                cwd: ".".into(),
                files: Vec::new(),
                summary: None,
            }),
            result: None,
        },
        AgentEvent::TurnCompleted {
            usage: crate::Usage {
                input_tokens: 3,
                output_tokens: 4,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
            stop_reason: crate::StopReason::EndTurn,
        },
    ]
}

/// `INSERT` statements for [`seeded_chat_events`] into the old `event`
/// table. Ids arrive as SQL literals, in the blob form the live store
/// binds. With a turn, the terminal row names it as the turn's receipt;
/// without one, every row is chat-scoped, the shape a journal has before
/// any turn table exists to name.
fn seeded_event_inserts(chat_id: &str, turn_id: Option<&str>) -> String {
    seeded_chat_events()
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let seq = index + 1;
            let terminal =
                matches!(event, crate::AgentEvent::TurnCompleted { .. }) && turn_id.is_some();
            let turn = match turn_id {
                Some(turn_id) if terminal => turn_id.to_owned(),
                _ => "NULL".to_owned(),
            };
            let terminal = if terminal { "TRUE" } else { "FALSE" };
            let payload = serde_json::to_string(event).unwrap().replace('\'', "''");
            format!(
                "INSERT INTO event (chat_id, seq, turn_id, terminal, payload, created_at) \
                 VALUES ({chat_id}, {seq}, {turn}, {terminal}, '{payload}', \
                 '2026-09-01T00:00:0{seq}Z');"
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chat replay read serves the seeded journal back: every event, in
/// order, with the same content, from the one journal.
async fn assert_chat_replay(db: &sea_orm::DatabaseConnection, chat_id: uuid::Uuid) {
    use crate::storage::Store as _;
    let store = crate::db::DbStore { conn: db.clone() };
    let replayed = store
        .list_events(crate::ChatId(chat_id), 0)
        .await
        .expect("the chat replay reads the one journal");
    assert_eq!(
        replayed.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=seeded_chat_events().len() as i64).collect::<Vec<_>>(),
        "the backfill keeps every sequence number"
    );
    assert_eq!(
        replayed
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        seeded_chat_events(),
        "the backfill keeps every event's content"
    );
}

async fn seed_pre_merge_conversations(db: &sea_orm::DatabaseConnection) {
    let events = seeded_event_inserts(
        "X'0000000000000000000000000000a001'",
        Some("X'0000000000000000000000000000a004'"),
    );
    db.execute_unprepared(&format!(
        r#"
INSERT INTO chat (
id, title, created_at, permission_mode, reasoning_effort, network_policy,
owner, engine_private
) VALUES (
X'0000000000000000000000000000a001', 'kept', '2026-09-01T00:00:00Z',
NULL, 'aggressive', '{{"mode":"open"}}', 'local', FALSE
);
INSERT INTO chat (
id, title, created_at, permission_mode, network_policy, owner, engine_private
) VALUES (
X'0000000000000000000000000000b001', 'private', '2026-09-01T00:00:00Z',
'plan', '{{"mode":"open"}}', 'local', TRUE
);
INSERT INTO code_session (
id, owner, workspace_id, kind, harness_kind, permission_mode, lifecycle,
spawn_epoch, attention_state, attention_source, created_at
) VALUES (
X'0000000000000000000000000000b001', 'local', NULL, 'interactive',
'internal', 'plan', 'idle', 1, '{{"type":"idle"}}', 'lifecycle',
'2026-09-01T00:00:00Z'
);
INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at) VALUES (
X'0000000000000000000000000000a002', X'0000000000000000000000000000a001',
X'0000000000000000000000000000a004', 1, 'user', 'hi', '2026-09-01T00:00:00Z'
);
INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at) VALUES (
X'0000000000000000000000000000a003', X'0000000000000000000000000000a001',
X'0000000000000000000000000000a004', 2, 'assistant', 'hello',
'2026-09-01T00:00:01Z'
);
INSERT INTO agent_run (
id, chat_id, tier, execution_location, depth, status, attempt_count,
max_attempts, claim_count, available_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000a005', X'0000000000000000000000000000a001',
'foreground', 'in_process', 0, 'active', 0, 0, 0, '2026-09-01T00:00:00Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z'
);
INSERT INTO turn_run (
id, chat_id, agent_run_id, input_message_id, output_message_id, model,
invoked_skills, status, attempt_count, max_attempts, claim_count,
available_at, started_at, finished_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000a004', X'0000000000000000000000000000a001',
X'0000000000000000000000000000a005', X'0000000000000000000000000000a002',
X'0000000000000000000000000000a003', 'scripted', '[]', 'completed', 1, 5, 1,
'2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z'
);
{events}
INSERT INTO tool_call (
id, chat_id, turn_id, provider_id, history_order, name, arguments, execution,
status, result, created_at, resolved_at
) VALUES (
X'0000000000000000000000000000a006', X'0000000000000000000000000000a001',
X'0000000000000000000000000000a004', 'scripted', 1, 'exec', '{{}}', 'server',
'completed', 'ok', '2026-09-01T00:00:01Z', '2026-09-01T00:00:01Z'
)"#
    ))
    .await
    .unwrap();
}

async fn count(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    db.query_one_raw(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT count(*) AS n FROM {sql}"),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "n")
    .unwrap()
}

/// What every path into the merged schema must leave behind: no chat
/// table, every seeded row reachable, every foreign key satisfied, the
/// plain chat a session with the conversation columns and the code
/// columns at rest, and the engine-private pair one row that kept its
/// code state.
async fn assert_conversations_merged(db: &sea_orm::DatabaseConnection) {
    assert!(db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        count(db, "sqlite_master WHERE type = 'table' AND name = 'chat'").await,
        0
    );
    assert_eq!(count(db, "code_session").await, 2);
    assert_eq!(
        count(
            db,
            "code_event WHERE session_id = X'0000000000000000000000000000a001'"
        )
        .await,
        seeded_chat_events().len() as i64,
        "the journal moved into code_event"
    );
    assert_chat_replay(db, uuid::Uuid::from_u128(0xa001)).await;
    for (table, expected) in [
        ("message", 2),
        ("agent_run", 1),
        ("code_turn", 1),
        ("tool_call", 1),
    ] {
        let filter = if table == "code_turn" {
            "session_id"
        } else {
            "chat_id"
        };
        assert_eq!(
            count(
                db,
                &format!("{table} WHERE {filter} = X'0000000000000000000000000000a001'")
            )
            .await,
            expected,
            "{table} rows did not survive"
        );
    }
    let plain = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT workspace_id, harness_kind, kind, lifecycle, spawn_epoch, \
             permission_mode, reasoning_effort, attention_state, attention_source, \
             title, network_policy, attachment_revision \
             FROM code_session WHERE id = X'0000000000000000000000000000a001'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the chat became a session");
    assert_eq!(
        plain.try_get::<Option<String>>("", "workspace_id").unwrap(),
        None
    );
    assert_eq!(
        plain.try_get::<String>("", "harness_kind").unwrap(),
        "internal"
    );
    assert_eq!(plain.try_get::<String>("", "kind").unwrap(), "interactive");
    assert_eq!(plain.try_get::<String>("", "lifecycle").unwrap(), "idle");
    assert_eq!(plain.try_get::<i64>("", "spawn_epoch").unwrap(), 0);
    assert_eq!(
        plain
            .try_get::<Option<String>>("", "permission_mode")
            .unwrap(),
        None,
        "an unset chat mode stays unset"
    );
    assert_eq!(
        plain
            .try_get::<Option<String>>("", "reasoning_effort")
            .unwrap(),
        None,
        "a token this build does not recognize is dropped, not fatal"
    );
    assert_eq!(
        plain.try_get::<String>("", "attention_state").unwrap(),
        r#"{"type":"idle"}"#
    );
    assert_eq!(
        plain.try_get::<String>("", "attention_source").unwrap(),
        "lifecycle"
    );
    assert_eq!(plain.try_get::<String>("", "title").unwrap(), "kept");
    assert_eq!(
        plain.try_get::<String>("", "network_policy").unwrap(),
        r#"{"mode":"open"}"#
    );
    assert_eq!(plain.try_get::<i64>("", "attachment_revision").unwrap(), 0);
    let pair = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT spawn_epoch, permission_mode, title \
             FROM code_session WHERE id = X'0000000000000000000000000000b001'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the engine-private pair is one row");
    assert_eq!(pair.try_get::<i64>("", "spawn_epoch").unwrap(), 1);
    assert_eq!(
        pair.try_get::<String>("", "permission_mode").unwrap(),
        "plan"
    );
    assert_eq!(pair.try_get::<String>("", "title").unwrap(), "private");
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();
    assert_eq!(schema_of(db).await, schema_of(&fresh).await);
}

/// A pre-merge SQLite profile keeps every row while the chat table
/// becomes session rows and twenty foreign keys move with it.
#[tokio::test]
async fn a_pre_merge_database_keeps_its_conversations_as_sessions() {
    use sea_orm_migration::MigrationTrait as _;

    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260902_000001_conversations_are_sessions")),
    )
    .await
    .unwrap();
    seed_pre_merge_conversations(&db).await;

    Migrator::up(&db, None).await.unwrap();

    assert_conversations_merged(&db).await;
    // A second pass finds the markers and changes nothing.
    let manager = SchemaManager::new(&db);
    super::ConversationsAreSessions.up(&manager).await.unwrap();
    super::one_journal::OneJournal.up(&manager).await.unwrap();
    assert_conversations_merged(&db).await;
}

/// The SQLite branch runs many autocommit steps. An attempt that added a
/// column and rebuilt one table before dying must finish on the next
/// start: the columns it added are skipped, the table it rebuilt comes
/// back unchanged, and the rest lands.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_interrupted_conversation_merge_finishes_on_retry() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260902_000001_conversations_are_sessions")),
    )
    .await
    .unwrap();
    seed_pre_merge_conversations(&db).await;
    let manager = SchemaManager::new(&db);
    let (column, definition) = super::SQLITE_CONVERSATION_COLUMNS[0];
    db.execute_unprepared(&format!(
        "ALTER TABLE \"code_session\" ADD COLUMN {definition}"
    ))
    .await
    .unwrap();
    assert!(manager.has_column("code_session", column).await.unwrap());
    super::rebuild_sqlite_table(&manager, "message", super::repoint_chat_reference)
        .await
        .unwrap();
    assert!(manager.has_table("chat").await.unwrap());

    Migrator::up(&db, None).await.unwrap();

    assert_conversations_merged(&db).await;
}

/// How many migrations run before the named one.
fn steps_before(name: &str) -> u32 {
    let position = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == name)
        .unwrap_or_else(|| panic!("{name} is in the chain"));
    u32::try_from(position).unwrap()
}

/// A conversation as the merge left it, one migration before the
/// journal moves: a session row, its turn, its `event` rows, and the
/// bridge's translated copy of the same history already in
/// `code_event` (#3010).
async fn seed_pre_journal_conversation(db: &sea_orm::DatabaseConnection) {
    let events = seeded_event_inserts(
        "X'0000000000000000000000000000c001'",
        Some("X'0000000000000000000000000000c004'"),
    );
    db.execute_unprepared(&format!(
        r#"
INSERT INTO code_session (
id, owner, workspace_id, kind, harness_kind, permission_mode, lifecycle,
spawn_epoch, attention_state, attention_source, created_at, title
) VALUES (
X'0000000000000000000000000000c001', 'local', NULL, 'interactive',
'internal', 'ask', 'idle', 1, '{{"type":"idle"}}', 'lifecycle',
'2026-09-01T00:00:00Z', 'bridged'
);
INSERT INTO agent_run (
id, chat_id, tier, execution_location, depth, status, attempt_count,
max_attempts, claim_count, available_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000c005', X'0000000000000000000000000000c001',
'foreground', 'in_process', 0, 'active', 0, 0, 0, '2026-09-01T00:00:00Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z'
);
INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at) VALUES (
X'0000000000000000000000000000c002', X'0000000000000000000000000000c001',
X'0000000000000000000000000000c004', 1, 'user', 'hi', '2026-09-01T00:00:00Z'
);
INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at) VALUES (
X'0000000000000000000000000000c003', X'0000000000000000000000000000c001',
X'0000000000000000000000000000c004', 2, 'assistant', 'hello',
'2026-09-01T00:00:01Z'
);
INSERT INTO turn_run (
id, chat_id, agent_run_id, input_message_id, output_message_id, model,
invoked_skills, status, attempt_count, max_attempts, claim_count,
available_at, started_at, finished_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000c004', X'0000000000000000000000000000c001',
X'0000000000000000000000000000c005', X'0000000000000000000000000000c002',
X'0000000000000000000000000000c003', 'scripted', '[]', 'completed', 1, 5, 1,
'2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z'
);
INSERT INTO code_event (owner, session_id, seq, event, created_at) VALUES (
'local', X'0000000000000000000000000000c001', 1,
'{{"type":"session_started","harness_kind":"internal","harness_version":"0"}}',
'2026-09-01T00:00:00Z'
);
INSERT INTO code_event (owner, session_id, seq, event, created_at) VALUES (
'local', X'0000000000000000000000000000c001', 2,
'{{"type":"assistant_message","text":"hello"}}', '2026-09-01T00:00:01Z'
);
{events}"#
    ))
    .await
    .unwrap();
}

/// What the journal move leaves behind: no `event` table, the receipts
/// and their unique indexes on `code_event`, every foreign key satisfied,
/// the bridge's copies replaced by the backfill, and the chat replay
/// serving the seeded journal in order.
async fn assert_one_journal(db: &sea_orm::DatabaseConnection) {
    assert!(db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        count(db, "sqlite_master WHERE type = 'table' AND name = 'event'").await,
        0
    );
    for index in [
        "idx_code_event_attempt_ordinal",
        "idx_code_event_scan_token",
        "idx_code_event_one_terminal_per_turn",
    ] {
        assert_eq!(
            count(
                db,
                &format!("sqlite_master WHERE type = 'index' AND name = '{index}'")
            )
            .await,
            1,
            "{index} exists"
        );
    }
    assert_eq!(
        count(
            db,
            "code_event WHERE session_id = X'0000000000000000000000000000c001'"
        )
        .await,
        seeded_chat_events().len() as i64,
        "the bridge's copies are replaced by the backfill"
    );
    let terminal = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT seq, turn_id, terminal FROM code_event \
             WHERE session_id = X'0000000000000000000000000000c001' AND terminal"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the terminal receipt moved");
    assert_eq!(
        terminal.try_get::<i64>("", "seq").unwrap(),
        seeded_chat_events().len() as i64
    );
    assert_eq!(
        terminal.try_get::<Vec<u8>>("", "turn_id").unwrap(),
        uuid::Uuid::from_u128(0xc004).as_bytes().to_vec()
    );
    assert_chat_replay(db, uuid::Uuid::from_u128(0xc001)).await;
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();
    assert_eq!(schema_of(db).await, schema_of(&fresh).await);
}

/// A merged SQLite profile keeps its chat journal while the rows move
/// into `code_event`, and a second pass changes nothing.
#[tokio::test]
async fn a_pre_journal_database_replays_its_chat_events_from_the_one_journal() {
    use sea_orm_migration::MigrationTrait as _;

    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, Some(steps_before("m20260902_000002_one_journal")))
        .await
        .unwrap();
    seed_pre_journal_conversation(&db).await;

    Migrator::up(&db, None).await.unwrap();

    assert_one_journal(&db).await;
    let manager = SchemaManager::new(&db);
    super::one_journal::OneJournal.up(&manager).await.unwrap();
    assert_one_journal(&db).await;
}

/// The SQLite branch runs autocommit steps. An attempt that rebuilt
/// `code_event` and copied the rows before dying must finish on the
/// next start: the rebuilt table comes back unchanged, the copy is
/// redone as one copy, and the rest lands.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_interrupted_one_journal_migration_finishes_on_retry() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, Some(steps_before("m20260902_000002_one_journal")))
        .await
        .unwrap();
    seed_pre_journal_conversation(&db).await;
    let manager = SchemaManager::new(&db);
    super::rebuild_sqlite_table(
        &manager,
        "code_event",
        super::one_journal::add_receipt_columns,
    )
    .await
    .unwrap();
    assert!(manager
        .has_column("code_event", "lease_token")
        .await
        .unwrap());
    assert!(manager.has_table("event").await.unwrap());

    Migrator::up(&db, None).await.unwrap();

    assert_one_journal(&db).await;
}

/// The `event` drop is the completion marker, so it has to be the last
/// step: an attempt that reached it left the indexes behind it already
/// in place, and the next start's early return skips nothing.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_one_journal_migration_drops_event_last() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&db, Some(steps_before("m20260902_000002_one_journal")))
        .await
        .unwrap();
    seed_pre_journal_conversation(&db).await;
    let manager = SchemaManager::new(&db);
    // Every step but the drop: the rebuilds and the indexes, in the
    // order the migration runs them, with `event` still standing.
    super::rebuild_sqlite_table(
        &manager,
        "code_event",
        super::one_journal::add_receipt_columns,
    )
    .await
    .unwrap();
    for table in super::one_journal::EVENT_REFERENCING_TABLES {
        super::rebuild_sqlite_table(&manager, table, super::one_journal::repoint_event_reference)
            .await
            .unwrap();
    }
    for index in super::one_journal::RECEIPT_INDEXES {
        db.execute_unprepared(index)
            .await
            .expect("the receipt indexes create while event still exists");
    }
    assert!(manager.has_table("event").await.unwrap());

    Migrator::up(&db, None).await.unwrap();

    assert_one_journal(&db).await;
}

/// A conversation as the journal move left it, one migration before the
/// cards merge: a pending consent card the judge owns and a call a
/// standing grant approved (both on `tool_call`), an answered questions
/// card, a rejected plan, the journal rows that named each, and the
/// bridge worker's copy of the consent card beside them (#3010).
async fn seed_pre_approval_surface_conversation(db: &sea_orm::DatabaseConnection) {
    let required = serde_json::json!({
        "type": "tool_approval_required",
        "call_id": "00000000-0000-0000-0000-00000000e010",
        "tool_name": "exec",
        "class": "sensitive",
        "kind": "exec_may_run_networked_command",
        "grant_scopes": [{"scope": "whole_tool"}],
    });
    let asked = serde_json::json!({
        "type": "questions_asked",
        "call_id": "00000000-0000-0000-0000-00000000e011",
        "turn_id": "00000000-0000-0000-0000-00000000e004",
    });
    let proposed = serde_json::json!({
        "type": "plan_proposed",
        "call_id": "00000000-0000-0000-0000-00000000e012",
        "turn_id": "00000000-0000-0000-0000-00000000e004",
    });
    let decided = serde_json::json!({
        "type": "tool_approval_decided",
        "call_id": "00000000-0000-0000-0000-00000000e013",
        "approved": true,
    });
    let bridge_hint = serde_json::json!({
        "type": "approval_requested",
        "approval_id": "00000000-0000-0000-0000-00000000e030",
    });
    db.execute_unprepared(&format!(
        r#"
INSERT INTO code_session (
id, owner, workspace_id, kind, harness_kind, permission_mode, lifecycle,
spawn_epoch, attention_state, attention_source, created_at, title
) VALUES (
X'0000000000000000000000000000e001', 'local', NULL, 'interactive',
'internal', 'ask', 'idle', 2, '{{"type":"idle"}}', 'lifecycle',
'2026-09-01T00:00:00Z', 'cards'
);
INSERT INTO agent_run (
id, chat_id, tier, execution_location, depth, status, attempt_count,
max_attempts, claim_count, available_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000e005', X'0000000000000000000000000000e001',
'foreground', 'in_process', 0, 'active', 0, 0, 0, '2026-09-01T00:00:00Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z'
);
INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at) VALUES (
X'0000000000000000000000000000e002', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e004', 1, 'user', 'hi', '2026-09-01T00:00:00Z'
);
INSERT INTO turn_run (
id, chat_id, agent_run_id, input_message_id, model, invoked_skills, status,
attempt_count, max_attempts, claim_count, available_at, started_at,
finished_at, created_at, updated_at
) VALUES (
X'0000000000000000000000000000e004', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e005', X'0000000000000000000000000000e002',
'scripted', '[]', 'cancelled', 1, 5, 1, '2026-09-01T00:00:00Z',
'2026-09-01T00:00:00Z', '2026-09-01T00:00:20Z', '2026-09-01T00:00:00Z',
'2026-09-01T00:00:20Z'
);
INSERT INTO code_turn (id, owner, session_id, ordinal, status, user_input, started_at)
VALUES (
X'0000000000000000000000000000e020', 'local', X'0000000000000000000000000000e001',
1, 'running', 'hi', '2026-09-01T00:00:00Z'
);
INSERT INTO code_event (owner, session_id, seq, event, created_at) VALUES
('local', X'0000000000000000000000000000e001', 1,
 '{{"type":"turn_started","turn_id":"00000000-0000-0000-0000-00000000e004"}}',
 '2026-09-01T00:00:00Z'),
('local', X'0000000000000000000000000000e001', 2, '{required}', '2026-09-01T00:00:01Z'),
('local', X'0000000000000000000000000000e001', 3, '{asked}', '2026-09-01T00:00:02Z'),
('local', X'0000000000000000000000000000e001', 4, '{proposed}', '2026-09-01T00:00:03Z'),
('local', X'0000000000000000000000000000e001', 5, '{decided}', '2026-09-01T00:00:04Z'),
('local', X'0000000000000000000000000000e001', 6, '{bridge_hint}', '2026-09-01T00:00:05Z');
INSERT INTO tool_call (
id, chat_id, turn_id, provider_id, history_order, name, arguments, execution,
status, approval_status, approval_class, approval_kind, approval_requested_at,
approval_event_seq, auto_judge_status, created_at
) VALUES (
X'0000000000000000000000000000e010', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e004', 'call-exec', 1, 'exec',
'{{"command":"cargo","args":["test"],"cwd":"."}}', 'server', 'pending',
'pending', 'sensitive', 'exec_may_run_networked_command',
'2026-09-01T00:00:01Z', 2, 'judging', '2026-09-01T00:00:01Z'
);
INSERT INTO tool_call (
id, chat_id, turn_id, provider_id, history_order, name, arguments, execution,
status, result, created_at, resolved_at
) VALUES (
X'0000000000000000000000000000e011', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e004', 'call-ask', 2, 'ask_user_questions',
'{{"questions":[]}}', 'orchestration', 'completed',
'{{"answers":[{{"question_id":"greeting","selected_option_ids":["hi"]}}]}}',
'2026-09-01T00:00:02Z', '2026-09-01T00:00:12Z'
);
INSERT INTO tool_call (
id, chat_id, turn_id, provider_id, history_order, name, arguments, execution,
status, result, created_at, resolved_at
) VALUES (
X'0000000000000000000000000000e012', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e004', 'call-plan', 3, 'exit_plan_mode',
'{{"title":"Ship","plan":"1. do it"}}', 'orchestration', 'completed',
'{{"decision":"rejected"}}', '2026-09-01T00:00:03Z', '2026-09-01T00:00:13Z'
);
INSERT INTO tool_call (
id, chat_id, turn_id, provider_id, history_order, name, arguments, execution,
status, result, approval_status, approval_class, approval_kind,
approval_requested_at, approval_decided_at, approval_grant_source_call_id,
created_at, resolved_at
) VALUES (
X'0000000000000000000000000000e013', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e004', 'call-search', 4, 'web_search',
'{{"query":"tides"}}', 'server', 'completed', 'ok', 'approved', 'sensitive',
'search_may_share_query_and_excerpts', '2026-09-01T00:00:04Z',
'2026-09-01T00:00:04Z', X'0000000000000000000000000000e010',
'2026-09-01T00:00:04Z', '2026-09-01T00:00:14Z'
);
INSERT INTO user_question_request (
call_id, turn_id, chat_id, status, event_seq, asked_at, resolved_at
) VALUES (
X'0000000000000000000000000000e011', X'0000000000000000000000000000e004',
X'0000000000000000000000000000e001', 'answered', 3, '2026-09-01T00:00:02Z',
'2026-09-01T00:00:12Z'
);
INSERT INTO user_question (
call_id, question_id, position, header, prompt, options, allow_free_form,
question_type, answer_selected_option_ids, response_recorded_at
) VALUES (
X'0000000000000000000000000000e011', 'greeting', 0, 'Greeting', 'Which greeting?',
'[{{"id":"hi","label":"hi","description":"short"}}]', FALSE, 'single_select',
'["hi"]', '2026-09-01T00:00:12Z'
);
INSERT INTO plan_request (
call_id, turn_id, chat_id, status, event_seq, title, plan, feedback,
proposed_at, resolved_at
) VALUES (
X'0000000000000000000000000000e012', X'0000000000000000000000000000e004',
X'0000000000000000000000000000e001', 'rejected', 4, 'Ship', '1. do it',
'not yet', '2026-09-01T00:00:03Z', '2026-09-01T00:00:13Z'
);
INSERT INTO code_approval (
id, owner, session_id, turn_id, kind, harness_raw, native_call_id,
worker_epoch, state, requested_at
) VALUES (
X'0000000000000000000000000000e030', 'local', X'0000000000000000000000000000e001',
X'0000000000000000000000000000e020', '{{"type":"other","summary":"exec"}}', 'null',
'00000000-0000-0000-0000-00000000e010', 2, 'pending', '2026-09-01T00:00:01Z'
)"#
    ))
    .await
    .unwrap();
}

/// What the merge leaves behind: the retired tables and columns gone,
/// every foreign key satisfied, each seeded card one approval row whose
/// id is its call id and the bridge's copy gone, the chat reads and the
/// session read serving the same rows, the journal rows rewritten in
/// place, and the schema equal to a fresh database's.
async fn assert_one_approval_surface(db: &sea_orm::DatabaseConnection) {
    use crate::storage::Store as _;
    assert!(db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await
        .unwrap()
        .is_empty());
    for table in super::one_approval_surface::RETIRED_TABLES {
        assert_eq!(
            count(
                db,
                &format!("sqlite_master WHERE type = 'table' AND name = '{table}'")
            )
            .await,
            0,
            "{table} is dropped"
        );
    }
    let tool_call_columns = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA table_info(\"tool_call\")".to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    for column in super::one_approval_surface::RETIRED_TOOL_CALL_COLUMNS {
        assert!(
            !tool_call_columns.iter().any(|name| name == column),
            "tool_call keeps {column}"
        );
    }
    let chat_id = crate::ChatId(uuid::Uuid::from_u128(0xe001));
    let store = crate::db::DbStore { conn: db.clone() };
    let owner = crate::OwnerId::local();

    let mut rows = crate::db::code::list_approvals(
        &store,
        &owner,
        None,
        Some(crate::CodeSessionId(chat_id.0)),
    )
    .await
    .unwrap();
    rows.sort_by_key(|row| row.id.0);
    assert_eq!(
        rows.iter().map(|row| row.id.0).collect::<Vec<_>>(),
        [0xe010, 0xe011, 0xe012, 0xe013]
            .map(uuid::Uuid::from_u128)
            .to_vec(),
        "one row per card, keyed by the call, and the bridge's copy gone"
    );
    let [card, questions, plan, granted] = rows.as_slice() else {
        unreachable!()
    };
    assert_eq!(card.state, crate::CodeApprovalState::Pending);
    assert!(
        matches!(&card.kind, crate::CodeApprovalKind::ToolUse { offered_grants, .. } if !offered_grants.is_empty())
    );
    assert_eq!(card.worker_epoch, Some(2));
    assert_eq!(
        card.native_call_id.as_deref(),
        Some("00000000-0000-0000-0000-00000000e010")
    );
    assert_eq!(
        card.auto_judge_status,
        Some(crate::AutoJudgeStatus::Judging)
    );
    assert_eq!(questions.state, crate::CodeApprovalState::Approved);
    assert!(
        matches!(&questions.kind, crate::CodeApprovalKind::Questions { questions } if questions.len() == 1 && questions[0].id == "greeting")
    );
    assert_eq!(plan.state, crate::CodeApprovalState::Denied);
    assert_eq!(plan.feedback.as_deref(), Some("not yet"));
    assert_eq!(
        crate::PlanProposalBody::from_raw(&plan.harness_raw).unwrap(),
        crate::PlanProposalBody {
            title: "Ship".into(),
            plan: "1. do it".into()
        }
    );
    assert_eq!(granted.state, crate::CodeApprovalState::Approved);

    let pending = store
        .list_pending_tool_call_approvals(chat_id, 100)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].call_id.0, uuid::Uuid::from_u128(0xe010));
    assert_eq!(
        pending[0].kind,
        crate::ToolApprovalKind::ExecMayRunNetworkedCommand
    );
    assert_eq!(
        pending[0].auto_judge_status,
        Some(crate::AutoJudgeStatus::Judging)
    );
    let by_grant = store
        .get_tool_call_approval(crate::CallId(uuid::Uuid::from_u128(0xe013)))
        .await
        .unwrap()
        .unwrap();
    assert!(by_grant.approved_by_standing_grant);
    assert_eq!(by_grant.status, crate::ToolApprovalStatus::Approved);

    let replay = store.list_events(chat_id, 0).await.unwrap();
    let replayed = replay
        .iter()
        .map(|event| (event.seq, event.event.clone()))
        .collect::<Vec<_>>();
    assert!(
        matches!(
            &replayed[1],
            (2, crate::AgentEvent::ApprovalRequired { call_id, kind: crate::ToolApprovalKind::ExecMayRunNetworkedCommand, grant_scopes, .. })
                if call_id.0 == uuid::Uuid::from_u128(0xe010) && grant_scopes.len() == 1
        ),
        "{replayed:?}"
    );
    assert!(
        matches!(
            &replayed[2],
            (3, crate::AgentEvent::UserQuestionsAsked { call_id, turn_id })
                if call_id.0 == uuid::Uuid::from_u128(0xe011) && turn_id.0 == uuid::Uuid::from_u128(0xe004)
        ),
        "{replayed:?}"
    );
    assert!(
        matches!(
            &replayed[3],
            (4, crate::AgentEvent::PlanProposed { call_id, .. })
                if call_id.0 == uuid::Uuid::from_u128(0xe012)
        ),
        "{replayed:?}"
    );
    assert!(
        matches!(
            &replayed[4],
            (5, crate::AgentEvent::ApprovalDecided { call_id, approved: true })
                if call_id.0 == uuid::Uuid::from_u128(0xe013)
        ),
        "{replayed:?}"
    );
    assert_eq!(replayed.len(), 5, "the bridge's hint is gone: {replayed:?}");

    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();
    assert_eq!(schema_of(db).await, schema_of(&fresh).await);
}

/// A merged SQLite profile keeps every card as one approval row, and a
/// second pass changes nothing.
#[tokio::test]
async fn a_pre_approval_surface_database_keeps_its_cards_as_approval_rows() {
    use sea_orm_migration::MigrationTrait as _;

    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260902_000003_one_approval_surface")),
    )
    .await
    .unwrap();
    seed_pre_approval_surface_conversation(&db).await;

    Migrator::up(&db, None).await.unwrap();

    assert_one_approval_surface(&db).await;
    let manager = SchemaManager::new(&db);
    super::one_approval_surface::OneApprovalSurface
        .up(&manager)
        .await
        .unwrap();
    assert_one_approval_surface(&db).await;
}

/// The SQLite branch runs autocommit steps. An attempt that rebuilt
/// `code_approval` and minted the rows before dying must finish on the
/// next start: the rebuilt table comes back unchanged, the rows it
/// minted are kept rather than minted twice, and the rest lands.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn an_interrupted_one_approval_surface_migration_finishes_on_retry() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(
        &db,
        Some(steps_before("m20260902_000003_one_approval_surface")),
    )
    .await
    .unwrap();
    seed_pre_approval_surface_conversation(&db).await;
    let manager = SchemaManager::new(&db);
    super::rebuild_sqlite_table(
        &manager,
        "code_approval",
        super::one_approval_surface::one_approval_row,
    )
    .await
    .unwrap();
    assert!(manager
        .has_column("code_approval", "auto_judge_status")
        .await
        .unwrap());
    let transaction = manager.begin().await.unwrap();
    super::one_approval_surface::backfill(transaction.get_connection())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(manager.has_table("plan_request").await.unwrap());

    Migrator::up(&db, None).await.unwrap();

    assert_one_approval_surface(&db).await;
}

/// The SQLite branch of the internal engine migration runs two
/// autocommit steps. An attempt that rebuilt `code_session` and died
/// before the marker column must finish on the next start, not report
/// success with the NOT NULL still in place.
#[tokio::test]
async fn an_interrupted_internal_engine_migration_finishes_on_retry() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let steps = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20260901_000002_internal_engine_sessions")
        .expect("the internal engine session migration is in the chain");
    Migrator::up(&db, Some(u32::try_from(steps).unwrap()))
        .await
        .unwrap();
    // The first attempt: the rebuild lands, the marker does not.
    let manager = SchemaManager::new(&db);
    super::rebuild_sqlite_code_session_without_workspace(&manager)
        .await
        .unwrap();
    assert!(!manager.has_column("chat", "engine_private").await.unwrap());

    Migrator::up(&db, Some(1)).await.unwrap();

    assert!(manager.has_column("chat", "engine_private").await.unwrap());
    let workspace_column = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT \"notnull\" AS not_null FROM pragma_table_info('code_session') \
             WHERE name = 'workspace_id'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workspace_column.try_get::<i32>("", "not_null").unwrap(), 0);
    // The rest of the chain then lands on the fresh schema.
    Migrator::up(&db, None).await.unwrap();
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();
    assert_eq!(schema_of(&db).await, schema_of(&fresh).await);
}

#[tokio::test]
async fn a_stepwise_upgrade_lands_on_the_fresh_schema() {
    let stepwise = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&stepwise, Some(1)).await.unwrap();
    Migrator::up(&stepwise, None).await.unwrap();

    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    Migrator::up(&fresh, None).await.unwrap();

    assert_eq!(
        schema_of(&stepwise).await,
        schema_of(&fresh).await,
        "an upgraded database and a fresh one describe different schemas; \
         an appended migration disagrees with the baseline"
    );
}

/// The durable self-host path: a database that recorded the pre-squash
/// chain keeps its code rows, skips the baseline, and gains the owner
/// column from [`super::CodeOwner`] instead of having `CREATE TABLE`
/// re-run against it.
#[tokio::test]
async fn an_existing_pre_owner_code_repo_is_backfilled_and_not_recreated() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE app (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            current_revision_id TEXT NOT NULL,
            revision_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT,
            owner TEXT NOT NULL DEFAULT 'local'
        )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE code_repo (
            id TEXT PRIMARY KEY NOT NULL,
            root_path TEXT NOT NULL UNIQUE,
            display_name TEXT NOT NULL,
            default_base_ref TEXT NOT NULL,
            branch_prefix TEXT NOT NULL,
            setup_script TEXT,
            archive_script TEXT,
            quick_actions TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_unprepared(concat!(
        "INSERT INTO code_repo (id, root_path, display_name, default_base_ref, ",
        "branch_prefix, quick_actions, created_at) VALUES ",
        "('00000000-0000-0000-0000-000000000003', '/srv/legacy', 'legacy', 'main', ",
        "'tidebreak/', '[]', '2026-08-14 00:00:00+00:00')"
    ))
    .await
    .unwrap();

    Migrator::up(&db, None).await.unwrap();

    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT owner FROM code_repo WHERE display_name = 'legacy'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "owner").unwrap(), "local");
}

/// The first hosted machine ran v0.58.0, whose recorded baseline lacked
/// repository lifecycle columns. The repair keeps its row and replaces
/// the old all-rows uniqueness with the soft-removal contract.
#[tokio::test]
async fn a_v058_code_repo_gains_the_pre_pin_lifecycle_schema() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE app (
            id TEXT PRIMARY KEY NOT NULL,
            owner TEXT NOT NULL DEFAULT 'local',
            name TEXT NOT NULL,
            current_revision_id TEXT NOT NULL,
            revision_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT
        );
        CREATE TABLE code_repo (
            id TEXT PRIMARY KEY NOT NULL,
            owner TEXT NOT NULL DEFAULT 'local',
            root_path TEXT NOT NULL,
            display_name TEXT NOT NULL,
            default_base_ref TEXT NOT NULL,
            branch_prefix TEXT NOT NULL,
            setup_script TEXT,
            archive_script TEXT,
            quick_actions TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX idx_code_repo_owner_root_path
            ON code_repo (owner, root_path);
        INSERT INTO code_repo (
            id, owner, root_path, display_name, default_base_ref,
            branch_prefix, quick_actions, created_at
        ) VALUES (
            '00000000-0000-0000-0000-000000000011', 'local', '/srv/pre-pin',
            'pre-pin', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
        )",
    )
    .await
    .unwrap();

    Migrator::up(&db, None).await.unwrap();

    db.execute_unprepared(
        "UPDATE code_repo
         SET removed_at = '2026-08-25T12:00:00Z'
         WHERE id = '00000000-0000-0000-0000-000000000011';
         INSERT INTO code_repo (
             id, owner, root_path, display_name, default_base_ref,
             branch_prefix, quick_actions, created_at
         ) VALUES (
             '00000000-0000-0000-0000-000000000012', 'local', '/srv/pre-pin',
             'pre-pin-again', 'main', 'tidebreak/', '[]',
             '2026-08-25T12:00:00Z'
         )",
    )
    .await
    .unwrap();

    let rows = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT count(*) AS repo_count,
                    sum(cloned_from IS NULL) AS null_clone_count
             FROM code_repo
             WHERE owner = 'local' AND root_path = '/srv/pre-pin'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rows.try_get::<i64>("", "repo_count").unwrap(), 2);
    assert_eq!(rows.try_get::<i64>("", "null_clone_count").unwrap(), 2);
}

#[tokio::test]
async fn an_existing_pre_owner_app_is_backfilled_and_not_recreated() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE app (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            current_revision_id TEXT NOT NULL,
            revision_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT
        )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        concat!(
            "INSERT INTO app (id, name, current_revision_id, revision_count, created_at, updated_at, deleted_at) ",
            "VALUES ('00000000-0000-0000-0000-000000000001', 'legacy', ",
            "'00000000-0000-0000-0000-000000000002', 1, ",
            "'2026-08-14 00:00:00+00:00', '2026-08-14 00:00:00+00:00', NULL)"
        ),
    )
    .await
    .unwrap();

    Migrator::up(&db, None).await.unwrap();

    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT owner FROM app WHERE name = 'legacy'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "owner").unwrap(), "local");
}

/// A self-host database created before later code tables joined the
/// baseline keeps its rows and gains every table that the current schema
/// expects.
#[tokio::test]
async fn a_pre_pin_database_gains_tables_added_to_the_baseline() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE app (
            id TEXT PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            current_revision_id TEXT NOT NULL,
            revision_count INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT,
            owner TEXT NOT NULL DEFAULT 'local'
        )",
    )
    .await
    .unwrap();
    db.execute_unprepared(concat!(
        "INSERT INTO app (id, name, current_revision_id, revision_count, ",
        "created_at, updated_at, deleted_at, owner) VALUES ",
        "('00000000-0000-0000-0000-000000000001', 'legacy', ",
        "'00000000-0000-0000-0000-000000000002', 1, ",
        "'2026-08-14 00:00:00+00:00', '2026-08-14 00:00:00+00:00', NULL, 'local')"
    ))
    .await
    .unwrap();

    Migrator::up(&db, None).await.unwrap();

    for table in [
        "code_watch",
        "code_trigger",
        "code_trigger_fire",
        "code_session_image",
    ] {
        let exists = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
                ),
            ))
            .await
            .unwrap();
        assert!(exists.is_some(), "repair did not create {table}");
    }
    let legacy = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM app WHERE id = '00000000-0000-0000-0000-000000000001'".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(legacy.try_get::<String>("", "name").unwrap(), "legacy");
}
/// A v0.60.0 SQLite database keeps release rows while the current chain
/// adds image dimensions and upgrades trigger fires into the outbox.
#[tokio::test]
async fn a_v060_sqlite_database_keeps_release_rows() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared(include_str!("../../../fixtures/schema-v0.60.0.sql"))
        .await
        .unwrap();
    Migrator::install(&db).await.unwrap();
    db.execute_unprepared(
        "INSERT INTO seaql_migrations (version, applied_at) \
         VALUES ('m20260814_000001_baseline', 0)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO code_repo (
            id, owner, root_path, display_name, default_base_ref,
            branch_prefix, quick_actions, created_at
         ) VALUES (
            '00000000-0000-0000-0000-000000000101', 'local', '/srv/release',
            'release', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
         );
         INSERT INTO code_workspace (
            id, owner, repo_id, title, worktree_path, branch_name, base_ref,
            status, created_at
         ) VALUES (
            '00000000-0000-0000-0000-000000000102', 'local',
            '00000000-0000-0000-0000-000000000101', 'release',
            '/srv/release-worktree', 'tidebreak/release', 'main', 'active',
            '2026-08-20T12:00:00Z'
         );
         INSERT INTO code_trigger (
            id, owner, repo_id, condition, action, enabled, created_at, updated_at
         ) VALUES (
            '00000000-0000-0000-0000-000000000106', 'local',
            '00000000-0000-0000-0000-000000000101', 'checks_failed',
            'deliver', TRUE, '2026-08-20T12:00:00Z', '2026-08-20T12:00:00Z'
         );
         INSERT INTO code_trigger_fire (
            owner, trigger_id, workspace_id, head_sha, pr_number, fired_at
         ) VALUES (
            'local', '00000000-0000-0000-0000-000000000106',
            '00000000-0000-0000-0000-000000000102', 'release-head', 42,
            '2026-08-20T12:01:00Z'
         );
         INSERT INTO code_session (
            id, owner, workspace_id, kind, harness_kind, permission_mode,
            lifecycle, attention_state, attention_source, created_at
         ) VALUES (
            '00000000-0000-0000-0000-000000000103', 'local',
            '00000000-0000-0000-0000-000000000102', 'interactive',
            'claude_code', 'ask', 'idle', '{}', 'lifecycle',
            '2026-08-20T12:00:00Z'
         );
         INSERT INTO code_turn (
            id, owner, session_id, ordinal, status, user_input, started_at
         ) VALUES (
            '00000000-0000-0000-0000-000000000104', 'local',
            '00000000-0000-0000-0000-000000000103', 1, 'completed',
            'keep this attachment', '2026-08-20T12:00:00Z'
         );
         INSERT INTO code_turn_attachment (
            owner, turn_id, ordinal, blob_id, media_type, byte_len
         ) VALUES (
            'local', '00000000-0000-0000-0000-000000000104', 0,
            '00000000-0000-0000-0000-000000000105', 'image/png', 8
         );
         INSERT INTO chat (id, title, created_at, network_policy, owner) VALUES (
            X'0000000000000000000000000000d001', 'release chat',
            '2026-08-20T12:00:00Z', '{\"mode\":\"off\"}', 'local'
         )",
    )
    .await
    .unwrap();
    // The v0.60 journal rows name no turn: the release schema's turn
    // tables are not what this test is about, and a row without a turn
    // is the shape a chat-scoped event has.
    db.execute_unprepared(&seeded_event_inserts(
        "X'0000000000000000000000000000d001'",
        None,
    ))
    .await
    .unwrap();

    Migrator::up(&db, None).await.unwrap();

    assert_chat_replay(&db, uuid::Uuid::from_u128(0xd001)).await;

    let attachment = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT blob_id, width, height, byte_len \
             FROM code_turn_attachment WHERE turn_id = \
             '00000000-0000-0000-0000-000000000104'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        attachment.try_get::<String>("", "blob_id").unwrap(),
        "00000000-0000-0000-0000-000000000105"
    );
    assert_eq!(attachment.try_get::<i32>("", "width").unwrap(), 1);
    assert_eq!(attachment.try_get::<i32>("", "height").unwrap(), 1);
    assert_eq!(attachment.try_get::<i64>("", "byte_len").unwrap(), 8);

    // The park rebuild recreates code_turn; the seeded row must survive
    // with its status intact and the new columns empty.
    let turn = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT status, park_ref IS NULL AS park_ref_null, \
                    park_wait IS NULL AS park_wait_null \
             FROM code_turn \
             WHERE id = '00000000-0000-0000-0000-000000000104'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(turn.try_get::<String>("", "status").unwrap(), "completed");
    assert!(turn.try_get::<bool>("", "park_ref_null").unwrap());
    assert!(turn.try_get::<bool>("", "park_wait_null").unwrap());

    let fire = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT delivery_id, state, attempt_count,
                    delivered_at = fired_at AS delivered_at_matches,
                    lease_token IS NULL AS lease_token_cleared,
                    lease_expires_at IS NULL AS lease_expiry_cleared,
                    next_attempt_at IS NULL AS next_attempt_cleared,
                    last_error IS NULL AS last_error_cleared
             FROM code_trigger_fire
             WHERE trigger_id = '00000000-0000-0000-0000-000000000106'
               AND workspace_id = '00000000-0000-0000-0000-000000000102'
               AND pr_number = 42
               AND head_sha = 'release-head'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        fire.try_get::<uuid::Uuid>("", "delivery_id").unwrap(),
        uuid::Uuid::nil()
    );
    assert_eq!(fire.try_get::<String>("", "state").unwrap(), "delivered");
    assert_eq!(fire.try_get::<i64>("", "attempt_count").unwrap(), 1);
    for column in [
        "delivered_at_matches",
        "lease_token_cleared",
        "lease_expiry_cleared",
        "next_attempt_cleared",
        "last_error_cleared",
    ] {
        assert!(fire.try_get::<bool>("", column).unwrap());
    }

    let fire_primary_key = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM pragma_table_info('code_trigger_fire')
             WHERE pk > 0 ORDER BY pk"
                .to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        fire_primary_key,
        ["trigger_id", "workspace_id", "pr_number", "head_sha"]
    );
    assert!(db
        .execute_unprepared(
            "UPDATE code_trigger_fire
             SET state = 'pending', delivered_at = NULL, next_attempt_at = NULL
             WHERE trigger_id = '00000000-0000-0000-0000-000000000106'"
        )
        .await
        .is_err());

    let columns = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA table_info('code_turn_attachment')".to_owned(),
        ))
        .await
        .unwrap();
    for name in ["width", "height"] {
        let column = columns
            .iter()
            .find(|column| column.try_get::<String>("", "name").unwrap() == name)
            .unwrap();
        assert_eq!(column.try_get::<i32>("", "notnull").unwrap(), 1);
        assert_eq!(
            column.try_get::<Option<String>>("", "dflt_value").unwrap(),
            None
        );
    }

    for (kind, name) in [
        ("table", "code_session_image"),
        ("table", "code_trigger_delivery_receipt"),
        ("index", "idx_code_session_image_blob"),
        ("index", "idx_code_turn_attachment_blob"),
    ] {
        let object = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "SELECT 1 AS present FROM sqlite_master \
                     WHERE type = '{kind}' AND name = '{name}'"
                ),
            ))
            .await
            .unwrap();
        assert!(object.is_some(), "upgrade did not create {name}");
    }

    let versions = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT version FROM seaql_migrations ORDER BY version".to_owned(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.try_get::<String>("", "version").unwrap())
        .collect::<Vec<_>>();
    assert!(versions.contains(&"m20260822_000004_baseline_repair".to_owned()));
    assert!(versions.contains(&"m20260822_000005_code_session_fast_mode".to_owned()));
    assert!(versions.contains(&"m20260822_000006_code_session_images".to_owned()));
    assert!(versions.contains(&"m20260822_000007_trigger_fire_outbox".to_owned()));
    assert!(versions.contains(&"m20260822_000008_trigger_delivery_receipts".to_owned()));
}

/// The baseline is frozen, and this is what holds it still.
///
/// `Migrator::up` records one name per migration and cannot tell that the
/// statements behind an already-recorded name changed. So an edit here
/// reaches a fresh database and no existing one — the two then run
/// different schemas against the same queries, each internally consistent,
/// with nothing to notice. That used to be survivable because an epoch
/// bump deleted the local database. Nothing deletes it now.
///
/// The fix for a diff here is therefore not a regenerated fixture. It is
/// an appended migration in [`Migrator::migrations`], which reaches both.
///
/// Both backends are rendered, because the two builders do not agree on
/// everything: a type that collapses in SQLite, or a constraint it parses
/// and ignores, is a real difference on PostgreSQL that a SQLite-only
/// fixture cannot see.
///
/// Regenerate with `UPDATE_SCHEMA_FIXTURE=1 cargo test -p tidebreak-core`
/// only when squashing the chain for a release, which is the one time the
/// baseline is supposed to move.
#[test]
fn the_schema_baseline_is_pinned() {
    // sea-query renders one statement per line. Break each column and
    // table constraint onto its own line so a one-column edit reviews as a
    // one-line diff rather than a rewritten table. Splitting on `, ` only
    // before a quoted identifier or a constraint keyword leaves list
    // literals inside a CHECK alone.
    fn readable(statement: &str) -> String {
        statement
            .replacen(" ( ", " (\n    ", 1)
            .replace(", \"", ",\n    \"")
            .replace(", CHECK", ",\n    CHECK")
            .replace(", FOREIGN KEY", ",\n    FOREIGN KEY")
            .replace(" )", "\n)")
    }

    // The builders are unit structs that `to_string` consumes and that
    // implement neither Copy nor Clone, so take a constructor and mint a
    // fresh one per statement.
    fn render<B>(builder: fn() -> B) -> String
    where
        B: sea_orm::sea_query::SchemaBuilder,
    {
        let mut rendered = String::new();
        for entry in super::baseline::tables() {
            rendered.push_str(&readable(&entry.table.to_string(builder())));
            rendered.push_str(";\n\n");
            for index in entry.indexes {
                rendered.push_str(&index.to_string(builder()));
                rendered.push_str(";\n");
            }
            rendered.push('\n');
        }
        for seed in super::baseline::SEED_STATEMENTS {
            rendered.push_str(seed);
            rendered.push_str(";\n");
        }
        rendered
    }

    let updating = std::env::var_os("UPDATE_SCHEMA_FIXTURE").is_some();
    for (fixture, rendered) in [
        (
            "fixtures/schema-baseline.sql",
            render(|| SqliteQueryBuilder),
        ),
        (
            "fixtures/schema-baseline.postgres.sql",
            render(|| PostgresQueryBuilder),
        ),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
        if updating {
            std::fs::create_dir_all(path.parent().expect("the fixture path has a parent"))
                .expect("the fixture directory is creatable");
            std::fs::write(&path, &rendered).expect("the fixture path is writable");
            continue;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            existing, rendered,
            "the schema baseline changed, and it is frozen; an edit here \
         reaches a fresh database and no existing one. Append a migration \
         in db/migration.rs instead. Regenerate with \
         UPDATE_SCHEMA_FIXTURE=1 cargo test -p tidebreak-core only when \
         squashing the chain for a release."
        );
    }
}

/// The condition-widening rebuild keeps every trigger and fire row, keeps
/// the outbox identity, accepts the fact-edge tokens, and still refuses a
/// garbage token. A wrong rebuild passes a fresh-database test easily —
/// only seeded data shows a dropped row or a lost constraint.
#[tokio::test]
async fn the_condition_widening_keeps_trigger_and_fire_rows() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    // Everything up to, but not including, the widening.
    Migrator::up(&db, Some(9)).await.unwrap();

    for statement in [
        "INSERT INTO code_repo (id, owner, root_path, display_name, default_base_ref, \
         branch_prefix, quick_actions, created_at) VALUES ('repo-1', 'local', '/tmp/r', \
         'r', 'main', 'tidebreak/', '[]', '2026-08-20T00:00:00Z')",
        "INSERT INTO code_workspace (id, owner, repo_id, title, worktree_path, branch_name, \
         base_ref, status, created_at) VALUES ('ws-1', 'local', 'repo-1', 'w', '/tmp/w', \
         'tidebreak/w', 'main', 'active', '2026-08-20T00:00:00Z')",
        "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
         created_at, updated_at) VALUES ('trig-1', 'local', 'repo-1', 'checks_failed', \
         'deliver', TRUE, '2026-08-20T00:00:00Z', '2026-08-20T00:00:00Z')",
        "INSERT INTO code_trigger_fire (owner, trigger_id, workspace_id, pr_number, \
         head_sha, fired_at, delivery_id, delivery_condition, delivery_action, \
         delivery_message, state, attempt_count, next_attempt_at) VALUES ('local', \
         'trig-1', 'ws-1', 41, 'aaa', '2026-08-21T00:00:00Z', 'deliv-1', 'checks_failed', \
         'deliver', 'm', 'pending', 0, '2026-08-21T00:00:00Z')",
    ] {
        db.execute_unprepared(statement).await.unwrap();
    }

    Migrator::up(&db, None).await.unwrap();

    let fire = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT head_sha, state, delivery_condition FROM code_trigger_fire \
             WHERE trigger_id = 'trig-1'"
                .to_owned(),
        ))
        .await
        .unwrap()
        .expect("the seeded fire survives the rebuild");
    assert_eq!(fire.try_get::<String>("", "head_sha").unwrap(), "aaa");
    assert_eq!(fire.try_get::<String>("", "state").unwrap(), "pending");

    // The widened vocabulary is accepted on both rebuilt CHECKs...
    db.execute_unprepared(
        "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
         created_at, updated_at) VALUES ('trig-2', 'local', 'repo-1', 'pr_opened', \
         'notify', TRUE, '2026-08-22T00:00:00Z', '2026-08-22T00:00:00Z')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO code_trigger_fire (owner, trigger_id, workspace_id, pr_number, \
         head_sha, fired_at, delivery_id, delivery_condition, delivery_action, \
         delivery_message, state, attempt_count, next_attempt_at) VALUES ('local', \
         'trig-2', 'ws-1', 42, 'opened', '2026-08-22T00:00:00Z', 'deliv-2', 'pr_opened', \
         'notify', 'm', 'pending', 0, '2026-08-22T00:00:00Z')",
    )
    .await
    .unwrap();

    // ...and a token outside it still fails.
    assert!(db
        .execute_unprepared(
            "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
             created_at, updated_at) VALUES ('trig-3', 'local', 'repo-1', 'pr_sparkled', \
             'notify', TRUE, '2026-08-22T00:00:00Z', '2026-08-22T00:00:00Z')",
        )
        .await
        .is_err());
}

#![cfg(feature = "postgres")]

use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};
use tidebreak_core::DbStore;

#[derive(Debug)]
struct UpgradeSnapshot {
    blob_id: uuid::Uuid,
    width: i32,
    height: i32,
    byte_len: i64,
    dimensions: Vec<(String, String, Option<String>)>,
    width_lower_bound_rejected: bool,
    height_upper_bound_rejected: bool,
    image_table: Option<String>,
    image_index: Option<String>,
    attachment_index: Option<String>,
    trigger_delivery_id: uuid::Uuid,
    trigger_state: String,
    trigger_attempt_count: i64,
    trigger_delivered_at_matches: bool,
    trigger_delivery_fields_cleared: bool,
    trigger_primary_key: Vec<String>,
    pending_without_next_attempt_rejected: bool,
    migration_count: i64,
}

#[tokio::test]
async fn postgres_v060_upgrade_keeps_release_rows() {
    let source_url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let suffix = format!("v060_{}", uuid::Uuid::new_v4().simple());
    let (database_name, database_url) =
        create_sibling_database(&source_url, &suffix).await.unwrap();

    let result = exercise_release_upgrade(&database_url).await;
    drop_sibling_database(&source_url, &database_name).await;
    let snapshot = result.unwrap();

    assert_eq!(
        snapshot.blob_id,
        uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000205").unwrap()
    );
    assert_eq!((snapshot.width, snapshot.height), (1, 1));
    assert_eq!(snapshot.byte_len, 8);
    assert_eq!(
        snapshot.dimensions,
        [
            ("height".to_owned(), "NO".to_owned(), None),
            ("width".to_owned(), "NO".to_owned(), None),
        ]
    );
    assert!(snapshot.width_lower_bound_rejected);
    assert!(snapshot.height_upper_bound_rejected);
    assert_eq!(snapshot.image_table.as_deref(), Some("code_session_image"));
    assert_eq!(
        snapshot.image_index.as_deref(),
        Some("idx_code_session_image_blob")
    );
    assert_eq!(
        snapshot.attachment_index.as_deref(),
        Some("idx_code_turn_attachment_blob")
    );
    assert!(!snapshot.trigger_delivery_id.is_nil());
    assert_eq!(snapshot.trigger_state, "delivered");
    assert_eq!(snapshot.trigger_attempt_count, 1);
    assert!(snapshot.trigger_delivered_at_matches);
    assert!(snapshot.trigger_delivery_fields_cleared);
    assert_eq!(
        snapshot.trigger_primary_key,
        ["trigger_id", "workspace_id", "pr_number", "head_sha"]
    );
    assert!(snapshot.pending_without_next_attempt_rejected);
    assert_eq!(snapshot.migration_count, 3);
}

async fn exercise_release_upgrade(url: &str) -> Result<UpgradeSnapshot, String> {
    let setup = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    setup
        .execute_unprepared(include_str!("../fixtures/schema-v0.60.0.postgres.sql"))
        .await
        .map_err(|error| error.to_string())?;
    setup
        .execute_unprepared(
            "CREATE TABLE seaql_migrations (
                version varchar NOT NULL PRIMARY KEY,
                applied_at bigint NOT NULL
             );
             INSERT INTO seaql_migrations (version, applied_at)
             VALUES ('m20260814_000001_baseline', 0);
             INSERT INTO code_repo (
                id, owner, root_path, display_name, default_base_ref,
                branch_prefix, quick_actions, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000201', 'local', '/srv/release',
                'release', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_workspace (
                id, owner, repo_id, title, worktree_path, branch_name, base_ref,
                status, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000202', 'local',
                '00000000-0000-0000-0000-000000000201', 'release',
                '/srv/release-worktree', 'tidebreak/release', 'main', 'active',
                '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_trigger (
                id, owner, repo_id, condition, action, enabled, created_at, updated_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000206', 'local',
                '00000000-0000-0000-0000-000000000201', 'checks_failed',
                'deliver', TRUE, '2026-08-20T12:00:00Z', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_trigger_fire (
                owner, trigger_id, workspace_id, head_sha, pr_number, fired_at
             ) VALUES (
                'local', '00000000-0000-0000-0000-000000000206',
                '00000000-0000-0000-0000-000000000202', 'release-head', 42,
                '2026-08-20T12:01:00Z'
             );
             INSERT INTO code_session (
                id, owner, workspace_id, kind, harness_kind, permission_mode,
                lifecycle, attention_state, attention_source, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000203', 'local',
                '00000000-0000-0000-0000-000000000202', 'interactive',
                'claude_code', 'ask', 'idle', '{}', 'lifecycle',
                '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_turn (
                id, owner, session_id, ordinal, status, user_input, started_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000204', 'local',
                '00000000-0000-0000-0000-000000000203', 1, 'completed',
                'keep this attachment', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_turn_attachment (
                owner, turn_id, ordinal, blob_id, media_type, byte_len
             ) VALUES (
                'local', '00000000-0000-0000-0000-000000000204', 0,
                '00000000-0000-0000-0000-000000000205', 'image/png', 8
             )",
        )
        .await
        .map_err(|error| error.to_string())?;
    setup.close().await.map_err(|error| error.to_string())?;

    let store = DbStore::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    let verifier = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;

    let attachment = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT blob_id, width, height, byte_len
             FROM code_turn_attachment
             WHERE turn_id = '00000000-0000-0000-0000-000000000204'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the release attachment row disappeared".to_owned())?;
    let dimensions = verifier
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT column_name, is_nullable, column_default
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND table_name = 'code_turn_attachment'
               AND column_name IN ('width', 'height')
             ORDER BY column_name"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String>("", "column_name")?,
                row.try_get::<String>("", "is_nullable")?,
                row.try_get::<Option<String>>("", "column_default")?,
            ))
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(|error| error.to_string())?;
    let width_lower_bound_rejected = verifier
        .execute_unprepared(
            "UPDATE code_turn_attachment SET width = 0
             WHERE turn_id = '00000000-0000-0000-0000-000000000204'",
        )
        .await
        .is_err();
    let height_upper_bound_rejected = verifier
        .execute_unprepared(
            "UPDATE code_turn_attachment SET height = 8001
             WHERE turn_id = '00000000-0000-0000-0000-000000000204'",
        )
        .await
        .is_err();
    let objects = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT
                to_regclass('public.code_session_image')::text AS image_table,
                to_regclass('public.idx_code_session_image_blob')::text AS image_index,
                to_regclass('public.idx_code_turn_attachment_blob')::text AS attachment_index"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the catalog object query returned no row".to_owned())?;
    let trigger_fire = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT delivery_id, state, attempt_count,
                    delivered_at = fired_at AS delivered_at_matches,
                    lease_token IS NULL
                        AND lease_expires_at IS NULL
                        AND next_attempt_at IS NULL
                        AND last_error IS NULL AS delivery_fields_cleared
             FROM code_trigger_fire
             WHERE trigger_id = '00000000-0000-0000-0000-000000000206'
               AND workspace_id = '00000000-0000-0000-0000-000000000202'
               AND pr_number = 42
               AND head_sha = 'release-head'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the release trigger fire row disappeared".to_owned())?;
    let trigger_primary_key = verifier
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT kcu.column_name
             FROM information_schema.table_constraints AS tc
             JOIN information_schema.key_column_usage AS kcu
               ON tc.constraint_catalog = kcu.constraint_catalog
              AND tc.constraint_schema = kcu.constraint_schema
              AND tc.constraint_name = kcu.constraint_name
             WHERE tc.table_schema = 'public'
               AND tc.table_name = 'code_trigger_fire'
               AND tc.constraint_type = 'PRIMARY KEY'
             ORDER BY kcu.ordinal_position"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|row| row.try_get::<String>("", "column_name"))
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(|error| error.to_string())?;
    let pending_without_next_attempt_rejected = verifier
        .execute_unprepared(
            "UPDATE code_trigger_fire
             SET state = 'pending', delivered_at = NULL, next_attempt_at = NULL
             WHERE trigger_id = '00000000-0000-0000-0000-000000000206'",
        )
        .await
        .is_err();
    let migrations = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS migration_count
             FROM seaql_migrations
             WHERE version IN (
                'm20260822_000004_baseline_repair',
                'm20260822_000006_code_session_images',
                'm20260822_000007_trigger_fire_outbox'
             )"
            .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the migration count query returned no row".to_owned())?;

    let snapshot = UpgradeSnapshot {
        blob_id: attachment
            .try_get("", "blob_id")
            .map_err(|error| error.to_string())?,
        width: attachment
            .try_get("", "width")
            .map_err(|error| error.to_string())?,
        height: attachment
            .try_get("", "height")
            .map_err(|error| error.to_string())?,
        byte_len: attachment
            .try_get("", "byte_len")
            .map_err(|error| error.to_string())?,
        dimensions,
        width_lower_bound_rejected,
        height_upper_bound_rejected,
        image_table: objects
            .try_get("", "image_table")
            .map_err(|error| error.to_string())?,
        image_index: objects
            .try_get("", "image_index")
            .map_err(|error| error.to_string())?,
        attachment_index: objects
            .try_get("", "attachment_index")
            .map_err(|error| error.to_string())?,
        trigger_delivery_id: trigger_fire
            .try_get("", "delivery_id")
            .map_err(|error| error.to_string())?,
        trigger_state: trigger_fire
            .try_get("", "state")
            .map_err(|error| error.to_string())?,
        trigger_attempt_count: trigger_fire
            .try_get("", "attempt_count")
            .map_err(|error| error.to_string())?,
        trigger_delivered_at_matches: trigger_fire
            .try_get("", "delivered_at_matches")
            .map_err(|error| error.to_string())?,
        trigger_delivery_fields_cleared: trigger_fire
            .try_get("", "delivery_fields_cleared")
            .map_err(|error| error.to_string())?,
        trigger_primary_key,
        pending_without_next_attempt_rejected,
        migration_count: migrations
            .try_get("", "migration_count")
            .map_err(|error| error.to_string())?,
    };
    verifier.close().await.map_err(|error| error.to_string())?;
    store.close().await.map_err(|error| error.to_string())?;
    Ok(snapshot)
}

async fn create_sibling_database(url: &str, suffix: &str) -> Result<(String, String), String> {
    let (prefix, database, query) = split_postgres_url(url);
    let head: String = database
        .chars()
        .take(62usize.saturating_sub(suffix.len()))
        .collect();
    let name = format!("{head}_{suffix}");
    let connection = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    connection
        .execute_unprepared(&format!("CREATE DATABASE \"{name}\""))
        .await
        .map_err(|error| error.to_string())?;
    connection
        .close()
        .await
        .map_err(|error| error.to_string())?;
    Ok((name.clone(), format!("{prefix}{name}{query}")))
}

async fn drop_sibling_database(url: &str, name: &str) {
    let connection = Database::connect(url).await.unwrap();
    connection
        .execute_unprepared(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
        .await
        .unwrap();
    connection.close().await.unwrap();
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

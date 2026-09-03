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

#[derive(Debug)]
struct PrePinUpgradeSnapshot {
    columns: Vec<(String, String)>,
    repo_count: i64,
    workspace_status: String,
    workspace_released_tip: Option<String>,
    session_reasoning_effort: Option<String>,
    approval_state: String,
    invalid_workspace_status_rejected: bool,
    invalid_reasoning_effort_rejected: bool,
    invalid_approval_state_rejected: bool,
    repair_migration_count: i64,
}

#[tokio::test]
async fn postgres_v058_upgrade_repairs_the_pre_pin_code_schema() {
    let source_url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let suffix = format!("v058_{}", uuid::Uuid::new_v4().simple());
    let (database_name, database_url) =
        create_sibling_database(&source_url, &suffix).await.unwrap();

    let result = exercise_pre_pin_upgrade(&database_url).await;
    drop_sibling_database(&source_url, &database_name).await;
    let snapshot = result.unwrap();

    assert_eq!(
        snapshot.columns,
        [
            ("code_repo".to_owned(), "cloned_from".to_owned()),
            ("code_repo".to_owned(), "removed_at".to_owned()),
            ("code_workspace".to_owned(), "bundle_bytes".to_owned()),
            ("code_workspace".to_owned(), "released_at".to_owned()),
            ("code_workspace".to_owned(), "released_tip".to_owned()),
            ("session".to_owned(), "reasoning_effort".to_owned()),
        ]
    );
    assert_eq!(snapshot.repo_count, 2);
    assert_eq!(snapshot.workspace_status, "released");
    assert_eq!(snapshot.workspace_released_tip.as_deref(), Some("deadbeef"));
    assert_eq!(snapshot.session_reasoning_effort.as_deref(), Some("high"));
    assert_eq!(snapshot.approval_state, "abandoned");
    assert!(snapshot.invalid_workspace_status_rejected);
    assert!(snapshot.invalid_reasoning_effort_rejected);
    assert!(snapshot.invalid_approval_state_rejected);
    assert_eq!(snapshot.repair_migration_count, 1);
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

async fn exercise_pre_pin_upgrade(url: &str) -> Result<PrePinUpgradeSnapshot, String> {
    let setup = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    setup
        .execute_unprepared(include_str!("../fixtures/schema-v0.60.0.postgres.sql"))
        .await
        .map_err(|error| error.to_string())?;
    // v0.60.0 made these changes by editing the recorded baseline. Undo that
    // delta to reproduce the v0.58.0 schema that the first hosted machine
    // kept until it upgraded directly to v0.65.0.
    setup
        .execute_unprepared(
            r#"
DROP TABLE "code_trigger_fire";
DROP TABLE "code_trigger";

DROP INDEX "idx_code_repo_owner_root_path";
ALTER TABLE "code_repo" DROP COLUMN "removed_at";
ALTER TABLE "code_repo" DROP COLUMN "cloned_from";
CREATE UNIQUE INDEX "idx_code_repo_owner_root_path"
    ON "code_repo" ("owner", "root_path");

ALTER TABLE "code_workspace" DROP COLUMN "released_at";
ALTER TABLE "code_workspace" DROP COLUMN "released_tip";
ALTER TABLE "code_workspace" DROP COLUMN "bundle_bytes";
DO $downgrade$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_workspace'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'status'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_workspace" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$downgrade$;
ALTER TABLE "code_workspace"
    ADD CONSTRAINT "code_workspace_status_check"
    CHECK ("status" IN ('creating', 'setup_failed', 'active', 'archived'));

ALTER TABLE "code_session" DROP COLUMN "reasoning_effort";

DO $downgrade$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_approval'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'state'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_approval" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$downgrade$;
ALTER TABLE "code_approval"
    ADD CONSTRAINT "code_approval_state_check"
    CHECK ("state" IN ('pending', 'approved', 'denied'));

CREATE TABLE seaql_migrations (
    version varchar NOT NULL PRIMARY KEY,
    applied_at bigint NOT NULL
);
INSERT INTO seaql_migrations (version, applied_at)
VALUES ('m20260814_000001_baseline', 0);

INSERT INTO code_repo (
    id, owner, root_path, display_name, default_base_ref,
    branch_prefix, quick_actions, created_at
) VALUES (
    '00000000-0000-0000-0000-000000000301', 'local', '/srv/pre-pin',
    'pre-pin', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
);
INSERT INTO code_workspace (
    id, owner, repo_id, title, worktree_path, branch_name, base_ref,
    status, created_at
) VALUES (
    '00000000-0000-0000-0000-000000000302', 'local',
    '00000000-0000-0000-0000-000000000301', 'pre-pin',
    '/srv/pre-pin-worktree', 'tidebreak/pre-pin', 'main', 'active',
    '2026-08-20T12:00:00Z'
);
INSERT INTO code_session (
    id, owner, workspace_id, kind, harness_kind, permission_mode,
    lifecycle, attention_state, attention_source, created_at
) VALUES (
    '00000000-0000-0000-0000-000000000303', 'local',
    '00000000-0000-0000-0000-000000000302', 'interactive',
    'claude_code', 'ask', 'idle', '{}', 'lifecycle',
    '2026-08-20T12:00:00Z'
);
INSERT INTO code_turn (
    id, owner, session_id, ordinal, status, user_input, started_at
) VALUES (
    '00000000-0000-0000-0000-000000000304', 'local',
    '00000000-0000-0000-0000-000000000303', 1, 'completed',
    'keep this turn', '2026-08-20T12:00:00Z'
);
INSERT INTO code_approval (
    id, owner, session_id, turn_id, kind, harness_raw, state, requested_at
) VALUES (
    '00000000-0000-0000-0000-000000000305', 'local',
    '00000000-0000-0000-0000-000000000303',
    '00000000-0000-0000-0000-000000000304', '{}', '{}', 'pending',
    '2026-08-20T12:00:00Z'
);
"#,
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

    let columns = verifier
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT table_name, column_name
             FROM information_schema.columns
             WHERE table_schema = 'public'
               AND (table_name, column_name) IN (
                   ('code_repo', 'removed_at'),
                   ('code_repo', 'cloned_from'),
                   ('code_workspace', 'released_at'),
                   ('code_workspace', 'released_tip'),
                   ('code_workspace', 'bundle_bytes'),
                   ('session', 'reasoning_effort')
               )
             ORDER BY table_name, column_name"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String>("", "table_name")?,
                row.try_get::<String>("", "column_name")?,
            ))
        })
        .collect::<Result<Vec<_>, sea_orm::DbErr>>()
        .map_err(|error| error.to_string())?;

    verifier
        .execute_unprepared(
            "UPDATE code_repo
             SET removed_at = '2026-08-25T12:00:00Z'
             WHERE id = '00000000-0000-0000-0000-000000000301';
             INSERT INTO code_repo (
                 id, owner, root_path, display_name, default_base_ref,
                 branch_prefix, quick_actions, created_at
             ) VALUES (
                 '00000000-0000-0000-0000-000000000306', 'local', '/srv/pre-pin',
                 'pre-pin-again', 'main', 'tidebreak/', '[]',
                 '2026-08-25T12:00:00Z'
             );
             UPDATE code_workspace
             SET status = 'released', released_tip = 'deadbeef', bundle_bytes = 42
             WHERE id = '00000000-0000-0000-0000-000000000302';
             UPDATE \"session\"
             SET reasoning_effort = 'high'
             WHERE id = '00000000-0000-0000-0000-000000000303';
             UPDATE approval
             SET state = 'abandoned'
             WHERE id = '00000000-0000-0000-0000-000000000305';",
        )
        .await
        .map_err(|error| error.to_string())?;

    let repo_count = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS repo_count
             FROM code_repo
             WHERE owner = 'local' AND root_path = '/srv/pre-pin'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the repository count query returned no row".to_owned())?
        .try_get("", "repo_count")
        .map_err(|error| error.to_string())?;
    let workspace = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT status, released_tip
             FROM code_workspace
             WHERE id = '00000000-0000-0000-0000-000000000302'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the pre-pin workspace disappeared".to_owned())?;
    let session = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT reasoning_effort
             FROM \"session\"
             WHERE id = '00000000-0000-0000-0000-000000000303'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the pre-pin session disappeared".to_owned())?;
    let approval = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT state
             FROM approval
             WHERE id = '00000000-0000-0000-0000-000000000305'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the pre-pin approval disappeared".to_owned())?;

    let invalid_workspace_status_rejected = verifier
        .execute_unprepared(
            "UPDATE code_workspace SET status = 'garbage'
             WHERE id = '00000000-0000-0000-0000-000000000302'",
        )
        .await
        .is_err();
    let invalid_reasoning_effort_rejected = verifier
        .execute_unprepared(
            "UPDATE \"session\" SET reasoning_effort = 'garbage'
             WHERE id = '00000000-0000-0000-0000-000000000303'",
        )
        .await
        .is_err();
    let invalid_approval_state_rejected = verifier
        .execute_unprepared(
            "UPDATE approval SET state = 'garbage'
             WHERE id = '00000000-0000-0000-0000-000000000305'",
        )
        .await
        .is_err();
    let repair_migration_count = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT count(*)::bigint AS migration_count
             FROM seaql_migrations
             WHERE version = 'm20260825_000015_pre_pin_code_lifecycle_repair'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the repair migration query returned no row".to_owned())?
        .try_get("", "migration_count")
        .map_err(|error| error.to_string())?;

    let snapshot = PrePinUpgradeSnapshot {
        columns,
        repo_count,
        workspace_status: workspace
            .try_get("", "status")
            .map_err(|error| error.to_string())?,
        workspace_released_tip: workspace
            .try_get("", "released_tip")
            .map_err(|error| error.to_string())?,
        session_reasoning_effort: session
            .try_get("", "reasoning_effort")
            .map_err(|error| error.to_string())?,
        approval_state: approval
            .try_get("", "state")
            .map_err(|error| error.to_string())?,
        invalid_workspace_status_rejected,
        invalid_reasoning_effort_rejected,
        invalid_approval_state_rejected,
        repair_migration_count,
    };
    verifier.close().await.map_err(|error| error.to_string())?;
    store.close().await.map_err(|error| error.to_string())?;
    Ok(snapshot)
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
             FROM turn_attachment
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
               AND table_name = 'turn_attachment'
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
            "UPDATE turn_attachment SET width = 0
             WHERE turn_id = '00000000-0000-0000-0000-000000000204'",
        )
        .await
        .is_err();
    let height_upper_bound_rejected = verifier
        .execute_unprepared(
            "UPDATE turn_attachment SET height = 8001
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

/// The chat side of a v0.60.0 database after the conversation merge: every
/// seeded row survives, the chat is a session row, and the foreign keys that
/// named `chat` name `session` with their delete actions intact.
struct ConversationMergeSnapshot {
    chat_table_present: bool,
    session_columns: Vec<(String, String)>,
    rows: Vec<(String, i64)>,
    foreign_keys: Vec<(String, String)>,
    orphan_rejected: bool,
    /// What the chat replay read serves for the seeded conversation after
    /// the journal moved into `event`.
    replayed: Vec<tidebreak_core::SequencedAgentEvent>,
}

/// The one journal row the v0.60 conversation carries: its turn's terminal
/// event, in the shape the release wrote it.
fn seeded_terminal_event() -> tidebreak_core::AgentEvent {
    tidebreak_core::AgentEvent::TurnCompleted {
        usage: tidebreak_core::Usage {
            input_tokens: 3,
            output_tokens: 4,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
        stop_reason: tidebreak_core::StopReason::EndTurn,
    }
}

#[tokio::test]
async fn postgres_v060_upgrade_merges_conversations_into_sessions() {
    let source_url = match std::env::var("TIDEBREAK_POSTGRES_TEST_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("TIDEBREAK_REQUIRE_POSTGRES_TEST").is_some() => {
            panic!("TIDEBREAK_POSTGRES_TEST_URL must name an isolated test database")
        }
        Err(_) => return,
    };
    let suffix = format!("merge_{}", uuid::Uuid::new_v4().simple());
    let (database_name, database_url) =
        create_sibling_database(&source_url, &suffix).await.unwrap();

    let result = exercise_conversation_merge(&database_url).await;
    drop_sibling_database(&source_url, &database_name).await;
    let snapshot = result.unwrap();

    assert!(!snapshot.chat_table_present, "the chat table survived");
    assert_eq!(
        snapshot.session_columns,
        [
            ("attention_source".to_owned(), "lifecycle".to_owned()),
            (
                "attention_state".to_owned(),
                r#"{"type": "idle"}"#.to_owned()
            ),
            ("harness_kind".to_owned(), "internal".to_owned()),
            ("kind".to_owned(), "interactive".to_owned()),
            ("lifecycle".to_owned(), "idle".to_owned()),
            ("memory_incognito".to_owned(), "false".to_owned()),
            ("network_policy".to_owned(), r#"{"mode":"open"}"#.to_owned()),
            ("permission_mode".to_owned(), "<null>".to_owned()),
            ("reasoning_effort".to_owned(), "<null>".to_owned()),
            ("spawn_epoch".to_owned(), "0".to_owned()),
            ("title".to_owned(), "kept".to_owned()),
            ("workspace_id".to_owned(), "<null>".to_owned()),
        ]
    );
    assert_eq!(
        snapshot.rows,
        [
            ("agent_run".to_owned(), 1),
            ("event".to_owned(), 1),
            ("turn".to_owned(), 1),
            ("message".to_owned(), 2),
            ("tool_call".to_owned(), 1),
        ]
    );
    assert_eq!(
        snapshot.replayed,
        [tidebreak_core::SequencedAgentEvent {
            seq: 1,
            event: seeded_terminal_event(),
        }],
        "the chat replay serves the journal from event"
    );
    assert_eq!(
        snapshot.foreign_keys,
        [
            ("agent_run".to_owned(), "r".to_owned()),
            ("approval".to_owned(), "a".to_owned()),
            ("chat_image_publication".to_owned(), "r".to_owned()),
            ("chat_root_attachment".to_owned(), "c".to_owned()),
            ("context_checkpoint".to_owned(), "c".to_owned()),
            ("event".to_owned(), "a".to_owned()),
            ("exec_file_change".to_owned(), "r".to_owned()),
            ("message".to_owned(), "a".to_owned()),
            ("message_identity".to_owned(), "a".to_owned()),
            ("output".to_owned(), "c".to_owned()),
            ("root_attachment_change".to_owned(), "r".to_owned()),
            ("standing_tool_grant".to_owned(), "c".to_owned()),
            ("task_plan".to_owned(), "r".to_owned()),
            ("tool_call".to_owned(), "a".to_owned()),
            ("turn".to_owned(), "a".to_owned()),
        ]
    );
    assert!(
        snapshot.orphan_rejected,
        "a message may name a missing session"
    );
}

async fn exercise_conversation_merge(url: &str) -> Result<ConversationMergeSnapshot, String> {
    let setup = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    setup
        .execute_unprepared(include_str!("../fixtures/schema-v0.60.0.postgres.sql"))
        .await
        .map_err(|error| error.to_string())?;
    setup
        .execute_unprepared(&format!(
            "CREATE TABLE seaql_migrations (
                version varchar NOT NULL PRIMARY KEY,
                applied_at bigint NOT NULL
             );
             INSERT INTO seaql_migrations (version, applied_at)
             VALUES ('m20260814_000001_baseline', 0);
             INSERT INTO chat (
                id, title, created_at, permission_mode, reasoning_effort,
                network_policy, owner
             ) VALUES (
                '00000000-0000-0000-0000-00000000a001', 'kept',
                '2026-09-01T00:00:00Z', NULL, 'aggressive', '{{\"mode\":\"open\"}}',
                'local'
             );
             INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at)
             VALUES (
                '00000000-0000-0000-0000-00000000a002',
                '00000000-0000-0000-0000-00000000a001',
                '00000000-0000-0000-0000-00000000a004', 1, 'user', 'hi',
                '2026-09-01T00:00:00Z'
             ), (
                '00000000-0000-0000-0000-00000000a003',
                '00000000-0000-0000-0000-00000000a001',
                '00000000-0000-0000-0000-00000000a004', 2, 'assistant', 'hello',
                '2026-09-01T00:00:01Z'
             );
             INSERT INTO agent_run (
                id, chat_id, tier, execution_location, depth, status,
                attempt_count, max_attempts, claim_count, available_at,
                created_at, updated_at
             ) VALUES (
                '00000000-0000-0000-0000-00000000a005',
                '00000000-0000-0000-0000-00000000a001', 'foreground',
                'in_process', 0, 'active', 0, 0, 0, '2026-09-01T00:00:00Z',
                '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z'
             );
             INSERT INTO turn_run (
                id, chat_id, agent_run_id, input_message_id, output_message_id,
                model, invoked_skills, status, attempt_count, max_attempts,
                claim_count, available_at, started_at, finished_at, created_at,
                updated_at
             ) VALUES (
                '00000000-0000-0000-0000-00000000a004',
                '00000000-0000-0000-0000-00000000a001',
                '00000000-0000-0000-0000-00000000a005',
                '00000000-0000-0000-0000-00000000a002',
                '00000000-0000-0000-0000-00000000a003', 'scripted', '[]',
                'completed', 1, 5, 1, '2026-09-01T00:00:00Z',
                '2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z',
                '2026-09-01T00:00:00Z', '2026-09-01T00:00:02Z'
             );
             INSERT INTO event (chat_id, seq, turn_id, terminal, payload, created_at)
             VALUES (
                '00000000-0000-0000-0000-00000000a001', 1,
                '00000000-0000-0000-0000-00000000a004', TRUE,
                '{terminal_event}', '2026-09-01T00:00:02Z'
             );
             INSERT INTO tool_call (
                id, chat_id, turn_id, provider_id, history_order, name, arguments,
                execution, status, result, created_at, resolved_at
             ) VALUES (
                '00000000-0000-0000-0000-00000000a006',
                '00000000-0000-0000-0000-00000000a001',
                '00000000-0000-0000-0000-00000000a004', 'scripted', 1, 'exec',
                '{{}}', 'server', 'completed', 'ok', '2026-09-01T00:00:01Z',
                '2026-09-01T00:00:01Z'
             )",
            terminal_event = serde_json::to_string(&seeded_terminal_event()).unwrap(),
        ))
        .await
        .map_err(|error| error.to_string())?;
    setup.close().await.map_err(|error| error.to_string())?;

    let store = DbStore::connect(url)
        .await
        .map_err(|error| error.to_string())?;
    let verifier = Database::connect(url)
        .await
        .map_err(|error| error.to_string())?;

    let chat_table_present = verifier
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT 1 AS present FROM information_schema.tables
             WHERE table_name = 'chat'"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .is_some();
    let session_columns = verifier
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT key, COALESCE(value, '<null>') AS value
             FROM (
                SELECT (each).key, (each).value
                FROM (
                    SELECT jsonb_each_text(to_jsonb(\"session\") - 'id' - 'owner' - 'created_at'
                        - 'model' - 'fast_mode' - 'permission_mode_revision'
                        - 'permission_mode_intent' - 'permission_mode_intent_revision'
                        - 'permission_mode_intent_epoch' - 'permission_mode_intent_lifecycle'
                        - 'harness_version' - 'harness_resume_ref' - 'fence_reason'
                        - 'child_pid' - 'child_process_identity' - 'subagents'
                        - 'unrecognized_event_count' - 'project_id' - 'attachment_revision') AS each
                    FROM \"session\"
                    WHERE id = '00000000-0000-0000-0000-00000000a001'
                ) AS pairs
             ) AS columns
             ORDER BY key"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String>("", "key")
                    .map_err(|error| error.to_string())?,
                row.try_get::<String>("", "value")
                    .map_err(|error| error.to_string())?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut rows = Vec::new();
    for (table, column) in [
        ("agent_run", "chat_id"),
        ("event", "session_id"),
        ("turn", "session_id"),
        ("message", "chat_id"),
        ("tool_call", "chat_id"),
    ] {
        let count: i64 = verifier
            .query_one_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT count(*)::bigint AS n FROM \"{table}\"
                     WHERE {column} = '00000000-0000-0000-0000-00000000a001'"
                ),
            ))
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("the {table} count returned no row"))?
            .try_get("", "n")
            .map_err(|error| error.to_string())?;
        rows.push((table.to_owned(), count));
    }
    let foreign_keys = verifier
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT conrelid::regclass::text AS table_name, confdeltype::text AS delete_action
             FROM pg_constraint
             WHERE contype = 'f' AND confrelid = '\"session\"'::regclass
               AND conrelid::regclass::text NOT LIKE 'code_%'
             ORDER BY table_name"
                .to_owned(),
        ))
        .await
        .map_err(|error| error.to_string())?
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<String>("", "table_name")
                    .map_err(|error| error.to_string())?,
                row.try_get::<String>("", "delete_action")
                    .map_err(|error| error.to_string())?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let orphan_rejected = verifier
        .execute_unprepared(
            "INSERT INTO message (id, chat_id, turn_id, seq, role, content, created_at)
             VALUES (
                '00000000-0000-0000-0000-00000000a007',
                '00000000-0000-0000-0000-00000000a009',
                '00000000-0000-0000-0000-00000000a004', 3, 'user', 'orphan',
                '2026-09-01T00:00:03Z'
             )",
        )
        .await
        .is_err();

    let replayed = {
        use tidebreak_core::Store as _;
        store
            .list_events(tidebreak_core::SessionId(uuid::Uuid::from_u128(0xa001)), 0)
            .await
            .map_err(|error| error.to_string())?
    };
    let snapshot = ConversationMergeSnapshot {
        chat_table_present,
        session_columns,
        rows,
        foreign_keys,
        orphan_rejected,
        replayed,
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

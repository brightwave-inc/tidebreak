use std::path::{Path, PathBuf};

use super::*;

fn sqlite_url(database: &Path) -> String {
    format!("sqlite://{}?mode=rwc", database.display())
}

/// A database an older build left behind: every migration but the newest, and
/// one row of data.
async fn older_build_database(database: &Path) {
    let steps = u32::try_from(migration_names().len() - 1).unwrap();
    migrate_sqlite_partially_for_tests(&sqlite_url(database), steps)
        .await
        .unwrap();
    let conn = Database::connect(sqlite_url(database)).await.unwrap();
    conn.execute_unprepared(
        "INSERT INTO setting (key, value_json) VALUES ('backup_probe', '\"kept\"')",
    )
    .await
    .unwrap();
    conn.close().await.unwrap();
}

fn pre_migration_copies(backups: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(backups) else {
        return Vec::new();
    };
    let mut copies: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pre-migration-"))
        })
        .collect();
    copies.sort();
    copies
}

async fn read_only(database: &Path) -> DatabaseConnection {
    Database::connect(format!("sqlite://{}?mode=ro", database.display()))
        .await
        .unwrap()
}

async fn recorded_count(conn: &DatabaseConnection) -> usize {
    recorded_migrations(conn).await.unwrap().len()
}

#[tokio::test]
async fn a_database_with_pending_migrations_is_copied_before_it_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let backups = dir.path().join("backups");
    older_build_database(&database).await;

    let store = DbStore::connect_with_pre_migration_backup(
        ConnectOptions::new(sqlite_url(&database)),
        &backups,
    )
    .await
    .unwrap();

    assert_eq!(
        store.get_setting("backup_probe").await.unwrap(),
        Some(serde_json::json!("kept"))
    );
    assert_eq!(
        recorded_count(store.conn.writer()).await,
        migration_names().len()
    );
    let copies = pre_migration_copies(&backups);
    assert_eq!(copies.len(), 1, "copies: {copies:?}");
    let name = copies[0].file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with(&format!("pre-migration-{}-", crate::VERSION)) && name.ends_with(".db"),
        "{name}"
    );
    // The copy is the database as the older build left it: its data, and
    // none of the migration this connect went on to apply.
    let copy = read_only(&copies[0]).await;
    assert_eq!(recorded_count(&copy).await, migration_names().len() - 1);
    let probe = copy
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT value_json FROM setting WHERE key = 'backup_probe'",
        ))
        .await
        .unwrap()
        .expect("the copy holds the older build's data");
    assert_eq!(
        probe.try_get::<String>("", "value_json").unwrap(),
        "\"kept\""
    );
}

#[tokio::test]
async fn a_fresh_or_current_database_is_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let backups = dir.path().join("backups");

    let fresh = DbStore::connect_with_pre_migration_backup(
        ConnectOptions::new(sqlite_url(&database)),
        &backups,
    )
    .await
    .unwrap();
    fresh.close().await.unwrap();
    DbStore::connect_with_pre_migration_backup(
        ConnectOptions::new(sqlite_url(&database)),
        &backups,
    )
    .await
    .unwrap();

    assert_eq!(pre_migration_copies(&backups), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn only_the_newest_copy_and_the_one_before_it_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let backups = dir.path().join("backups");
    older_build_database(&database).await;
    std::fs::create_dir_all(backups.join("unrecognized-20250101T000000Z")).unwrap();
    // By timestamp, 0.10.0 is the newest older copy. By name it sorts first,
    // so a prune that compared names would keep 0.9.0 instead.
    for name in [
        "pre-migration-0.8.0-20250101T000000Z.db",
        "pre-migration-0.9.0-20260101T000000Z.db",
        "pre-migration-0.10.0-20260102T000000Z.db",
        "notes.txt",
    ] {
        std::fs::write(backups.join(name), name).unwrap();
    }

    DbStore::connect_with_pre_migration_backup(
        ConnectOptions::new(sqlite_url(&database)),
        &backups,
    )
    .await
    .unwrap();

    let names: Vec<String> = pre_migration_copies(&backups)
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 2, "kept: {names:?}");
    assert!(
        names.contains(&"pre-migration-0.10.0-20260102T000000Z.db".to_owned()),
        "kept: {names:?}"
    );
    let this_update = format!("pre-migration-{}-", crate::VERSION);
    assert!(
        names.iter().any(|name| name.starts_with(&this_update)),
        "kept: {names:?}"
    );
    assert!(backups.join("notes.txt").is_file());
    assert!(backups.join("unrecognized-20250101T000000Z").is_dir());
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let backups = dir.path().join("backups");
    DbStore::connect(&sqlite_url(&database))
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let newer = Database::connect(sqlite_url(&database)).await.unwrap();
    newer
        .execute_unprepared(
            "INSERT INTO seaql_migrations (version, applied_at) \
             VALUES ('m20991231_000001_from_a_newer_build', 0)",
        )
        .await
        .unwrap();
    newer.close().await.unwrap();

    let refused = DbStore::connect_with_pre_migration_backup(
        ConnectOptions::new(sqlite_url(&database)),
        &backups,
    )
    .await
    .err()
    .expect("a database from a newer build must be refused");
    let without_backups = DbStore::connect(&sqlite_url(&database))
        .await
        .err()
        .expect("a database from a newer build must be refused");

    assert_eq!(
        refused.to_string(),
        format!(
            "This Tidebreak profile was written by a newer version. Install that version or \
             later, or restore a backup from {}.",
            backups.display()
        )
    );
    assert_eq!(
        without_backups.to_string(),
        "This Tidebreak profile was written by a newer version. Install that version or \
         later, or restore a backup."
    );
    assert_eq!(pre_migration_copies(&backups), Vec::<PathBuf>::new());
    let untouched = read_only(&database).await;
    assert_eq!(
        recorded_count(&untouched).await,
        migration_names().len() + 1
    );
}

/// A backup copies the database through SQLite rather than the file system,
/// so the copy is whole while the store keeps its connections open and keeps
/// writing. A plain file copy of a WAL database can leave the newest rows in
/// the log it did not copy.
#[tokio::test]
async fn a_snapshot_of_a_live_database_holds_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let store = DbStore::connect(&sqlite_url(&database)).await.unwrap();
    store
        .set_setting("snapshot_probe", &serde_json::json!("kept"))
        .await
        .unwrap();
    let path = store
        .sqlite_database_path()
        .await
        .unwrap()
        .expect("a file-backed SQLite store names its file");
    assert_eq!(
        path.canonicalize().unwrap(),
        database.canonicalize().unwrap()
    );

    let copy = dir.path().join("copy.db");
    backup::snapshot_sqlite(&path, &copy).await.unwrap();
    // The live store keeps writing after the copy is taken.
    store
        .set_setting("after_snapshot", &serde_json::json!(true))
        .await
        .unwrap();

    let copied = DbStore::connect(&sqlite_url(&copy)).await.unwrap();
    assert_eq!(
        copied.get_setting("snapshot_probe").await.unwrap(),
        Some(serde_json::json!("kept"))
    );
    assert_eq!(copied.get_setting("after_snapshot").await.unwrap(), None);
    assert_eq!(
        store.get_setting("after_snapshot").await.unwrap(),
        Some(serde_json::json!(true))
    );
}

/// A snapshot never writes over, or removes, a file that was already there.
#[tokio::test]
async fn a_snapshot_refuses_a_path_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("tidebreak.db");
    let store = DbStore::connect(&sqlite_url(&database)).await.unwrap();
    let path = store.sqlite_database_path().await.unwrap().unwrap();
    let occupied = dir.path().join("occupied.db");
    std::fs::write(&occupied, b"someone else's file").unwrap();

    backup::snapshot_sqlite(&path, &occupied)
        .await
        .expect_err("an existing path must be refused");

    assert_eq!(std::fs::read(&occupied).unwrap(), b"someone else's file");
}

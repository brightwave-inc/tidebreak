//! What a failed desktop boot means to the person in front of it.
//!
//! A boot failure reaches the desktop as the error's text, because the text
//! is what the log and the copied debug report need. The boot screen needs
//! one more thing: which of a handful of known failures this is, so it can
//! say what happened in a plain sentence and offer the one action that helps.
//! [`classify_boot_failure`] reads that from the text the producers write.
//! The tests pin each kind to its real producer where one can run here, so a
//! reworded error fails a test instead of falling through to the generic
//! screen.

use serde::Serialize;

/// Prefixed to a database the desktop could not open or bring up to date, so
/// the boot screen can tell it from the other failures a store error covers.
pub const DATABASE_FAILURE: &str = "could not open or update the local database";

/// A known reason the embedded server did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootFailureKind {
    /// Another process serves the data folder: its instance lock is taken.
    InstanceLock,
    /// A newer build wrote the database, which records migrations this build
    /// does not have.
    NewerVersion,
    /// The schema marker names a lifecycle or epoch this build does not keep,
    /// or cannot be read.
    UnrecognizedData,
    /// This build's major version has no local profile lifecycle, the guard
    /// a release that skipped the 1.0 checklist lands on.
    UnsupportedVersion,
    /// The database could not be opened or migrated.
    Migration,
    /// The disk has no room left.
    DiskFull,
    /// The keychain refused or failed.
    Keychain,
    /// None of the above.
    Unknown,
}

/// Which known failure a boot error is, from its text.
pub fn classify_boot_failure(error: &str) -> BootFailureKind {
    let error = error.to_lowercase();
    let says = |phrase: &str| error.contains(phrase);
    if says("already running on the data directory") {
        BootFailureKind::InstanceLock
    // Before the database checks: a migration that ran out of room is a
    // full disk, and freeing space is what fixes it.
    } else if says("database or disk is full")
        || says("no space left on device")
        || says("not enough space on the disk")
        || says("os error 28")
        || says("os error 112")
    {
        BootFailureKind::DiskFull
    } else if says("written by a newer version") {
        BootFailureKind::NewerVersion
    } else if says("no local profile lifecycle is defined") {
        BootFailureKind::UnsupportedVersion
    } else if says("schema marker") {
        BootFailureKind::UnrecognizedData
    } else if error.starts_with("secret error") || says("keychain") {
        BootFailureKind::Keychain
    } else if says(DATABASE_FAILURE) || says("before updating it") {
        BootFailureKind::Migration
    } else {
        BootFailureKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_server_on_the_same_folder_is_an_instance_lock() {
        let dir = tempfile::tempdir().unwrap();
        let config = tidebreak_core::Config::desktop(dir.path());
        let _held = crate::InstanceLock::acquire(&config).unwrap();

        let error = match crate::InstanceLock::acquire(&config) {
            Ok(_) => panic!("a second lock on one folder was granted"),
            Err(error) => error.to_string(),
        };

        assert_eq!(
            classify_boot_failure(&error),
            BootFailureKind::InstanceLock,
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_database_a_newer_build_wrote_is_a_newer_version() {
        use sea_orm::{ConnectionTrait, Database};

        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("tidebreak.db").display()
        );
        tidebreak_core::DbStore::connect(&url)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let newer = Database::connect(&url).await.unwrap();
        newer
            .execute_unprepared(
                "INSERT INTO seaql_migrations (version, applied_at) \
                 VALUES ('m20991231_000001_from_a_newer_build', 0)",
            )
            .await
            .unwrap();
        newer.close().await.unwrap();

        let error = match tidebreak_core::DbStore::connect(&url).await {
            Ok(_) => panic!("a database from a newer build was opened"),
            Err(error) => error.to_string(),
        };

        assert_eq!(
            classify_boot_failure(&error),
            BootFailureKind::NewerVersion,
            "{error}"
        );
    }

    #[test]
    fn a_full_disk_wins_over_the_step_that_hit_it() {
        for error in [
            "store error: could not open or update the local database: \
             Execution Error: error returned from database: (code: 13) \
             database or disk is full",
            "store error: Tidebreak could not save a backup of this profile in \
             /tmp/backups before updating it, so it changed nothing: No space \
             left on device (os error 28). Make sure that folder is writable \
             and the disk has free space, then open Tidebreak again.",
            "configuration error: failed to create data dir: There is not \
             enough space on the disk. (os error 112)",
        ] {
            assert_eq!(
                classify_boot_failure(error),
                BootFailureKind::DiskFull,
                "{error}"
            );
        }
    }

    #[test]
    fn a_database_that_would_not_open_or_migrate_is_a_migration_failure() {
        for error in [
            format!("store error: {DATABASE_FAILURE}: Migration Error: no such table: turn"),
            "store error: Tidebreak could not save a backup of this profile in \
             /tmp/backups before updating it, so it changed nothing: the copy of \
             the database failed its integrity check: no answer. Make sure that \
             folder is writable and the disk has free space, then open \
             Tidebreak again."
                .to_owned(),
        ] {
            assert_eq!(
                classify_boot_failure(&error),
                BootFailureKind::Migration,
                "{error}"
            );
        }
    }

    #[test]
    fn a_keychain_failure_is_named_as_one() {
        assert_eq!(
            classify_boot_failure("secret error: the user denied keychain access"),
            BootFailureKind::Keychain
        );
    }

    #[test]
    fn anything_else_is_unknown() {
        for error in [
            "",
            "server error: address in use",
            "store error: something else entirely",
        ] {
            assert_eq!(
                classify_boot_failure(error),
                BootFailureKind::Unknown,
                "{error}"
            );
        }
    }

    #[test]
    fn the_kinds_serialize_as_the_renderer_reads_them() {
        assert_eq!(
            serde_json::to_value(BootFailureKind::InstanceLock).unwrap(),
            serde_json::json!("instance_lock")
        );
        assert_eq!(
            serde_json::to_value(BootFailureKind::UnrecognizedData).unwrap(),
            serde_json::json!("unrecognized_data")
        );
    }
}

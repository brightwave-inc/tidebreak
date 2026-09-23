//! Copies of a local SQLite database taken before anything changes it.
//!
//! An update that appends a migration rewrites the local database in place.
//! On SQLite, SeaORM wraps a migration in a transaction only when the
//! migration asks for one, so a migration that fails partway can leave a
//! half-applied step. A person who wants the previous build back also needs
//! the database as that build left it. The connect that is about to migrate
//! therefore copies the database first, with `VACUUM INTO`: a consistent,
//! compacted file that any build able to read the old schema opens as
//! `tidebreak.db`.
//!
//! The desktop profile keeps these copies in `<data_dir>/backups`, beside the
//! folders its schema lifecycle sets aside when it cannot read a profile at
//! all. Both name themselves with [`timestamp`], so one listing sorts them by
//! age.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

use crate::error::{AgentError, Result};

/// Every pre-migration copy is named `pre-migration-<version>-<timestamp>.db`.
const PRE_MIGRATION_PREFIX: &str = "pre-migration-";
const PRE_MIGRATION_EXTENSION: &str = ".db";

/// How many pre-migration copies a backups directory keeps: the one this
/// update just took, and the one before it.
pub(crate) const PRE_MIGRATION_COPIES_KEPT: usize = 2;

/// How many one-second steps a name may move forward to avoid an existing
/// file. Only a clock that stepped backwards, or two copies in the same
/// second, ever needs one.
const NAME_ATTEMPTS: i64 = 60;

/// The UTC timestamp a backup is named with, such as `20260923T101500Z`.
///
/// Fixed width with no separators that a file system rejects, so names that
/// share a prefix sort by age.
pub fn timestamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Create a backups directory and any missing parents. On Unix the new
/// directories are readable by their owner only, because a backup holds the
/// same conversations as the database it copies.
pub fn create_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// Copy the database behind `conn` into `backups`, then drop all but the
/// newest [`PRE_MIGRATION_COPIES_KEPT`] copies. Returns the new copy's path.
///
/// A copy that fails removes its partial file and fails the connect, so no
/// migration runs without a backup to return to.
pub(crate) async fn copy_before_migrating(
    conn: &DatabaseConnection,
    backups: &Path,
    version: &str,
    now: DateTime<Utc>,
) -> Result<PathBuf> {
    create_directory(backups).map_err(|error| copy_failed(backups, &error.to_string()))?;
    let copy = unused_copy_path(backups, version, now);
    let target = copy.to_str().ok_or_else(|| {
        copy_failed(
            backups,
            "the backup path is not valid UTF-8, which SQLite needs",
        )
    })?;
    let copied = conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "VACUUM INTO ?",
            [target.to_owned().into()],
        ))
        .await;
    if let Err(error) = copied {
        // Only ever the file this call just started: the name was unused.
        let _ = std::fs::remove_file(&copy);
        return Err(copy_failed(backups, &error.to_string()));
    }
    prune_copies(backups, &copy, PRE_MIGRATION_COPIES_KEPT);
    Ok(copy)
}

fn copy_failed(backups: &Path, cause: &str) -> AgentError {
    AgentError::Store(format!(
        "Tidebreak could not save a backup of this profile in {} before updating it, so it \
         changed nothing: {cause}. Make sure that folder is writable and the disk has free \
         space, then open Tidebreak again.",
        backups.display()
    ))
}

/// A `pre-migration-<version>-<timestamp>.db` path in `backups` that does not
/// exist yet. `VACUUM INTO` refuses a target that already holds data.
fn unused_copy_path(backups: &Path, version: &str, now: DateTime<Utc>) -> PathBuf {
    let named = |at: DateTime<Utc>| {
        backups.join(format!(
            "{PRE_MIGRATION_PREFIX}{version}-{}{PRE_MIGRATION_EXTENSION}",
            timestamp(at)
        ))
    };
    (0..NAME_ATTEMPTS)
        .map(|step| named(now + chrono::Duration::seconds(step)))
        .find(|path| std::fs::symlink_metadata(path).is_err())
        .unwrap_or_else(|| named(now + chrono::Duration::seconds(NAME_ATTEMPTS)))
}

/// Keep `kept` copies: `newest`, which this update just took, and the most
/// recent others by the timestamp in their names. The version in a name does
/// not sort (`0.10.0` is newer than `0.9.0`), so it plays no part.
///
/// `newest` is kept whatever its timestamp says, so a clock that ran fast on
/// an earlier boot cannot prune the copy this boot depends on. Failures are
/// logged, never returned: a stale copy left behind costs disk space, while a
/// failed connect costs the whole app.
fn prune_copies(backups: &Path, newest: &Path, kept: usize) {
    let entries = match std::fs::read_dir(backups) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                backups = %backups.display(),
                %error,
                "could not list older pre-migration backups to prune them"
            );
            return;
        }
    };
    let mut older: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| path != newest)
        .filter_map(|path| {
            let stamp = copy_timestamp(path.file_name()?.to_str()?)?.to_owned();
            Some((stamp, path))
        })
        .collect();
    // Newest first by timestamp. The path only breaks a tie between two copies
    // from the same second, so `0.9.0` never outranks a later `0.10.0`.
    older.sort_by(|(left_stamp, left_path), (right_stamp, right_path)| {
        right_stamp
            .cmp(left_stamp)
            .then_with(|| right_path.cmp(left_path))
    });
    for (_, path) in older.into_iter().skip(kept.saturating_sub(1)) {
        if let Err(error) = std::fs::remove_file(&path) {
            tracing::warn!(
                backup = %path.display(),
                %error,
                "could not remove an older pre-migration backup"
            );
        }
    }
}

/// The timestamp in a pre-migration copy's file name, or `None` for any other
/// file. The version may itself contain `-` (`1.0.0-rc.1`), so the timestamp is
/// whatever follows the last one.
fn copy_timestamp(name: &str) -> Option<&str> {
    let stem = name
        .strip_prefix(PRE_MIGRATION_PREFIX)?
        .strip_suffix(PRE_MIGRATION_EXTENSION)?;
    let (_, stamp) = stem.rsplit_once('-')?;
    let well_formed = stamp.len() == 16
        && stamp.bytes().enumerate().all(|(index, byte)| match index {
            8 => byte == b'T',
            15 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        });
    well_formed.then_some(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_name_yields_its_timestamp_whatever_the_version() {
        assert_eq!(
            copy_timestamp("pre-migration-0.114.0-20260923T101500Z.db"),
            Some("20260923T101500Z")
        );
        assert_eq!(
            copy_timestamp("pre-migration-1.0.0-rc.1-20260923T101500Z.db"),
            Some("20260923T101500Z")
        );
        assert_eq!(copy_timestamp("pre-migration-0.114.0-yesterday.db"), None);
        assert_eq!(copy_timestamp("tidebreak.db"), None);
    }

    /// As text, `0.9.0` sorts above `0.10.0`. A prune that ranked copies by
    /// name would keep the stale 0.9.0 copy and drop the newer one.
    #[test]
    fn pruning_keeps_the_newer_copy_by_timestamp_not_by_version() {
        let dir = tempfile::tempdir().unwrap();
        let just_taken = dir.path().join("pre-migration-0.11.0-20260103T000000Z.db");
        let older = dir.path().join("pre-migration-0.9.0-20260101T000000Z.db");
        let newer = dir.path().join("pre-migration-0.10.0-20260102T000000Z.db");
        for path in [&just_taken, &older, &newer] {
            std::fs::write(path, b"copy").unwrap();
        }

        prune_copies(dir.path(), &just_taken, PRE_MIGRATION_COPIES_KEPT);

        assert!(just_taken.exists());
        assert!(newer.exists(), "the newer 0.10.0 copy must survive");
        assert!(!older.exists(), "the older 0.9.0 copy must be pruned");
    }
}

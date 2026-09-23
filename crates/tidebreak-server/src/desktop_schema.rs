//! The lifecycle for the local SQLite profile.
//!
//! A schema change is an appended migration in `tidebreak-core`'s
//! `db::migration` chain, and an appended migration reaches an existing
//! database without deleting it. Nothing in this file deletes a database
//! either. It decides one thing: whether this build can carry a profile
//! forward, or has to set it aside and start a fresh one.
//!
//! [`LAST_RESET_EPOCH`] is the pin the chain starts from. Before it, a schema
//! change was an in-place baseline edit plus an epoch bump, and the bump
//! deleted the database because no migration could reach it. A profile still
//! sitting below the pin holds some baseline revision nothing recorded, so no
//! migration can reach it now either: it is set aside once. A profile at the
//! pin holds exactly the baseline the chain starts from, so it converges: the
//! marker is re-stamped and the chain takes it from there. Neither case
//! happens twice, and the epoch never moves again. 1.0 keeps this chain rather
//! than squashing it, so 0.x and 1.x builds open the same profiles.
//!
//! A database without a marker is read rather than assumed. If the migrations
//! it recorded are a prefix of this build's chain that goes past the baseline,
//! a build from after the pin wrote it, so it keeps its data and gets a fresh
//! marker. Anything else is set aside the same way as a profile below the pin.
//!
//! Setting a profile aside moves the SQLite files, the document bytes under
//! `blobs/`, and the host-broker's durable authority that keys on their
//! conversation ids into `backups/unrecognized-<timestamp>/`. It deletes
//! nothing and touches nothing else in the data directory. Durable state that
//! must outlive it lives in sidecar files here by design: the schema marker
//! itself, and the provisioned gateway policy (`gateway-policy.json`, see
//! [`crate::managed_policy`]), whose loss would resolve the profile unmanaged
//! and orphan the gateway session it authorized.
//!
//! The data directory is not the whole story, though. Code worktrees live
//! outside it on purpose (Decision 53), so setting the database aside strands
//! every tree on disk with nothing pointing at it. Those are user work and
//! nothing here touches them; they are recorded first, in a third sidecar
//! written by [`crate::code::worktree_orphans`].

use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseBackend, Statement};
use serde::{Deserialize, Serialize};
use tidebreak_core::db::backup;
use tidebreak_core::{replace_file, sync_directory, AgentError, Config, DbStore, Result};

const DATABASE_FILE: &str = "tidebreak.db";
const MARKER_FILE: &str = "tidebreak-schema.json";
/// A profile whose schema still came from an in-place baseline edit, kept
/// current by deleting it. Nothing writes this any more; it is read so a
/// profile written before the pin can be recognized and converged.
const PRE_V1_LIFECYCLE: &str = "pre_v1";
/// A profile the migration chain maintains. Its schema changes by appended
/// migration, and its data survives.
const MIGRATED_LIFECYCLE: &str = "migrations";
const VECTOR_DIRECTORY: &str = "vectors";
const MAX_MARKER_BYTES: u64 = 1_024;
/// Where the profile keeps its backups: the copies taken before a migration
/// (see [`backup`]) and the profiles this lifecycle set aside.
const BACKUPS_DIRECTORY: &str = "backups";
/// Each profile set aside lands in its own `unrecognized-<timestamp>` folder.
const UNRECOGNIZED_PREFIX: &str = "unrecognized-";
/// The document bytes the database points at, where the desktop blob store
/// keeps them. They move with a profile that is set aside: left behind,
/// nothing in the fresh profile would reference them, and the blob orphan
/// auditor retires unreferenced blobs after a day.
const BLOB_DIRECTORY: &str = "blobs";

/// Host-broker files that outlive SQLite unless they move with it.
///
/// Conversation-scoped grants and attachments name product UUIDs. Once the
/// database they came from is set aside, those UUIDs are gone from the live
/// journal, so leaving these files would keep live authority for subjects that
/// no longer exist and show ghost chats on the Permissions surface.
const HOST_BROKER_DURABLE_FILES: [&str; 4] = [
    "host-broker-state.json",
    "host-broker.lock",
    "host-broker-audit.jsonl",
    "host-broker-audit.previous.jsonl",
];

/// Product majors whose builds open a local profile with this lifecycle.
///
/// 1.0 keeps the 0.x migration chain, so both majors open the same profiles.
/// A later major has to decide how it opens them before it ships, so it is
/// refused until it is listed here.
const SUPPORTED_PRODUCT_MAJORS: [&str; 2] = ["0", "1"];

/// The last epoch that ever deleted a local database, and the baseline the
/// migration chain starts from.
///
/// Frozen. Do not bump it for a schema change — append a migration instead, so
/// the change reaches a database that already exists rather than only a fresh
/// one. It never moves again: 1.0 keeps the chain instead of squashing it into
/// a new baseline.
const LAST_RESET_EPOCH: u32 = 41;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaMarker {
    lifecycle: String,
    epoch: u32,
}

impl SchemaMarker {
    fn current() -> Self {
        Self {
            lifecycle: MIGRATED_LIFECYCLE.to_owned(),
            epoch: LAST_RESET_EPOCH,
        }
    }

    /// A profile written before the pin, by a binary that kept the schema
    /// current by deleting the database.
    fn is_pre_pin(&self) -> bool {
        self.lifecycle == PRE_V1_LIFECYCLE && self.epoch <= LAST_RESET_EPOCH
    }
}

pub(super) async fn connect(config: &Config) -> Result<DbStore> {
    connect_for_product_major(config, product_major()).await
}

/// This build's major version, which decides whether it may open a local
/// profile at all.
fn product_major() -> &'static str {
    tidebreak_core::VERSION
        .split_once('.')
        .map_or(tidebreak_core::VERSION, |(major, _)| major)
}

async fn connect_for_product_major(config: &Config, product_major: &str) -> Result<DbStore> {
    let backups = config.data_dir.join(BACKUPS_DIRECTORY);
    connect_with(config, product_major, |database_url| async move {
        DbStore::connect_with_pre_migration_backup(
            crate::desktop_connect_options(&database_url),
            &backups,
        )
        .await
    })
    .await
}

async fn connect_with<C, F>(config: &Config, product_major: &str, connector: C) -> Result<DbStore>
where
    C: FnOnce(String) -> F,
    F: Future<Output = Result<DbStore>>,
{
    let needs_marker = prepare_for_product_major(&config.data_dir, product_major).await?;
    let store = connector(config.database_url()?).await?;
    if needs_marker {
        write_current_marker(&config.data_dir)?;
    }
    Ok(store)
}

/// Prepare the SQLite files and return whether a current marker must be
/// recorded after migrations succeed.
async fn prepare_for_product_major(data_dir: &Path, product_major: &str) -> Result<bool> {
    if !SUPPORTED_PRODUCT_MAJORS.contains(&product_major) {
        return Err(AgentError::config(format!(
            "this build is major version {product_major}, and no local profile lifecycle is \
             defined for it; decide how it opens 0.x and 1.x profiles before releasing it"
        )));
    }
    let database = data_dir.join(DATABASE_FILE);
    let marker = data_dir.join(MARKER_FILE);
    let saved = read_marker(&marker)?;

    match saved {
        Some(saved) if saved == SchemaMarker::current() => {
            // Vector data was derived and rebuildable; remove the retired
            // feature's stale directory before opening the database.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            Ok(false)
        }
        // At the pin, written by a binary from before the chain existed. The
        // tables are already the ones the chain starts from, so there is
        // nothing to repair: re-stamp the marker and let the migrations run.
        // This is the only path that keeps a pre-v1 profile's data.
        Some(saved) if saved.is_pre_pin() && saved.epoch == LAST_RESET_EPOCH => {
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            Ok(true)
        }
        // Below the pin. The schema is some baseline revision that was edited
        // in place and never recorded, so no migration can know what it holds.
        // It is set aside once, and this profile never takes that path again.
        Some(saved) if saved.is_pre_pin() => {
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            set_aside(
                data_dir,
                &format!(
                    "the profile predates the migration chain (schema epoch {})",
                    saved.epoch
                ),
            )
            .await?;
            Ok(true)
        }
        // A database with no marker: deleted by hand, or never written because
        // the process stopped right after migrating. What the database
        // recorded says which build wrote it.
        None if database.exists() => {
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            if let Recorded::Unrecognized(reason) = read_recorded(&database).await {
                set_aside(data_dir, &reason).await?;
            }
            Ok(true)
        }
        None => {
            // A fresh profile. SQLite journals or broker files with no
            // database beside them belong to a profile that is gone, and a new
            // database must not inherit them.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            set_aside(
                data_dir,
                "files were left from a database that is no longer there",
            )
            .await?;
            Ok(true)
        }
        Some(saved) => Err(AgentError::config(format!(
            "refusing to open local SQLite database for schema marker lifecycle {:?}, epoch {}; this binary maintains {:?}, epoch {}",
            saved.lifecycle, saved.epoch, MIGRATED_LIFECYCLE, LAST_RESET_EPOCH
        ))),
    }
}

/// What a database with no marker recorded, measured against this build's
/// migration chain.
#[derive(Debug, PartialEq, Eq)]
enum Recorded {
    /// A build from after the pin wrote it. This build carries it forward, or
    /// refuses it as a newer build's once connected.
    Chain,
    /// Anything else, and why, for the log.
    Unrecognized(String),
}

async fn read_recorded(database: &Path) -> Recorded {
    match read_recorded_migrations(database).await {
        Ok(recorded) => classify_recorded(&recorded, &tidebreak_core::db::migration_names()),
        Err(error) => {
            Recorded::Unrecognized(format!("its migration record could not be read: {error}"))
        }
    }
}

/// Classify the migration names a database recorded against `chain`.
///
/// Every build since the pin records at least the baseline and the owner
/// migrations after it before it writes a marker, and the last build before
/// the pin recorded the baseline alone. So a prefix of the chain that goes
/// past the baseline is a profile from after the pin, while the baseline alone
/// is a profile from below the pin that lost its marker.
///
/// A database that records the whole chain and then migrations this build
/// does not know came from a newer build. It counts as the chain's own, so the
/// connect refuses it with the downgrade message and moves nothing.
fn classify_recorded(recorded: &[String], chain: &[String]) -> Recorded {
    let known = recorded.iter().filter(|name| chain.contains(name)).count();
    let Some(expected) = chain.get(..known) else {
        return Recorded::Unrecognized("it records the same migration twice".to_owned());
    };
    if !expected.iter().all(|name| recorded.contains(name)) {
        Recorded::Unrecognized(
            "its recorded migrations are not a prefix of this build's chain".to_owned(),
        )
    } else if known < recorded.len() && known < chain.len() {
        Recorded::Unrecognized(
            "it records migrations this build does not know, without this build's whole chain"
                .to_owned(),
        )
    } else if known < 2 {
        Recorded::Unrecognized(
            "it records no migration past the baseline, like a profile from before the chain"
                .to_owned(),
        )
    } else {
        Recorded::Chain
    }
}

/// The migration names a database recorded.
///
/// Opens the file read-write, the way the connect that follows does, so
/// SQLite can recover a WAL or roll back a hot journal that an unclean
/// shutdown left behind. That recovery keeps every committed transaction. The
/// open never creates the file, so a missing database still fails.
async fn read_recorded_migrations(database: &Path) -> std::result::Result<Vec<String>, String> {
    // One connection, closed before anything moves the file: Windows refuses
    // to rename a file a handle still holds.
    let mut options = ConnectOptions::new(format!("sqlite://{}?mode=rw", database.display()));
    options.max_connections(1).min_connections(0);
    let connection = Database::connect(options)
        .await
        .map_err(|error| error.to_string())?;
    let rows = connection
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT version FROM seaql_migrations",
        ))
        .await;
    if let Err(error) = connection.close().await {
        tracing::warn!(
            database = %database.display(),
            %error,
            "could not close the database after reading its migration record"
        );
    }
    rows.map_err(|error| error.to_string())?
        .iter()
        .map(|row| {
            row.try_get::<String>("", "version")
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn read_marker(marker: &Path) -> Result<Option<SchemaMarker>> {
    let metadata = match std::fs::symlink_metadata(marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AgentError::config(format!(
                "failed to inspect local SQLite schema marker {}: {error}",
                marker.display()
            )))
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AgentError::config(format!(
            "refusing to open local SQLite database with non-regular schema marker {}",
            marker.display()
        )));
    }
    if metadata.len() > MAX_MARKER_BYTES {
        return Err(AgentError::config(format!(
            "refusing to open local SQLite database with oversized schema marker {}",
            marker.display()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(marker)
        .and_then(|file| file.take(MAX_MARKER_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            AgentError::config(format!(
                "failed to read local SQLite schema marker {}: {error}",
                marker.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(AgentError::config(format!(
            "refusing to open local SQLite database with oversized schema marker {}",
            marker.display()
        )));
    }
    serde_json::from_slice::<SchemaMarker>(&bytes)
        .map(Some)
        .map_err(|error| {
            AgentError::config(format!(
                "refusing to open local SQLite database with unreadable schema marker {}: {error}",
                marker.display()
            ))
        })
}

/// Move a profile this build cannot carry forward into
/// `backups/unrecognized-<timestamp>/`, so the connect that follows starts a
/// fresh one. `reason` goes to the log.
///
/// Moves the SQLite files, the host-broker's durable files, and the document
/// bytes under `blobs/`, and deletes nothing. When none of them exist, it
/// moves nothing and creates no folder.
async fn set_aside(data_dir: &Path, reason: &str) -> Result<()> {
    let database = data_dir.join(DATABASE_FILE);
    // Code worktrees are the one piece of durable state this can strand
    // without touching: they live outside the data directory, and the rows
    // naming them are inside the database about to move. Read them out while
    // the database is still in place, so the trees are written down rather
    // than silently orphaned.
    crate::code::worktree_orphans::record_orphaned_worktrees(&database, data_dir).await;
    let blobs = data_dir.join(BLOB_DIRECTORY);
    let present: Vec<PathBuf> = sqlite_files(&database)
        .into_iter()
        .chain(
            HOST_BROKER_DURABLE_FILES
                .iter()
                .map(|name| data_dir.join(name)),
        )
        .filter(|path| std::fs::symlink_metadata(path).is_ok())
        .chain(holds_entries(&blobs).then_some(blobs))
        .collect();
    if present.is_empty() {
        return Ok(());
    }
    let destination = create_unrecognized_directory(data_dir)?;
    // A copy of the marker makes the folder a whole profile: restored
    // together, the build that wrote it opens it as it was. Without it, a
    // build before the pin reads the database as unmarked and deletes it.
    // The live marker stays until the fresh profile records its own.
    let marker = data_dir.join(MARKER_FILE);
    if marker.is_file() {
        if let Err(error) = std::fs::copy(&marker, destination.join(MARKER_FILE)) {
            tracing::warn!(
                marker = %marker.display(),
                %error,
                "could not copy the schema marker into the folder of a profile set aside"
            );
        }
    }
    for path in &present {
        let name = path.file_name().expect("every profile file has a name");
        move_profile_file(path, &destination.join(name))?;
    }
    for directory in [data_dir, destination.as_path()] {
        if let Err(error) = sync_directory(directory) {
            tracing::warn!(
                directory = %directory.display(),
                %error,
                "could not sync a directory after moving a local profile aside"
            );
        }
    }
    tracing::warn!(
        backup = %destination.display(),
        reason,
        "moved a local profile this version cannot open into a backup folder and started a \
         fresh one; nothing was deleted"
    );
    Ok(())
}

/// Create an empty `backups/unrecognized-<timestamp>` folder. A second
/// set-aside in the same second takes the next free second instead of
/// sharing a folder.
fn create_unrecognized_directory(data_dir: &Path) -> Result<PathBuf> {
    let backups = data_dir.join(BACKUPS_DIRECTORY);
    backup::create_directory(&backups).map_err(|error| {
        AgentError::msg(format!(
            "Tidebreak could not create the backups folder {}: {error}. Make sure the data \
             folder is writable, then open Tidebreak again.",
            backups.display()
        ))
    })?;
    let now = chrono::Utc::now();
    for step in 0..60 {
        let folder = backups.join(format!(
            "{UNRECOGNIZED_PREFIX}{}",
            backup::timestamp(now + chrono::Duration::seconds(step))
        ));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AgentError::msg(format!(
                    "Tidebreak could not create the backup folder {}: {error}. Make sure the \
                     data folder is writable, then open Tidebreak again.",
                    folder.display()
                )))
            }
        }
    }
    Err(AgentError::msg(format!(
        "Tidebreak could not find a free backup folder name in {}. Move older \
         unrecognized-* folders somewhere else, then open Tidebreak again.",
        backups.display()
    )))
}

/// Move one profile file into its set-aside folder. Windows can still hold
/// the SQLite files for a beat after `close()` returns (WAL mapping), so a
/// sharing or lock violation retries instead of failing the move.
fn move_profile_file(from: &Path, to: &Path) -> Result<()> {
    let delays_ms: &[u64] = if cfg!(windows) {
        &[0, 10, 50, 100, 250, 500, 1000]
    } else {
        &[0]
    };
    let mut last_error = None;
    for (attempt, delay_ms) in delays_ms.iter().enumerate() {
        if *delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(*delay_ms));
        }
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt + 1 < delays_ms.len() && is_windows_sharing_violation(&error) => {
                last_error = Some(error);
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }
    Err(AgentError::msg(format!(
        "Tidebreak could not move {} into {}: {}. Move it out of the data folder yourself, \
         then open Tidebreak again.",
        from.display(),
        to.display(),
        last_error.expect("every failed attempt keeps its error")
    )))
}

fn is_windows_sharing_violation(error: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
    matches!(error.raw_os_error(), Some(32 | 33))
}

/// Whether `directory` exists and holds anything. An empty one is not worth a
/// backup folder of its own.
fn holds_entries(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_some())
}

fn sqlite_files(database: &Path) -> [PathBuf; 4] {
    let path = database.as_os_str().to_string_lossy();
    [
        database.to_path_buf(),
        PathBuf::from(format!("{path}-wal")),
        PathBuf::from(format!("{path}-shm")),
        PathBuf::from(format!("{path}-journal")),
    ]
}

fn remove_retired_vectors(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(AgentError::config(format!(
                "failed to inspect retired vector data {}: {error}",
                path.display()
            )))
        }
    };
    let removed = if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|error| {
        AgentError::config(format!(
            "failed to remove retired vector data {}: {error}",
            path.display()
        ))
    })
}

fn write_current_marker(data_dir: &Path) -> Result<()> {
    write_current_marker_inner(data_dir, MarkerWriteFailure::None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerWriteFailure {
    None,
    #[cfg(test)]
    BeforePublish,
    #[cfg(test)]
    AfterPublish,
}

fn write_current_marker_inner(data_dir: &Path, failure: MarkerWriteFailure) -> Result<()> {
    #[cfg(not(test))]
    let _ = failure;
    let marker = data_dir.join(MARKER_FILE);
    let temporary = data_dir.join(format!(".{MARKER_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let mut bytes = serde_json::to_vec(&SchemaMarker::current())?;
    bytes.push(b'\n');
    let mut published = false;
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(test)]
        if failure == MarkerWriteFailure::BeforePublish {
            return Err(std::io::Error::other("injected pre-publication failure"));
        }
        replace_file(&temporary, &marker)?;
        published = true;
        #[cfg(test)]
        if failure == MarkerWriteFailure::AfterPublish {
            return Err(std::io::Error::other("injected post-publication failure"));
        }
        sync_directory(data_dir)
    })();
    if result.is_err() && !published {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| {
        AgentError::config(format!(
            "failed to install local SQLite schema marker {}: {error}",
            marker.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tidebreak_core::{
        db, Chat, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, OwnerId, RepoId, SessionId, Store,
        WorkspaceId,
    };

    use super::*;
    use crate::code;

    fn chat() -> Chat {
        Chat {
            id: SessionId::new(),
            project_id: None,
            title: Some("preservation probe".to_owned()),
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        }
    }

    /// Every `backups/unrecognized-*` folder, oldest first.
    fn set_aside_folders(data_dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(data_dir.join(BACKUPS_DIRECTORY)) else {
            return Vec::new();
        };
        let mut folders: Vec<PathBuf> = entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(UNRECOGNIZED_PREFIX))
            })
            .collect();
        folders.sort();
        folders
    }

    fn saved_marker(data_dir: &Path) -> Option<SchemaMarker> {
        read_marker(&data_dir.join(MARKER_FILE)).unwrap()
    }

    /// A database a build from after the pin wrote keeps its data when only
    /// its marker is gone, and so does everything beside it.
    #[tokio::test]
    async fn a_database_without_a_marker_keeps_its_data_when_it_recorded_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let unmarked = DbStore::connect(&config.database_url().unwrap())
            .await
            .unwrap();
        let expected = chat();
        unmarked.create_chat(&expected).await.unwrap();
        unmarked.close().await.unwrap();

        let blob = dir.path().join("blobs").join("keep-me");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"not database state").unwrap();
        let scratch = dir.path().join("scratch").join("keep-me");
        std::fs::create_dir_all(scratch.parent().unwrap()).unwrap();
        std::fs::write(&scratch, b"unreachable private scratch").unwrap();
        let receipt = dir.path().join("client-executions").join("keep-me");
        std::fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        std::fs::write(&receipt, b"native recovery state").unwrap();
        let broker = dir.path().join("host-broker-state.json");
        std::fs::write(&broker, b"native broker state").unwrap();
        let policy = dir.path().join("gateway-policy.json");
        std::fs::write(&policy, br#"{"gateway_url": "https://corp.gateway/"}"#).unwrap();
        let vectors = dir.path().join(VECTOR_DIRECTORY).join("stale-index");
        std::fs::create_dir_all(vectors.parent().unwrap()).unwrap();
        std::fs::write(&vectors, b"stale searchable text").unwrap();

        let kept = connect(&config).await.unwrap();

        assert_eq!(kept.get_chat(expected.id).await.unwrap(), Some(expected));
        assert_eq!(std::fs::read(blob).unwrap(), b"not database state");
        assert_eq!(
            std::fs::read(scratch).unwrap(),
            b"unreachable private scratch"
        );
        assert_eq!(std::fs::read(receipt).unwrap(), b"native recovery state");
        assert_eq!(
            std::fs::read(&broker).unwrap(),
            b"native broker state",
            "a kept profile keeps its host-broker authority"
        );
        assert_eq!(
            std::fs::read(&policy).unwrap(),
            br#"{"gateway_url": "https://corp.gateway/"}"#
        );
        assert!(!dir.path().join(VECTOR_DIRECTORY).exists());
        assert_eq!(saved_marker(dir.path()), Some(SchemaMarker::current()));
        assert!(set_aside_folders(dir.path()).is_empty());
    }

    /// An unclean shutdown leaves committed transactions in the WAL, not yet
    /// checkpointed into the database file. Reading the migration record has
    /// to see them the way the connect that follows does, or a healthy
    /// profile reads as unrecognizable and is set aside.
    #[tokio::test]
    async fn a_database_without_a_marker_keeps_the_commits_only_its_wal_holds() {
        let writer_dir = tempfile::tempdir().unwrap();
        let written = writer_dir.path().join(DATABASE_FILE);
        let mut options = ConnectOptions::new(format!("sqlite://{}?mode=rwc", written.display()));
        options.max_connections(1).min_connections(1);
        let writer = Database::connect(options).await.unwrap();
        writer
            .execute_unprepared("PRAGMA journal_mode=WAL")
            .await
            .unwrap();
        // One connection that never checkpoints: every migration and the row
        // below stay in the WAL, and the database file holds only its header.
        writer
            .execute_unprepared("PRAGMA wal_autocheckpoint=0")
            .await
            .unwrap();
        db::migrate_for_tests(&writer).await.unwrap();
        writer
            .execute_unprepared(
                "INSERT INTO setting (key, value_json) VALUES ('wal_probe', '\"kept\"')",
            )
            .await
            .unwrap();
        // The files a crash leaves behind: the database and its WAL, with no
        // usable index. Copied while the writer is open, so nothing closing it
        // can checkpoint them first.
        let crashed = tempfile::tempdir().unwrap();
        for file in sqlite_files(&written).iter().take(2) {
            std::fs::copy(file, crashed.path().join(file.file_name().unwrap())).unwrap();
        }
        writer.close().await.unwrap();
        let crashed_files = sqlite_files(&crashed.path().join(DATABASE_FILE));
        assert!(
            std::fs::metadata(&crashed_files[0]).unwrap().len() <= 4096,
            "the database file must hold no more than its header page"
        );
        assert!(
            std::fs::metadata(&crashed_files[1]).unwrap().len() > 4096,
            "the WAL must hold the committed migrations"
        );

        let kept = connect(&Config::desktop(crashed.path())).await.unwrap();

        assert_eq!(
            kept.get_setting("wal_probe").await.unwrap(),
            Some(serde_json::json!("kept"))
        );
        assert!(set_aside_folders(crashed.path()).is_empty());
        assert_eq!(saved_marker(crashed.path()), Some(SchemaMarker::current()));
    }

    #[tokio::test]
    async fn a_database_without_a_marker_that_this_build_cannot_read_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let database = dir.path().join(DATABASE_FILE);
        std::fs::write(&database, b"not a sqlite database").unwrap();
        // Junk journals too. Reading the migration record opens the database
        // read-write, and SQLite discards a journal or WAL it finds invalid,
        // so these must not stop a fresh profile from opening. Only the files
        // SQLite leaves alone are checked below.
        for sidecar in sqlite_files(&database).into_iter().skip(1) {
            std::fs::write(sidecar, b"stale sqlite state").unwrap();
        }
        for name in HOST_BROKER_DURABLE_FILES {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let blob = dir.path().join(BLOB_DIRECTORY).join("source-bytes");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"an attached document").unwrap();
        // The provisioned gateway policy is a sidecar file precisely so this
        // cannot orphan the session it authorizes: it stays where it is.
        let policy = dir.path().join("gateway-policy.json");
        std::fs::write(&policy, br#"{"gateway_url": "https://corp.gateway/"}"#).unwrap();

        let fresh = connect(&config).await.unwrap();

        assert!(fresh.list_chats().await.unwrap().is_empty());
        let folders = set_aside_folders(dir.path());
        assert_eq!(folders.len(), 1, "folders: {folders:?}");
        assert_eq!(
            std::fs::read(folders[0].join(DATABASE_FILE)).unwrap(),
            b"not a sqlite database"
        );
        for name in HOST_BROKER_DURABLE_FILES {
            assert_eq!(
                std::fs::read(folders[0].join(name)).unwrap(),
                name.as_bytes()
            );
            assert!(
                !dir.path().join(name).exists(),
                "{name} must not keep authorizing work in the fresh profile"
            );
        }
        // Left behind, the fresh profile's orphan auditor would retire it.
        assert!(!blob.exists());
        assert_eq!(
            std::fs::read(folders[0].join(BLOB_DIRECTORY).join("source-bytes")).unwrap(),
            b"an attached document"
        );
        assert_eq!(
            std::fs::read(&policy).unwrap(),
            br#"{"gateway_url": "https://corp.gateway/"}"#
        );
        assert_eq!(saved_marker(dir.path()), Some(SchemaMarker::current()));
    }

    #[tokio::test]
    async fn journals_left_without_a_database_are_moved_aside_before_a_fresh_start() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let wal = sqlite_files(&dir.path().join(DATABASE_FILE))[1].clone();
        std::fs::write(&wal, b"a journal from a database that is gone").unwrap();
        std::fs::write(
            dir.path().join("host-broker-state.json"),
            b"native broker state",
        )
        .unwrap();

        let fresh = connect(&config).await.unwrap();

        assert!(fresh.list_chats().await.unwrap().is_empty());
        let folders = set_aside_folders(dir.path());
        assert_eq!(folders.len(), 1, "folders: {folders:?}");
        assert_eq!(
            std::fs::read(folders[0].join(wal.file_name().unwrap())).unwrap(),
            b"a journal from a database that is gone"
        );
        assert_eq!(
            std::fs::read(folders[0].join("host-broker-state.json")).unwrap(),
            b"native broker state"
        );
    }

    #[tokio::test]
    async fn setting_a_profile_aside_records_the_code_worktrees_it_orphans_and_leaves_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        // A worktree lives outside the data directory on purpose, so it is
        // still there after the profile moves with nothing left pointing at it.
        let worktree = dir.path().parent().unwrap().join("orphan-worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join("uncommitted.rs"), b"user work").unwrap();
        // A workspace whose tree is already gone is not an orphan.
        let vanished = dir.path().parent().unwrap().join("already-gone");
        seed_workspace(&first, "orphan", &worktree).await;
        seed_workspace(&first, "vanished", &vanished).await;
        first.close().await.unwrap();
        write_older_epoch_marker(dir.path());

        let fresh = connect(&config).await.unwrap();

        assert!(fresh.list_chats().await.unwrap().is_empty());
        let recorded: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                dir.path()
                    .join(code::worktree_orphans::ORPHANED_WORKTREES_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        let worktrees = recorded["worktrees"].as_array().unwrap();
        assert_eq!(worktrees.len(), 1, "recorded: {recorded:#}");
        assert_eq!(
            worktrees[0]["worktree_path"],
            worktree.display().to_string()
        );
        assert_eq!(worktrees[0]["branch_name"], "tidebreak/orphan");
        assert_eq!(worktrees[0]["repo_root_path"], "/nonexistent-repo/orphan");
        assert_eq!(worktrees[0]["title"], "orphan");
        assert_eq!(
            std::fs::read(worktree.join("uncommitted.rs")).unwrap(),
            b"user work",
            "setting a profile aside must not touch a tree it only recorded"
        );
    }

    #[tokio::test]
    async fn setting_a_profile_aside_with_no_code_worktrees_writes_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        connect(&config).await.unwrap().close().await.unwrap();
        write_older_epoch_marker(dir.path());

        connect(&config).await.unwrap();

        assert!(!dir
            .path()
            .join(code::worktree_orphans::ORPHANED_WORKTREES_FILE)
            .exists());
    }

    async fn seed_workspace(store: &DbStore, title: &str, worktree_path: &Path) {
        let repo_id = RepoId::new();
        db::code::insert_repo(
            store,
            &CodeRepo {
                id: repo_id,
                owner: OwnerId::local(),
                root_path: format!("/nonexistent-repo/{title}"),
                display_name: "reset-test".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: vec![],
                created_at: Utc::now(),
                removed_at: None,
                cloned_from: None,
                origin_host: None,
                origin_owner: None,
                origin_name: None,
            },
        )
        .await
        .unwrap();
        db::code::insert_workspace(
            store,
            &CodeWorkspace {
                id: WorkspaceId::new(),
                owner: OwnerId::local(),
                repo_id,
                title: title.to_owned(),
                worktree_path: worktree_path.display().to_string(),
                branch_name: format!("tidebreak/{title}"),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: Utc::now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
                setup_error: None,
            },
        )
        .await
        .unwrap();
    }

    fn write_older_epoch_marker(data_dir: &Path) {
        std::fs::write(
            data_dir.join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: LAST_RESET_EPOCH - 1,
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_migrated_profile_is_kept_and_its_retired_vectors_removed() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        assert!(
            !dir.path().join(BACKUPS_DIRECTORY).exists(),
            "a fresh profile has nothing to back up or set aside"
        );
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        drop(first);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"current vector state").unwrap();

        let reopened = connect(&config).await.unwrap();

        assert_eq!(
            reopened.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
        assert!(!vector.exists());
    }

    /// Below the pin the schema is some in-place baseline revision nothing
    /// recorded, so there is no migration anyone could write for it. The
    /// profile is set aside once, whole, and a fresh one takes its place.
    #[tokio::test]
    async fn a_profile_below_the_pin_is_moved_aside_with_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        // An explicit close, not a drop: setting the profile aside renames the
        // SQLite files, which Windows refuses while a handle is open.
        first.close().await.unwrap();
        write_older_epoch_marker(dir.path());
        let broker = dir.path().join("host-broker-state.json");
        std::fs::write(&broker, b"native broker state").unwrap();

        let fresh = connect(&config).await.unwrap();

        assert!(fresh.list_chats().await.unwrap().is_empty());
        assert!(
            !broker.exists(),
            "grants for the old profile's conversations must not stay live"
        );
        let folders = set_aside_folders(dir.path());
        assert_eq!(folders.len(), 1, "folders: {folders:?}");
        assert_eq!(
            std::fs::read(folders[0].join("host-broker-state.json")).unwrap(),
            b"native broker state"
        );
        assert_eq!(
            read_marker(&folders[0].join(MARKER_FILE)).unwrap(),
            Some(SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: LAST_RESET_EPOCH - 1,
            }),
            "the folder keeps the marker its database was written under"
        );
        assert_eq!(saved_marker(dir.path()), Some(SchemaMarker::current()));
        // The folder holds the whole profile: opened on its own, it still has
        // the chat.
        let restored_dir = tempfile::tempdir().unwrap();
        for entry in std::fs::read_dir(&folders[0]).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), restored_dir.path().join(entry.file_name())).unwrap();
        }
        let restored =
            DbStore::connect(&Config::desktop(restored_dir.path()).database_url().unwrap())
                .await
                .unwrap();
        assert_eq!(
            restored.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
    }

    /// The promise the posture rests on: a profile written by the last pre-v1
    /// binary holds exactly the baseline the chain starts from, so it keeps
    /// its data and is re-stamped rather than set aside.
    ///
    /// This is the one migration path nobody gets to test twice. Every
    /// contributor's profile is sitting at this marker right now, and the
    /// arm that reads it runs once per profile and then never again.
    #[tokio::test]
    async fn a_pre_v1_profile_at_the_pin_keeps_its_data_and_converges() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        drop(first);
        // What the last epoch-driven binary left behind.
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: LAST_RESET_EPOCH,
            })
            .unwrap(),
        )
        .unwrap();

        let converged = connect(&config).await.unwrap();

        assert_eq!(
            converged.get_chat(expected.id).await.unwrap(),
            Some(expected),
            "a profile at the pin must survive the switch to migrations"
        );
        assert_eq!(
            read_marker(&dir.path().join(MARKER_FILE)).unwrap(),
            Some(SchemaMarker::current()),
            "the profile must be re-stamped, so the next boot takes the migrated path"
        );
    }

    /// Before an update migrates a profile, the profile's own backups folder
    /// gets a copy of it.
    #[tokio::test]
    async fn pending_migrations_are_copied_into_the_profile_backups_folder() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let steps = u32::try_from(db::migration_names().len() - 1).unwrap();
        db::migrate_sqlite_partially_for_tests(&config.database_url().unwrap(), steps)
            .await
            .unwrap();
        write_current_marker(dir.path()).unwrap();

        connect(&config).await.unwrap();

        let backups: Vec<String> = std::fs::read_dir(dir.path().join(BACKUPS_DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(backups.len(), 1, "backups: {backups:?}");
        assert!(
            backups[0].starts_with(&format!("pre-migration-{}-", tidebreak_core::VERSION)),
            "backups: {backups:?}"
        );
    }

    /// A downgrade is refused with a message that names this profile's
    /// backups folder, and leaves the profile exactly as the newer build did.
    #[tokio::test]
    async fn a_profile_from_a_newer_build_is_refused_and_names_its_backups_folder() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        first.close().await.unwrap();
        let future = "m20991231_000001_from_a_newer_build";
        let newer = Database::connect(config.database_url().unwrap())
            .await
            .unwrap();
        newer
            .execute_unprepared(&format!(
                "INSERT INTO seaql_migrations (version, applied_at) VALUES ('{future}', 0)"
            ))
            .await
            .unwrap();
        newer.close().await.unwrap();

        let error = match connect(&config).await {
            Ok(_) => panic!("a profile from a newer build was opened"),
            Err(error) => error,
        };

        assert_eq!(
            error.to_string(),
            format!(
                "This Tidebreak profile was written by a newer version. Install that version \
                 or later, or restore a backup from {}.",
                dir.path().join(BACKUPS_DIRECTORY).display()
            )
        );
        assert!(set_aside_folders(dir.path()).is_empty());
        // Nothing was lost: without the newer build's migration the same
        // profile opens with its chat.
        let older = Database::connect(config.database_url().unwrap())
            .await
            .unwrap();
        older
            .execute_unprepared(&format!(
                "DELETE FROM seaql_migrations WHERE version = '{future}'"
            ))
            .await
            .unwrap();
        older.close().await.unwrap();
        let reopened = connect(&config).await.unwrap();
        assert_eq!(
            reopened.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn only_a_chain_prefix_past_the_baseline_is_carried_forward() {
        let chain: Vec<String> = ["baseline", "app_owner", "code_owner", "later"]
            .map(String::from)
            .to_vec();
        let classify = |names: &[&str]| {
            let recorded: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
            classify_recorded(&recorded, &chain)
        };

        assert_eq!(classify(&["baseline", "app_owner"]), Recorded::Chain);
        assert_eq!(
            classify(&["later", "code_owner", "app_owner", "baseline"]),
            Recorded::Chain,
            "recorded order does not matter"
        );
        // A newer build's database carries the whole chain and more. The
        // connect refuses it with the downgrade message; nothing moves.
        assert_eq!(
            classify(&["baseline", "app_owner", "code_owner", "later", "newer"]),
            Recorded::Chain
        );
        for names in [
            &[][..],
            // The last build before the pin recorded the baseline alone.
            &["baseline"][..],
            &["baseline", "code_owner"][..],
            &["baseline", "app_owner", "newer"][..],
            // A chain from before the 2026-08-14 squash.
            &["m20260804_000001_old_baseline"][..],
        ] {
            assert!(
                matches!(classify(names), Recorded::Unrecognized(_)),
                "{names:?} must be set aside"
            );
        }
    }

    #[tokio::test]
    async fn stable_lifecycle_marker_fails_closed_without_destroying_database() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        drop(first);
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: "v1".to_owned(),
                epoch: 1,
            })
            .unwrap(),
        )
        .unwrap();

        let error = match connect(&config).await {
            Ok(_) => panic!("future lifecycle marker was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("refusing to open"));

        let unchanged = DbStore::connect(&config.database_url().unwrap())
            .await
            .unwrap();
        assert_eq!(
            unchanged.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn future_epoch_fails_closed_without_destroying_database() {
        assert_rejected_marker_preserves_database(
            serde_json::to_vec(&SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: LAST_RESET_EPOCH + 1,
            })
            .unwrap(),
        )
        .await;
    }

    #[tokio::test]
    async fn malformed_marker_fails_closed_without_destroying_database() {
        assert_rejected_marker_preserves_database(b"not valid json".to_vec()).await;
    }

    #[tokio::test]
    async fn oversized_marker_fails_closed_without_destroying_database() {
        assert_rejected_marker_preserves_database(vec![b' '; MAX_MARKER_BYTES as usize + 1]).await;
    }

    #[tokio::test]
    async fn failed_database_connect_does_not_install_marker() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());

        let result = connect_with(&config, "0", |_| async {
            Err::<DbStore, _>(AgentError::Store("injected migration failure".to_owned()))
        })
        .await;

        assert!(result.is_err());
        assert!(!dir.path().join(MARKER_FILE).exists());
    }

    #[tokio::test]
    async fn failed_database_connect_retains_the_older_marker() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let older = SchemaMarker {
            lifecycle: PRE_V1_LIFECYCLE.to_owned(),
            epoch: LAST_RESET_EPOCH - 1,
        };
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&older).unwrap(),
        )
        .unwrap();

        let result = connect_with(&config, "0", |_| async {
            Err::<DbStore, _>(AgentError::Store("injected migration failure".to_owned()))
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            serde_json::from_slice::<SchemaMarker>(
                &std::fs::read(dir.path().join(MARKER_FILE)).unwrap()
            )
            .unwrap(),
            older
        );
    }

    #[test]
    fn marker_failure_before_publish_retains_old_marker_and_cleans_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let old = serde_json::to_vec(&SchemaMarker {
            lifecycle: PRE_V1_LIFECYCLE.to_owned(),
            epoch: LAST_RESET_EPOCH - 1,
        })
        .unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), &old).unwrap();

        assert!(write_current_marker_inner(dir.path(), MarkerWriteFailure::BeforePublish).is_err());

        assert_eq!(std::fs::read(dir.path().join(MARKER_FILE)).unwrap(), old);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn marker_failure_after_publish_leaves_current_marker_readable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: LAST_RESET_EPOCH - 1,
            })
            .unwrap(),
        )
        .unwrap();

        assert!(write_current_marker_inner(dir.path(), MarkerWriteFailure::AfterPublish).is_err());

        assert_eq!(
            read_marker(&dir.path().join(MARKER_FILE)).unwrap(),
            Some(SchemaMarker::current())
        );
    }

    /// 1.0 keeps the 0.x migration chain, so a 1.x build opens a profile a
    /// 0.x build wrote, data and all.
    #[tokio::test]
    async fn a_major_one_build_opens_a_0x_profile_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let written_by_0x = connect_for_product_major(&config, "0").await.unwrap();
        let expected = chat();
        written_by_0x.create_chat(&expected).await.unwrap();
        written_by_0x.close().await.unwrap();
        assert_eq!(saved_marker(dir.path()), Some(SchemaMarker::current()));

        let opened_by_1x = connect_for_product_major(&config, "1").await.unwrap();

        assert_eq!(
            opened_by_1x.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
        assert_eq!(saved_marker(dir.path()), Some(SchemaMarker::current()));
        assert!(!dir.path().join(BACKUPS_DIRECTORY).exists());
    }

    #[tokio::test]
    async fn an_unknown_future_major_is_refused_without_touching_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::write(&database, b"database").unwrap();
        for sidecar in sqlite_files(&database).into_iter().skip(1) {
            std::fs::write(sidecar, b"sidecar").unwrap();
        }
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"vector").unwrap();

        let error = prepare_for_product_major(dir.path(), "2")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("major version 2"), "{error}");
        assert_eq!(std::fs::read(database).unwrap(), b"database");
        assert_eq!(std::fs::read(vector).unwrap(), b"vector");
        for sidecar in sqlite_files(&dir.path().join(DATABASE_FILE))
            .into_iter()
            .skip(1)
        {
            assert_eq!(std::fs::read(sidecar).unwrap(), b"sidecar");
        }
        assert!(!dir.path().join(BACKUPS_DIRECTORY).exists());
    }

    #[tokio::test]
    async fn unsupported_lifecycle_performs_no_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::write(&database, b"database").unwrap();
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"vector").unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: "v1".to_owned(),
                epoch: 1,
            })
            .unwrap(),
        )
        .unwrap();

        assert!(prepare_for_product_major(dir.path(), "0").await.is_err());

        assert_eq!(std::fs::read(database).unwrap(), b"database");
        assert_eq!(std::fs::read(vector).unwrap(), b"vector");
    }

    async fn assert_rejected_marker_preserves_database(marker: Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        let expected = chat();
        first.create_chat(&expected).await.unwrap();
        drop(first);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"current vector state").unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), marker).unwrap();

        let error = match connect(&config).await {
            Ok(_) => panic!("unsupported schema marker was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("refusing to open"));

        let unchanged = DbStore::connect(&config.database_url().unwrap())
            .await
            .unwrap();
        assert_eq!(
            unchanged.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
        assert_eq!(std::fs::read(vector).unwrap(), b"current vector state");
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_and_lock_violations_are_retried() {
        assert!(is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(32)
        ));
        assert!(is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(33)
        ));
        assert!(!is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(2)
        ));
        assert!(!is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(5)
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_unlink_does_not_treat_busy_as_a_sharing_violation() {
        assert!(!is_windows_sharing_violation(
            &std::io::Error::from_raw_os_error(16)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn moving_a_profile_file_retries_after_the_holding_handle_is_dropped() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        let destination = dir.path().join("moved.db");
        std::fs::write(&database, b"held").unwrap();
        // SQLite maps WAL without FILE_SHARE_DELETE. Match that so the first
        // rename fails with os error 32 until this handle is dropped.
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&database)
            .unwrap();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(hold);
        });
        move_profile_file(&database, &destination).unwrap();
        worker.join().unwrap();
        assert!(!database.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"held");
    }
}

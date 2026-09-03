//! The lifecycle for the local SQLite profile.
//!
//! A schema change is an appended migration in `tidebreak-core`'s
//! `db::migration` chain, and an appended migration reaches an existing
//! database without deleting it. So this file no longer decides whether to
//! keep local data. It decides one thing: whether a database predates the pin
//! the chain starts from.
//!
//! [`LAST_RESET_EPOCH`] is that pin. Before it, a schema change was an
//! in-place baseline edit plus an epoch bump, and the bump deleted the
//! database because no migration could reach it. A profile still sitting below
//! the pin holds some baseline revision nothing recorded, so there is no
//! migration anyone could write for it — it gets one last reset. A profile at
//! the pin holds exactly the baseline the chain starts from, so it converges:
//! the marker is re-stamped and the chain takes it from there. Neither case
//! happens twice, and the epoch never moves again.
//!
//! The reset deletes the SQLite files (and the host-broker's durable
//! authority, which keys on conversation ids the reset invalidates) — and
//! nothing else in the data directory. Durable state that must survive it
//! lives in sidecar files here by design: the schema marker itself, and the
//! provisioned gateway policy (`gateway-policy.json`, see
//! [`crate::managed_policy`]), whose loss would resolve the profile unmanaged
//! and orphan the gateway session it authorized.
//!
//! The data directory is not the whole story, though. Code worktrees live
//! outside it on purpose (Decision 53), so dropping the database strands every
//! tree on disk with nothing pointing at it. Those are user work and the reset
//! does not touch them; it records them first, in a third sidecar written by
//! [`crate::code::worktree_orphans`].

use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
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

/// The last epoch that ever deleted a local database, and the baseline the
/// migration chain starts from.
///
/// Frozen. Do not bump it for a schema change — append a migration instead, so
/// the change reaches a database that already exists rather than only a fresh
/// one. It moves again only at the `1.0.0` squash, when the chain collapses
/// back into a single baseline.
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
    connect_with(config, |database_url| async move {
        DbStore::connect_with_options(crate::host_connect_options(&database_url)).await
    })
    .await
}

async fn connect_with<C, F>(config: &Config, connector: C) -> Result<DbStore>
where
    C: FnOnce(String) -> F,
    F: Future<Output = Result<DbStore>>,
{
    let needs_marker = prepare(&config.data_dir).await?;
    let store = connector(config.database_url()?).await?;
    if needs_marker {
        write_current_marker(&config.data_dir)?;
    }
    Ok(store)
}

/// Prepare the SQLite files and return whether a current marker must be
/// recorded after migrations succeed.
async fn prepare(data_dir: &Path) -> Result<bool> {
    let product_major = tidebreak_core::VERSION
        .split_once('.')
        .map_or(tidebreak_core::VERSION, |(major, _)| major);
    prepare_for_product_major(data_dir, product_major).await
}

async fn prepare_for_product_major(data_dir: &Path, product_major: &str) -> Result<bool> {
    if product_major != "0" {
        return Err(AgentError::config(format!(
            "local SQLite schema lifecycle is disabled for product major {product_major}"
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
        // One last reset, and this profile never takes another.
        Some(saved) if saved.is_pre_pin() => {
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database).await?;
            Ok(true)
        }
        None if database.exists() => {
            // Databases predating this marker are from the disposable pre-v1
            // development line, below the pin by definition.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database).await?;
            Ok(true)
        }
        None => {
            // Clean up journals left after an interrupted reset even when the
            // main database file is already gone.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database).await?;
            Ok(true)
        }
        Some(saved) => Err(AgentError::config(format!(
            "refusing to open local SQLite database for schema marker lifecycle {:?}, epoch {}; this binary maintains {:?}, epoch {}",
            saved.lifecycle, saved.epoch, MIGRATED_LIFECYCLE, LAST_RESET_EPOCH
        ))),
    }
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

async fn reset_pre_v1_state(database: &Path) -> Result<()> {
    let data_dir = database
        .parent()
        .ok_or_else(|| AgentError::config("local SQLite database path has no parent directory"))?;
    // Code worktrees are the one piece of durable state a reset can strand
    // without touching: they live outside the data directory, and the rows
    // naming them are inside the database about to be deleted. Read them out
    // while the database still exists, so the trees are written down rather
    // than silently orphaned.
    crate::code::worktree_orphans::record_orphaned_worktrees(database, data_dir).await;
    remove_sqlite_files(database)?;
    // Host-broker authority is durable beside SQLite, not inside it. An epoch
    // wipe throws away every conversation id the product will ever know; grants
    // and attachments keyed to those ids would otherwise keep authorizing work
    // against subjects that can never come back. Clear the broker's durable
    // files with the database during disposable pre-v1 resets.
    remove_host_broker_durable_state(data_dir)
}

fn remove_sqlite_files(database: &Path) -> Result<()> {
    for path in sqlite_files(database) {
        remove_reset_file(&path, "local SQLite database file")?;
    }
    Ok(())
}

/// Delete one reset target. Windows can still hold the SQLite files for a
/// beat after `close()` returns (WAL mapping), so a sharing or lock violation
/// retries instead of failing the epoch reset.
fn remove_reset_file(path: &Path, kind: &str) -> Result<()> {
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
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) if attempt + 1 < delays_ms.len() && is_windows_sharing_violation(&error) => {
                last_error = Some(error);
            }
            Err(error) => {
                return Err(AgentError::config(format!(
                    "failed to reset {kind} {}: {error}",
                    path.display()
                )))
            }
        }
    }
    Err(AgentError::config(format!(
        "failed to reset {kind} {}: {}",
        path.display(),
        last_error.expect("a sharing-violation retry keeps the last error")
    )))
}

fn is_windows_sharing_violation(error: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
    matches!(error.raw_os_error(), Some(32 | 33))
}

/// Durable host-broker files that outlive SQLite unless explicitly cleared.
///
/// Conversation-scoped grants and attachments name product UUIDs. After a
/// pre-v1 epoch reset those UUIDs are gone from the journal, so leaving these
/// files would keep live authority for deleted subjects and show ghost chats
/// on the Permissions surface.
fn remove_host_broker_durable_state(data_dir: &Path) -> Result<()> {
    for name in [
        "host-broker-state.json",
        "host-broker.lock",
        "host-broker-audit.jsonl",
        "host-broker-audit.previous.jsonl",
    ] {
        let path = data_dir.join(name);
        remove_reset_file(&path, "host-broker durable file")?;
    }
    Ok(())
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
        db, Chat, ChatId, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, OwnerId, RepoId, Store,
        WorkspaceId,
    };

    use super::*;
    use crate::code;

    fn chat() -> Chat {
        Chat {
            id: ChatId::new(),
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

    #[tokio::test]
    async fn legacy_database_and_vectors_are_reset_with_explicit_preservation_policy() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let legacy = DbStore::connect(&config.database_url().unwrap())
            .await
            .unwrap();
        legacy.create_chat(&chat()).await.unwrap();
        // An explicit close, not a drop: the reset below deletes and rewrites
        // the SQLite files, which Windows refuses while a handle is open.
        legacy.close().await.unwrap();

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
        // The provisioned gateway policy is a sidecar file precisely so this
        // reset cannot orphan the session it authorizes: it must survive
        // with the other non-database durable state.
        let policy = dir.path().join("gateway-policy.json");
        std::fs::write(&policy, br#"{"gateway_url": "https://corp.gateway/"}"#).unwrap();
        let vectors = dir.path().join(VECTOR_DIRECTORY).join("stale-index");
        std::fs::create_dir_all(vectors.parent().unwrap()).unwrap();
        std::fs::write(&vectors, b"stale searchable text").unwrap();
        let database = dir.path().join(DATABASE_FILE);
        for sidecar in sqlite_files(&database).into_iter().skip(1) {
            std::fs::write(sidecar, b"stale sqlite state").unwrap();
        }

        let reset = connect(&config).await.unwrap();

        assert!(reset.list_chats().await.unwrap().is_empty());
        assert_eq!(std::fs::read(blob).unwrap(), b"not database state");
        assert_eq!(
            std::fs::read(scratch).unwrap(),
            b"unreachable private scratch"
        );
        assert_eq!(std::fs::read(receipt).unwrap(), b"native recovery state");
        assert!(
            !broker.exists(),
            "epoch reset must clear host-broker durable authority with the database"
        );
        assert_eq!(
            std::fs::read(&policy).unwrap(),
            br#"{"gateway_url": "https://corp.gateway/"}"#,
            "epoch reset must leave the provisioned gateway policy in place"
        );
        assert!(!dir.path().join(VECTOR_DIRECTORY).exists());
        assert_eq!(
            serde_json::from_slice::<SchemaMarker>(
                &std::fs::read(dir.path().join(MARKER_FILE)).unwrap()
            )
            .unwrap(),
            SchemaMarker::current()
        );
    }

    #[tokio::test]
    async fn epoch_reset_records_the_code_worktrees_it_orphans_and_leaves_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        // A worktree lives outside the data directory on purpose, so it is
        // still there after the reset with nothing left pointing at it.
        let worktree = dir.path().parent().unwrap().join("orphan-worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join("uncommitted.rs"), b"user work").unwrap();
        // A workspace whose tree is already gone is not an orphan.
        let vanished = dir.path().parent().unwrap().join("already-gone");
        seed_workspace(&first, "orphan", &worktree).await;
        seed_workspace(&first, "vanished", &vanished).await;
        first.close().await.unwrap();
        write_older_epoch_marker(dir.path());

        let reset = connect(&config).await.unwrap();

        assert!(reset.list_chats().await.unwrap().is_empty());
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
            "the reset must not touch a tree it only recorded"
        );
    }

    #[tokio::test]
    async fn epoch_reset_with_no_code_worktrees_writes_no_record() {
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
    /// recorded, so there is no migration anyone could write for it. This is
    /// the last reset such a profile ever takes.
    #[tokio::test]
    async fn a_profile_below_the_pin_takes_one_last_reset() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        first.create_chat(&chat()).await.unwrap();
        // An explicit close, not a drop: the epoch reset below deletes the
        // SQLite files, which Windows refuses while a handle is open.
        first.close().await.unwrap();
        write_older_epoch_marker(dir.path());

        let reset = connect(&config).await.unwrap();

        assert!(reset.list_chats().await.unwrap().is_empty());
    }

    /// The promise the posture rests on: a profile written by the last pre-v1
    /// binary holds exactly the baseline the chain starts from, so it keeps
    /// its data and is re-stamped rather than deleted.
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

        let result = connect_with(&config, |_| async {
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

        let result = connect_with(&config, |_| async {
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

    #[tokio::test]
    async fn released_package_guard_prevents_all_reset_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::write(&database, b"database").unwrap();
        for sidecar in sqlite_files(&database).into_iter().skip(1) {
            std::fs::write(sidecar, b"sidecar").unwrap();
        }
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"vector").unwrap();

        let error = prepare_for_product_major(dir.path(), "1")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("disabled for product major 1"));
        assert_eq!(std::fs::read(database).unwrap(), b"database");
        assert_eq!(std::fs::read(vector).unwrap(), b"vector");
        for sidecar in sqlite_files(&dir.path().join(DATABASE_FILE))
            .into_iter()
            .skip(1)
        {
            assert_eq!(std::fs::read(sidecar).unwrap(), b"sidecar");
        }
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

        assert!(prepare(dir.path()).await.is_err());

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
    fn remove_sqlite_files_retries_after_the_holding_handle_is_dropped() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        std::fs::write(&database, b"held").unwrap();
        // SQLite maps WAL without FILE_SHARE_DELETE. Match that so the first
        // remove_file fails with os error 32 until this handle is dropped.
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&database)
            .unwrap();
        let database_for_delete = database.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(hold);
        });
        remove_sqlite_files(&database_for_delete).unwrap();
        worker.join().unwrap();
        assert!(!database.exists());
    }
}

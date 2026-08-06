//! Pre-v1 lifecycle for the local SQLite profile.
//!
//! During `0.0.0` development we intentionally edit the schema baseline.
//! SeaORM cannot detect that an already-recorded baseline changed, so the
//! local profile keeps a small schema epoch outside SQLite. An older pre-v1
//! epoch (or a database from before epochs existed) is disposable and gets
//! rebuilt. Once the lifecycle changes for v1, pre-v1 binaries fail closed
//! instead.

use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use openwave_core::{AgentError, Config, DbStore, Result};
use serde::{Deserialize, Serialize};

const DATABASE_FILE: &str = "openwave.db";
const MARKER_FILE: &str = "openwave-schema.json";
const PRE_V1_LIFECYCLE: &str = "pre_v1";
const VECTOR_DIRECTORY: &str = "vectors";
const MAX_MARKER_BYTES: u64 = 1_024;

/// Bump this whenever the pre-v1 schema baseline changes incompatibly.
const DESKTOP_SCHEMA_EPOCH: u32 = 9;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaMarker {
    lifecycle: String,
    epoch: u32,
}

impl SchemaMarker {
    fn current() -> Self {
        Self {
            lifecycle: PRE_V1_LIFECYCLE.to_owned(),
            epoch: DESKTOP_SCHEMA_EPOCH,
        }
    }
}

pub(super) async fn connect(config: &Config) -> Result<DbStore> {
    connect_with(config, |database_url| async move {
        DbStore::connect(&database_url).await
    })
    .await
}

async fn connect_with<C, F>(config: &Config, connector: C) -> Result<DbStore>
where
    C: FnOnce(String) -> F,
    F: Future<Output = Result<DbStore>>,
{
    let needs_marker = prepare(&config.data_dir)?;
    let store = connector(config.database_url()?).await?;
    if needs_marker {
        write_current_marker(&config.data_dir)?;
    }
    Ok(store)
}

/// Prepare the SQLite files and return whether a current marker must be
/// recorded after migrations succeed.
fn prepare(data_dir: &Path) -> Result<bool> {
    let product_major = openwave_core::VERSION
        .split_once('.')
        .map_or(openwave_core::VERSION, |(major, _)| major);
    prepare_for_product_major(data_dir, product_major)
}

fn prepare_for_product_major(data_dir: &Path, product_major: &str) -> Result<bool> {
    if product_major != "0" {
        return Err(AgentError::config(format!(
            "pre-v1 local SQLite schema lifecycle is disabled for product major {product_major}"
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
        Some(saved)
            if saved.lifecycle == PRE_V1_LIFECYCLE && saved.epoch < DESKTOP_SCHEMA_EPOCH =>
        {
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database)?;
            Ok(true)
        }
        None if database.exists() => {
            // Databases predating this marker are from the disposable pre-v1
            // development line. The v1 lifecycle will always retain a marker.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database)?;
            Ok(true)
        }
        None => {
            // Clean up journals left after an interrupted reset even when the
            // main database file is already gone.
            remove_retired_vectors(&data_dir.join(VECTOR_DIRECTORY))?;
            reset_pre_v1_state(&database)?;
            Ok(true)
        }
        Some(saved) => Err(AgentError::config(format!(
            "refusing to reset local SQLite database for schema marker lifecycle {:?}, epoch {}; this binary supports {:?}, epoch {}",
            saved.lifecycle, saved.epoch, PRE_V1_LIFECYCLE, DESKTOP_SCHEMA_EPOCH
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
            "refusing to reset local SQLite database with non-regular schema marker {}",
            marker.display()
        )));
    }
    if metadata.len() > MAX_MARKER_BYTES {
        return Err(AgentError::config(format!(
            "refusing to reset local SQLite database with oversized schema marker {}",
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
            "refusing to reset local SQLite database with oversized schema marker {}",
            marker.display()
        )));
    }
    serde_json::from_slice::<SchemaMarker>(&bytes)
        .map(Some)
        .map_err(|error| {
            AgentError::config(format!(
                "refusing to reset local SQLite database with unreadable schema marker {}: {error}",
                marker.display()
            ))
        })
}

fn reset_pre_v1_state(database: &Path) -> Result<()> {
    remove_sqlite_files(database)
}

fn remove_sqlite_files(database: &Path) -> Result<()> {
    for path in sqlite_files(database) {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(AgentError::config(format!(
                    "failed to reset local SQLite database file {}: {error}",
                    path.display()
                )))
            }
        }
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

#[cfg(unix)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(_temporary: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "atomic schema marker replacement is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use openwave_core::{Chat, ChatId, Store};

    use super::*;

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
        assert_eq!(std::fs::read(broker).unwrap(), b"native broker state");
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
    async fn current_schema_epoch_preserves_database_and_removes_retired_vectors() {
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

    #[tokio::test]
    async fn older_pre_v1_epoch_resets_database() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        let first = connect(&config).await.unwrap();
        first.create_chat(&chat()).await.unwrap();
        // An explicit close, not a drop: the epoch reset below deletes the
        // SQLite files, which Windows refuses while a handle is open.
        first.close().await.unwrap();
        std::fs::write(
            dir.path().join(MARKER_FILE),
            serde_json::to_vec(&SchemaMarker {
                lifecycle: PRE_V1_LIFECYCLE.to_owned(),
                epoch: DESKTOP_SCHEMA_EPOCH - 1,
            })
            .unwrap(),
        )
        .unwrap();

        let reset = connect(&config).await.unwrap();

        assert!(reset.list_chats().await.unwrap().is_empty());
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
        assert!(error.to_string().contains("refusing to reset"));

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
                epoch: DESKTOP_SCHEMA_EPOCH + 1,
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
            epoch: DESKTOP_SCHEMA_EPOCH - 1,
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
            epoch: DESKTOP_SCHEMA_EPOCH - 1,
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
                epoch: DESKTOP_SCHEMA_EPOCH - 1,
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

    #[test]
    fn released_package_guard_prevents_all_reset_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join(DATABASE_FILE);
        let vector = dir.path().join(VECTOR_DIRECTORY).join("keep-me");
        std::fs::write(&database, b"database").unwrap();
        for sidecar in sqlite_files(&database).into_iter().skip(1) {
            std::fs::write(sidecar, b"sidecar").unwrap();
        }
        std::fs::create_dir_all(vector.parent().unwrap()).unwrap();
        std::fs::write(&vector, b"vector").unwrap();

        let error = prepare_for_product_major(dir.path(), "1").unwrap_err();

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

    #[test]
    fn unsupported_lifecycle_performs_no_deletions() {
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

        assert!(prepare(dir.path()).is_err());

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
        assert!(error.to_string().contains("refusing to reset"));

        let unchanged = DbStore::connect(&config.database_url().unwrap())
            .await
            .unwrap();
        assert_eq!(
            unchanged.get_chat(expected.id).await.unwrap(),
            Some(expected)
        );
        assert_eq!(std::fs::read(vector).unwrap(), b"current vector state");
    }
}

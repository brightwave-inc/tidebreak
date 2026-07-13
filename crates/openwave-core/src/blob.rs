//! Durable local [`BlobStore`](crate::BlobStore) implementation.

#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{AgentError, BlobStore, Result};

/// Filesystem-backed opaque byte storage.
///
/// Blob ids are UUIDs and become flat filenames under `root`; caller-controlled
/// paths never participate in path resolution. Writes sync an adjacent temporary
/// file before atomically publishing a new destination, so readers never observe
/// partial bytes. Existing ids accept identical bytes idempotently and reject
/// replacement. Clones share an in-process lock; atomic no-replace publication
/// also coordinates independent instances using the same directory.
#[derive(Clone)]
pub struct FsBlobStore {
    root: Arc<PathBuf>,
    access: Arc<RwLock<()>>,
}

impl FsBlobStore {
    /// Create a store rooted at `root`. The directory is created on first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
            access: Arc::new(RwLock::new(())),
        }
    }

    /// Root directory containing blob files.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blob_path(&self, id: Uuid) -> PathBuf {
        self.root.join(format!("{id}.blob"))
    }
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<()> {
        let destination = self.blob_path(id);
        let root = Arc::clone(&self.root);
        let access = Arc::clone(&self.access);
        tokio::task::spawn_blocking(move || {
            fs::create_dir_all(&*root).map_err(|error| blob_error("create directory", error))?;
            if let Some(parent) = root.parent().filter(|path| !path.as_os_str().is_empty()) {
                sync_directory(parent)?;
            }
            let temporary = root.join(format!(".{}.tmp", Uuid::new_v4()));
            let result = (|| {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                options.mode(0o600);
                let mut file = options
                    .open(&temporary)
                    .map_err(|error| blob_error("create temporary file", error))?;
                file.write_all(&bytes)
                    .map_err(|error| blob_error("write temporary file", error))?;
                file.sync_all()
                    .map_err(|error| blob_error("sync temporary file", error))?;

                let _guard = access
                    .write()
                    .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
                match publish_new_file(&temporary, &destination) {
                    Ok(()) => {
                        // The destination must be durable before best-effort
                        // temporary-name cleanup can report or fail.
                        sync_directory(&root)
                    }
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        let existing = fs::read(&destination)
                            .map_err(|error| blob_error("read immutable file", error))?;
                        if existing == bytes {
                            // A retry may be observing a link published just
                            // before a prior process crashed. Re-establish the
                            // namespace durability barrier before acknowledging.
                            sync_directory(&root)
                        } else {
                            Err(AgentError::Store(
                                "immutable blob id already contains different bytes".into(),
                            ))
                        }
                    }
                    Err(error) => Err(blob_error("publish immutable file", error)),
                }
            })();
            if fs::remove_file(&temporary).is_ok() {
                // Publication is already durable. Failure to persist removal
                // can only leave an unreferenced, hidden temporary file.
                let _ = sync_directory(&root);
            }
            result
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob write task failed: {error}")))?
    }

    async fn get(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(id);
        let access = Arc::clone(&self.access);
        tokio::task::spawn_blocking(move || {
            let _guard = access
                .read()
                .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
            match fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(blob_error("read file", error)),
            }
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob read task failed: {error}")))?
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let path = self.blob_path(id);
        let root = Arc::clone(&self.root);
        let access = Arc::clone(&self.access);
        tokio::task::spawn_blocking(move || {
            let _guard = access
                .write()
                .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
            match fs::remove_file(path) {
                Ok(()) => sync_directory(&root),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => Err(blob_error("delete file", error)),
            }
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob delete task failed: {error}")))?
    }
}

#[cfg(not(windows))]
fn publish_new_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(temporary, destination)
}

#[cfg(windows)]
fn publish_new_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let temporary: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that remain
    // alive for the duration of the call. The paths share one directory/volume.
    let published = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if published == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| blob_error("sync directory", error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn blob_error(action: &str, error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to {action} for blob: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrips_reopens_and_deletes_idempotently() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let store = FsBlobStore::new(directory.path());

        assert_eq!(store.get(id).await.unwrap(), None);
        store.put(id, b"first".to_vec()).await.unwrap();
        assert_eq!(store.get(id).await.unwrap().as_deref(), Some(&b"first"[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.blob_path(id))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.put(id, b"first".to_vec()).await.unwrap();

        let reopened = FsBlobStore::new(directory.path());
        assert_eq!(
            reopened.get(id).await.unwrap().as_deref(),
            Some(&b"first"[..])
        );
        reopened.delete(id).await.unwrap();
        reopened.delete(id).await.unwrap();
        assert_eq!(reopened.get(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn immutable_publication_is_idempotent_and_rejects_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let store = FsBlobStore::new(directory.path());

        store.put(id, b"retained source".to_vec()).await.unwrap();
        store.put(id, b"retained source".to_vec()).await.unwrap();
        assert!(store.put(id, b"different source".to_vec()).await.is_err());
        assert_eq!(
            store.get(id).await.unwrap().as_deref(),
            Some(&b"retained source"[..])
        );
    }

    #[tokio::test]
    async fn independent_stores_choose_one_complete_immutable_value() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let left_store = FsBlobStore::new(directory.path());
        let right_store = FsBlobStore::new(directory.path());
        let first = vec![b'a'; 128 * 1024];
        let second = vec![b'b'; 128 * 1024];

        let (left, right) = tokio::join!(
            left_store.put(id, first.clone()),
            right_store.put(id, second.clone())
        );
        assert_ne!(left.is_ok(), right.is_ok());
        let stored = left_store.get(id).await.unwrap().unwrap();
        assert!(stored == first || stored == second);
    }
}

//! Durable local [`BlobStore`](crate::BlobStore) implementation.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::StreamExt;
use uuid::Uuid;

use crate::{
    AgentError, BlobInventoryItem, BlobMetadata, BlobStore, BlobStream, DocumentBlob, Result,
};

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

    async fn put_stream(&self, source: DocumentBlob, mut chunks: BlobStream) -> Result<()> {
        if !source.has_content_addressed_id() {
            return Err(AgentError::Store(
                "streamed source blob id does not match its SHA-256 digest".into(),
            ));
        }
        let root = Arc::clone(&self.root);
        let access = Arc::clone(&self.access);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let writer = tokio::task::spawn_blocking(move || {
            write_streamed_blob(&root, &access, source, &mut receiver)
        });

        while let Some(chunk) = chunks.next().await {
            let is_failure = chunk.is_err();
            if sender.send(chunk).await.is_err() {
                break;
            }
            if is_failure {
                break;
            }
        }
        drop(sender);
        writer
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

    async fn metadata(&self, id: Uuid) -> Result<Option<BlobMetadata>> {
        let path = self.blob_path(id);
        let access = Arc::clone(&self.access);
        tokio::task::spawn_blocking(move || {
            let _guard = access
                .read()
                .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
            match fs::metadata(path) {
                Ok(metadata) => Ok(Some(BlobMetadata {
                    byte_len: metadata.len(),
                })),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(blob_error("read metadata", error)),
            }
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob metadata task failed: {error}")))?
    }

    async fn read_range(
        &self,
        id: Uuid,
        range: std::ops::Range<u64>,
    ) -> Result<Option<BlobStream>> {
        if range.start > range.end {
            return Err(AgentError::Store("blob range start exceeds its end".into()));
        }
        let path = self.blob_path(id);
        let access = Arc::clone(&self.access);
        let start = range.start;
        let file = tokio::task::spawn_blocking(move || {
            let _guard = access
                .read()
                .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
            let mut file = match File::open(path) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(blob_error("open range reader", error)),
            };
            file.seek(SeekFrom::Start(start))
                .map_err(|error| blob_error("seek range reader", error))?;
            Ok(Some(file))
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob range task failed: {error}")))??;
        let Some(file) = file else {
            return Ok(None);
        };

        let remaining = range.end - range.start;
        let stream = async_stream::try_stream! {
            use tokio::io::AsyncReadExt;

            let mut file = tokio::fs::File::from_std(file);
            let mut remaining = remaining;
            let mut buffer = vec![0; 64 * 1024];
            while remaining > 0 {
                let limit = usize::try_from(remaining.min(buffer.len() as u64))
                    .expect("bounded buffer length fits in usize");
                let read = file
                    .read(&mut buffer[..limit])
                    .await
                    .map_err(|error| blob_error("read range", error))?;
                if read == 0 {
                    Err(AgentError::Store("blob ended before the requested range".into()))?;
                }
                remaining -= u64::try_from(read).expect("usize always fits in u64");
                yield buffer[..read].to_vec();
            }
        };
        Ok(Some(Box::pin(stream)))
    }

    async fn inventory(&self) -> Result<Vec<BlobInventoryItem>> {
        let root = Arc::clone(&self.root);
        tokio::task::spawn_blocking(move || inventory(&root))
            .await
            .map_err(|error| AgentError::Store(format!("blob inventory task failed: {error}")))?
    }

    async fn modified_at(&self, id: Uuid) -> Result<Option<std::time::SystemTime>> {
        let path = self.blob_path(id);
        tokio::task::spawn_blocking(move || match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => metadata
                .modified()
                .map(Some)
                .map_err(|error| blob_error("read modification time", error)),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(blob_error("read metadata", error)),
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob metadata task failed: {error}")))?
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

fn inventory(root: &Path) -> Result<Vec<BlobInventoryItem>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(blob_error("read directory", error)),
    };
    let mut items = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| blob_error("read directory entry", error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = name.strip_suffix(".blob").and_then(|id| id.parse().ok()) else {
            continue;
        };
        if name != format!("{id}.blob") {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| blob_error("read file type", error))?;
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| blob_error("read metadata", error))?;
        items.push(BlobInventoryItem {
            id,
            modified_at: metadata
                .modified()
                .map_err(|error| blob_error("read modification time", error))?,
        });
    }
    items.sort_unstable_by_key(|item| item.id);
    Ok(items)
}

fn write_streamed_blob(
    root: &Path,
    access: &RwLock<()>,
    source: DocumentBlob,
    receiver: &mut tokio::sync::mpsc::Receiver<Result<Vec<u8>>>,
) -> Result<()> {
    use sha2::{Digest, Sha256};

    fs::create_dir_all(root).map_err(|error| blob_error("create directory", error))?;
    if let Some(parent) = root.parent().filter(|path| !path.as_os_str().is_empty()) {
        sync_directory(parent)?;
    }
    let temporary = root.join(format!(".{}.tmp", Uuid::new_v4()));
    let destination = root.join(format!("{}.blob", source.id));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary)
            .map_err(|error| blob_error("create temporary file", error))?;
        let mut digest = Sha256::new();
        let mut byte_len = 0_u64;
        while let Some(chunk) = receiver.blocking_recv() {
            let chunk = chunk?;
            byte_len = byte_len
                .checked_add(u64::try_from(chunk.len()).map_err(|_| {
                    AgentError::Store("streamed blob chunk length exceeds u64".into())
                })?)
                .ok_or_else(|| AgentError::Store("streamed blob length exceeds u64".into()))?;
            if byte_len > source.byte_len {
                return Err(AgentError::Store(
                    "streamed blob exceeds its declared byte length".into(),
                ));
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .map_err(|error| blob_error("write temporary file", error))?;
        }
        let sha256: [u8; 32] = digest.finalize().into();
        if DocumentBlob::from_digest(sha256, byte_len) != source {
            return Err(AgentError::Store(
                "streamed blob does not match its declared digest".into(),
            ));
        }
        file.sync_all()
            .map_err(|error| blob_error("sync temporary file", error))?;
        drop(file);

        let _guard = access
            .write()
            .map_err(|_| AgentError::Store("blob store lock poisoned".into()))?;
        match publish_new_file(&temporary, &destination) {
            Ok(()) => sync_directory(root),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if existing_file_matches_source(&destination, &source)? {
                    sync_directory(root)
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
        let _ = sync_directory(root);
    }
    result
}

fn existing_file_matches_source(path: &Path, source: &DocumentBlob) -> Result<bool> {
    use sha2::{Digest, Sha256};

    let mut file =
        fs::File::open(path).map_err(|error| blob_error("read immutable file", error))?;
    let mut digest = Sha256::new();
    let mut byte_len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| blob_error("read immutable file", error))?;
        if read == 0 {
            break;
        }
        byte_len = byte_len
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| AgentError::Store("stored blob length exceeds u64".into()))?;
        digest.update(&buffer[..read]);
    }
    let sha256: [u8; 32] = digest.finalize().into();
    Ok(DocumentBlob::from_digest(sha256, byte_len) == *source)
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

fn sync_directory(path: &Path) -> Result<()> {
    crate::sync_directory(path).map_err(|error| blob_error("sync directory", error))
}

fn blob_error(action: &str, error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to {action} for blob: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;

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
    async fn reads_metadata_and_a_bounded_range_stream() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let store = FsBlobStore::new(directory.path());
        let bytes: Vec<u8> = (0..(2 * 64 * 1024))
            .map(|index| u8::try_from(index % 251).expect("remainder fits in u8"))
            .collect();
        store.put(id, bytes.clone()).await.unwrap();

        assert_eq!(
            store.metadata(id).await.unwrap(),
            Some(BlobMetadata {
                byte_len: u64::try_from(bytes.len()).expect("usize always fits in u64"),
            })
        );
        let range = 123..(123 + 64 * 1024 + 17);
        let chunks = store
            .read_range(id, range.clone())
            .await
            .unwrap()
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(chunks.iter().all(|chunk| chunk.len() <= 64 * 1024));
        assert_eq!(
            chunks.concat(),
            bytes[usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()]
        );
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
    async fn streams_content_addressed_bytes_without_a_full_input_buffer() {
        use futures::{stream, StreamExt};

        let directory = tempfile::tempdir().unwrap();
        let store = FsBlobStore::new(directory.path());
        let bytes = [vec![b'a'; 64 * 1024], vec![b'b'; 64 * 1024], vec![b'c'; 17]].concat();
        let source = DocumentBlob::from_bytes(&bytes);
        let chunks = stream::iter(vec![
            Ok(bytes[..64 * 1024].to_vec()),
            Ok(bytes[64 * 1024..128 * 1024].to_vec()),
            Ok(bytes[128 * 1024..].to_vec()),
        ])
        .boxed();

        store.put_stream(source.clone(), chunks).await.unwrap();
        assert_eq!(
            store.get(source.id).await.unwrap().as_deref(),
            Some(bytes.as_slice())
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

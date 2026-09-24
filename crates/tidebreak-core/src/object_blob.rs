//! Object-storage [`BlobStore`](crate::BlobStore) implementation for the
//! self-host profile: an S3-compatible bucket, or a directory on the machine's
//! own disk behind the same `object_store` contract.
//!
//! On local disk this module also does the work a bucket does for itself.
//! Every blob and staged upload is readable by the server's user only, and
//! upload leftovers are removed: a failed upload removes its own staged parts
//! at once, and a boot removes whatever a crash left more than a day ago.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::StreamExt;
use object_store::aws::{AmazonS3Builder, S3CopyIfNotExists};
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{
    Error as ObjectError, ObjectStore, ObjectStoreExt, PutMode, PutOptions, PutPayload,
};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::{
    AgentError, BlobInventoryItem, BlobMetadata, BlobStore, BlobStream, DocumentBlob, Result,
};

const MULTIPART_CHUNK_BYTES: usize = 5 * 1024 * 1024;

/// The refusal for a URL that names neither backend. It never echoes the
/// value, which may carry credentials.
const UNSUPPORTED_BLOB_STORE_URL: &str =
    "TIDEBREAK_BLOB_STORE_URL must be s3://bucket[/prefix] or file:///absolute/path";

/// Where writes land before they publish, below the store's prefix.
const UPLOADS: &str = "_uploads";

/// How old an entry in `_uploads/` must be before a boot removes it. One
/// server process owns a deployment and no upload takes a day, so an entry
/// that old was left by a crash or a failed publish.
const STALE_UPLOAD_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct ObjectBlobStore {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
    /// The same store when it is a directory on this machine, for what
    /// `object_store` leaves to its caller there: file modes and upload
    /// leftovers.
    disk: Option<LocalFileSystem>,
}

impl ObjectBlobStore {
    #[must_use]
    pub fn new(store: Arc<dyn ObjectStore>, prefix: Path) -> Self {
        Self {
            store,
            prefix,
            disk: None,
        }
    }

    /// Build the self-host backend `TIDEBREAK_BLOB_STORE_URL` names: an S3
    /// bucket (`s3://bucket[/prefix]`) or a directory on this machine's disk
    /// (`file:///absolute/path`).
    pub fn from_url(value: &str) -> Result<Self> {
        let url = Url::parse(value).map_err(|_| AgentError::config(UNSUPPORTED_BLOB_STORE_URL))?;
        match url.scheme() {
            "s3" => Self::from_s3_url(value),
            "file" => Self::from_file_url(value),
            _ => Err(AgentError::config(UNSUPPORTED_BLOB_STORE_URL)),
        }
    }

    /// Build the self-host backend from `s3://bucket[/prefix]` and standard
    /// `AWS_*` settings.
    pub fn from_s3_url(value: &str) -> Result<Self> {
        let prefix = parse_s3_prefix(value)?;
        let store = AmazonS3Builder::from_env()
            .with_url(value)
            .with_copy_if_not_exists(S3CopyIfNotExists::Multipart)
            .build()
            .map_err(|_| AgentError::config("invalid S3 object-store configuration"))?;
        Ok(Self::new(Arc::new(store), prefix))
    }

    /// Build the self-host backend over a directory on this machine's disk,
    /// named by `file:///absolute/path`.
    ///
    /// A missing directory is created readable by its owner only, and so is
    /// its `_uploads/` staging area. Every write lands there first, is
    /// restricted to the server's user, and only then publishes by hard link,
    /// so a blob is never readable by others, even for a moment. Every write
    /// is synced before it is acknowledged, as a bucket would be. Staged
    /// uploads older than a day are removed here, at boot.
    pub fn from_file_url(value: &str) -> Result<Self> {
        let root = parse_file_root(value)?;
        create_private_directory(&root)?;
        let disk = LocalFileSystem::new_with_prefix(&root)
            .map_err(|error| {
                AgentError::config(format!(
                    "cannot open the blob directory {}: {}",
                    root.display(),
                    failure_kind(&error)
                ))
            })?
            .with_fsync(true);
        let uploads = disk
            .path_to_filesystem(&Path::from(UPLOADS))
            .map_err(|error| {
                AgentError::config(format!(
                    "cannot open the blob directory {}: {}",
                    root.display(),
                    failure_kind(&error)
                ))
            })?;
        create_private_directory(&uploads)?;
        reclaim_stale_uploads(&uploads, STALE_UPLOAD_AGE);
        Ok(Self {
            store: Arc::new(disk.clone()),
            prefix: Path::default(),
            disk: Some(disk),
        })
    }

    /// Check at boot that the store takes a write, a delete, and a listing,
    /// so a store the server cannot use refuses the boot with the reason
    /// instead of failing every upload later.
    pub async fn probe(&self) -> Result<()> {
        let object = self
            .prefix
            .clone()
            .join(UPLOADS)
            .join(format!("probe-{}.tmp", Uuid::new_v4()));
        self.store
            .put(
                &object,
                PutPayload::from_static(b"tidebreak blob store probe"),
            )
            .await
            .map_err(|error| object_error("write a test object to the blob store", &error))?;
        self.store
            .delete(&object)
            .await
            .map_err(|error| object_error("delete a test object from the blob store", &error))?;
        match &self.disk {
            // One directory read. A recursive listing would open every
            // subdirectory, including ones the server has no reason to read.
            Some(disk) => {
                let root = disk
                    .path_to_filesystem(&Path::from(UPLOADS))
                    .ok()
                    .and_then(|uploads| uploads.parent().map(std::path::Path::to_path_buf));
                if let Some(root) = root {
                    std::fs::read_dir(&root)
                        .and_then(|mut entries| entries.next().transpose())
                        .map_err(|error| {
                            AgentError::Store(format!(
                                "failed to list the blob directory: {}",
                                error.kind()
                            ))
                        })?;
                }
            }
            None => {
                if let Some(result) = self.store.list(Some(&self.prefix)).next().await {
                    result.map_err(|error| object_error("list the blob store", &error))?;
                }
            }
        }
        Ok(())
    }

    fn path(&self, id: Uuid) -> Path {
        self.prefix.clone().join(format!("{id}.blob"))
    }

    fn temporary_path(&self) -> Path {
        self.prefix
            .clone()
            .join(UPLOADS)
            .join(format!("{}.tmp", Uuid::new_v4()))
    }

    /// On local disk, make a staged object readable by the server's user
    /// only. It sits in the owner-only `_uploads/`, so it was never exposed;
    /// this is what keeps it private once it is linked into place.
    async fn restrict(&self, temporary: &Path) -> std::result::Result<(), ObjectError> {
        let Some(disk) = &self.disk else {
            return Ok(());
        };
        let path = disk.path_to_filesystem(temporary)?;
        tokio::task::spawn_blocking(move || restrict_to_owner(&path))
            .await
            .map_err(|error| ObjectError::JoinError { source: error })?
            .map_err(|error| ObjectError::Generic {
                store: "LocalFileSystem",
                source: Box::new(error),
            })
    }

    /// Remove what a failed upload left in `_uploads/`. On local disk that
    /// includes the parts `object_store` stages as `<object>#<n>`: it cannot
    /// address those names itself, and after a failed completion its own
    /// abort no longer knows them.
    async fn discard_failed_upload(&self, temporary: &Path) {
        if let Some(disk) = &self.disk {
            if let Ok(path) = disk.path_to_filesystem(temporary) {
                let _ = tokio::task::spawn_blocking(move || remove_staged_parts(&path)).await;
            }
        }
    }

    /// Local disk: stage the bytes under `_uploads/`, restrict them, and link
    /// them into place. A bucket publishes with one conditional put instead.
    async fn publish_through_uploads(
        &self,
        path: &Path,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), ObjectError> {
        let temporary = self.temporary_path();
        let result = async {
            self.store.put(&temporary, bytes.into()).await?;
            self.restrict(&temporary).await?;
            self.store.copy_if_not_exists(&temporary, path).await
        }
        .await;
        if result.is_err() {
            self.discard_failed_upload(&temporary).await;
        }
        let _ = self.store.delete(&temporary).await;
        result
    }

    async fn existing_matches(&self, id: Uuid, expected: &[u8]) -> Result<bool> {
        let Some(bytes) = self.get(id).await? else {
            return Ok(false);
        };
        Ok(bytes == expected)
    }

    async fn existing_matches_source(&self, source: DocumentBlob) -> Result<bool> {
        let path = self.path(source.id);
        let result = match self.store.get(&path).await {
            Ok(result) => result,
            Err(ObjectError::NotFound { .. }) => return Ok(false),
            Err(error) => return Err(object_error("read immutable object", &error)),
        };
        let mut digest = Sha256::new();
        let mut byte_len = 0_u64;
        let mut stream = result.into_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| object_error("read immutable object", &error))?;
            byte_len =
                byte_len
                    .checked_add(u64::try_from(chunk.len()).map_err(|_| {
                        AgentError::Store("stored object length exceeds u64".into())
                    })?)
                    .ok_or_else(|| AgentError::Store("stored object length exceeds u64".into()))?;
            digest.update(&chunk);
        }
        Ok(DocumentBlob::from_digest(digest.finalize().into(), byte_len) == source)
    }
}

fn parse_s3_prefix(value: &str) -> Result<Path> {
    let url =
        Url::parse(value).map_err(|_| AgentError::config("invalid TIDEBREAK_BLOB_STORE_URL"))?;
    if url.scheme() != "s3"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AgentError::config(
            "TIDEBREAK_BLOB_STORE_URL must be s3://bucket[/prefix] with no credentials, query, or fragment",
        ));
    }
    Path::from_url_path(url.path())
        .map_err(|_| AgentError::config("invalid TIDEBREAK_BLOB_STORE_URL prefix"))
}

/// The directory a `file:///absolute/path` URL names.
///
/// The spelling is checked before the URL is trusted, and the path again
/// after decoding: a parser reads `file:blobs` as `/blobs` and resolves `..`,
/// and an encoded `/` becomes a separator only when the path is decoded, so a
/// relative or unnormalized value would quietly move the store somewhere the
/// operator did not write.
fn parse_file_root(value: &str) -> Result<std::path::PathBuf> {
    use std::path::Component;

    let invalid = || {
        AgentError::config(
            "TIDEBREAK_BLOB_STORE_URL must be file:///absolute/path naming a directory below \
             the root: no host, no `.` or `..` segments, no encoded separators, no query or \
             fragment, and special characters percent-encoded",
        )
    };
    let path = value.strip_prefix("file://").ok_or_else(invalid)?;
    let spelled = path.to_ascii_lowercase();
    if !path.starts_with('/')
        || path.contains("//")
        || spelled.contains("%2f")
        || spelled.contains("%5c")
        || spelled.contains("%00")
    {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    if url.path() != path || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid());
    }
    let root = url.to_file_path().map_err(|_| invalid())?;
    let mut names = 0_usize;
    for component in root.components() {
        match component {
            Component::Normal(_) => names += 1,
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir | Component::ParentDir => return Err(invalid()),
        }
    }
    if !root.is_absolute() || names == 0 {
        return Err(invalid());
    }
    Ok(root)
}

/// Create `dir` and any missing parents, readable by the server's user only.
/// An existing directory keeps the permissions its operator gave it.
fn create_private_directory(dir: &std::path::Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|error| {
        AgentError::config(format!(
            "cannot create the blob directory {}: {error}",
            dir.display()
        ))
    })
}

fn restrict_to_owner(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Remove the `<object>#<n>` parts staged for one upload's `object`.
fn remove_staged_parts(object: &std::path::Path) {
    let (Some(dir), Some(name)) = (object.parent(), object.file_name()) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut staged = name.to_os_string();
    staged.push("#");
    for entry in entries.flatten() {
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(staged.as_encoded_bytes())
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Remove the files in `uploads` last changed more than `age` ago. Returns
/// how many it removed.
fn reclaim_stale_uploads(uploads: &std::path::Path, age: Duration) -> usize {
    let Some(cutoff) = SystemTime::now().checked_sub(age) else {
        return 0;
    };
    let entries = match std::fs::read_dir(uploads) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                "could not read {} to remove stale uploads: {}",
                uploads.display(),
                error.kind()
            );
            return 0;
        }
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let stale = metadata.modified().is_ok_and(|modified| modified < cutoff);
        if metadata.is_file() && stale && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(
            removed,
            "removed uploads older than a day from {}",
            uploads.display()
        );
    }
    removed
}

#[async_trait]
impl BlobStore for ObjectBlobStore {
    async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<()> {
        let path = self.path(id);
        let published = if self.disk.is_some() {
            self.publish_through_uploads(&path, bytes.clone()).await
        } else {
            self.store
                .put_opts(
                    &path,
                    bytes.clone().into(),
                    PutOptions::from(PutMode::Create),
                )
                .await
                .map(|_| ())
        };
        match published {
            Ok(()) => Ok(()),
            Err(ObjectError::AlreadyExists { .. }) => {
                if self.existing_matches(id, &bytes).await? {
                    Ok(())
                } else {
                    Err(AgentError::Store(
                        "immutable blob id already contains different bytes".into(),
                    ))
                }
            }
            Err(error) => Err(object_error("publish immutable object", &error)),
        }
    }

    async fn put_stream(&self, source: DocumentBlob, mut chunks: BlobStream) -> Result<()> {
        if !source.has_content_addressed_id() {
            return Err(AgentError::Store(
                "streamed source blob id does not match its SHA-256 digest".into(),
            ));
        }

        let temporary = self.temporary_path();
        let mut upload = self
            .store
            .put_multipart(&temporary)
            .await
            .map_err(|error| object_error("start streamed object upload", &error))?;
        let mut digest = Sha256::new();
        let mut byte_len = 0_u64;
        let mut buffered = Vec::with_capacity(MULTIPART_CHUNK_BYTES);
        let mut multipart_completed = false;
        let result: Result<()> = async {
            while let Some(chunk) = chunks.next().await {
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
                let mut offset = 0;
                while offset < chunk.len() {
                    let available = MULTIPART_CHUNK_BYTES - buffered.len();
                    let take = available.min(chunk.len() - offset);
                    buffered.extend_from_slice(&chunk[offset..offset + take]);
                    offset += take;
                    if buffered.len() == MULTIPART_CHUNK_BYTES {
                        upload
                            .put_part(std::mem::take(&mut buffered).into())
                            .await
                            .map_err(|error| object_error("write streamed object part", &error))?;
                        buffered = Vec::with_capacity(MULTIPART_CHUNK_BYTES);
                    }
                }
            }
            if DocumentBlob::from_digest(digest.finalize().into(), byte_len) != source {
                return Err(AgentError::Store(
                    "streamed blob does not match its declared digest".into(),
                ));
            }
            if !buffered.is_empty() {
                upload
                    .put_part(std::mem::take(&mut buffered).into())
                    .await
                    .map_err(|error| object_error("write streamed object part", &error))?;
            }
            upload
                .complete()
                .await
                .map_err(|error| object_error("complete streamed object upload", &error))?;
            multipart_completed = true;
            self.restrict(&temporary)
                .await
                .map_err(|error| object_error("restrict streamed object", &error))?;

            let destination = self.path(source.id);
            match self
                .store
                .copy_if_not_exists(&temporary, &destination)
                .await
            {
                Ok(()) => Ok(()),
                Err(ObjectError::AlreadyExists { .. }) => {
                    if self.existing_matches_source(source).await? {
                        Ok(())
                    } else {
                        Err(AgentError::Store(
                            "immutable blob id already contains different bytes".into(),
                        ))
                    }
                }
                Err(error) => Err(object_error("publish streamed object", &error)),
            }
        }
        .await;

        if result.is_err() {
            if !multipart_completed {
                let _ = upload.abort().await;
            }
            self.discard_failed_upload(&temporary).await;
        }
        let _ = self.store.delete(&temporary).await;
        result
    }

    async fn get(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
        let result = match self.store.get(&self.path(id)).await {
            Ok(result) => result,
            Err(ObjectError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(object_error("read object", &error)),
        };
        let bytes = result
            .bytes()
            .await
            .map_err(|error| object_error("read object body", &error))?;
        Ok(Some(bytes.to_vec()))
    }

    async fn metadata(&self, id: Uuid) -> Result<Option<BlobMetadata>> {
        match self.store.head(&self.path(id)).await {
            Ok(metadata) => Ok(Some(BlobMetadata {
                byte_len: metadata.size,
            })),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(error) => Err(object_error("read object metadata", &error)),
        }
    }

    async fn read_range(
        &self,
        id: Uuid,
        range: std::ops::Range<u64>,
    ) -> Result<Option<BlobStream>> {
        if range.start > range.end {
            return Err(AgentError::Store("blob range start exceeds its end".into()));
        }
        let result = match self
            .store
            .get_opts(
                &self.path(id),
                object_store::GetOptions::new().with_range(Some(range)),
            )
            .await
        {
            Ok(result) => result,
            Err(ObjectError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(object_error("read object range", &error)),
        };
        let stream = result.into_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|error| object_error("read object range body", &error))
        });
        Ok(Some(Box::pin(stream)))
    }

    async fn inventory(&self) -> Result<Vec<BlobInventoryItem>> {
        // Blobs sit directly below the prefix, so list that one level. A
        // nested directory the server cannot read, such as `lost+found` at the
        // root of a mounted volume, is never opened, and neither is
        // `_uploads/`.
        let listing = self
            .store
            .list_with_delimiter(Some(&self.prefix))
            .await
            .map_err(|error| object_error("list objects", &error))?;
        let mut items = Vec::new();
        for metadata in listing.objects {
            let Some(mut suffix) = metadata.location.prefix_match(&self.prefix) else {
                continue;
            };
            let Some(part) = suffix.next() else {
                continue;
            };
            if suffix.next().is_some() {
                continue;
            }
            let name = part.as_ref();
            let Some(id) = name.strip_suffix(".blob").and_then(|id| id.parse().ok()) else {
                continue;
            };
            if name != format!("{id}.blob") {
                continue;
            }
            items.push(BlobInventoryItem {
                id,
                modified_at: metadata.last_modified.into(),
            });
        }
        items.sort_unstable_by_key(|item| item.id);
        Ok(items)
    }

    async fn modified_at(&self, id: Uuid) -> Result<Option<std::time::SystemTime>> {
        match self.store.head(&self.path(id)).await {
            Ok(metadata) => Ok(Some(metadata.last_modified.into())),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(error) => Err(object_error("read object metadata", &error)),
        }
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        match self.store.delete(&self.path(id)).await {
            Ok(()) | Err(ObjectError::NotFound { .. }) => Ok(()),
            Err(error) => Err(object_error("delete object", &error)),
        }
    }
}

/// A storage failure that names its cause by kind only. The full error text
/// carries a request URL for a bucket and a path for a directory, so it stays
/// out of messages callers may show.
fn object_error(action: &str, error: &ObjectError) -> AgentError {
    AgentError::Store(format!("failed to {action}: {}", failure_kind(error)))
}

fn failure_kind(error: &ObjectError) -> String {
    match error {
        ObjectError::NotFound { .. } => "not found".into(),
        ObjectError::AlreadyExists { .. } => "already exists".into(),
        ObjectError::PermissionDenied { .. } => "permission denied".into(),
        ObjectError::Unauthenticated { .. } => "the store did not accept the credentials".into(),
        ObjectError::Precondition { .. } | ObjectError::NotModified { .. } => {
            "the object changed during the request".into()
        }
        ObjectError::NotSupported { .. } | ObjectError::NotImplemented { .. } => {
            "not supported by this store".into()
        }
        _ => io_error_kind(error).map_or_else(
            || "storage error".into(),
            |kind| {
                if kind == std::io::ErrorKind::Other {
                    "storage error".into()
                } else {
                    kind.to_string()
                }
            },
        ),
    }
}

/// The kind of the first I/O error in `error`'s source chain.
fn io_error_kind(error: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use futures::stream;
    use object_store::memory::InMemory;

    use super::*;

    fn store() -> ObjectBlobStore {
        ObjectBlobStore::new(Arc::new(InMemory::new()), Path::from("tidebreak/blobs"))
    }

    fn file_url(path: &std::path::Path) -> String {
        Url::from_file_path(path).unwrap().to_string()
    }

    /// A blob directory inside a fresh temporary directory, and the guard
    /// that removes it.
    fn disk_store() -> (ObjectBlobStore, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        (
            ObjectBlobStore::from_url(&file_url(&root)).unwrap(),
            root,
            dir,
        )
    }

    /// Run one contract against both backends the self-host profile can
    /// select. The disk one differs where it matters: it refuses a second
    /// create through a hard link, stages multipart parts as `#`-suffixed
    /// files, and lists a real directory tree.
    async fn for_each_backend<F, Fut>(check: F)
    where
        F: Fn(ObjectBlobStore) -> Fut,
        Fut: Future<Output = ()>,
    {
        check(store()).await;
        let (disk, _root, _dir) = disk_store();
        check(disk).await;
    }

    /// Every regular file below `root`, relative to it.
    fn files_below(root: &std::path::Path) -> Vec<String> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        files.sort();
        files
    }

    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Whether this process ignores file permissions, as root does. The
    /// permission tests have nothing to observe there.
    #[cfg(unix)]
    fn permissions_are_enforced(dir: &std::path::Path) -> bool {
        let probe = dir.join("permission-probe");
        std::fs::create_dir(&probe).unwrap();
        set_mode(&probe, 0o500);
        let enforced = std::fs::write(probe.join("file"), b"x").is_err();
        set_mode(&probe, 0o700);
        std::fs::remove_dir_all(&probe).unwrap();
        enforced
    }

    fn backdate(path: &std::path::Path, age: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn s3_urls_accept_an_optional_prefix_and_redact_rejected_credentials() {
        assert_eq!(parse_s3_prefix("s3://bucket").unwrap(), Path::default());
        assert_eq!(
            parse_s3_prefix("s3://bucket/company/tidebreak").unwrap(),
            Path::from("company/tidebreak")
        );

        for value in [
            "https://bucket/company",
            "s3:///company",
            "s3://bucket:9000/company",
            "s3://access:secret@bucket/company",
            "s3://bucket/company?token=secret",
            "s3://bucket/company#secret",
        ] {
            let error = parse_s3_prefix(value).unwrap_err().to_string();
            assert!(!error.contains("access"));
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn blob_store_urls_dispatch_by_scheme_and_name_both_forms() {
        for value in [
            "https://access:secret@bucket/company",
            "gs://bucket/secret",
            "blobs/secret",
            "",
        ] {
            let error = ObjectBlobStore::from_url(value).err().unwrap().to_string();
            assert!(error.contains("s3://bucket[/prefix]"), "{error}");
            assert!(error.contains("file:///absolute/path"), "{error}");
            assert!(!error.contains("access") && !error.contains("secret"));
        }
        // Each scheme reaches its own validation.
        let s3 = ObjectBlobStore::from_url("s3://access:secret@bucket/company")
            .err()
            .unwrap()
            .to_string();
        assert!(
            s3.contains("s3://bucket[/prefix] with no credentials"),
            "{s3}"
        );
        assert!(!s3.contains("access") && !s3.contains("secret"));
        let file = ObjectBlobStore::from_url("file:blobs")
            .err()
            .unwrap()
            .to_string();
        assert!(
            file.contains("file:///absolute/path naming a directory"),
            "{file}"
        );
    }

    #[test]
    fn file_urls_name_one_absolute_directory() {
        assert_eq!(
            parse_file_root("file:///var/lib/tidebreak/blobs").unwrap(),
            std::path::PathBuf::from("/var/lib/tidebreak/blobs")
        );
        assert_eq!(
            parse_file_root("file:///srv/tidebreak%20blobs/").unwrap(),
            std::path::PathBuf::from("/srv/tidebreak blobs/")
        );
        for value in [
            // Relative, however it is spelled.
            "file:blobs",
            "file:./blobs",
            "file://blobs",
            "file://./blobs",
            // A host, even the local one.
            "file://localhost/var/lib/tidebreak/blobs",
            "file://server/share/blobs",
            // The root itself, and an empty segment.
            "file:///",
            "file:////var/lib/blobs",
            "file:///var//lib/blobs",
            // Anything a parser would rewrite.
            "file:///var/lib/../blobs",
            "file:///var/./lib/blobs",
            "file:///var/lib/%2e%2e/blobs",
            "file:///var\\lib\\blobs",
            "file:///var/lib/tidebreak blobs",
            "file:///var/lib/blobs?mode=0700",
            "file:///var/lib/blobs#blobs",
            "FILE:///var/lib/blobs",
            // A separator that appears only once the path is decoded.
            "file:///srv/tidebreak/%2F..%2Fescaped",
            "file:///srv/tidebreak/..%2fescaped",
            "file:///srv/tidebreak/%5C..%5Cescaped",
            "file:///srv/tidebreak/blobs%00",
        ] {
            let error = parse_file_root(value).unwrap_err().to_string();
            assert!(error.contains("file:///absolute/path"), "{value}: {error}");
        }
    }

    #[test]
    fn a_missing_blob_directory_is_created_for_its_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data").join("blobs");
        ObjectBlobStore::from_url(&file_url(&root)).unwrap();
        assert!(root.is_dir());
        assert!(root.join(UPLOADS).is_dir());
        #[cfg(unix)]
        {
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&root.join(UPLOADS)), 0o700);
        }

        // A file where the directory should be is refused at boot, by path.
        let occupied = dir.path().join("occupied");
        std::fs::write(&occupied, b"not a directory").unwrap();
        let error = ObjectBlobStore::from_url(&file_url(&occupied))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("blob directory"), "{error}");
        assert!(error.contains("occupied"), "{error}");
    }

    /// An operator may create the directory with wider permissions. The
    /// server leaves those alone, and still writes nothing others can read.
    #[cfg(unix)]
    #[tokio::test]
    async fn blobs_and_staged_uploads_are_readable_by_the_server_user_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        std::fs::create_dir(&root).unwrap();
        set_mode(&root, 0o755);
        let store = ObjectBlobStore::from_url(&file_url(&root)).unwrap();
        store.probe().await.unwrap();

        let small = Uuid::new_v4();
        store.put(small, b"small".to_vec()).await.unwrap();
        let bytes = vec![5_u8; MULTIPART_CHUNK_BYTES + 3];
        let streamed = DocumentBlob::from_bytes(&bytes);
        store
            .put_stream(streamed.clone(), stream::iter(vec![Ok(bytes)]).boxed())
            .await
            .unwrap();

        assert_eq!(mode(&root), 0o755);
        assert_eq!(mode(&root.join(UPLOADS)), 0o700);
        assert_eq!(mode(&root.join(format!("{small}.blob"))), 0o600);
        assert_eq!(mode(&root.join(format!("{}.blob", streamed.id))), 0o600);
        assert_eq!(files_below(&root), {
            let mut expected = vec![format!("{small}.blob"), format!("{}.blob", streamed.id)];
            expected.sort();
            expected
        });
    }

    #[tokio::test]
    async fn a_boot_removes_uploads_a_crash_left_more_than_a_day_ago() {
        let (store, root, _dir) = disk_store();

        // A process that dies mid-upload runs no destructor; forgetting the
        // upload stands in for that. Its staged part stays on disk, hidden
        // from listings by object_store's `#<n>` naming.
        let bytes = vec![9_u8; MULTIPART_CHUNK_BYTES * 2];
        let source = DocumentBlob::from_bytes(&bytes);
        let chunks = stream::iter(vec![Ok(bytes[..=MULTIPART_CHUNK_BYTES].to_vec())])
            .chain(stream::pending())
            .boxed();
        let mut upload = Box::pin(store.put_stream(source, chunks));
        let _ = tokio::time::timeout(Duration::from_millis(500), &mut upload).await;
        std::mem::forget(upload);
        let crashed: Vec<_> = files_below(&root.join(UPLOADS));
        assert_eq!(crashed.len(), 1, "{crashed:?}");
        assert!(crashed[0].ends_with(".tmp#1"), "{crashed:?}");

        let kept = Uuid::new_v4();
        store.put(kept, b"kept".to_vec()).await.unwrap();
        let old_temporary = root.join(UPLOADS).join(format!("{}.tmp", Uuid::new_v4()));
        std::fs::write(&old_temporary, b"left by a failed publish").unwrap();
        let fresh = root.join(UPLOADS).join(format!("{}.tmp#1", Uuid::new_v4()));
        std::fs::write(&fresh, b"an upload in progress").unwrap();

        // A boot within the day keeps everything; a later one removes the
        // two old entries and nothing else.
        ObjectBlobStore::from_url(&file_url(&root)).unwrap();
        assert_eq!(files_below(&root.join(UPLOADS)).len(), 3);
        backdate(
            &root.join(UPLOADS).join(&crashed[0]),
            Duration::from_secs(25 * 3600),
        );
        backdate(&old_temporary, Duration::from_secs(25 * 3600));
        backdate(
            &root.join(format!("{kept}.blob")),
            Duration::from_secs(25 * 3600),
        );
        let restarted = ObjectBlobStore::from_url(&file_url(&root)).unwrap();
        assert_eq!(
            files_below(&root.join(UPLOADS)),
            vec![fresh.file_name().unwrap().to_string_lossy().into_owned()]
        );
        assert_eq!(restarted.get(kept).await.unwrap(), Some(b"kept".to_vec()));
    }

    #[tokio::test]
    async fn a_failed_upload_removes_its_own_staged_parts() {
        let (store, root, _dir) = disk_store();
        let temporary = store.temporary_path();
        let name = temporary.filename().unwrap().to_owned();
        let uploads = root.join(UPLOADS);
        // What a completion that failed after object_store took its staged
        // path leaves behind: parts its abort can no longer find.
        for part in [format!("{name}#1"), format!("{name}#2"), name.clone()] {
            std::fs::write(uploads.join(part), b"part").unwrap();
        }
        let other = format!("{}.tmp#1", Uuid::new_v4());
        std::fs::write(uploads.join(&other), b"another upload").unwrap();

        store.discard_failed_upload(&temporary).await;
        let _ = store.store.delete(&temporary).await;
        assert_eq!(files_below(&uploads), vec![other]);

        // The failure paths leave nothing behind either.
        let rejected = DocumentBlob::from_bytes(b"declared");
        let bytes = vec![1_u8; MULTIPART_CHUNK_BYTES + 1];
        store
            .put_stream(rejected, stream::iter(vec![Ok(bytes)]).boxed())
            .await
            .unwrap_err();
        assert_eq!(files_below(&uploads).len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_the_server_cannot_write_refuses_boot_and_says_why() {
        let (store, root, dir) = disk_store();
        if !permissions_are_enforced(dir.path()) {
            return;
        }
        set_mode(&root.join(UPLOADS), 0o500);
        set_mode(&root, 0o500);
        let probe = store.probe().await.unwrap_err().to_string();
        let put = store
            .put(Uuid::new_v4(), b"bytes".to_vec())
            .await
            .unwrap_err()
            .to_string();
        set_mode(&root, 0o700);
        set_mode(&root.join(UPLOADS), 0o700);

        assert_eq!(
            probe,
            "store error: failed to write a test object to the blob store: permission denied"
        );
        assert_eq!(
            put,
            "store error: failed to publish immutable object: permission denied"
        );
        assert!(!probe.contains(&*root.to_string_lossy()));
    }

    /// `lost+found` at the root of a mounted volume is the usual case: a
    /// directory inside the blob directory that the server cannot read.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_subdirectory_does_not_break_the_probe_or_the_inventory() {
        let (store, root, _dir) = disk_store();
        let id = Uuid::new_v4();
        store.put(id, b"listed".to_vec()).await.unwrap();
        std::fs::create_dir(root.join("lost+found")).unwrap();
        set_mode(&root.join("lost+found"), 0o000);

        let probe = store.probe().await;
        let inventory = store.inventory().await;
        set_mode(&root.join("lost+found"), 0o700);

        probe.unwrap();
        let inventory = inventory.unwrap();
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].id, id);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_root_resolves_at_boot_and_a_dangling_one_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("volume");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("blobs");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let store = ObjectBlobStore::from_url(&file_url(&link)).unwrap();
        let id = Uuid::new_v4();
        store.put(id, b"through a link".to_vec()).await.unwrap();
        assert!(target.join(format!("{id}.blob")).is_file());

        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing"), &dangling).unwrap();
        let error = ObjectBlobStore::from_url(&file_url(&dangling))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("cannot create the blob directory"),
            "{error}"
        );
        assert!(error.contains("dangling"), "{error}");
    }

    #[test]
    fn storage_failures_name_the_cause_but_not_the_path_or_url() {
        let disk_full = ObjectError::Generic {
            store: "LocalFileSystem",
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "/srv/tidebreak/blobs/_uploads/private.tmp#1",
            )),
        };
        assert_eq!(
            object_error("write streamed object part", &disk_full).to_string(),
            "store error: failed to write streamed object part: no storage space"
        );
        let denied = ObjectError::PermissionDenied {
            path: "company/tidebreak/private.blob".into(),
            source: "https://bucket.s3.amazonaws.com/company/tidebreak/private.blob".into(),
        };
        assert_eq!(
            object_error("read object", &denied).to_string(),
            "store error: failed to read object: permission denied"
        );
        let opaque = ObjectError::Generic {
            store: "S3",
            source: "error sending request for url (https://bucket.s3.amazonaws.com/private)"
                .into(),
        };
        let message = object_error("list objects", &opaque).to_string();
        assert_eq!(
            message,
            "store error: failed to list objects: storage error"
        );
    }

    #[tokio::test]
    async fn immutable_puts_are_idempotent_and_reject_replacement() {
        for_each_backend(|store| async move {
            let id = Uuid::new_v4();
            store.put(id, b"one".to_vec()).await.unwrap();
            store.put(id, b"one".to_vec()).await.unwrap();
            let error = store.put(id, b"two".to_vec()).await.unwrap_err();
            assert!(error.to_string().contains("different bytes"));
            assert_eq!(store.get(id).await.unwrap(), Some(b"one".to_vec()));
        })
        .await;
    }

    #[tokio::test]
    async fn streamed_sources_keep_ranges_inventory_and_delete() {
        for_each_backend(|store| async move {
            let bytes = vec![7_u8; MULTIPART_CHUNK_BYTES + 17];
            let source = DocumentBlob::from_bytes(&bytes);
            let chunks =
                stream::iter(vec![Ok(bytes[..31].to_vec()), Ok(bytes[31..].to_vec())]).boxed();
            store.put_stream(source.clone(), chunks).await.unwrap();
            store
                .put_stream(
                    source.clone(),
                    stream::iter(vec![Ok(bytes.clone())]).boxed(),
                )
                .await
                .unwrap();

            assert_eq!(
                store.metadata(source.id).await.unwrap().unwrap().byte_len,
                source.byte_len
            );
            let range = store
                .read_range(source.id, 4..19)
                .await
                .unwrap()
                .unwrap()
                .collect::<Vec<_>>()
                .await;
            assert_eq!(
                range
                    .into_iter()
                    .collect::<Result<Vec<_>>>()
                    .unwrap()
                    .concat(),
                bytes[4..19]
            );
            assert_eq!(store.get(source.id).await.unwrap(), Some(bytes.clone()));
            let inventory = store.inventory().await.unwrap();
            assert_eq!(inventory.len(), 1);
            assert_eq!(inventory[0].id, source.id);
            assert_eq!(
                store.modified_at(source.id).await.unwrap(),
                Some(inventory[0].modified_at)
            );
            assert!(store
                .store
                .list(Some(&store.prefix.clone().join(UPLOADS)))
                .next()
                .await
                .is_none());

            store.delete(source.id).await.unwrap();
            store.delete(source.id).await.unwrap();
            assert_eq!(store.get(source.id).await.unwrap(), None);
            assert_eq!(store.metadata(source.id).await.unwrap(), None);
            assert!(store.inventory().await.unwrap().is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn streamed_sources_reject_wrong_declared_content() {
        for_each_backend(|store| async move {
            let source = DocumentBlob::from_bytes(b"expected");
            let chunks = stream::iter(vec![Ok(b"changed!".to_vec())]).boxed();
            let error = store.put_stream(source, chunks).await.unwrap_err();
            assert!(error.to_string().contains("declared digest"));
            assert!(store.inventory().await.unwrap().is_empty());
            assert!(store
                .store
                .list(Some(&store.prefix.clone().join(UPLOADS)))
                .next()
                .await
                .is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn streamed_sources_refuse_an_id_holding_other_bytes() {
        for_each_backend(|store| async move {
            let source = DocumentBlob::from_bytes(b"declared source");
            store.put(source.id, b"other bytes".to_vec()).await.unwrap();
            let error = store
                .put_stream(
                    source.clone(),
                    stream::iter(vec![Ok(b"declared source".to_vec())]).boxed(),
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("different bytes"));
            assert_eq!(
                store.get(source.id).await.unwrap(),
                Some(b"other bytes".to_vec())
            );
        })
        .await;
    }

    #[tokio::test]
    async fn streamed_empty_sources_work_at_the_bucket_root() {
        let (disk, _root, _dir) = disk_store();
        for store in [
            ObjectBlobStore::new(Arc::new(InMemory::new()), Path::default()),
            disk,
        ] {
            let source = DocumentBlob::from_bytes(b"");
            store
                .put_stream(source.clone(), stream::empty::<Result<Vec<u8>>>().boxed())
                .await
                .unwrap();

            assert_eq!(store.get(source.id).await.unwrap(), Some(Vec::new()));
            assert_eq!(store.inventory().await.unwrap()[0].id, source.id);
        }
    }

    #[tokio::test]
    async fn a_disk_store_keeps_blobs_and_uploads_inside_its_directory() {
        let (store, root, _dir) = disk_store();
        store.probe().await.unwrap();
        let bytes = vec![3_u8; MULTIPART_CHUNK_BYTES * 2 + 5];
        let source = DocumentBlob::from_bytes(&bytes);
        store
            .put_stream(
                source.clone(),
                stream::iter(vec![Ok(bytes.clone())]).boxed(),
            )
            .await
            .unwrap();
        let rejected = DocumentBlob::from_bytes(b"declared");
        store
            .put_stream(
                rejected,
                stream::iter(vec![Ok(b"streamed".to_vec())]).boxed(),
            )
            .await
            .unwrap_err();
        let small = Uuid::new_v4();
        store.put(small, b"small".to_vec()).await.unwrap();

        // Only published blobs remain: no staged part, no temporary upload,
        // no probe object, and nothing outside the configured directory.
        let mut expected = vec![format!("{}.blob", source.id), format!("{small}.blob")];
        expected.sort();
        assert_eq!(files_below(&root), expected);
        assert!(root.join(UPLOADS).is_dir());
        assert_eq!(
            std::fs::read(root.join(format!("{}.blob", source.id))).unwrap(),
            bytes
        );

        // A second process over the same directory sees the same blobs.
        let reopened = ObjectBlobStore::from_url(&file_url(&root)).unwrap();
        assert_eq!(reopened.get(small).await.unwrap(), Some(b"small".to_vec()));
        assert_eq!(reopened.inventory().await.unwrap().len(), 2);
    }
}

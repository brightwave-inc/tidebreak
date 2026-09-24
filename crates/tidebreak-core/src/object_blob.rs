//! Object-storage [`BlobStore`](crate::BlobStore) implementation for the
//! self-host profile: an S3-compatible bucket, or a directory on the machine's
//! own disk behind the same `object_store` contract.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use object_store::aws::{AmazonS3Builder, S3CopyIfNotExists};
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::{Error as ObjectError, ObjectStore, ObjectStoreExt, PutMode, PutOptions};
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

#[derive(Clone)]
pub struct ObjectBlobStore {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl ObjectBlobStore {
    #[must_use]
    pub fn new(store: Arc<dyn ObjectStore>, prefix: Path) -> Self {
        Self { store, prefix }
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
    /// A missing directory is created readable by its owner only. Blobs and
    /// the `_uploads/` staging area both live inside it, so a streamed upload
    /// publishes by hard link within one filesystem, and every write is synced
    /// before it is acknowledged, as a bucket would be.
    pub fn from_file_url(value: &str) -> Result<Self> {
        let root = parse_file_root(value)?;
        create_private_directory(&root)?;
        let store = LocalFileSystem::new_with_prefix(&root)
            .map_err(|error| {
                AgentError::config(format!(
                    "cannot open the blob directory {}: {error}",
                    root.display()
                ))
            })?
            .with_fsync(true);
        Ok(Self::new(Arc::new(store), Path::default()))
    }

    pub async fn probe(&self) -> Result<()> {
        if let Some(result) = self.store.list(Some(&self.prefix)).next().await {
            result.map_err(|_| object_error("probe"))?;
        }
        Ok(())
    }

    fn path(&self, id: Uuid) -> Path {
        self.prefix.clone().join(format!("{id}.blob"))
    }

    fn temporary_path(&self) -> Path {
        self.prefix
            .clone()
            .join("_uploads")
            .join(format!("{}.tmp", Uuid::new_v4()))
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
            Err(_) => return Err(object_error("read immutable object")),
        };
        let mut digest = Sha256::new();
        let mut byte_len = 0_u64;
        let mut stream = result.into_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| object_error("read immutable object"))?;
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
/// The spelling is checked before the URL is trusted: a parser reads
/// `file:blobs` as `/blobs` and resolves `..`, so a relative or unnormalized
/// value would quietly move the store somewhere the operator did not write.
fn parse_file_root(value: &str) -> Result<std::path::PathBuf> {
    let invalid = || {
        AgentError::config(
            "TIDEBREAK_BLOB_STORE_URL must be file:///absolute/path: no host, no `.` or `..` \
             segments, no query or fragment, and special characters percent-encoded",
        )
    };
    let path = value.strip_prefix("file://").ok_or_else(invalid)?;
    if !path.starts_with('/') {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    if url.path() != path || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid());
    }
    let root = url.to_file_path().map_err(|_| invalid())?;
    if !root.is_absolute() {
        return Err(invalid());
    }
    Ok(root)
}

/// Create `root` and any missing parents, readable by the server's user
/// only. An existing directory keeps the permissions its operator gave it.
fn create_private_directory(root: &std::path::Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(root).map_err(|error| {
        AgentError::config(format!(
            "cannot create the blob directory {}: {error}",
            root.display()
        ))
    })
}

#[async_trait]
impl BlobStore for ObjectBlobStore {
    async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<()> {
        let path = self.path(id);
        match self
            .store
            .put_opts(
                &path,
                bytes.clone().into(),
                PutOptions::from(PutMode::Create),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(ObjectError::AlreadyExists { .. }) => {
                if self.existing_matches(id, &bytes).await? {
                    Ok(())
                } else {
                    Err(AgentError::Store(
                        "immutable blob id already contains different bytes".into(),
                    ))
                }
            }
            Err(_) => Err(object_error("publish immutable object")),
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
            .map_err(|_| object_error("start streamed object upload"))?;
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
                            .map_err(|_| object_error("write streamed object part"))?;
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
                    .map_err(|_| object_error("write streamed object part"))?;
            }
            upload
                .complete()
                .await
                .map_err(|_| object_error("complete streamed object upload"))?;
            multipart_completed = true;

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
                Err(_) => Err(object_error("publish streamed object")),
            }
        }
        .await;

        if result.is_err() && !multipart_completed {
            let _ = upload.abort().await;
        }
        let _ = self.store.delete(&temporary).await;
        result
    }

    async fn get(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
        let result = match self.store.get(&self.path(id)).await {
            Ok(result) => result,
            Err(ObjectError::NotFound { .. }) => return Ok(None),
            Err(_) => return Err(object_error("read object")),
        };
        let bytes = result
            .bytes()
            .await
            .map_err(|_| object_error("read object body"))?;
        Ok(Some(bytes.to_vec()))
    }

    async fn metadata(&self, id: Uuid) -> Result<Option<BlobMetadata>> {
        match self.store.head(&self.path(id)).await {
            Ok(metadata) => Ok(Some(BlobMetadata {
                byte_len: metadata.size,
            })),
            Err(ObjectError::NotFound { .. }) => Ok(None),
            Err(_) => Err(object_error("read object metadata")),
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
            Err(_) => return Err(object_error("read object range")),
        };
        let stream = result.into_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|_| object_error("read object range body"))
        });
        Ok(Some(Box::pin(stream)))
    }

    async fn inventory(&self) -> Result<Vec<BlobInventoryItem>> {
        let mut stream = self.store.list(Some(&self.prefix));
        let mut items = Vec::new();
        while let Some(metadata) = stream.next().await {
            let metadata = metadata.map_err(|_| object_error("list objects"))?;
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
            Err(_) => Err(object_error("read object metadata")),
        }
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        match self.store.delete(&self.path(id)).await {
            Ok(()) | Err(ObjectError::NotFound { .. }) => Ok(()),
            Err(_) => Err(object_error("delete object")),
        }
    }
}

fn object_error(action: &str) -> AgentError {
    AgentError::Store(format!("failed to {action}"))
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

    /// A blob directory inside a fresh temporary directory, and the guard
    /// that removes it.
    fn disk_store() -> (ObjectBlobStore, std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        let url = Url::from_file_path(&root).unwrap();
        (ObjectBlobStore::from_url(url.as_str()).unwrap(), root, dir)
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
        assert!(file.contains("file:///absolute/path: no host"), "{file}");
    }

    #[test]
    fn file_urls_name_one_absolute_directory() {
        assert_eq!(
            parse_file_root("file:///var/lib/tidebreak/blobs").unwrap(),
            std::path::PathBuf::from("/var/lib/tidebreak/blobs")
        );
        assert_eq!(
            parse_file_root("file:///srv/tidebreak%20blobs").unwrap(),
            std::path::PathBuf::from("/srv/tidebreak blobs")
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
            // Anything a parser would rewrite.
            "file:///var/lib/../blobs",
            "file:///var/./lib/blobs",
            "file:///var/lib/%2e%2e/blobs",
            "file:///var\\lib\\blobs",
            "file:///var/lib/tidebreak blobs",
            "file:///var/lib/blobs?mode=0700",
            "file:///var/lib/blobs#blobs",
            "FILE:///var/lib/blobs",
        ] {
            let error = parse_file_root(value).unwrap_err().to_string();
            assert!(error.contains("file:///absolute/path"), "{value}: {error}");
        }
    }

    #[test]
    fn a_missing_blob_directory_is_created_for_its_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data").join("blobs");
        ObjectBlobStore::from_url(Url::from_file_path(&root).unwrap().as_str()).unwrap();
        assert!(root.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&root).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }

        // A file where the directory should be is refused at boot, by path.
        let occupied = dir.path().join("occupied");
        std::fs::write(&occupied, b"not a directory").unwrap();
        let error = ObjectBlobStore::from_url(Url::from_file_path(&occupied).unwrap().as_str())
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("blob directory"), "{error}");
        assert!(error.contains("occupied"), "{error}");
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
                .list(Some(&store.prefix.clone().join("_uploads")))
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
                .list(Some(&store.prefix.clone().join("_uploads")))
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
        // and nothing outside the configured directory.
        let mut expected = vec![format!("{}.blob", source.id), format!("{small}.blob")];
        expected.sort();
        assert_eq!(files_below(&root), expected);
        assert!(root.join("_uploads").is_dir());
        assert_eq!(
            std::fs::read(root.join(format!("{}.blob", source.id))).unwrap(),
            bytes
        );

        // A second process over the same directory sees the same blobs.
        let reopened =
            ObjectBlobStore::from_url(Url::from_file_path(&root).unwrap().as_str()).unwrap();
        assert_eq!(reopened.get(small).await.unwrap(), Some(b"small".to_vec()));
        assert_eq!(reopened.inventory().await.unwrap().len(), 2);
    }
}

//! S3-compatible [`BlobStore`](crate::BlobStore) implementation.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use object_store::aws::{AmazonS3Builder, S3CopyIfNotExists};
use object_store::path::Path;
use object_store::{Error as ObjectError, ObjectStore, ObjectStoreExt, PutMode, PutOptions};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::{
    AgentError, BlobInventoryItem, BlobMetadata, BlobStore, BlobStream, DocumentBlob, Result,
};

const MULTIPART_CHUNK_BYTES: usize = 5 * 1024 * 1024;

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
    use futures::stream;
    use object_store::memory::InMemory;

    use super::*;

    fn store() -> ObjectBlobStore {
        ObjectBlobStore::new(Arc::new(InMemory::new()), Path::from("tidebreak/blobs"))
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

    #[tokio::test]
    async fn immutable_puts_are_idempotent_and_reject_replacement() {
        let store = store();
        let id = Uuid::new_v4();
        store.put(id, b"one".to_vec()).await.unwrap();
        store.put(id, b"one".to_vec()).await.unwrap();
        let error = store.put(id, b"two".to_vec()).await.unwrap_err();
        assert!(error.to_string().contains("different bytes"));
    }

    #[tokio::test]
    async fn streamed_sources_keep_ranges_inventory_and_delete() {
        let store = store();
        let bytes = vec![7_u8; MULTIPART_CHUNK_BYTES + 17];
        let source = DocumentBlob::from_bytes(&bytes);
        let chunks = stream::iter(vec![Ok(bytes[..31].to_vec()), Ok(bytes[31..].to_vec())]).boxed();
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
        assert_eq!(store.inventory().await.unwrap()[0].id, source.id);
        assert!(store
            .store
            .list(Some(&store.prefix.clone().join("_uploads")))
            .next()
            .await
            .is_none());

        store.delete(source.id).await.unwrap();
        assert_eq!(store.get(source.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn streamed_sources_reject_wrong_declared_content() {
        let store = store();
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
    }

    #[tokio::test]
    async fn streamed_empty_sources_work_at_the_bucket_root() {
        let store = ObjectBlobStore::new(Arc::new(InMemory::new()), Path::default());
        let source = DocumentBlob::from_bytes(b"");
        store
            .put_stream(source.clone(), stream::empty::<Result<Vec<u8>>>().boxed())
            .await
            .unwrap();

        assert_eq!(store.get(source.id).await.unwrap(), Some(Vec::new()));
        assert_eq!(store.inventory().await.unwrap()[0].id, source.id);
    }
}

use async_trait::async_trait;
use futures::{
    stream::{self},
    StreamExt,
};
use std::ops::Range;

use crate::error::{AgentError, Result};
use crate::model::DocumentBlob;

use super::types::BlobStream;

/// Metadata that can be read without materializing a blob's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobMetadata {
    /// Number of bytes in the immutable blob.
    pub byte_len: u64,
}

/// Opaque byte storage for documents, images, and exports.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Publish immutable bytes under `id`.
    ///
    /// Repeating the same publication is a no-op; publishing different bytes
    /// under an existing id fails without changing the stored value. Callers
    /// allocate a new id when content changes.
    async fn put(&self, id: uuid::Uuid, bytes: Vec<u8>) -> Result<()>;

    /// Publish a content-addressed source from a stream of chunks.
    ///
    /// Filesystem-backed storage overrides this to write each chunk directly
    /// to its durable temporary file. Other implementations retain a correct
    /// fallback while they add their own streaming primitive.
    async fn put_stream(&self, source: DocumentBlob, mut chunks: BlobStream) -> Result<()> {
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let chunk_len = u64::try_from(chunk.len())
                .map_err(|_| AgentError::Store("blob chunk length exceeds u64".into()))?;
            let next_len = u64::try_from(bytes.len())
                .map_err(|_| AgentError::Store("blob length exceeds u64".into()))?
                .checked_add(chunk_len)
                .ok_or_else(|| AgentError::Store("blob length exceeds u64".into()))?;
            if next_len > source.byte_len {
                return Err(AgentError::Store(
                    "streamed blob exceeds its declared byte length".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if DocumentBlob::from_bytes(&bytes) != source {
            return Err(AgentError::Store(
                "streamed blob does not match its declared digest".into(),
            ));
        }
        self.put(source.id, bytes).await
    }

    /// Fetch bytes by `id`, or `None` if absent.
    async fn get(&self, id: uuid::Uuid) -> Result<Option<Vec<u8>>>;

    /// Fetch a blob's length without reading its bytes.
    ///
    /// Backends should override this when their storage can obtain metadata
    /// independently. The compatibility implementation keeps existing custom
    /// stores correct while they adopt the bounded-read API.
    async fn metadata(&self, id: uuid::Uuid) -> Result<Option<BlobMetadata>> {
        self.get(id).await.map(|bytes| {
            bytes.map(|bytes| BlobMetadata {
                byte_len: u64::try_from(bytes.len()).expect("usize always fits in u64"),
            })
        })
    }

    /// Read the half-open byte `range` without materializing bytes outside it.
    ///
    /// A backend that cannot yet stream uses the compatibility implementation;
    /// production stores should override it so response ranges remain bounded.
    async fn read_range(&self, id: uuid::Uuid, range: Range<u64>) -> Result<Option<BlobStream>> {
        let Some(bytes) = self.get(id).await? else {
            return Ok(None);
        };
        let start = usize::try_from(range.start)
            .map_err(|_| AgentError::Store("blob range start exceeds usize".into()))?;
        let end = usize::try_from(range.end)
            .map_err(|_| AgentError::Store("blob range end exceeds usize".into()))?;
        let bytes = bytes
            .get(start..end)
            .ok_or_else(|| AgentError::Store("requested byte range is outside the blob".into()))?
            .to_vec();
        Ok(Some(stream::once(async move { Ok(bytes) }).boxed()))
    }

    /// Delete a blob synchronously; a no-op if it doesn't exist.
    ///
    /// Async callers must move this operation to a blocking executor. This
    /// boundary lets a lifecycle guard remain owned by the blocking operation
    /// even when its awaiting worker is cancelled.
    fn delete(&self, id: uuid::Uuid) -> Result<()>;
}

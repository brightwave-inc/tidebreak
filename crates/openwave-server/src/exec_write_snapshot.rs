//! Retaining the bytes a turn's write-back is about to replace.
//!
//! The overlay applies a turn's staged edits to the user's real folders. This
//! is the thing that runs immediately before each of those edits and keeps a
//! copy of what is being destroyed, so undo has something to restore. The
//! retained bytes go into the ordinary content-addressed blob store and gain
//! their liveness from the journal row committed here — the same mechanism
//! documents and image attachments use, so the orphan auditor and retirement
//! worker already know how to leave them alone.
//!
//! Ordering matters and is the reason the blob write and the row commit are
//! split. Bytes are published as each file is recorded; rows are committed once
//! for the whole turn after the overlay is applied. A blob briefly without a
//! row is an ordinary young orphan, which the auditor's grace period ignores
//! and the retirement path would reclaim harmlessly. A row without its blob
//! would be an undo that restores nothing, which is why it cannot happen in
//! that order.

use std::sync::{Arc, Mutex};

use openwave_code_execution::{PriorContents, StagedChange, WriteSnapshotSink};
use openwave_core::{
    BlobStore, ChatId, DocumentSourceBlob, ExecFileChange, ExecFileSnapshotRecord, ExecUndoState,
    Store, TurnId,
};

use crate::state::BlobWriteGuard;

/// Accumulates one turn's file changes while the overlay is applied.
pub(crate) struct TurnSnapshotSink {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    blob_writes: Arc<BlobWriteGuard>,
    files: Mutex<Vec<ExecFileSnapshotRecord>>,
}

impl TurnSnapshotSink {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        blob_writes: Arc<BlobWriteGuard>,
    ) -> Self {
        Self {
            store,
            blobs,
            blob_writes,
            files: Mutex::new(Vec::new()),
        }
    }

    /// Commit the journal for `turn_id`, making its retained bytes live.
    pub(crate) async fn commit(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
    ) -> openwave_core::Result<()> {
        let files = std::mem::take(
            &mut *self
                .files
                .lock()
                .expect("exec snapshot buffer is not poisoned"),
        );
        self.store
            .record_exec_file_snapshots(chat_id, turn_id, &files)
            .await
    }
}

#[async_trait::async_trait]
impl WriteSnapshotSink for TurnSnapshotSink {
    async fn record(&self, change: StagedChange<'_>) -> Result<(), String> {
        let change_kind = match (&change.prior, change.next) {
            (_, None) => ExecFileChange::Deleted,
            (PriorContents::Absent, Some(_)) => ExecFileChange::Created,
            (_, Some(_)) => ExecFileChange::Overwritten,
        };
        let new_sha256 = change.next.map(sha256_hex);
        let (prior_blob_id, prior_byte_len, undo) = match change.prior {
            PriorContents::Absent => (None, None, ExecUndoState::Available),
            PriorContents::TooLarge { byte_len } => {
                (None, Some(byte_len), ExecUndoState::PriorTooLarge)
            }
            PriorContents::Unreadable => (None, None, ExecUndoState::PriorUnreadable),
            PriorContents::Bytes(bytes) => {
                let blob = DocumentSourceBlob::from_bytes(&bytes);
                // Serialize writers for this exact content address, exactly as
                // the attachment and document publish paths do, so a retirer
                // deleting the same bytes cannot interleave with this write.
                let _permit = self
                    .blob_writes
                    .acquire(blob.id)
                    .await
                    .map_err(|error| error.to_string())?;
                self.blobs
                    .put(blob.id, bytes)
                    .await
                    .map_err(|error| error.to_string())?;
                (Some(blob.id), Some(blob.byte_len), ExecUndoState::Available)
            }
        };
        self.files
            .lock()
            .expect("exec snapshot buffer is not poisoned")
            .push(ExecFileSnapshotRecord {
                folder_path: change.folder.display().to_string(),
                relative_path: change.relative.to_owned(),
                change: change_kind,
                prior_blob_id,
                prior_byte_len,
                new_sha256,
                undo,
            });
        Ok(())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

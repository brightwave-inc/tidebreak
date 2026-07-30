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

use std::path::Path;
use std::sync::{Arc, Mutex};

use openwave_code_execution::{
    try_resolve_scratch_directory, FilePrecondition, PreparedWriteSnapshot, PriorContents,
    ScratchDir, StagedChange, WriteSnapshotSink,
};
use openwave_core::{
    BlobStore, ChatId, DocumentSourceBlob, ExecFileChange, ExecFileSnapshot,
    ExecFileSnapshotRecord, ExecUndoState, Store, TurnId,
};
use serde::{Deserialize, Serialize};

use crate::state::BlobWriteGuard;

/// Accumulates one turn's file changes while the overlay is applied.
pub(crate) struct TurnSnapshotSink {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    blob_writes: Arc<BlobWriteGuard>,
    files: Arc<Mutex<Vec<ExecFileSnapshotRecord>>>,
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
            files: Arc::new(Mutex::new(Vec::new())),
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

struct PendingSnapshot {
    files: Arc<Mutex<Vec<ExecFileSnapshotRecord>>>,
    file: ExecFileSnapshotRecord,
}

impl PreparedWriteSnapshot for PendingSnapshot {
    fn applied(self: Box<Self>) {
        let Self { files, file } = *self;
        files
            .lock()
            .expect("exec snapshot buffer is not poisoned")
            .push(file);
    }
}

#[async_trait::async_trait]
impl WriteSnapshotSink for TurnSnapshotSink {
    async fn prepare(
        &self,
        change: StagedChange<'_>,
    ) -> Result<Box<dyn PreparedWriteSnapshot>, String> {
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
        Ok(Box::new(PendingSnapshot {
            files: Arc::clone(&self.files),
            file: ExecFileSnapshotRecord {
                folder_path: change.folder.display().to_string(),
                relative_path: change.relative.to_owned(),
                change: change_kind,
                prior_blob_id,
                prior_byte_len,
                new_sha256,
                undo,
            },
        }))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Result of replaying one turn's durable file-change journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecTurnUndoOutcome {
    pub chat_id: ChatId,
    pub turn_id: TurnId,
    pub files: Vec<ExecFileUndoOutcome>,
}

/// Result of undoing one journaled file change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecFileUndoOutcome {
    pub folder_path: String,
    pub relative_path: String,
    pub status: ExecFileUndoStatus,
}

/// Stable per-file outcome for an undo request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFileUndoStatus {
    /// Prior bytes replaced the turn's output, including a file the turn deleted.
    Restored,
    /// A file the turn created was removed.
    Deleted,
    /// The filesystem already has the state this undo would produce.
    AlreadyUndone,
    /// The file no longer matches the bytes this turn left behind.
    Stale,
    /// The journal explicitly says the prior bytes were not retained.
    NotAvailable,
    /// The journal names retained bytes that are absent or invalid.
    SnapshotMissing,
    /// The journal path or filesystem operation could not be used safely.
    Unavailable,
}

/// Undo every journaled file change from one turn.
///
/// Each mutation is guarded by the digest that the turn recorded after
/// materialization. A later user edit therefore wins rather than being
/// overwritten by undo. The operation is restart-safe without a second state
/// machine: a retry recognizes the prior digest (or absence for a created
/// file) as already undone, so a crash between files can resume from the same
/// durable journal rows.
pub(crate) async fn undo_turn_file_changes(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    chat_id: ChatId,
    turn_id: TurnId,
) -> openwave_core::Result<ExecTurnUndoOutcome> {
    let snapshots = store.list_exec_file_snapshots(chat_id).await?;
    let mut files = Vec::new();
    for snapshot in snapshots
        .into_iter()
        .filter(|snapshot| snapshot.turn_id == turn_id)
    {
        let status = undo_file_change(blobs, &snapshot).await;
        files.push(ExecFileUndoOutcome {
            folder_path: snapshot.file.folder_path,
            relative_path: snapshot.file.relative_path,
            status,
        });
    }
    Ok(ExecTurnUndoOutcome {
        chat_id,
        turn_id,
        files,
    })
}

async fn undo_file_change(
    blobs: &dyn BlobStore,
    snapshot: &ExecFileSnapshot,
) -> ExecFileUndoStatus {
    if snapshot.file.undo != ExecUndoState::Available {
        return ExecFileUndoStatus::NotAvailable;
    }
    let Some((prefix, name)) = split_journal_path(&snapshot.file.relative_path) else {
        return ExecFileUndoStatus::Unavailable;
    };
    let folder = Path::new(&snapshot.file.folder_path);
    if !folder.is_absolute() {
        return ExecFileUndoStatus::Unavailable;
    }

    match snapshot.file.change {
        ExecFileChange::Created => {
            let Some(expected) = snapshot.file.new_sha256.as_deref().and_then(parse_sha256) else {
                return ExecFileUndoStatus::Unavailable;
            };
            let Some(parent) = try_resolve_scratch_directory(folder, prefix, false)
                .await
                .ok()
            else {
                // If a later action removed the containing directory, the file
                // created by this turn is already absent.
                return ExecFileUndoStatus::AlreadyUndone;
            };
            match current_digest(&parent, name).await {
                Ok(None) => ExecFileUndoStatus::AlreadyUndone,
                Ok(Some(current)) if current != expected => ExecFileUndoStatus::Stale,
                Ok(Some(_)) => match parent
                    .remove_file_if_matches(name, FilePrecondition::Sha256(expected))
                    .await
                {
                    Ok(true) => ExecFileUndoStatus::Deleted,
                    Ok(false) => classify_current(&parent, name, None).await,
                    Err(_) => ExecFileUndoStatus::Unavailable,
                },
                Err(()) => ExecFileUndoStatus::Unavailable,
            }
        }
        ExecFileChange::Overwritten | ExecFileChange::Deleted => {
            let Some(prior) = load_prior_bytes(blobs, snapshot).await else {
                return ExecFileUndoStatus::SnapshotMissing;
            };
            let prior_digest = sha256(&prior);
            let Some(parent) = try_resolve_scratch_directory(folder, prefix, false)
                .await
                .ok()
            else {
                return ExecFileUndoStatus::Stale;
            };
            let expected = match snapshot.file.change {
                ExecFileChange::Overwritten => {
                    let Some(expected) = snapshot.file.new_sha256.as_deref().and_then(parse_sha256)
                    else {
                        return ExecFileUndoStatus::Unavailable;
                    };
                    FilePrecondition::Sha256(expected)
                }
                ExecFileChange::Deleted => FilePrecondition::Absent,
                ExecFileChange::Created => unreachable!(),
            };
            match current_digest(&parent, name).await {
                Ok(Some(current)) if current == prior_digest => {
                    return ExecFileUndoStatus::AlreadyUndone;
                }
                Ok(Some(current)) if !matches!(expected, FilePrecondition::Sha256(digest) if digest == current) =>
                {
                    return ExecFileUndoStatus::Stale;
                }
                Ok(None) if expected != FilePrecondition::Absent => {
                    return ExecFileUndoStatus::Stale;
                }
                Ok(None | Some(_)) => {}
                Err(()) => return ExecFileUndoStatus::Unavailable,
            }
            #[cfg(unix)]
            let mode = parent.file_stamp(name).await.map(|stamp| stamp.mode);
            #[cfg(not(unix))]
            let mode = None;
            match parent
                .write_file_with_mode_if_matches(name, &prior, mode, expected)
                .await
            {
                Ok(true) => ExecFileUndoStatus::Restored,
                Ok(false) => classify_current(&parent, name, Some(prior_digest)).await,
                Err(_) => ExecFileUndoStatus::Unavailable,
            }
        }
    }
}

async fn load_prior_bytes(blobs: &dyn BlobStore, snapshot: &ExecFileSnapshot) -> Option<Vec<u8>> {
    let blob_id = snapshot.file.prior_blob_id?;
    let bytes = blobs.get(blob_id).await.ok()??;
    let source = DocumentSourceBlob::from_bytes(&bytes);
    let byte_len = snapshot.file.prior_byte_len?;
    (source.id == blob_id && source.byte_len == byte_len).then_some(bytes)
}

async fn current_digest(directory: &ScratchDir, name: &str) -> Result<Option<[u8; 32]>, ()> {
    match directory.file_sha256(name).await {
        Ok(digest) => Ok(Some(digest)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

async fn classify_current(
    directory: &ScratchDir,
    name: &str,
    restored_digest: Option<[u8; 32]>,
) -> ExecFileUndoStatus {
    match current_digest(directory, name).await {
        Ok(None) if restored_digest.is_none() => ExecFileUndoStatus::AlreadyUndone,
        Ok(Some(current)) if restored_digest == Some(current) => ExecFileUndoStatus::AlreadyUndone,
        Ok(_) => ExecFileUndoStatus::Stale,
        Err(()) => ExecFileUndoStatus::Unavailable,
    }
}

fn split_journal_path(relative: &str) -> Option<(&str, &str)> {
    if relative.is_empty()
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || (cfg!(windows) && relative.contains(['\\', ':']))
    {
        return None;
    }
    Some(
        relative
            .rsplit_once('/')
            .map_or(("", relative), |(prefix, name)| (prefix, name)),
    )
}

fn parse_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (output, encoded) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let encoded = std::str::from_utf8(encoded).ok()?;
        *output = u8::from_str_radix(encoded, 16).ok()?;
    }
    Some(digest)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use chrono::Utc;
    use openwave_code_execution::WriteOverlay;
    use openwave_core::{Chat, DbStore, FsBlobStore};

    use crate::state::BlobWriteGuard;

    /// The issue contract: the turn journal survives process state, and undo
    /// restores overwritten/deleted bytes while removing a file the turn made.
    #[tokio::test]
    async fn a_turns_journaled_file_changes_undo_after_restart() {
        let home = tempfile::tempdir().unwrap();
        let database = home.path().join("openwave.db");
        let database_url = format!("sqlite://{}?mode=rwc", database.display());
        let blob_root = home.path().join("blobs");
        let scratch = home.path().join("scratch");
        let granted = home.path().join("granted");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&granted).unwrap();

        let original = b"original bytes";
        let original_source = DocumentSourceBlob::from_bytes(original);
        std::fs::write(granted.join("notes.md"), original).unwrap();
        std::fs::write(granted.join("removed.txt"), b"bring me back").unwrap();

        let store = Arc::new(DbStore::connect(&database_url).await.unwrap());
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let blobs = Arc::new(FsBlobStore::new(&blob_root));
        let sink = TurnSnapshotSink::new(
            store.clone(),
            blobs.clone(),
            Arc::new(BlobWriteGuard::new(home.path().join("blob-locks"))),
        );
        let turn_id = TurnId::new();
        let overlay = WriteOverlay::prepare(
            &scratch,
            &chat.id.to_string(),
            std::slice::from_ref(&granted),
        )
        .await
        .unwrap();
        let staged = overlay.slots()[0].overlay();
        std::fs::write(staged.join("notes.md"), b"replacement").unwrap();
        std::fs::write(staged.join("created.txt"), b"new file").unwrap();
        std::fs::remove_file(staged.join("removed.txt")).unwrap();

        let materialized = overlay.materialize(Some(&sink)).await;
        assert_eq!(materialized.written.len(), 3);
        assert!(materialized.rejected.is_empty());
        sink.commit(chat.id, turn_id).await.unwrap();
        assert_eq!(
            std::fs::read(granted.join("notes.md")).unwrap(),
            b"replacement"
        );
        assert!(!granted.join("removed.txt").exists());

        // Reopen both durable stores to prove undo does not depend on the
        // process-local sink or overlay registry that made the changes.
        drop(sink);
        drop(store);
        drop(blobs);
        let restarted_store = DbStore::connect(&database_url).await.unwrap();
        let restarted_blobs = FsBlobStore::new(&blob_root);
        let outcome = undo_turn_file_changes(&restarted_store, &restarted_blobs, chat.id, turn_id)
            .await
            .unwrap();
        assert_eq!(
            outcome
                .files
                .iter()
                .map(|file| (file.relative_path.as_str(), file.status))
                .collect::<BTreeMap<_, _>>(),
            BTreeMap::from([
                ("created.txt", ExecFileUndoStatus::Deleted),
                ("notes.md", ExecFileUndoStatus::Restored),
                ("removed.txt", ExecFileUndoStatus::Restored),
            ])
        );

        let restored = std::fs::read(granted.join("notes.md")).unwrap();
        assert_eq!(restored, original);
        assert_eq!(
            DocumentSourceBlob::from_bytes(&restored).sha256,
            original_source.sha256
        );
        assert_eq!(
            std::fs::read(granted.join("removed.txt")).unwrap(),
            b"bring me back"
        );
        assert!(!granted.join("created.txt").exists());

        let retried = undo_turn_file_changes(&restarted_store, &restarted_blobs, chat.id, turn_id)
            .await
            .unwrap();
        assert!(retried
            .files
            .iter()
            .all(|file| file.status == ExecFileUndoStatus::AlreadyUndone));
    }
}

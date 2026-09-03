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
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidebreak_code_execution::{
    try_resolve_scratch_directory, FilePrecondition, PreparedWriteSnapshot, PriorContents,
    ScratchDir, StagedChange, WriteSnapshotSink,
};
use tidebreak_core::{
    sha256_hex, BlobStore, DocumentBlob, ExecFileChange, ExecFileRejectionReason, ExecFileSnapshot,
    ExecFileSnapshotRecord, ExecUndoState, ImageMediaType, ScratchPriorContents, SessionId, Store,
    TurnId, MAX_EXEC_WORKSPACE_FILE_BYTES,
};
use ts_rs::TS;

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
        chat_id: SessionId,
        turn_id: TurnId,
    ) -> tidebreak_core::Result<()> {
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

/// Journals one turn's private-scratch overwrites through the same retained
/// blob, digest guard, and undo window as folder write-back.
///
/// The chat's private scratch is an ordinary absolute directory, so its rows
/// need nothing the journal does not already carry: the scratch directory is
/// the folder, the tool's path is the relative path, and undo resolves and
/// restores it exactly as it does a granted folder's file. Each overwrite
/// commits on its own, the way a connected-app publication does, because a
/// structured write has no later write-back step to batch behind.
pub(crate) struct TurnScratchJournal {
    sink: TurnSnapshotSink,
    folder: std::path::PathBuf,
    chat_id: SessionId,
    turn_id: TurnId,
}

impl TurnScratchJournal {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        blob_writes: Arc<BlobWriteGuard>,
        folder: std::path::PathBuf,
        chat_id: SessionId,
        turn_id: TurnId,
    ) -> Self {
        Self {
            sink: TurnSnapshotSink::new(store, blobs, blob_writes),
            folder,
            chat_id,
            turn_id,
        }
    }
}

#[async_trait::async_trait]
impl tidebreak_core::ScratchWriteJournal for TurnScratchJournal {
    async fn record_overwrite(
        &self,
        relative_path: &str,
        prior: ScratchPriorContents,
        next: &[u8],
    ) {
        let prior = match prior {
            ScratchPriorContents::Bytes(bytes) => PriorContents::Bytes(bytes),
            ScratchPriorContents::TooLarge { byte_len } => PriorContents::TooLarge { byte_len },
            ScratchPriorContents::Unreadable => PriorContents::Unreadable,
        };
        let prepared = self
            .sink
            .prepare(StagedChange {
                folder: &self.folder,
                relative: relative_path,
                prior,
                next: Some(next),
            })
            .await;
        match prepared {
            // The bytes are already written; the retained copy is what is at
            // stake here, so a failure costs the undo and nothing else.
            Ok(prepared) => prepared.applied(),
            Err(error) => {
                tracing::error!(
                    chat = %self.chat_id,
                    turn = %self.turn_id,
                    %error,
                    "could not retain the bytes a scratch write replaced; undo is unavailable"
                );
                return;
            }
        }
        if let Err(error) = self.sink.commit(self.chat_id, self.turn_id).await {
            tracing::error!(
                chat = %self.chat_id,
                turn = %self.turn_id,
                %error,
                "could not journal a scratch write; undo is unavailable"
            );
        }
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
                let blob = DocumentBlob::from_bytes(&bytes);
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

/// Result of replaying one turn's durable file-change journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecTurnUndoOutcome {
    pub chat_id: SessionId,
    pub turn_id: TurnId,
    pub files: Vec<ExecFileUndoOutcome>,
}

/// Result of undoing one journaled file change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ExecFileUndoOutcome {
    pub snapshot_id: String,
    pub folder_name: String,
    pub relative_path: String,
    pub status: ExecFileUndoStatus,
}

/// Stable per-file outcome for an undo request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
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
    chat_id: SessionId,
    turn_id: TurnId,
) -> tidebreak_core::Result<ExecTurnUndoOutcome> {
    let snapshots = store.list_exec_file_snapshots(chat_id).await?;
    let mut files = Vec::new();
    for snapshot in snapshots
        .into_iter()
        .filter(|snapshot| snapshot.turn_id == turn_id)
    {
        let status = undo_file_change(blobs, &snapshot).await;
        files.push(ExecFileUndoOutcome {
            snapshot_id: snapshot.id.to_string(),
            folder_name: display_folder_name(&snapshot.file.folder_path),
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

/// Undo one journaled file without touching its siblings.
pub(crate) async fn undo_one_file_change(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    chat_id: SessionId,
    turn_id: TurnId,
    snapshot_id: uuid::Uuid,
) -> tidebreak_core::Result<Option<ExecFileUndoOutcome>> {
    let snapshot = store
        .list_exec_file_snapshots(chat_id)
        .await?
        .into_iter()
        .find(|snapshot| snapshot.turn_id == turn_id && snapshot.id == snapshot_id);
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    let status = undo_file_change(blobs, &snapshot).await;
    Ok(Some(ExecFileUndoOutcome {
        snapshot_id: snapshot.id.to_string(),
        folder_name: display_folder_name(&snapshot.file.folder_path),
        relative_path: snapshot.file.relative_path,
        status,
    }))
}

/// Renderer-owned classification of one journal row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFileChangeClassification {
    Applied,
    Rejected,
}

/// The successful filesystem effect, absent for a rejected write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFileChangeKind {
    Created,
    Overwritten,
    Deleted,
}

/// Whether an applied file can still be safely reverted now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFileUndoAvailability {
    Available,
    AlreadyUndone,
    Stale,
    NotAvailable,
}

/// A binary format handled by the bundled #1056 document renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFilePreviewFormat {
    Pdf,
    Docx,
    Xlsx,
}

impl ExecFilePreviewFormat {
    fn for_path(path: &str) -> Option<Self> {
        match Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())?
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" => Some(Self::Pdf),
            "docx" => Some(Self::Docx),
            "xlsx" => Some(Self::Xlsx),
            _ => None,
        }
    }

    fn helper(self) -> &'static str {
        match self {
            Self::Pdf => "render_pdf.py",
            Self::Docx => "render_office.py",
            Self::Xlsx => "analyze_xlsx.py",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Pdf => "pdf",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
        }
    }
}

/// Whether one side of a binary before/after comparison can be requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFilePreviewAvailability {
    Available,
    Empty,
    Stale,
    TooLarge,
    Unavailable,
}

/// Renderer-safe selection metadata; bytes remain behind the scoped endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
pub(crate) struct ExecFileBinaryPreview {
    pub format: ExecFilePreviewFormat,
    pub before: ExecFilePreviewAvailability,
    pub after: ExecFilePreviewAvailability,
}

/// One renderer-safe row in a terminal turn's file-change summary, including a
/// bounded unified diff when both revisions are text or a binary preview
/// selector whose bytes remain behind the scoped preview endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub(crate) struct ExecFileChangeSummary {
    pub snapshot_id: String,
    pub folder_name: String,
    pub relative_path: String,
    pub classification: ExecFileChangeClassification,
    pub change: Option<ExecFileChangeKind>,
    pub rejection_reason: Option<ExecFileRejectionReason>,
    pub undo: ExecFileUndoAvailability,
    pub diff: Option<String>,
    pub binary_preview: Option<ExecFileBinaryPreview>,
}

const MAX_TEXT_DIFF_BYTES: u64 = 512 * 1_024;
const MAX_FILE_PREVIEW_INPUT_BYTES: u64 = MAX_EXEC_WORKSPACE_FILE_BYTES as u64;
const FILE_PREVIEW_TIMEOUT: Duration = Duration::from_secs(20);

/// Build durable change summaries grouped by turn without exposing host paths.
pub(crate) async fn list_file_change_summaries(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    chat_id: SessionId,
    scratch_folder: Option<&Path>,
) -> tidebreak_core::Result<std::collections::HashMap<TurnId, Vec<ExecFileChangeSummary>>> {
    let mut by_turn = std::collections::HashMap::<TurnId, Vec<_>>::new();
    for snapshot in store.list_exec_file_snapshots(chat_id).await? {
        let turn_id = snapshot.turn_id;
        by_turn
            .entry(turn_id)
            .or_default()
            .push(summarize_applied_change(blobs, snapshot, scratch_folder).await);
    }
    for rejection in store.list_exec_file_rejections(chat_id).await? {
        by_turn
            .entry(rejection.turn_id)
            .or_default()
            .push(ExecFileChangeSummary {
                snapshot_id: format!("rejected:{}", rejection.id),
                folder_name: folder_label(&rejection.file.folder_path, scratch_folder),
                relative_path: rejection.file.relative_path,
                classification: ExecFileChangeClassification::Rejected,
                change: None,
                rejection_reason: Some(rejection.file.reason),
                undo: ExecFileUndoAvailability::NotAvailable,
                diff: None,
                binary_preview: None,
            });
    }
    for files in by_turn.values_mut() {
        files.sort_by(|left, right| {
            (&left.folder_name, &left.relative_path)
                .cmp(&(&right.folder_name, &right.relative_path))
        });
    }
    Ok(by_turn)
}

async fn summarize_applied_change(
    blobs: &dyn BlobStore,
    snapshot: ExecFileSnapshot,
    scratch_folder: Option<&Path>,
) -> ExecFileChangeSummary {
    let change = match snapshot.file.change {
        ExecFileChange::Created => ExecFileChangeKind::Created,
        ExecFileChange::Overwritten => ExecFileChangeKind::Overwritten,
        ExecFileChange::Deleted => ExecFileChangeKind::Deleted,
    };
    let (undo, current) = inspect_current_change(blobs, &snapshot).await;
    let binary_preview = binary_preview_for(&snapshot, undo);
    let diff = if binary_preview.is_none() && undo == ExecFileUndoAvailability::Available {
        text_diff(blobs, &snapshot, current.as_deref()).await
    } else {
        None
    };
    ExecFileChangeSummary {
        snapshot_id: snapshot.id.to_string(),
        folder_name: folder_label(&snapshot.file.folder_path, scratch_folder),
        relative_path: snapshot.file.relative_path,
        classification: ExecFileChangeClassification::Applied,
        change: Some(change),
        rejection_reason: None,
        undo,
        diff,
        binary_preview,
    }
}

fn binary_preview_for(
    snapshot: &ExecFileSnapshot,
    undo: ExecFileUndoAvailability,
) -> Option<ExecFileBinaryPreview> {
    let format = ExecFilePreviewFormat::for_path(&snapshot.file.relative_path)?;
    let before = match snapshot.file.change {
        ExecFileChange::Created => ExecFilePreviewAvailability::Empty,
        ExecFileChange::Overwritten | ExecFileChange::Deleted => {
            if snapshot.file.prior_blob_id.is_none() {
                ExecFilePreviewAvailability::Unavailable
            } else if snapshot
                .file
                .prior_byte_len
                .is_some_and(|byte_len| byte_len > MAX_FILE_PREVIEW_INPUT_BYTES)
            {
                ExecFilePreviewAvailability::TooLarge
            } else {
                ExecFilePreviewAvailability::Available
            }
        }
    };
    let after = match snapshot.file.change {
        ExecFileChange::Deleted => ExecFilePreviewAvailability::Empty,
        ExecFileChange::Created | ExecFileChange::Overwritten => match undo {
            ExecFileUndoAvailability::Available | ExecFileUndoAvailability::NotAvailable => {
                ExecFilePreviewAvailability::Available
            }
            ExecFileUndoAvailability::Stale => ExecFilePreviewAvailability::Stale,
            ExecFileUndoAvailability::AlreadyUndone => ExecFilePreviewAvailability::Unavailable,
        },
    };
    Some(ExecFileBinaryPreview {
        format,
        before,
        after,
    })
}

async fn inspect_current_change(
    blobs: &dyn BlobStore,
    snapshot: &ExecFileSnapshot,
) -> (ExecFileUndoAvailability, Option<Vec<u8>>) {
    if snapshot.file.undo != ExecUndoState::Available {
        return (ExecFileUndoAvailability::NotAvailable, None);
    }
    let Some((prefix, name)) = split_journal_path(&snapshot.file.relative_path) else {
        return (ExecFileUndoAvailability::NotAvailable, None);
    };
    let folder = Path::new(&snapshot.file.folder_path);
    if !folder.is_absolute() {
        return (ExecFileUndoAvailability::NotAvailable, None);
    }
    let parent = try_resolve_scratch_directory(folder, prefix, false).await;
    let Ok(parent) = parent else {
        return match snapshot.file.change {
            ExecFileChange::Created => (ExecFileUndoAvailability::AlreadyUndone, None),
            ExecFileChange::Overwritten | ExecFileChange::Deleted => {
                (ExecFileUndoAvailability::Stale, None)
            }
        };
    };
    let current = current_digest(&parent, name).await;
    match snapshot.file.change {
        ExecFileChange::Created | ExecFileChange::Overwritten => {
            let Some(expected) = snapshot.file.new_sha256.as_deref().and_then(parse_sha256) else {
                return (ExecFileUndoAvailability::NotAvailable, None);
            };
            match current {
                Ok(Some(digest)) if digest == expected => {
                    let bytes = read_bounded(&parent, name)
                        .await
                        .filter(|bytes| sha256(bytes) == expected);
                    (ExecFileUndoAvailability::Available, bytes)
                }
                Ok(None) if snapshot.file.change == ExecFileChange::Created => {
                    (ExecFileUndoAvailability::AlreadyUndone, None)
                }
                Ok(Some(digest)) if prior_digest(blobs, snapshot).await == Some(digest) => {
                    (ExecFileUndoAvailability::AlreadyUndone, None)
                }
                _ => (ExecFileUndoAvailability::Stale, None),
            }
        }
        ExecFileChange::Deleted => match current {
            Ok(None) => (ExecFileUndoAvailability::Available, None),
            Ok(Some(digest)) if prior_digest(blobs, snapshot).await == Some(digest) => {
                (ExecFileUndoAvailability::AlreadyUndone, None)
            }
            _ => (ExecFileUndoAvailability::Stale, None),
        },
    }
}

async fn prior_digest(blobs: &dyn BlobStore, snapshot: &ExecFileSnapshot) -> Option<[u8; 32]> {
    Some(sha256(&load_prior_bytes(blobs, snapshot).await?))
}

async fn read_bounded(directory: &ScratchDir, name: &str) -> Option<Vec<u8>> {
    let mut file = directory.open_file(name).await.ok()?;
    let byte_len = file.metadata().ok()?.len();
    if byte_len > MAX_TEXT_DIFF_BYTES {
        return None;
    }
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut bytes = Vec::with_capacity(usize::try_from(byte_len).unwrap_or_default());
        file.read_to_end(&mut bytes).map(|_| bytes)
    })
    .await
    .ok()?
    .ok()
}

async fn text_diff(
    blobs: &dyn BlobStore,
    snapshot: &ExecFileSnapshot,
    current: Option<&[u8]>,
) -> Option<String> {
    let prior = match snapshot.file.change {
        ExecFileChange::Created => Vec::new(),
        ExecFileChange::Overwritten | ExecFileChange::Deleted => {
            if snapshot.file.prior_byte_len? > MAX_TEXT_DIFF_BYTES {
                return None;
            }
            load_prior_bytes(blobs, snapshot).await?
        }
    };
    let next = match snapshot.file.change {
        ExecFileChange::Deleted => &[][..],
        ExecFileChange::Created | ExecFileChange::Overwritten => current?,
    };
    let before = std::str::from_utf8(&prior).ok()?;
    let after = std::str::from_utf8(next).ok()?;
    if before.contains('\0') || after.contains('\0') {
        return None;
    }
    Some(
        similar::TextDiff::from_lines(before, after)
            .unified_diff()
            .context_radius(3)
            .header("before", "after")
            .to_string(),
    )
}

/// Which immutable side of a journaled change the renderer requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecFilePreviewRevision {
    Before,
    After,
}

/// One ephemeral, already-bounded raster result from the bundled renderer.
pub(crate) struct RenderedExecFilePreview {
    pub media_type: ImageMediaType,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Safe failure vocabulary for the renderer-facing preview endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecFilePreviewError {
    NotFound,
    Unsupported,
    Empty,
    Stale,
    TooLarge,
    Unavailable,
    RenderFailed,
}

pub(crate) struct ExecFilePreviewRequest<'a> {
    pub chat_id: SessionId,
    pub turn_id: TurnId,
    pub snapshot_id: uuid::Uuid,
    pub revision: ExecFilePreviewRevision,
    pub scripts_dir: Option<&'a Path>,
    pub temp_root: &'a Path,
}

/// Authorize, select, and render one revision without publishing a reusable
/// file or image identity. All filesystem paths remain server-side.
pub(crate) async fn render_file_change_preview(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    request: ExecFilePreviewRequest<'_>,
) -> Result<RenderedExecFilePreview, ExecFilePreviewError> {
    let snapshot = store
        .list_exec_file_snapshots(request.chat_id)
        .await
        .map_err(|_| ExecFilePreviewError::Unavailable)?
        .into_iter()
        .find(|snapshot| snapshot.turn_id == request.turn_id && snapshot.id == request.snapshot_id)
        .ok_or(ExecFilePreviewError::NotFound)?;
    let format = ExecFilePreviewFormat::for_path(&snapshot.file.relative_path)
        .ok_or(ExecFilePreviewError::Unsupported)?;
    let bytes = match request.revision {
        ExecFilePreviewRevision::Before => preview_before_bytes(blobs, &snapshot).await?,
        ExecFilePreviewRevision::After => preview_after_bytes(&snapshot).await?,
    };
    let scripts_dir = request
        .scripts_dir
        .ok_or(ExecFilePreviewError::Unavailable)?;
    render_document_bytes(format, bytes, scripts_dir, request.temp_root).await
}

async fn preview_before_bytes(
    blobs: &dyn BlobStore,
    snapshot: &ExecFileSnapshot,
) -> Result<Vec<u8>, ExecFilePreviewError> {
    if snapshot.file.change == ExecFileChange::Created {
        return Err(ExecFilePreviewError::Empty);
    }
    if snapshot
        .file
        .prior_byte_len
        .is_some_and(|byte_len| byte_len > MAX_FILE_PREVIEW_INPUT_BYTES)
    {
        return Err(ExecFilePreviewError::TooLarge);
    }
    load_prior_bytes(blobs, snapshot)
        .await
        .ok_or(ExecFilePreviewError::Unavailable)
}

async fn preview_after_bytes(snapshot: &ExecFileSnapshot) -> Result<Vec<u8>, ExecFilePreviewError> {
    if snapshot.file.change == ExecFileChange::Deleted {
        return Err(ExecFilePreviewError::Empty);
    }
    let expected = snapshot
        .file
        .new_sha256
        .as_deref()
        .and_then(parse_sha256)
        .ok_or(ExecFilePreviewError::Unavailable)?;
    let Some((prefix, name)) = split_journal_path(&snapshot.file.relative_path) else {
        return Err(ExecFilePreviewError::Unavailable);
    };
    let folder = Path::new(&snapshot.file.folder_path);
    if !folder.is_absolute() {
        return Err(ExecFilePreviewError::Unavailable);
    }
    let parent = try_resolve_scratch_directory(folder, prefix, false)
        .await
        .map_err(|_| ExecFilePreviewError::Stale)?;
    match current_digest(&parent, name).await {
        Ok(Some(digest)) if digest == expected => {}
        Ok(_) => return Err(ExecFilePreviewError::Stale),
        Err(()) => return Err(ExecFilePreviewError::Unavailable),
    }
    let mut file = parent.open_file(name).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ExecFilePreviewError::Stale
        } else {
            ExecFilePreviewError::Unavailable
        }
    })?;
    let byte_len = file
        .metadata()
        .map_err(|_| ExecFilePreviewError::Unavailable)?
        .len();
    if byte_len > MAX_FILE_PREVIEW_INPUT_BYTES {
        return Err(ExecFilePreviewError::TooLarge);
    }
    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        let mut bytes = Vec::with_capacity(usize::try_from(byte_len).unwrap_or_default());
        file.read_to_end(&mut bytes).map(|_| bytes)
    })
    .await
    .map_err(|_| ExecFilePreviewError::Unavailable)?
    .map_err(|_| ExecFilePreviewError::Unavailable)?;
    if sha256(&bytes) != expected {
        return Err(ExecFilePreviewError::Stale);
    }
    Ok(bytes)
}

async fn render_document_bytes(
    format: ExecFilePreviewFormat,
    bytes: Vec<u8>,
    scripts_dir: &Path,
    temp_root: &Path,
) -> Result<RenderedExecFilePreview, ExecFilePreviewError> {
    tokio::fs::create_dir_all(temp_root)
        .await
        .map_err(|_| ExecFilePreviewError::Unavailable)?;
    let temporary = tempfile::Builder::new()
        .prefix("file-preview-")
        .tempdir_in(temp_root)
        .map_err(|_| ExecFilePreviewError::Unavailable)?;
    let input = temporary
        .path()
        .join(format!("revision.{}", format.extension()));
    let output = temporary.path().join("preview");
    tokio::fs::create_dir(&output)
        .await
        .map_err(|_| ExecFilePreviewError::Unavailable)?;
    tokio::fs::write(&input, bytes)
        .await
        .map_err(|_| ExecFilePreviewError::Unavailable)?;

    let helper = scripts_dir.join(format.helper());
    if !helper.is_file() {
        return Err(ExecFilePreviewError::Unavailable);
    }
    run_document_helper(&helper, &input, &output, temporary.path()).await?;

    let scan = tokio::task::spawn_blocking(move || {
        tidebreak_code_execution::scan_preview_directory(&output)
    })
    .await
    .map_err(|_| ExecFilePreviewError::RenderFailed)?;
    let (image, data) = scan
        .images
        .into_iter()
        .next()
        .ok_or(ExecFilePreviewError::RenderFailed)?;
    Ok(RenderedExecFilePreview {
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        bytes: data.bytes().to_vec(),
    })
}

async fn run_document_helper(
    helper: &Path,
    input: &Path,
    output: &Path,
    working_directory: &Path,
) -> Result<(), ExecFilePreviewError> {
    for python in ["python3", "python"] {
        let mut command = tokio::process::Command::new(python);
        command
            .arg(helper)
            .arg(input)
            .arg("--preview-dir")
            .arg(output)
            .current_dir(working_directory)
            .kill_on_drop(true);
        match tokio::time::timeout(FILE_PREVIEW_TIMEOUT, command.output()).await {
            Ok(Ok(completed)) if completed.status.success() => return Ok(()),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                return Err(ExecFilePreviewError::RenderFailed);
            }
        }
    }
    Err(ExecFilePreviewError::Unavailable)
}

/// What the transcript calls the folder a change landed in.
///
/// The chat's private scratch is a runtime directory named after the chat id,
/// which is neither a folder the user attached nor a name they would recognize.
/// It is labelled by what it is instead, and the path stays server-side.
fn folder_label(folder_path: &str, scratch_folder: Option<&Path>) -> String {
    if scratch_folder.is_some_and(|scratch| scratch == Path::new(folder_path)) {
        return SCRATCH_FOLDER_LABEL.to_owned();
    }
    display_folder_name(folder_path)
}

/// How a private-scratch change names its location in the transcript.
const SCRATCH_FOLDER_LABEL: &str = "Scratch";

fn display_folder_name(folder_path: &str) -> String {
    Path::new(folder_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Folder")
        .to_owned()
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
    let source = DocumentBlob::from_bytes(&bytes);
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
    for (output, encoded) in digest.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
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
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use tidebreak_code_execution::{TrashSink, WriteOverlay};
    use tidebreak_core::{
        Chat, DbStore, ExecFileRejectionRecord, ExecFileSnapshotRecord, FsBlobStore,
    };

    use crate::state::BlobWriteGuard;

    struct TestTrash(std::sync::Mutex<Vec<(String, Vec<u8>)>>);

    #[async_trait::async_trait]
    impl TrashSink for TestTrash {
        async fn trash(&self, path: &Path) -> Result<(), String> {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "trash name is not UTF-8".to_owned())?
                .to_owned();
            let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
            self.0.lock().unwrap().push((name, bytes));
            Ok(())
        }
    }

    /// The issue contract: the turn journal survives process state, and undo
    /// restores overwritten/deleted bytes while removing a file the turn made.
    #[tokio::test]
    async fn a_turns_journaled_file_changes_undo_after_restart() {
        let home = tempfile::tempdir().unwrap();
        let database = home.path().join("tidebreak.db");
        let database_url = format!("sqlite://{}?mode=rwc", database.display());
        let blob_root = home.path().join("blobs");
        let scratch = home.path().join("scratch");
        let granted = home.path().join("granted");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&granted).unwrap();

        let original = b"original bytes";
        let original_source = DocumentBlob::from_bytes(original);
        std::fs::write(granted.join("notes.md"), original).unwrap();
        std::fs::write(granted.join("removed.txt"), b"bring me back").unwrap();

        let store = Arc::new(DbStore::connect(&database_url).await.unwrap());
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
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

        let trash = TestTrash(std::sync::Mutex::new(Vec::new()));
        let materialized = overlay.materialize_with_trash(Some(&sink), &trash).await;
        assert_eq!(materialized.written.len(), 3);
        assert!(materialized.rejected.is_empty());
        assert_eq!(
            *trash.0.lock().unwrap(),
            vec![("removed.txt".to_owned(), b"bring me back".to_vec())]
        );
        sink.commit(chat.id, turn_id).await.unwrap();
        store
            .record_exec_file_rejections(
                chat.id,
                turn_id,
                &[ExecFileRejectionRecord {
                    folder_path: granted.display().to_string(),
                    relative_path: "stale.md".into(),
                    reason: ExecFileRejectionReason::Stale,
                }],
            )
            .await
            .unwrap();
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
        let summaries =
            list_file_change_summaries(&restarted_store, &restarted_blobs, chat.id, None)
                .await
                .unwrap();
        let files = &summaries[&turn_id];
        assert_eq!(files.len(), 4);
        assert!(files.iter().any(|file| {
            file.relative_path == "stale.md"
                && file.classification == ExecFileChangeClassification::Rejected
                && file.rejection_reason == Some(ExecFileRejectionReason::Stale)
        }));
        let notes = files
            .iter()
            .find(|file| file.relative_path == "notes.md")
            .unwrap();
        assert_eq!(notes.undo, ExecFileUndoAvailability::Available);
        assert!(notes
            .diff
            .as_deref()
            .is_some_and(|diff| diff.contains("-original bytes") && diff.contains("+replacement")));

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
            DocumentBlob::from_bytes(&restored).sha256,
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

    /// The reason this journal exists: `write_file` is a structured call, so an
    /// overwrite it performs restores through exactly the path a folder
    /// write-back's does — and a create, which destroys nothing, journals
    /// nothing.
    #[tokio::test]
    async fn a_scratch_overwrite_restores_through_the_turn_journal() {
        use cap_std::ambient_authority;
        use cap_std::fs::Dir;
        use tidebreak_core::{Tool, ToolCtx, ToolScratch, WriteFile};

        let home = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            home.path().join("tidebreak.db").display()
        );
        let store = Arc::new(DbStore::connect(&database_url).await.unwrap());
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let blobs = Arc::new(FsBlobStore::new(home.path().join("blobs")));
        let scratch = home.path().join("scratch").join(chat.id.to_string());
        std::fs::create_dir_all(&scratch).unwrap();

        let turn_id = TurnId::new();
        let journal = Arc::new(TurnScratchJournal::new(
            store.clone(),
            blobs.clone(),
            Arc::new(BlobWriteGuard::new(home.path().join("blob-locks"))),
            scratch.clone(),
            chat.id,
            turn_id,
        ));
        let ctx = ToolCtx::with_private_scratch(
            chat.id,
            None,
            ToolScratch::from_dir(Dir::open_ambient_dir(&scratch, ambient_authority()).unwrap())
                .with_write_journal(journal),
        );

        let write = |content: &'static str| {
            WriteFile.execute(
                &ctx,
                serde_json::json!({ "path": "notes/plan.md", "content": content }),
            )
        };
        assert!(!write("first draft").await.unwrap().is_error);
        assert!(
            store
                .list_exec_file_snapshots(chat.id)
                .await
                .unwrap()
                .is_empty(),
            "creating a file destroys nothing and must not retain a copy"
        );
        assert!(!write("second draft").await.unwrap().is_error);

        let summaries = list_file_change_summaries(&*store, &*blobs, chat.id, Some(&scratch))
            .await
            .unwrap();
        let files = &summaries[&turn_id];
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "notes/plan.md");
        assert_eq!(files[0].folder_name, SCRATCH_FOLDER_LABEL);
        assert_eq!(files[0].change, Some(ExecFileChangeKind::Overwritten));
        assert_eq!(files[0].undo, ExecFileUndoAvailability::Available);
        assert!(files[0]
            .diff
            .as_deref()
            .is_some_and(|diff| diff.contains("-first draft") && diff.contains("+second draft")));

        let outcome = undo_turn_file_changes(&*store, &*blobs, chat.id, turn_id)
            .await
            .unwrap();
        assert_eq!(
            outcome
                .files
                .iter()
                .map(|file| file.status)
                .collect::<Vec<_>>(),
            vec![ExecFileUndoStatus::Restored]
        );
        assert_eq!(
            std::fs::read(scratch.join("notes").join("plan.md")).unwrap(),
            b"first draft"
        );
    }

    /// The binary-preview contract crosses the durable journal, retained blob,
    /// digest-guarded granted-root file, bundled helper, and bounded image scan.
    #[tokio::test]
    async fn a_workbook_change_renders_distinct_before_and_after_previews() {
        if !["python3", "python"].iter().any(|python| {
            std::process::Command::new(python)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        }) {
            return;
        }

        let home = tempfile::tempdir().unwrap();
        let database = home.path().join("tidebreak.db");
        let database_url = format!("sqlite://{}?mode=rwc", database.display());
        let granted = home.path().join("granted");
        let scripts = home.path().join("scripts");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&scripts).unwrap();

        let store = DbStore::connect(&database_url).await.unwrap();
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let blobs = FsBlobStore::new(home.path().join("blobs"));
        let before = b"old workbook bytes";
        let after = b"new workbook bytes";
        let prior = DocumentBlob::from_bytes(before);
        blobs.put(prior.id, before.to_vec()).await.unwrap();
        std::fs::write(granted.join("forecast.xlsx"), after).unwrap();
        let turn_id = TurnId::new();
        store
            .record_exec_file_snapshots(
                chat.id,
                turn_id,
                &[ExecFileSnapshotRecord {
                    folder_path: granted.display().to_string(),
                    relative_path: "forecast.xlsx".into(),
                    change: ExecFileChange::Overwritten,
                    prior_blob_id: Some(prior.id),
                    prior_byte_len: Some(prior.byte_len),
                    new_sha256: Some(sha256_hex(after)),
                    undo: ExecUndoState::Available,
                }],
            )
            .await
            .unwrap();
        let snapshot = store
            .list_exec_file_snapshots(chat.id)
            .await
            .unwrap()
            .pop()
            .unwrap();

        let summary = list_file_change_summaries(&store, &blobs, chat.id, None)
            .await
            .unwrap();
        assert_eq!(
            summary[&turn_id][0].binary_preview,
            Some(ExecFileBinaryPreview {
                format: ExecFilePreviewFormat::Xlsx,
                before: ExecFilePreviewAvailability::Available,
                after: ExecFilePreviewAvailability::Available,
            })
        );
        assert!(summary[&turn_id][0].diff.is_none());

        write_preview_fixture(&scripts.join("old.png"), [180, 20, 20]);
        write_preview_fixture(&scripts.join("new.png"), [20, 20, 180]);
        std::fs::write(
            scripts.join("analyze_xlsx.py"),
            r#"import shutil
import sys
from pathlib import Path

source = Path(sys.argv[1]).read_bytes()
output = Path(sys.argv[sys.argv.index("--preview-dir") + 1])
output.mkdir(parents=True, exist_ok=True)
fixture = "old.png" if source == b"old workbook bytes" else "new.png"
shutil.copyfile(Path(__file__).with_name(fixture), output / "overview-grid.png")
"#,
        )
        .unwrap();

        let before_preview = render_file_change_preview(
            &store,
            &blobs,
            ExecFilePreviewRequest {
                chat_id: chat.id,
                turn_id,
                snapshot_id: snapshot.id,
                revision: ExecFilePreviewRevision::Before,
                scripts_dir: Some(&scripts),
                temp_root: &home.path().join("preview-temp"),
            },
        )
        .await
        .unwrap();
        let after_preview = render_file_change_preview(
            &store,
            &blobs,
            ExecFilePreviewRequest {
                chat_id: chat.id,
                turn_id,
                snapshot_id: snapshot.id,
                revision: ExecFilePreviewRevision::After,
                scripts_dir: Some(&scripts),
                temp_root: &home.path().join("preview-temp"),
            },
        )
        .await
        .unwrap();

        assert_eq!(before_preview.media_type, ImageMediaType::Png);
        assert_eq!(after_preview.media_type, ImageMediaType::Png);
        assert_ne!(before_preview.bytes, after_preview.bytes);
    }

    fn write_preview_fixture(path: &Path, color: [u8; 3]) {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 2, Rgb(color)))
            .save_with_format(path, ImageFormat::Png)
            .unwrap();
    }
}

//! Host-side sync of the execution `output/` directory into the durable
//! conversation output record.
//!
//! Files an agent saves under `output/` during code execution are the
//! files-first creation path for user-visible outputs: after each command the
//! host scans the directory and publishes what it finds, keyed by filename
//! within the conversation. A new filename creates an output at revision 1; an
//! existing live output with the same filename gains a revision when the bytes
//! changed and is left untouched when they did not. The model never names
//! output identities — it just writes files — and deleting a file from
//! `output/` never deletes the durable record.
//!
//! The scan is deliberately non-fatal: a file the host cannot accept (too
//! large, unreadable, over the revision cap) becomes a note for the model
//! rather than a failed command.

use std::path::Path;

use cap_std::fs::Dir;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::deliverable::{
    binary_media_type_for_extension, deliverable_media_type, output_revision_relative_path,
    validate_portable_filename, CreateOutput, DeliverableKind, NewOutputRevision, RevisionProducer,
    MAX_BINARY_DELIVERABLE_BYTES, MAX_DELIVERABLE_BYTES,
};
use crate::error::{AgentError, Result};
use crate::id::{CallId, ChatId, OutputId, OutputRevisionId};
use crate::storage::Store;
use crate::OutputRecord;

/// Conventional directory, relative to a conversation's private scratch, whose
/// files the host publishes as durable outputs.
pub const EXEC_OUTPUT_DIRECTORY: &str = "output";

/// Most files considered by one scan. Files beyond the cap are named in a note
/// rather than silently ignored.
pub const MAX_OUTPUT_SCAN_FILES: usize = 64;

/// Upper bound on the filename lookup when matching scan files to existing
/// outputs. Far above the catalog's own display cap. Shared with the agent's
/// filename resolution for output write-backs so both match the same window.
pub(crate) const OUTPUT_LOOKUP_LIMIT: u64 = 1_000;

/// What one scanned file did to the output record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSyncStatus {
    /// A new output was created at revision 1.
    Created,
    /// An existing output gained a new revision.
    Updated,
    /// The file's bytes match the output's current revision; nothing was
    /// written.
    Unchanged,
}

/// One file the scan accepted (or matched) against the output record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSyncEntry {
    /// Display filename, equal to the file's name under `output/`.
    pub filename: String,
    /// The durable output the file landed on.
    pub output_id: OutputId,
    /// The output's current revision ordinal after the sync.
    pub ordinal: u32,
    /// Whether the file created, updated, or matched the output.
    pub status: OutputSyncStatus,
}

/// The outcome of scanning one conversation's `output/` directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputDirectorySync {
    /// Accepted or matched files, in the deterministic scan order.
    pub entries: Vec<OutputSyncEntry>,
    /// Files the scan could not accept, with actionable reasons.
    pub notes: Vec<String>,
}

/// A candidate file read out of `output/`, classified but not yet recorded.
struct ScanCandidate {
    filename: String,
    kind: DeliverableKind,
    content: Vec<u8>,
}

/// Scan `output/` under one conversation's private scratch and publish its
/// files as durable outputs.
///
/// `scratch` is the exact owning conversation's private scratch directory.
/// Identities derive from the (call, filename) pair, so an exact retry of the
/// same call republishes idempotently instead of forking records.
pub async fn sync_output_directory(
    store: &dyn Store,
    scratch: &Dir,
    chat_id: ChatId,
    call_id: CallId,
    producer: RevisionProducer,
    now: DateTime<Utc>,
) -> Result<OutputDirectorySync> {
    let mut sync = OutputDirectorySync::default();

    // Reading is blocking capability I/O; keep it off the async runtime.
    let read_scratch = scratch
        .try_clone()
        .map_err(|error| AgentError::Store(format!("could not open private scratch: {error}")))?;
    let (candidates, mut read_notes) =
        tokio::task::spawn_blocking(move || read_output_candidates(&read_scratch))
            .await
            .map_err(|error| AgentError::Store(format!("output scan task failed: {error}")))?;
    sync.notes.append(&mut read_notes);
    if candidates.is_empty() {
        return Ok(sync);
    }

    // One lookup serves every candidate: the newest live output per filename.
    let existing = store.list_outputs(chat_id, OUTPUT_LOOKUP_LIMIT).await?;

    for candidate in candidates {
        match record_candidate(
            store, scratch, chat_id, call_id, producer, now, &existing, &candidate,
        )
        .await
        {
            Ok(entry) => sync.entries.push(entry),
            Err(error) => sync.notes.push(format!(
                "output/{} could not be published: {error}",
                candidate.filename
            )),
        }
    }
    Ok(sync)
}

/// Record one classified candidate against the output record.
#[allow(clippy::too_many_arguments)]
async fn record_candidate(
    store: &dyn Store,
    scratch: &Dir,
    chat_id: ChatId,
    call_id: CallId,
    producer: RevisionProducer,
    now: DateTime<Utc>,
    existing: &[OutputRecord],
    candidate: &ScanCandidate,
) -> Result<OutputSyncEntry> {
    let sha256: [u8; 32] = Sha256::digest(&candidate.content).into();
    let byte_len = candidate.content.len() as u64;

    // `list_outputs` orders newest-updated first and excludes deleted outputs,
    // so the first filename match is the live record this file addresses.
    let matched = existing
        .iter()
        .find(|output| output.filename == candidate.filename);

    if let Some(output) = matched {
        let current = store
            .get_output_revision(output.current_revision)
            .await?
            .ok_or_else(|| AgentError::Store("current revision is missing".into()))?;
        if current.sha256 == sha256 && current.byte_len == byte_len {
            return Ok(OutputSyncEntry {
                filename: candidate.filename.clone(),
                output_id: output.id,
                ordinal: current.ordinal,
                status: OutputSyncStatus::Unchanged,
            });
        }
        let revision_id = OutputRevisionId::for_call_artifact(call_id, &candidate.filename);
        publish_revision_bytes(scratch, output.id, revision_id, &candidate.content).await?;
        let updated = store
            .append_output_revision(
                output.id,
                &NewOutputRevision {
                    id: revision_id,
                    byte_len,
                    sha256,
                    turn_id: None,
                    producing_run_id: None,
                    created_at: now,
                }
                .with_producer(producer),
            )
            .await?;
        return Ok(OutputSyncEntry {
            filename: candidate.filename.clone(),
            output_id: updated.id,
            ordinal: updated.revision_count,
            status: OutputSyncStatus::Updated,
        });
    }

    let output_id = OutputId::for_call_artifact(call_id, &candidate.filename);
    let revision_id = OutputRevisionId::for_call_artifact(call_id, &candidate.filename);
    publish_revision_bytes(scratch, output_id, revision_id, &candidate.content).await?;
    let created = store
        .create_output(&CreateOutput {
            id: output_id,
            chat_id,
            filename: candidate.filename.clone(),
            kind: candidate.kind.clone(),
            revision: NewOutputRevision {
                id: revision_id,
                byte_len,
                sha256,
                turn_id: None,
                producing_run_id: None,
                created_at: now,
            }
            .with_producer(producer),
        })
        .await?;
    Ok(OutputSyncEntry {
        filename: candidate.filename.clone(),
        output_id: created.id,
        ordinal: created.revision_count,
        status: OutputSyncStatus::Created,
    })
}

/// Publish revision bytes at the write-once derived path.
async fn publish_revision_bytes(
    scratch: &Dir,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    content: &[u8],
) -> Result<()> {
    let relative_path = output_revision_relative_path(output_id, revision_id);
    let scratch = scratch
        .try_clone()
        .map_err(|error| AgentError::Store(format!("could not open private scratch: {error}")))?;
    let content = content.to_vec();
    tokio::task::spawn_blocking(move || {
        crate::tools::private_scratch::publish_immutable_file(&scratch, &relative_path, &content)
    })
    .await
    .map_err(|error| AgentError::Store(format!("publication task failed: {error}")))?
    .map_err(|error| AgentError::Store(format!("could not publish output revision: {error}")))
}

/// Read and classify the acceptable files directly under `output/`.
///
/// Deterministic (alphabetical) order, bounded at [`MAX_OUTPUT_SCAN_FILES`].
/// Anything skipped is explained in a note.
fn read_output_candidates(scratch: &Dir) -> (Vec<ScanCandidate>, Vec<String>) {
    let mut notes = Vec::new();

    // A symlinked `output/` planted by local exec would hand host files from an
    // arbitrary directory to the catalog; refuse it rather than follow it.
    match scratch.symlink_metadata(EXEC_OUTPUT_DIRECTORY) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            notes.push(
                "outputs unavailable: output/ is not a private workspace directory. Remove it and rerun."
                    .into(),
            );
            return (Vec::new(), notes);
        }
        Err(_) => return (Vec::new(), notes),
    }
    let Ok(directory) = scratch.open_dir(EXEC_OUTPUT_DIRECTORY) else {
        notes.push("outputs unavailable: output/ could not be read".into());
        return (Vec::new(), notes);
    };
    let Ok(entries) = directory.entries() else {
        notes.push("outputs unavailable: output/ could not be read".into());
        return (Vec::new(), notes);
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            notes.push("a file with a non-UTF-8 name was ignored".into());
            continue;
        };
        if name.starts_with('.') {
            // Dotfiles are workspace plumbing and never publishable.
            continue;
        }
        match entry.metadata().map(|metadata| metadata.file_type()) {
            Ok(file_type) if file_type.is_file() => names.push(name),
            Ok(file_type) if file_type.is_dir() => notes.push(format!(
                "output/{name}/ was not published: only files directly in output/ become outputs; save each file at the top level or produce a single archive"
            )),
            _ => notes.push(format!(
                "output/{name} was not published: it is not a regular file"
            )),
        }
    }
    names.sort();
    if names.len() > MAX_OUTPUT_SCAN_FILES {
        notes.push(format!(
            "output/ file cap is {MAX_OUTPUT_SCAN_FILES}; ignored {}",
            names[MAX_OUTPUT_SCAN_FILES..].join(", ")
        ));
        names.truncate(MAX_OUTPUT_SCAN_FILES);
    }

    let mut candidates = Vec::new();
    for name in names {
        match classify_candidate(&directory, &name) {
            Ok(candidate) => candidates.push(candidate),
            Err(reason) => notes.push(format!("output/{name} was not published: {reason}")),
        }
    }
    (candidates, notes)
}

/// Read one file and classify it as a text deliverable or binary artifact.
fn classify_candidate(directory: &Dir, name: &str) -> std::result::Result<ScanCandidate, String> {
    validate_portable_filename(name).map_err(str::to_owned)?;

    let text_media_type = deliverable_media_type(name);
    let ceiling = if text_media_type.is_some() {
        MAX_DELIVERABLE_BYTES
    } else {
        MAX_BINARY_DELIVERABLE_BYTES
    };

    let metadata = directory
        .metadata(Path::new(name))
        .map_err(|_| "the file could not be read".to_owned())?;
    if metadata.len() == 0 {
        return Err("the file is empty".into());
    }
    if metadata.len() > ceiling as u64 {
        return Err(if text_media_type.is_some() {
            format!(
                "text outputs are limited to {MAX_DELIVERABLE_BYTES} bytes; split it or produce a binary format"
            )
        } else {
            format!("files are limited to {MAX_BINARY_DELIVERABLE_BYTES} bytes")
        });
    }
    let content = directory
        .read(Path::new(name))
        .map_err(|_| "the file could not be read".to_owned())?;
    if content.len() as u64 != metadata.len() || content.len() > ceiling {
        return Err("the file changed while being read".into());
    }

    let kind = match text_media_type {
        Some(_) => {
            let text = std::str::from_utf8(&content)
                .map_err(|_| "text outputs must be valid UTF-8".to_owned())?;
            if text.contains('\0') {
                return Err("text outputs must not contain NUL characters".into());
            }
            DeliverableKind::Text
        }
        None => DeliverableKind::Binary {
            media_type: binary_media_type_for_extension(name).to_owned(),
        },
    };
    Ok(ScanCandidate {
        filename: name.to_owned(),
        kind,
        content,
    })
}

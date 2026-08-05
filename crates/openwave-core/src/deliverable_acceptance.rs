//! Host-side acceptance of workspace artifacts into the conversation output
//! record.
//!
//! A file produced inside an execution provider's durable workspace is only a
//! *proposal* until the host accepts it. Acceptance is a host operation under
//! the live-consent invariant — the model never self-accepts — and it is what
//! turns bytes pulled back with `WorkspaceLifecycle::get_workspace_file` into a
//! durable, exportable output revision. The bytes land in the exact
//! conversation's private scratch under the same write-once revision path as a
//! text deliverable, so every downstream surface (the desktop catalog, preview,
//! and Save As… export) treats an accepted binary artifact exactly like any
//! other output.

use std::io::Read as _;

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{Dir, OpenOptions};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::deliverable::{
    output_revision_relative_path, revision_byte_ceiling, CreateOutput, DeliverableKind,
    NewOutputRevision, OutputRevision, RevisionProducer, MAX_BINARY_DELIVERABLE_BYTES,
    OUTPUTS_DIRECTORY,
};
use crate::error::{AgentError, Result};
use crate::id::{ChatId, OutputId, OutputRevisionId};
use crate::storage::Store;
use crate::OutputRecord;

/// A workspace artifact the host proposes to accept into an output record.
///
/// The bytes are already in hand — pulled from the provider workspace by
/// `WorkspaceLifecycle::get_workspace_file` — so acceptance is decoupled from
/// any particular provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceArtifactProposal {
    /// Caller-minted output identity. A fresh id creates a new output; an
    /// existing id (with `revise` set) appends a revision, so an ambiguous
    /// acceptance can be retried without forking the record.
    pub output_id: OutputId,
    /// Conversation that will own the accepted output.
    pub chat_id: ChatId,
    /// Portable display filename shown in the catalog and save dialog.
    pub filename: String,
    /// Declared media type of the artifact, validated as binary.
    pub media_type: String,
    /// Caller-minted revision identity, also the private-scratch filename.
    pub revision_id: OutputRevisionId,
    /// The turn or background run that produced the artifact.
    pub producer: RevisionProducer,
    /// Append a revision to an existing output rather than create a new one.
    pub revise: bool,
    /// Artifact bytes pulled from the workspace.
    pub content: Vec<u8>,
    /// Host-stamped acceptance time.
    pub created_at: DateTime<Utc>,
}

/// Accept a workspace artifact into the conversation output record.
///
/// `scratch` is the exact owning conversation's private scratch directory. The
/// bytes are published once at the derived revision path and then recorded, so a
/// display filename can never steer where they are written and a retry with the
/// same identity and bytes is idempotent.
pub async fn accept_workspace_artifact(
    store: &dyn Store,
    scratch: &Dir,
    proposal: &WorkspaceArtifactProposal,
) -> Result<OutputRecord> {
    if proposal.content.is_empty() {
        return Err(AgentError::Store("workspace artifact is empty".into()));
    }
    if proposal.content.len() > MAX_BINARY_DELIVERABLE_BYTES {
        return Err(AgentError::Store(format!(
            "workspace artifact is too large (maximum {MAX_BINARY_DELIVERABLE_BYTES} bytes)"
        )));
    }

    let byte_len = proposal.content.len() as u64;
    let sha256: [u8; 32] = Sha256::digest(&proposal.content).into();
    let relative_path = output_revision_relative_path(proposal.output_id, proposal.revision_id);

    // Publishing is blocking capability I/O; keep it off the async runtime.
    let scratch = scratch
        .try_clone()
        .map_err(|error| AgentError::Store(format!("could not open private scratch: {error}")))?;
    let content = proposal.content.clone();
    tokio::task::spawn_blocking(move || {
        crate::tools::private_scratch::publish_immutable_file(&scratch, &relative_path, &content)
    })
    .await
    .map_err(|error| AgentError::Store(format!("artifact publication task failed: {error}")))?
    .map_err(|error| AgentError::Store(format!("could not publish accepted artifact: {error}")))?;

    let revision = NewOutputRevision {
        id: proposal.revision_id,
        byte_len,
        sha256,
        // A binary artifact carries no retrieval citations: it is host-accepted
        // bytes, not a turn's cited text.
        turn_id: None,
        producing_run_id: None,
        created_at: proposal.created_at,
    }
    .with_producer(proposal.producer);

    if proposal.revise {
        store
            .append_output_revision(proposal.output_id, &revision)
            .await
    } else {
        store
            .create_output(&CreateOutput {
                id: proposal.output_id,
                chat_id: proposal.chat_id,
                filename: proposal.filename.clone(),
                kind: DeliverableKind::Binary {
                    media_type: proposal.media_type.clone(),
                },
                revision,
            })
            .await
    }
}

/// Restore an output to the content of one of its earlier revisions.
///
/// Restoring is append-only, Google-Docs style: the target revision's bytes are
/// republished as a **new** revision at the head of the history, so nothing is
/// rewound, renumbered, or lost, and the step can itself be undone by another
/// restore. Restoring the revision that is already current is a no-op that
/// returns the record unchanged.
///
/// The appended revision carries no producer — neither a turn nor a run — which
/// durably marks it as a user action. Its identity derives from the (target
/// revision, new ordinal) pair, so retrying an ambiguous restore lands on the
/// same revision instead of appending twice.
pub async fn restore_output_to_revision(
    store: &dyn Store,
    scratch: &Dir,
    chat_id: ChatId,
    output_id: OutputId,
    target_revision_id: OutputRevisionId,
    now: DateTime<Utc>,
) -> Result<OutputRecord> {
    let output = store
        .get_output(output_id)
        .await?
        .filter(|output| output.chat_id == chat_id && output.deleted_at.is_none())
        .ok_or_else(|| AgentError::Store("output not found in this conversation".into()))?;
    let target = store
        .get_output_revision(target_revision_id)
        .await?
        .filter(|revision| revision.output_id == output.id)
        .ok_or_else(|| AgentError::Store("that version does not belong to this output".into()))?;
    if output.current_revision == target.id {
        return Ok(output);
    }
    // Also a no-op when the head already carries the target's exact content —
    // which is what a retried restore observes after its first attempt
    // committed. Comparing content rather than identity makes the retry
    // idempotent without a caller-threaded expected ordinal.
    let current = store
        .get_output_revision(output.current_revision)
        .await?
        .ok_or_else(|| AgentError::Store("current revision is missing".into()))?;
    if current.sha256 == target.sha256 && current.byte_len == target.byte_len {
        return Ok(output);
    }

    let content = read_revision_bytes(scratch, &output, &target).await?;
    let revision_id = OutputRevisionId::for_restore(target.id, output.revision_count + 1);
    let relative_path = output_revision_relative_path(output.id, revision_id);
    let publish_scratch = scratch
        .try_clone()
        .map_err(|error| AgentError::Store(format!("could not open private scratch: {error}")))?;
    let publish_content = content.clone();
    tokio::task::spawn_blocking(move || {
        crate::tools::private_scratch::publish_immutable_file(
            &publish_scratch,
            &relative_path,
            &publish_content,
        )
    })
    .await
    .map_err(|error| AgentError::Store(format!("restore publication task failed: {error}")))?
    .map_err(|error| AgentError::Store(format!("could not publish restored revision: {error}")))?;

    let restored = store
        .append_output_revision(
            output.id,
            &NewOutputRevision {
                id: revision_id,
                byte_len: target.byte_len,
                sha256: target.sha256,
                // No producer: both-absent durably marks a user action.
                turn_id: None,
                producing_run_id: None,
                created_at: now,
            },
        )
        .await?;

    // A host note tells the model what the user did between turns, so its next
    // turn re-reads the file instead of overwriting the restore with stale
    // context. The next `load_transcript` picks the `System` message up; the
    // adapters serialize it as ordinary user-context text. Best-effort: the
    // restore has already committed, and a lost note must not fail it.
    let _ = store
        .append_message(&crate::model::Message {
            id: crate::id::MessageId::new(),
            chat_id,
            // No turn produced this; messages carry no turn foreign key, so a
            // fresh id is an inert marker rather than a claim on turn state.
            turn_id: crate::id::TurnId::new(),
            role: crate::model::Role::System,
            reasoning: Default::default(),
            content: format!(
                "User restored output '{}' to the content of version {} \
                 (now the latest version, v{}). Re-read output/{} before \
                 relying on or changing it.",
                output.filename, target.ordinal, restored.revision_count, output.filename
            ),
            created_at: now,
        })
        .await;
    Ok(restored)
}

/// Read and verify one revision's bytes from the conversation's private
/// scratch, refusing symlinks at every component.
async fn read_revision_bytes(
    scratch: &Dir,
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<Vec<u8>> {
    let scratch = scratch
        .try_clone()
        .map_err(|error| AgentError::Store(format!("could not open private scratch: {error}")))?;
    let output = output.clone();
    let revision = revision.clone();
    tokio::task::spawn_blocking(move || {
        let unavailable = || AgentError::Store("that version's content is unavailable".into());
        let ceiling = revision_byte_ceiling(&output.media_type);
        if revision.byte_len > ceiling as u64 {
            return Err(unavailable());
        }
        let open_child = |parent: &Dir, name: &str| -> Result<Dir> {
            let metadata = parent.symlink_metadata(name).map_err(|_| unavailable())?;
            if !metadata.is_dir() {
                return Err(unavailable());
            }
            parent.open_dir_nofollow(name).map_err(|_| unavailable())
        };
        let outputs = open_child(&scratch, OUTPUTS_DIRECTORY)?;
        let revisions = open_child(&outputs, &output.id.to_string())?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = revisions
            .open_with(revision.id.to_string(), &options)
            .map_err(|_| unavailable())?;
        let metadata = file.metadata().map_err(|_| unavailable())?;
        if !metadata.is_file() || metadata.len() != revision.byte_len {
            return Err(unavailable());
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(ceiling as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        if bytes.len() as u64 != revision.byte_len
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != revision.sha256
        {
            return Err(unavailable());
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| AgentError::Store(format!("revision read task failed: {error}")))?
}

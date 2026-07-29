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

use cap_std::fs::Dir;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::deliverable::{
    output_revision_relative_path, CreateOutput, DeliverableKind, NewOutputRevision,
    RevisionProducer, MAX_BINARY_DELIVERABLE_BYTES,
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
        citations: Vec::new(),
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

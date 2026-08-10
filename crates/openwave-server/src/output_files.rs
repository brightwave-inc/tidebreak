//! Reading a conversation output's immutable bytes out of private scratch.
//!
//! Outputs are stored per conversation under `scratch/<chat>/outputs/<output>/
//! <revision>`, and every reader has to answer the same questions before it
//! touches one: does this revision belong to this output, does this output
//! belong to this conversation, and are the bytes on disk still the exact
//! bytes the store recorded. Those checks used to live in the desktop shell,
//! where only a Tauri command could reach them. They live here so the HTTP
//! routes and the desktop's native save dialog share one implementation
//! instead of two that can drift.
//!
//! Nothing here trusts a path. Every directory component is opened without
//! following symlinks and re-checked as a regular directory, and the bytes are
//! verified against the revision's recorded length and digest before they are
//! returned.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use sha2::{Digest, Sha256};

use openwave_core::{
    revision_byte_ceiling, ChatId, OutputId, OutputRecord, OutputRevision, OutputRevisionId, Store,
    OUTPUTS_DIRECTORY,
};

/// The one live output plus its current revision, bound to one conversation.
///
/// A soft-deleted output, an output owned by another conversation, and an
/// output that never existed are all the same answer: callers must not be able
/// to tell them apart from the outside.
pub async fn require_live_output(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    output_id: OutputId,
) -> Result<(OutputRecord, OutputRevision), String> {
    let output = store
        .get_output(output_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|output| output.chat_id == chat_id && output.deleted_at.is_none())
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    let revision = store
        .get_output_revision(output.current_revision)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|revision| revision.output_id == output.id)
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    Ok((output, revision))
}

/// One exact revision of a live output, by id.
pub async fn require_output_revision(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    output_id: OutputId,
    revision_id: OutputRevisionId,
) -> Result<(OutputRecord, OutputRevision), String> {
    let (output, current) = require_live_output(store, chat_id, output_id).await?;
    if current.id == revision_id {
        return Ok((output, current));
    }
    let revision = store
        .get_output_revision(revision_id)
        .await
        .map_err(|_| "Could not load that version".to_owned())?
        .filter(|revision| revision.output_id == output.id)
        .ok_or_else(|| "That version does not belong to this output".to_owned())?;
    Ok((output, revision))
}

/// Re-assert that the exact revision is still current and still content-identical.
///
/// A native save dialog can stay open while the output or the conversation is
/// deleted, so the one authorized host write revalidates durable identity
/// immediately before it happens.
pub async fn require_exact_revision(
    store: &Arc<dyn Store>,
    chat_id: ChatId,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    byte_len: u64,
    sha256: [u8; 32],
) -> Result<(), String> {
    let output = store
        .get_output(output_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|output| {
            output.chat_id == chat_id
                && output.deleted_at.is_none()
                && output.current_revision == revision_id
        })
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    let revision = store
        .get_output_revision(revision_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|revision| {
            revision.output_id == output.id
                && revision.byte_len == byte_len
                && revision.sha256 == sha256
        })
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    if revision.id != revision_id {
        return Err("Output not found in this conversation".to_owned());
    }
    Ok(())
}

/// Open the exact conversation's private scratch directory, refusing symlinked
/// components. This is the directory the append-only revision writers in
/// `openwave-core` publish into.
pub fn open_chat_scratch(scratch_root: &Path, chat_id: ChatId) -> Result<Dir, String> {
    let Some(root) = open_regular_directory(scratch_root)? else {
        return Err("Output content is unavailable".to_owned());
    };
    let chat_name = chat_id.to_string();
    if !is_regular_child_directory(&root, &chat_name)? {
        return Err("Output content is unavailable".to_owned());
    }
    root.open_dir_nofollow(&chat_name)
        .map_err(|_| "Output content is unavailable".to_owned())
}

/// Read one immutable revision's complete bytes.
///
/// Blocking file I/O — call it from a blocking context.
pub fn read_output_revision_bytes(
    scratch_root: &Path,
    chat_id: ChatId,
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<Vec<u8>, String> {
    let ceiling = revision_byte_ceiling(&output.media_type);
    if output.chat_id != chat_id
        || output.deleted_at.is_some()
        || revision.output_id != output.id
        || revision.byte_len > ceiling as u64
    {
        return Err("Output not found in this conversation".to_owned());
    }
    let Some(root) = open_regular_directory(scratch_root)? else {
        return Err("Output content is unavailable".to_owned());
    };
    let chat_name = chat_id.to_string();
    let output_name = output.id.to_string();
    if !is_regular_child_directory(&root, &chat_name)? {
        return Err("Output content is unavailable".to_owned());
    }
    let chat = root
        .open_dir_nofollow(&chat_name)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !is_regular_child_directory(&chat, OUTPUTS_DIRECTORY)? {
        return Err("Output content is unavailable".to_owned());
    }
    let outputs = chat
        .open_dir_nofollow(OUTPUTS_DIRECTORY)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !is_regular_child_directory(&outputs, &output_name)? {
        return Err("Output content is unavailable".to_owned());
    }
    let revisions = outputs
        .open_dir_nofollow(&output_name)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = revisions
        .open_with(revision.id.to_string(), &options)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() != revision.byte_len {
        return Err("Output content is unavailable".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((ceiling + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if bytes.len() as u64 != revision.byte_len
        || bytes.len() > ceiling
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != revision.sha256
    {
        return Err("Output content is unavailable".to_owned());
    }
    Ok(bytes)
}

fn open_regular_directory(path: &Path) -> Result<Option<Dir>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not open private outputs".to_owned()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Private output storage is invalid".to_owned());
    }
    let directory = Dir::open_ambient_dir(path, ambient_authority())
        .map_err(|_| "Could not open private outputs".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory
            .dir_metadata()
            .map_err(|_| "Could not open private outputs".to_owned())?;
        if metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err("Private output storage changed while it was opened".to_owned());
        }
    }
    Ok(Some(directory))
}

fn is_regular_child_directory(parent: &Dir, name: &str) -> Result<bool, String> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Private output storage is invalid".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Could not inspect private outputs".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{TimeZone as _, Utc};
    use openwave_core::deliverable_media_type;

    use super::*;

    fn output_record(
        chat_id: ChatId,
        filename: &str,
        content: &[u8],
    ) -> (OutputRecord, OutputRevision) {
        let output_id = OutputId::new();
        let revision_id = OutputRevisionId::new();
        let created_at = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        (
            OutputRecord {
                id: output_id,
                chat_id,
                filename: filename.to_owned(),
                media_type: deliverable_media_type(filename).unwrap().to_owned(),
                current_revision: revision_id,
                revision_count: 1,
                created_at,
                updated_at: created_at,
                deleted_at: None,
            },
            OutputRevision {
                id: revision_id,
                output_id,
                ordinal: 1,
                byte_len: content.len() as u64,
                sha256: Sha256::digest(content).into(),
                turn_id: None,
                producing_run_id: None,
                created_at,
            },
        )
    }

    fn revision_path(root: &Path, output: &OutputRecord, revision: &OutputRevision) -> PathBuf {
        root.join(output.chat_id.to_string())
            .join(OUTPUTS_DIRECTORY)
            .join(output.id.to_string())
            .join(revision.id.to_string())
    }

    /// Moved from the desktop shell with the reader it guards. A revision read
    /// is scoped to one conversation and content-addressed: another
    /// conversation's id cannot reach it, and bytes that no longer digest to
    /// what the store recorded are refused rather than served.
    #[test]
    fn immutable_revision_reads_are_exactly_scoped_and_content_addressed() {
        let scratch = tempfile::tempdir().unwrap();
        let content = b"private";
        let (output, revision) = output_record(ChatId::new(), "brief.txt", content);
        let path = revision_path(scratch.path(), &output, &revision);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();

        assert_eq!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).unwrap(),
            content
        );
        assert!(
            read_output_revision_bytes(scratch.path(), ChatId::new(), &output, &revision).is_err()
        );
        std::fs::write(path, b"tampered").unwrap();
        assert!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_revision_file_is_refused() {
        use std::os::unix::fs::symlink;

        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let content = b"private";
        let (output, revision) = output_record(ChatId::new(), "brief.txt", content);
        let path = revision_path(scratch.path(), &output, &revision);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outside_source = outside.path().join("source.txt");
        std::fs::write(&outside_source, content).unwrap();
        symlink(&outside_source, &path).unwrap();
        assert!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).is_err()
        );
    }
}

//! Turn staging for connected-folder writes and workspace materialization.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use async_trait::async_trait;
use openwave_code_execution::{
    resolve_scratch_directory, CodeExecutionError, MaterializationPrecondition,
    MaterializedChangeKind, RejectedChangeReason, WorkspaceFilePath, WriteOverlay,
    DOCUMENT_SCRIPTS_DIR, DOCUMENT_SCRIPT_FILES,
};
use openwave_core::{
    exec_attachment_file_name, BlobStore, ChatId, HostRootId, MessageDocumentAttachment, Store,
    TurnId, MAX_EXEC_WORKSPACE_FILE_BYTES,
};

/// One turn's staging for one chat.
///
/// The overlay itself is addressed by folder path, which is what exec needs.
/// The host folder tools arrive with a product root id instead, so the same
/// staging is also indexed that way rather than making every caller re-resolve
/// a path through the broker to find it.
pub(super) struct StagedTurn {
    /// The turn that opened this staging. The journal written at close belongs
    /// to the turn that staged the changes, not to whatever is running when the
    /// write-back applies them.
    pub(super) turn: TurnId,
    pub(super) overlay: WriteOverlay,
    pub(super) staged_roots: HashMap<HostRootId, PathBuf>,
}

/// Where a chat's current turn stages writes for one granted folder.
///
/// For the length of a turn, exec addresses a private copy of each writable
/// granted folder rather than the folder itself, so the user's folder is the
/// stale view: a file the agent has just written is not in it yet, and one the
/// agent has deleted is still there. A host tool that reads the same folder in
/// the same turn consults this first, so the model is never shown two versions
/// of one folder.
///
/// The broker knows nothing about turn staging, but remains the live authority
/// behind every root resolution. Structured publications resolve that
/// authority again immediately before entering the shared materializer.
#[async_trait]
pub trait StagedFolders: Send + Sync {
    /// The staged copy of `root_id` for this chat's current turn, if the turn
    /// stages that folder. `None` covers every case where the user's folder is
    /// still the only view — no turn in flight, a read-only grant, or a folder
    /// the overlay could not stage.
    fn staged_root(&self, chat: ChatId, root_id: HostRootId) -> Option<PathBuf>;

    /// Publish one trusted file through the same conditional materializer and
    /// turn journal as an overlay write.
    async fn materialize_connected_file(
        &self,
        chat: ChatId,
        turn: TurnId,
        root_id: HostRootId,
        relative: &str,
        content: &[u8],
        expected: MaterializationPrecondition,
    ) -> std::result::Result<MaterializedChangeKind, RejectedChangeReason>;

    /// Reconcile an interrupted publication against its exact content.
    async fn connected_file_matches(
        &self,
        chat: ChatId,
        root_id: HostRootId,
        relative: &str,
        byte_len: u64,
        sha256: [u8; 32],
    ) -> bool;
}

/// Host infrastructure staged into every managed workspace regardless of the
/// model's listed set: the conventional-directory markers that make `output/`
/// and `preview/` exist remotely so commands can write into them, and the
/// bundled document helpers the tool description tells the model to invoke
/// without listing. All of it is host-authored, bounded, and digest-skipped on
/// a reused session.
pub(super) fn implicit_staged_paths(
    with_document_scripts: bool,
    with_skills: bool,
) -> Vec<WorkspaceFilePath> {
    let mut paths = vec![
        "output/.openwave-directory".to_owned(),
        "preview/.openwave-directory".to_owned(),
    ];
    if with_document_scripts {
        paths.push(DOCUMENT_SCRIPTS_DIR.to_owned());
    }
    if with_skills {
        paths.push(openwave_code_execution::SKILLS_DIR.to_owned());
    }
    paths
        .into_iter()
        .filter_map(|path| WorkspaceFilePath::parse(path).ok())
        .collect()
}

/// One bounded line naming what this call staged, appended to a failed managed
/// command so a missing-input failure points at the `files` argument.
pub(super) fn staged_set_note(files: &[WorkspaceFilePath]) -> String {
    const SHOWN: usize = 8;
    if files.is_empty() {
        return "staged: none — list the files the command needs in the exec 'files' argument"
            .into();
    }
    let shown: Vec<&str> = files
        .iter()
        .take(SHOWN)
        .map(WorkspaceFilePath::as_str)
        .collect();
    let mut note = format!("staged: {}", shown.join(", "));
    let omitted = files.len().saturating_sub(SHOWN);
    if omitted > 0 {
        note.push_str(&format!(" (+{omitted} more)"));
    }
    note
}

pub(super) async fn materialize_chat_attachments(
    store: &dyn Store,
    blobs: &dyn BlobStore,
    chat_id: ChatId,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let attachments = store
        .list_message_document_attachments(chat_id)
        .await
        .map_err(|_| CodeExecutionError::Unavailable("attachment storage is unavailable".into()))?;
    materialize_attachments(&attachments, blobs, host_dir).await
}

pub(super) async fn materialize_attachments(
    attachments: &[MessageDocumentAttachment],
    blobs: &dyn BlobStore,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let documents_dir = host_dir.join("documents");
    let metadata = tokio::fs::symlink_metadata(&documents_dir)
        .await
        .map_err(|_| CodeExecutionError::Sandbox("documents/ is unavailable".into()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CodeExecutionError::Sandbox(
            "documents/ is not a private workspace directory".into(),
        ));
    }

    let mut materialized = HashSet::new();
    for attachment in attachments {
        let Some(source_blob) = attachment.source_blob.as_ref() else {
            continue;
        };
        if source_blob.byte_len > MAX_EXEC_WORKSPACE_FILE_BYTES as u64 {
            continue;
        }
        let file_name =
            exec_attachment_file_name(attachment.title.as_deref(), attachment.document_id);
        if !materialized.insert(file_name.clone()) {
            continue;
        }
        let bytes = blobs.get(source_blob.id).await.map_err(|_| {
            CodeExecutionError::Unavailable("attached document bytes are unavailable".into())
        })?;
        let Some(bytes) = bytes else {
            return Err(CodeExecutionError::Unavailable(
                "attached document bytes are unavailable".into(),
            ));
        };
        if bytes.len() > MAX_EXEC_WORKSPACE_FILE_BYTES
            || openwave_core::DocumentSourceBlob::from_bytes(&bytes) != *source_blob
        {
            return Err(CodeExecutionError::Unavailable(
                "attached document bytes do not match their stored descriptor".into(),
            ));
        }
        let destination = documents_dir.join(&file_name);
        match tokio::fs::symlink_metadata(&destination).await {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(CodeExecutionError::Sandbox(format!(
                    "documents/{file_name} is not a regular workspace file"
                )));
            }
            Ok(_) => {
                if tokio::fs::read(&destination)
                    .await
                    .is_ok_and(|existing| existing == bytes)
                {
                    continue;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(CodeExecutionError::Sandbox(format!(
                    "documents/{file_name} is unavailable"
                )));
            }
        }
        tokio::fs::write(&destination, bytes).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "attached document documents/{file_name} could not be materialized"
            ))
        })?;
    }
    Ok(())
}

/// The host tools a staged set of skills needs: what the manifests declare,
/// plus what they imply.
///
/// A skill declares the npm packages it uses and never the interpreter that
/// runs them, so a non-empty npm list is the declaration of a Node dependency
/// — the only one there is. Deriving it here keeps the two from disagreeing:
/// there is no manifest spelling of `node` for a skill to omit or to claim
/// without the packages behind it.
pub(super) fn required_host_deps(
    skills: &[openwave_code_execution::LoadedSkill],
) -> Vec<openwave_code_execution::HostDep> {
    let mut required: Vec<openwave_code_execution::HostDep> = Vec::new();
    for skill in skills {
        let implied =
            (!skill.package.npm_deps.is_empty()).then_some(openwave_code_execution::HostDep::Node);
        for dep in skill.package.host_deps.iter().copied().chain(implied) {
            if !required.contains(&dep) {
                required.push(dep);
            }
        }
    }
    required
}

pub(super) async fn prepare_execution_directories(
    host_dir: &std::path::Path,
    mirrored: bool,
    document_scripts_source: Option<&std::path::Path>,
    skills: &[openwave_code_execution::LoadedSkill],
) -> std::result::Result<(), CodeExecutionError> {
    // The scratch directory itself is host-owned and named after the chat, but
    // everything inside it is writable by local exec, which can plant
    // `<scratch>/output -> /any/dir` between two runs. `create_dir_all` and a
    // plain `write` both follow a symlinked parent, so each conventional
    // directory is resolved a component at a time into an open descriptor and
    // the marker is written relative to that descriptor, without following a
    // link at the final component either.
    tokio::fs::create_dir_all(host_dir).await.map_err(|_| {
        CodeExecutionError::Sandbox("the private workspace directory is unavailable".into())
    })?;
    for name in ["output", "preview", "documents"] {
        let unavailable = || {
            CodeExecutionError::Sandbox(format!(
                "private workspace directory '{name}/' is unavailable"
            ))
        };
        let directory = resolve_scratch_directory(host_dir, name, true)
            .await
            .ok_or_else(unavailable)?;
        if mirrored {
            // Staging transfers files rather than empty directories. A hidden
            // zero-byte marker makes the conventional directories exist in
            // managed workspaces without becoming a user artifact.
            directory
                .write_file(".openwave-directory", &[])
                .await
                .map_err(|_| unavailable())?;
        }
    }
    if let Some(source) = document_scripts_source {
        install_document_scripts(source, host_dir).await?;
    }
    install_skills(skills, host_dir).await?;
    Ok(())
}

/// Stage the validated skills (built-in and user-authored) into
/// `.openwave/skills/<name>/`: the manifest, plus any helper files the package
/// carries under `scripts/`. Anything already staged under a name that is not
/// in `skills` is removed, so the staged tree is exactly the enabled set.
///
/// Each destination is resolved a component at a time for the same reason the
/// helper install is: `.openwave/` is writable by local exec, so a planted
/// symlink must not relocate the staged files. Content was validated at
/// configuration; a failure here means the workspace itself is unusable.
pub(super) async fn install_skills(
    skills: &[openwave_code_execution::LoadedSkill],
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    let root = resolve_scratch_directory(host_dir, openwave_code_execution::SKILLS_DIR, true)
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox("the staged skills directory is unavailable".into())
        })?;
    // Staging is the whole set, not an accumulation. A skill the install has
    // switched off — or one the user deleted — must leave a workspace that
    // staged it on an earlier turn, or the model could still `read_file`
    // instructions the catalog no longer advertises. Removal is best effort:
    // a leftover directory is untidy, but failing the command over it would
    // take working execution down with it.
    for entry in root.entries().await.unwrap_or_default() {
        if skills.iter().any(|skill| skill.package.name == entry.name) {
            continue;
        }
        let removed = match entry.kind {
            openwave_code_execution::ScratchEntryKind::Directory => {
                root.remove_dir_all(&entry.name).await
            }
            kind => root.remove(&entry.name, kind).await,
        };
        if let Err(error) = removed {
            tracing::warn!(
                "a skill no longer staged could not be removed from the workspace: {error}"
            );
        }
    }
    for skill in skills {
        let name = &skill.package.name;
        let destination = resolve_scratch_directory(
            host_dir,
            &format!("{}/{name}", openwave_code_execution::SKILLS_DIR),
            true,
        )
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox(format!("skill directory '{name}' is unavailable"))
        })?;
        destination
            .write_file(
                openwave_code_execution::SKILL_MANIFEST_FILE,
                skill.manifest.as_bytes(),
            )
            .await
            .map_err(|_| {
                CodeExecutionError::Sandbox(format!("skill '{name}' could not be installed"))
            })?;
        if skill.scripts.is_empty() {
            continue;
        }
        let scripts = resolve_scratch_directory(
            host_dir,
            &format!(
                "{}/{name}/{}",
                openwave_code_execution::SKILLS_DIR,
                openwave_code_execution::SKILL_SCRIPTS_DIR
            ),
            true,
        )
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox(format!("skill '{name}' scripts directory is unavailable"))
        })?;
        for script in &skill.scripts {
            scripts
                .write_file(&script.name, &script.content)
                .await
                .map_err(|_| {
                    CodeExecutionError::Sandbox(format!(
                        "skill '{name}' script '{}' could not be installed",
                        script.name
                    ))
                })?;
        }
    }
    Ok(())
}

pub(super) async fn install_document_scripts(
    source: &std::path::Path,
    host_dir: &std::path::Path,
) -> std::result::Result<(), CodeExecutionError> {
    // `.openwave/` sits inside the scratch directory local exec writes to, so
    // a planted `.openwave -> /elsewhere` would relocate the helper install
    // and truncate known filenames there. Resolve it a component at a time and
    // keep the descriptor, so the helpers land in the directory the walk proved
    // rather than whatever the name points at by the time we write.
    let destination = resolve_scratch_directory(host_dir, DOCUMENT_SCRIPTS_DIR, true)
        .await
        .ok_or_else(|| {
            CodeExecutionError::Sandbox("document helper directory is unavailable".into())
        })?;
    for name in DOCUMENT_SCRIPT_FILES {
        let source_file = source.join(name);
        let metadata = tokio::fs::symlink_metadata(&source_file)
            .await
            .map_err(|_| {
                CodeExecutionError::Sandbox(format!(
                    "bundled document helper '{name}' is unavailable"
                ))
            })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' is not a regular file"
            )));
        }
        let content = tokio::fs::read(&source_file).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' could not be read"
            ))
        })?;
        if content.len() > openwave_code_execution::MAX_WORKSPACE_FILE_BYTES {
            return Err(CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' exceeds the workspace file limit"
            )));
        }
        destination.write_file(name, &content).await.map_err(|_| {
            CodeExecutionError::Sandbox(format!(
                "bundled document helper '{name}' could not be installed"
            ))
        })?;
    }
    Ok(())
}

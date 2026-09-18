//! Read-only inspection of a remote workspace from its retained checkpoint.
//!
//! A remote workspace's checkout lives in a sandbox, not on this host. The
//! stored `worktree_path` is a `remote:<id>` marker and must never be handed
//! to local filesystem or worktree code. Inspection uses the registered
//! repository clone plus the incarnation's last WIP ref — the existing
//! sandbox checkpoint contract.
//!
//! Live sandbox file/tree/diff APIs are not part of the pinned runtime
//! contract (`spawn` / `status` / `events` / `messages` / `cancel`). When a
//! sandbox is still running and no checkpoint exists, callers get an explicit
//! unavailable state rather than a different revision.

use std::path::{Path, PathBuf};

use tidebreak_core::db::code::{
    latest_incarnation, latest_pushed_wip_ref, list_sessions_for_workspace,
};
use tidebreak_core::{CodeWorkspace, CodeWorkspaceStatus, Diffstat, IncarnationState, OwnerId};

use super::checkpoint::{list_changed_files, produce_diff, ChangedFile, DiffBounds};
use super::git_runner;
use super::runtime::CodeRuntime;
use super::types::WorkspaceContentRevision;
use super::worktree::{WorktreeBlob, WorktreeFile};
use crate::error::ServerError;

const MAX_BLOB_BYTES: usize = 512 * 1_024;
const MAX_VIEWABLE_FILE_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_TREE_LIMIT: u32 = 5_000;

/// Where a remote files/tree/diff/blob payload came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContentSource {
    pub revision: WorkspaceContentRevision,
    pub revision_ref: Option<String>,
}

/// A resolved retained checkpoint that can be read through git, never through
/// the workspace's `remote:` marker.
#[derive(Debug, Clone)]
pub struct RemoteCheckout {
    pub repo_root: PathBuf,
    pub source: WorkspaceContentSource,
    pub from_oid: String,
    pub to_oid: String,
}

impl CodeRuntime {
    /// Restore an archived remote workspace without touching a host checkout.
    ///
    /// Session rows stay the same ids. Ended sessions return to idle so one
    /// later authorized turn can resume from the retained checkpoint.
    pub(crate) async fn restore_remote_workspace(
        &self,
        owner: &OwnerId,
        mut workspace: CodeWorkspace,
    ) -> Result<CodeWorkspace, ServerError> {
        if !workspace.is_remote() {
            return Err(ServerError::internal(
                "restore_remote_workspace requires a remote workspace",
            ));
        }
        refuse_remote_filesystem_path(&workspace.worktree_path)?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        if workspace.status != CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let checkpoint = required_restore_checkpoint(self, owner, &workspace).await?;
        let _ = checkpoint;
        workspace.status = CodeWorkspaceStatus::Active;
        workspace.archived_at = None;
        if !tidebreak_core::db::code::save_workspace(&self.db, &workspace).await? {
            return Err(ServerError::not_found(format!(
                "workspace {} not found",
                workspace.id
            )));
        }
        tidebreak_core::db::code::revive_ended_workspace_sessions(&self.db, owner, workspace.id)
            .await?;
        crate::code::attention::emit_workspace_digests(&self.db, &self.bus, owner, workspace.id)
            .await;
        Ok(workspace)
    }

    /// Resolve a remote workspace onto its retained checkpoint.
    pub(crate) async fn remote_checkout(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Result<RemoteCheckout, ServerError> {
        if !workspace.is_remote() {
            return Err(ServerError::internal(
                "remote_checkout requires a remote workspace",
            ));
        }
        refuse_remote_filesystem_path(&workspace.worktree_path)?;
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let repo_root = PathBuf::from(&repo.root_path);
        require_local_git_dir(&repo_root)?;
        let reference = retained_checkpoint(self, owner, workspace).await?;
        let to_oid = resolve_commit(&repo_root, &reference).await?;
        let from_oid = merge_base_oid(&repo_root, &workspace.base_ref, &to_oid).await?;
        Ok(RemoteCheckout {
            repo_root,
            source: WorkspaceContentSource {
                revision: WorkspaceContentRevision::Retained,
                revision_ref: Some(reference),
            },
            from_oid,
            to_oid,
        })
    }
}

/// Restore needs a stopped checkpoint that a later spawn can resume from.
async fn required_restore_checkpoint(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace: &CodeWorkspace,
) -> Result<String, ServerError> {
    let sessions = list_sessions_for_workspace(&runtime.db, owner, workspace.id).await?;
    if sessions.is_empty() {
        return Err(checkpoint_missing());
    }
    let mut chosen: Option<String> = None;
    for session in &sessions {
        if let Some(row) = latest_incarnation(&runtime.db, owner, session.id).await? {
            if let Some((kind, message)) = crate::code::remote::driver::recovery_block(&row, true) {
                return Err(ServerError::conflict_kind(kind, message));
            }
            if row.state == IncarnationState::Active || row.state == IncarnationState::Intent {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    "a sandbox is still running in this workspace; archive it again after it stops",
                ));
            }
        }
        if let Some(reference) = latest_pushed_wip_ref(&runtime.db, owner, session.id).await? {
            chosen = Some(reference);
        }
    }
    chosen.ok_or_else(checkpoint_missing)
}

/// The newest WIP ref across the workspace's sessions, and whether a sandbox
/// is still live. Live sandboxes without a checkpoint are unavailable rather
/// than a silently different tree.
async fn retained_checkpoint(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace: &CodeWorkspace,
) -> Result<String, ServerError> {
    let sessions = list_sessions_for_workspace(&runtime.db, owner, workspace.id).await?;
    let mut live = false;
    let mut reference = None;
    for session in sessions {
        if let Some(row) = latest_incarnation(&runtime.db, owner, session.id).await? {
            live |= matches!(
                row.state,
                IncarnationState::Active | IncarnationState::Intent
            );
            if let Some((kind, message)) = crate::code::remote::driver::recovery_block(&row, true) {
                if reference.is_none() {
                    return Err(ServerError::conflict_kind(kind, message));
                }
            }
        }
        if let Some(wip) = latest_pushed_wip_ref(&runtime.db, owner, session.id).await? {
            reference = Some(wip);
        }
    }
    match reference {
        Some(reference) => Ok(reference),
        None if live => Err(live_unavailable()),
        None => Err(checkpoint_missing()),
    }
}

pub async fn list_checkout_tree(
    checkout: &RemoteCheckout,
    query: &str,
    limit: u32,
) -> Result<(Vec<String>, bool), ServerError> {
    require_local_git_dir(&checkout.repo_root)?;
    let limit = tree_limit(limit);
    let listed = git_nul(
        &checkout.repo_root,
        &["ls-tree", "-r", "--name-only", "-z", &checkout.to_oid],
    )
    .await
    .map_err(|err| {
        ServerError::conflict_kind(
            "workspace_sandbox_unavailable",
            format!("could not list the retained checkpoint: {err}"),
        )
    })?;
    let needle = query.trim().to_ascii_lowercase();
    let mut matched = listed
        .into_iter()
        .filter(|path| !path.is_empty())
        .filter(|path| needle.is_empty() || path_name_matches(path, &needle))
        .collect::<Vec<_>>();
    matched.sort();
    matched.dedup();
    let truncated = matched.len() > limit;
    matched.truncate(limit);
    Ok((matched, truncated))
}

pub async fn list_checkout_files(
    checkout: &RemoteCheckout,
) -> Result<(Vec<ChangedFile>, bool, Diffstat), ServerError> {
    require_local_git_dir(&checkout.repo_root)?;
    let listed = list_changed_files(
        &checkout.repo_root,
        &checkout.from_oid,
        &checkout.to_oid,
        DiffBounds::default(),
    )
    .await
    .map_err(map_inspect_checkpoint)?;
    Ok((listed.files, listed.truncated, listed.stat))
}

pub async fn produce_checkout_diff(
    checkout: &RemoteCheckout,
    file: Option<&str>,
) -> Result<(String, bool, Diffstat), ServerError> {
    require_local_git_dir(&checkout.repo_root)?;
    if let Some(path) = file {
        validate_relative_file(path)?;
    }
    let produced = produce_diff(
        &checkout.repo_root,
        &checkout.from_oid,
        &checkout.to_oid,
        file,
        DiffBounds::default(),
    )
    .await
    .map_err(map_inspect_checkpoint)?;
    Ok((produced.diff, produced.truncated, produced.stat))
}

pub async fn read_checkout_blob(
    checkout: &RemoteCheckout,
    path: &str,
) -> Result<WorktreeBlob, ServerError> {
    let bytes = read_blob_bytes(checkout, path, MAX_BLOB_BYTES, true).await?;
    let truncated = bytes.truncated;
    let path = bytes.path;
    if bytes.bytes.contains(&0) {
        return Ok(WorktreeBlob {
            path,
            content: String::new(),
            truncated: false,
            binary: true,
        });
    }
    Ok(WorktreeBlob {
        path,
        content: String::from_utf8_lossy(&bytes.bytes).into_owned(),
        truncated,
        binary: false,
    })
}

pub async fn read_checkout_file(
    checkout: &RemoteCheckout,
    path: &str,
) -> Result<WorktreeFile, ServerError> {
    let bytes = read_blob_bytes(checkout, path, MAX_VIEWABLE_FILE_BYTES, false).await?;
    let media_type = crate::media_type::sniff_media_type(&bytes.bytes, Some(&bytes.path));
    Ok(WorktreeFile {
        path: bytes.path,
        bytes: bytes.bytes,
        media_type,
    })
}

struct BlobBytes {
    path: String,
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_blob_bytes(
    checkout: &RemoteCheckout,
    path: &str,
    max_bytes: usize,
    truncate: bool,
) -> Result<BlobBytes, ServerError> {
    require_local_git_dir(&checkout.repo_root)?;
    let rel = validate_relative_file(path)?;
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    let spec = format!("{}:{rel_str}", checkout.to_oid);
    let mode = tree_entry_mode(&checkout.repo_root, &checkout.to_oid, &rel_str).await?;
    if mode == "120000" || mode == "160000" {
        return Err(ServerError::bad_request_kind(
            "path",
            "path must stay inside the workspace checkout",
        ));
    }
    let size = git_stdout(&checkout.repo_root, &["cat-file", "-s", &spec])
        .await
        .map_err(|_| ServerError::not_found(format!("file not found: {rel_str}")))?;
    let size: u64 = size.parse().unwrap_or(0);
    if !truncate && size > max_bytes as u64 {
        return Err(ServerError::bad_request_kind(
            "path",
            "file is too large to preview; open it in your editor",
        ));
    }
    let (bytes, read_truncated) = git_bytes(
        &checkout.repo_root,
        &["cat-file", "-p", &spec],
        max_bytes,
        truncate,
    )
    .await
    .map_err(|_| ServerError::not_found(format!("file not found: {rel_str}")))?;
    let truncated = read_truncated || size > max_bytes as u64;
    let mut bytes = bytes;
    if truncated && bytes.len() > max_bytes {
        bytes.truncate(max_bytes);
    }
    Ok(BlobBytes {
        path: rel_str,
        bytes,
        truncated,
    })
}

async fn tree_entry_mode(repo_root: &Path, oid: &str, path: &str) -> Result<String, ServerError> {
    let listed = git_stdout(repo_root, &["ls-tree", "--full-tree", oid, "--", path])
        .await
        .map_err(|_| ServerError::not_found(format!("file not found: {path}")))?;
    let mode = listed
        .split_whitespace()
        .next()
        .ok_or_else(|| ServerError::not_found(format!("file not found: {path}")))?;
    Ok(mode.to_owned())
}

async fn resolve_commit(repo_root: &Path, reference: &str) -> Result<String, ServerError> {
    let reference = validate_git_ref(reference)?;
    if let Ok(oid) = git_stdout(
        repo_root,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
    .await
    {
        return Ok(oid);
    }
    Err(checkpoint_missing())
}

async fn merge_base_oid(
    repo_root: &Path,
    base_ref: &str,
    tip: &str,
) -> Result<String, ServerError> {
    let base_ref = validate_git_ref(base_ref).unwrap_or("HEAD");
    if let Ok(oid) = git_stdout(repo_root, &["merge-base", base_ref, tip]).await {
        if !oid.is_empty() {
            return Ok(oid);
        }
    }
    if let Ok(oid) = git_stdout(
        repo_root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )
    .await
    {
        return Ok(oid);
    }
    git_stdout(
        repo_root,
        &["rev-parse", "--verify", &format!("{tip}^{{commit}}")],
    )
    .await
    .map_err(|err| {
        ServerError::conflict_kind(
            "workspace_sandbox_unavailable",
            format!("could not resolve the checkpoint base: {err}"),
        )
    })
}

fn require_local_git_dir(path: &Path) -> Result<(), ServerError> {
    let display = path.to_string_lossy();
    if display.is_empty() || display.starts_with("remote:") {
        return Err(ServerError::internal(
            "refused to treat a remote workspace marker as a filesystem path",
        ));
    }
    if !path.exists() {
        return Err(live_unavailable());
    }
    Ok(())
}

fn refuse_remote_filesystem_path(worktree_path: &str) -> Result<(), ServerError> {
    if worktree_path.is_empty() || worktree_path.starts_with("remote:") {
        return Ok(());
    }
    Err(ServerError::internal(
        "restore_remote_workspace received a host worktree path",
    ))
}

pub fn validate_relative_file(value: &str) -> Result<PathBuf, ServerError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ServerError::bad_request_kind("path", "path is required"));
    }
    if trimmed.contains('\0') || trimmed.contains(':') {
        return Err(ServerError::bad_request_kind(
            "path",
            "path must be a relative workspace file",
        ));
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ServerError::bad_request_kind(
            "path",
            "path must stay inside the workspace checkout",
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_git_ref(value: &str) -> Result<&str, ServerError> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.contains("..") {
        return Err(checkpoint_missing());
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    {
        return Err(checkpoint_missing());
    }
    Ok(value)
}

fn tree_limit(limit: u32) -> usize {
    (limit.max(1)).min(MAX_TREE_LIMIT) as usize
}

fn path_name_matches(path: &str, needle: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    name.contains(needle) || path.to_ascii_lowercase().contains(needle)
}

fn checkpoint_missing() -> ServerError {
    ServerError::conflict_kind(
        "sandbox_checkpoint_missing",
        "This sandbox stopped without a saved checkpoint. Its work cannot be restored from the host, so Files shows an unavailable state instead of a different revision.",
    )
}

fn live_unavailable() -> ServerError {
    ServerError::conflict_kind(
        "workspace_sandbox_unavailable",
        "Live sandbox files are not inspectable from this machine. Open the transcript or pull request, or wait until the sandbox checkpoints its work.",
    )
}

fn map_inspect_checkpoint(err: super::checkpoint::CheckpointError) -> ServerError {
    match err {
        super::checkpoint::CheckpointError::User(message) => {
            ServerError::bad_request_kind("checkpoint", message)
        }
        super::checkpoint::CheckpointError::Internal(message) => ServerError::internal(message),
    }
}

async fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let (bytes, _) = git_bytes(cwd, args, git_runner::STDOUT_BYTES, false).await?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

async fn git_nul(cwd: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let (bytes, _) = git_bytes(cwd, args, git_runner::STDOUT_BYTES, true).await?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect())
}

async fn git_bytes(
    cwd: &Path,
    args: &[&str],
    max_bytes: usize,
    accept_truncated: bool,
) -> Result<(Vec<u8>, bool), String> {
    require_local_git_dir(cwd).map_err(|err| err.message().to_owned())?;
    let mut command = git_runner::git_command(Some(cwd));
    command.args(args);
    let description = format!("git {}", args.join(" "));
    let output = git_runner::wait_command_bounded(
        &mut command,
        git_runner::GIT_TIMEOUT,
        tidebreak_harness::OutputBudget::head(
            max_bytes.saturating_add(1),
            git_runner::STDOUT_LINES,
        ),
        git_runner::default_stderr_budget(),
        &description,
    )
    .await
    .map_err(|err| err.into_message(&description))?;
    git_runner::finish_bounded_command(
        output,
        accept_truncated,
        "git output exceeded its limit",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) {
        assert!(
            StdCommand::new(args[0])
                .args(&args[1..])
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()
                .unwrap()
                .success(),
            "{args:?}"
        );
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("origin");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["git", "init", "-b", "main"]);
        run(&repo, &["git", "config", "user.email", "dev@example.com"]);
        run(&repo, &["git", "config", "user.name", "Dev"]);
        run(&repo, &["git", "config", "commit.gpgsign", "false"]);
        run(&repo, &["git", "config", "core.autocrlf", "false"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run(&repo, &["git", "add", "README.md"]);
        run(&repo, &["git", "commit", "-m", "init"]);
        (dir, repo)
    }

    async fn checkout(repo: PathBuf, git_ref: &str) -> RemoteCheckout {
        let to_oid = resolve_commit(&repo, git_ref).await.unwrap();
        let from_oid = merge_base_oid(&repo, "main", &to_oid).await.unwrap();
        RemoteCheckout {
            repo_root: repo,
            source: WorkspaceContentSource {
                revision: WorkspaceContentRevision::Retained,
                revision_ref: Some(git_ref.to_owned()),
            },
            from_oid,
            to_oid,
        }
    }

    #[test]
    fn relative_paths_refuse_traversal() {
        assert!(validate_relative_file("../secret").is_err());
        assert!(validate_relative_file("/etc/passwd").is_err());
        assert!(validate_relative_file("foo/../../etc/passwd").is_err());
        assert!(validate_relative_file("foo:bar").is_err());
        assert!(validate_relative_file("src/lib.rs").is_ok());
    }

    #[test]
    fn remote_markers_are_not_filesystem_paths() {
        assert!(require_local_git_dir(Path::new("remote:ws-1")).is_err());
        assert!(require_local_git_dir(Path::new("")).is_err());
    }

    #[tokio::test]
    async fn tree_blob_and_diff_read_a_checkpoint_not_the_worktree_marker() {
        let (_dir, repo) = init_repo();
        std::fs::write(repo.join("changed.txt"), "from the checkpoint\n").unwrap();
        run(&repo, &["git", "add", "changed.txt"]);
        run(&repo, &["git", "commit", "-m", "wip"]);
        run(
            &repo,
            &["git", "update-ref", "refs/heads/mg-wip/sb-1-i1", "HEAD"],
        );
        run(&repo, &["git", "reset", "--hard", "HEAD~1"]);
        std::fs::write(repo.join("changed.txt"), "dirty worktree\n").unwrap();

        let checkout = checkout(repo.clone(), "mg-wip/sb-1-i1").await;
        assert!(!checkout.repo_root.to_string_lossy().starts_with("remote:"));

        let (paths, truncated) = list_checkout_tree(&checkout, "", 50).await.unwrap();
        assert!(paths.contains(&"changed.txt".to_owned()));
        assert!(paths.contains(&"README.md".to_owned()));
        assert!(!truncated);

        let blob = read_checkout_blob(&checkout, "changed.txt").await.unwrap();
        assert_eq!(blob.content, "from the checkpoint\n");
        assert!(!blob.binary);

        let (files, _, stat) = list_checkout_files(&checkout).await.unwrap();
        assert!(files
            .iter()
            .any(|file| file.path.to_wire() == "changed.txt"));
        assert!(stat.files >= 1);

        let (diff, _, _) = produce_checkout_diff(&checkout, Some("changed.txt"))
            .await
            .unwrap();
        assert!(diff.contains("from the checkpoint"));
        assert!(!diff.contains("dirty worktree"));
    }

    #[tokio::test]
    async fn symlink_blobs_are_refused() {
        let (_dir, repo) = init_repo();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", repo.join("link.txt")).unwrap();
            run(&repo, &["git", "add", "link.txt"]);
            run(&repo, &["git", "commit", "-m", "link"]);
            run(
                &repo,
                &["git", "update-ref", "refs/heads/mg-wip/sb-1-i1", "HEAD"],
            );
            let checkout = checkout(repo, "mg-wip/sb-1-i1").await;
            let error = read_checkout_blob(&checkout, "link.txt").await.unwrap_err();
            assert_eq!(error.kind(), "path");
        }
    }

    #[tokio::test]
    async fn oversized_file_preview_is_refused() {
        let (_dir, repo) = init_repo();
        let huge = vec![b'a'; MAX_VIEWABLE_FILE_BYTES + 8];
        std::fs::write(repo.join("huge.bin"), &huge).unwrap();
        run(&repo, &["git", "add", "huge.bin"]);
        run(&repo, &["git", "commit", "-m", "huge"]);
        run(
            &repo,
            &["git", "update-ref", "refs/heads/mg-wip/sb-1-i1", "HEAD"],
        );
        let checkout = checkout(repo, "mg-wip/sb-1-i1").await;
        let error = read_checkout_file(&checkout, "huge.bin").await.unwrap_err();
        assert_eq!(error.kind(), "path");
        let blob = read_checkout_blob(&checkout, "huge.bin").await.unwrap();
        assert!(blob.truncated);
        assert!(blob.content.len() <= MAX_BLOB_BYTES);
    }

    #[test]
    fn git_refs_reject_option_injection() {
        assert!(validate_git_ref("--output=/tmp/x").is_err());
        assert!(validate_git_ref("mg-wip/sb-1-i1").is_ok());
        assert!(validate_git_ref("main").is_ok());
    }
}

//! Read-only inspection of a remote workspace from its WIP checkpoints.
//!
//! A remote workspace's checkout lives in a sandbox, not on this host. The
//! stored `worktree_path` is a `remote:<id>` marker and must never be handed
//! to local filesystem or worktree code. Inspection uses the registered
//! repository clone plus the incarnation's last WIP ref — the existing
//! sandbox checkpoint contract.
//!
//! The runtime contract (`spawn` / `status` / `events` / `messages` /
//! `cancel`) has no file, tree, or diff API, and this module does not add
//! one. Live inspection rides the checkpoints the supervisor already pushes:
//! one about every 60 seconds while a turn runs (`wip_pushed` with
//! `checkpoint: "periodic"`), one after each successful turn, and a terminal
//! one. Ingest stores each as the incarnation's `last_wip_ref` and
//! `last_wip_at`, and every read here re-fetches the ref from origin, so a
//! later push replaces what the reader sees.
//!
//! - A checkpoint pushed by a sandbox that is still running is
//!   [`WorkspaceContentRevision::Live`]. Every other checkpoint is
//!   [`WorkspaceContentRevision::Retained`]. Both carry the ref and when it
//!   was saved, so a reader can tell how old the view is.
//! - A running sandbox with no checkpoint yet is `workspace_sandbox_unavailable`.
//!   Its first periodic checkpoint lands within about a minute of the turn
//!   starting.
//! - A checkpoint the host cannot refresh from origin is refused, never
//!   served from a cached copy. When origin refuses an anonymous fetch, the
//!   host retries once with a repository credential borrowed for that one
//!   git invocation, as the identity the workspace's sessions act as.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidebreak_core::db::code::{
    latest_incarnation, latest_pushed_wip_incarnation, list_sessions_for_workspace,
};
use tidebreak_core::{
    CodeWorkspace, CodeWorkspaceStatus, Diffstat, IncarnationState, OwnerId, WorkspaceId,
};

use super::checkpoint::{list_changed_files, produce_diff, ChangedFile, DiffBounds};
use super::git_runner;
use super::runtime::CodeRuntime;
use super::types::WorkspaceContentRevision;
use super::worktree::{WorktreeBlob, WorktreeFile};
use crate::error::ServerError;
use crate::obo_gateway::{GitCredential, GitCredentialLender, GitForgeAttributionRequest};

const MAX_BLOB_BYTES: usize = 512 * 1_024;
const MAX_VIEWABLE_FILE_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_TREE_LIMIT: u32 = 5_000;

/// Where a remote files/tree/diff/blob payload came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContentSource {
    pub revision: WorkspaceContentRevision,
    pub revision_ref: Option<String>,
    /// When the checkpoint was pushed. Absent for checkpoints ingested
    /// before the time was recorded.
    pub saved_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The newest checkpoint across a workspace's sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceCheckpoint {
    reference: String,
    saved_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The sandbox that pushed it is still running.
    live: bool,
}

/// Lends one repository credential for an origin fetch the host could not
/// make anonymously.
///
/// Asked only after the anonymous fetch fails, so a public origin never
/// borrows anything. `None` means no credential is available, and the fetch
/// fails with its ordinary unavailable error.
#[async_trait::async_trait]
pub(crate) trait OriginCredentials: Send + Sync {
    async fn borrow(&self) -> Option<GitCredential>;
}

/// The workspace's own lending path: the connection that created it, the
/// identity its sessions act as, and the forge repository it was registered
/// from.
struct WorkspaceOriginCredentials {
    lender: Arc<dyn GitCredentialLender>,
    owner: OwnerId,
    repository: String,
    attributions: Vec<GitForgeAttributionRequest>,
}

#[async_trait::async_trait]
impl OriginCredentials for WorkspaceOriginCredentials {
    async fn borrow(&self) -> Option<GitCredential> {
        for attribution in &self.attributions {
            match self
                .lender
                .git_credential(&self.owner, &self.repository, *attribution)
                .await
            {
                Ok(credential) => return Some(credential),
                Err(refusal) => {
                    tracing::debug!(
                        ?refusal,
                        attribution = attribution.as_str(),
                        "no repository credential for a checkpoint fetch"
                    );
                }
            }
        }
        None
    }
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
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let repo_root = PathBuf::from(&repo.root_path);
        require_local_git_dir(&repo_root)?;
        // Restore is only recoverable when this ref actually resolves, not
        // merely because some session stored a string.
        let credentials = self.origin_credentials(owner, &workspace).await;
        resolve_commit(
            &repo_root,
            &checkpoint,
            credentials.as_ref().map(|lending| lending as _),
        )
        .await?;
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
        let checkpoint = workspace_checkpoint(self, owner, workspace).await?;
        let credentials = self.origin_credentials(owner, workspace).await;
        let credentials = credentials.as_ref().map(|lending| lending as _);
        let to_oid = resolve_commit(&repo_root, &checkpoint.reference, credentials).await?;
        let from_oid =
            merge_base_oid(&repo_root, &workspace.base_ref, &to_oid, credentials).await?;
        Ok(RemoteCheckout {
            repo_root,
            source: WorkspaceContentSource {
                revision: if checkpoint.live {
                    WorkspaceContentRevision::Live
                } else {
                    WorkspaceContentRevision::Retained
                },
                revision_ref: Some(checkpoint.reference),
                saved_at: checkpoint.saved_at,
            },
            from_oid,
            to_oid,
        })
    }

    /// How this host may authenticate a checkpoint fetch from a private
    /// origin, or `None` when it cannot.
    ///
    /// The lender is the one the workspace's sandbox borrowed from: the
    /// connection that created it, else this machine's own. The identity is
    /// the one its sessions act as; a person who has not connected their
    /// account falls back to the deployment's installation, which is enough
    /// to read. Only a registered forge origin on the lending host qualifies.
    async fn origin_credentials(
        &self,
        owner: &OwnerId,
        workspace: &CodeWorkspace,
    ) -> Option<WorkspaceOriginCredentials> {
        let (lender, _) = self.workspace_git_lender(owner, workspace).await.ok()?;
        let lender = lender?;
        let target = self
            .workspace_repository_target(owner, workspace)
            .await
            .filter(|target| {
                target
                    .host
                    .eq_ignore_ascii_case(super::gh::GIT_CREDENTIAL_FORGE_HOST)
            })?;
        let attributions = match self.workspace_acts_as(owner, workspace.id).await {
            tidebreak_core::ActsAs::Person => vec![
                GitForgeAttributionRequest::Person,
                GitForgeAttributionRequest::Installation,
            ],
            tidebreak_core::ActsAs::Bot => vec![GitForgeAttributionRequest::Installation],
        };
        Some(WorkspaceOriginCredentials {
            lender,
            owner: owner.clone(),
            repository: format!("{}/{}", target.owner, target.name),
            attributions,
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
    }
    newest_workspace_checkpoint(runtime, owner, workspace.id)
        .await?
        .map(|checkpoint| checkpoint.reference)
        .ok_or_else(checkpoint_missing)
}

/// The newest WIP checkpoint across the workspace's sessions. Live sandboxes
/// without a checkpoint are unavailable rather than a silently different tree.
async fn workspace_checkpoint(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace: &CodeWorkspace,
) -> Result<WorkspaceCheckpoint, ServerError> {
    let sessions = list_sessions_for_workspace(&runtime.db, owner, workspace.id).await?;
    let mut live = false;
    let mut blocked = None;
    for session in &sessions {
        if let Some(row) = latest_incarnation(&runtime.db, owner, session.id).await? {
            live |= matches!(
                row.state,
                IncarnationState::Active | IncarnationState::Intent
            );
            if blocked.is_none() {
                if let Some(block) = crate::code::remote::driver::recovery_block(&row, true) {
                    blocked = Some(block);
                }
            }
        }
    }
    if let Some(checkpoint) = newest_workspace_checkpoint(runtime, owner, workspace.id).await? {
        return Ok(checkpoint);
    }
    if live {
        return Err(live_unavailable());
    }
    if let Some((kind, message)) = blocked {
        return Err(ServerError::conflict_kind(kind, message));
    }
    Err(checkpoint_missing())
}

/// Newest checkpoint: the most recently created session that pushed a WIP
/// ref, using that session's latest pushed ref.
///
/// `list_sessions_for_workspace` is CreatedAt DESC. Taking the first pushed
/// ref is the contract; looping and overwriting would select the oldest.
///
/// The checkpoint is live only when the incarnation that pushed it is still
/// running. A session whose new sandbox has not pushed yet reads its previous
/// sandbox's checkpoint, which is retained work, not live work.
async fn newest_workspace_checkpoint(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Option<WorkspaceCheckpoint>, ServerError> {
    let sessions = list_sessions_for_workspace(&runtime.db, owner, workspace_id).await?;
    for session in sessions {
        let Some(row) = latest_pushed_wip_incarnation(&runtime.db, owner, session.id).await? else {
            continue;
        };
        let Some(reference) = row.last_wip_ref else {
            continue;
        };
        return Ok(Some(WorkspaceCheckpoint {
            reference,
            saved_at: row.last_wip_at,
            live: matches!(
                row.state,
                IncarnationState::Active | IncarnationState::Intent
            ),
        }));
    }
    Ok(None)
}

/// Per-turn file history is a host-worktree contract. Retained sandbox
/// checkpoints only expose the latest saved ref.
pub(crate) fn historical_turn_unsupported() -> ServerError {
    ServerError::bad_request_kind(
        "sandbox_historical_turn_unsupported",
        "Retained sandbox checkpoints do not support per-turn file history. Inspect the latest retained checkpoint instead.",
    )
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
            hash: None,
        });
    }
    // A sandbox checkout is read-only here, so it offers no base to save over.
    Ok(WorktreeBlob {
        path,
        content: String::from_utf8_lossy(&bytes.bytes).into_owned(),
        truncated,
        binary: false,
        hash: None,
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

async fn resolve_commit(
    repo_root: &Path,
    reference: &str,
    credentials: Option<&dyn OriginCredentials>,
) -> Result<String, ServerError> {
    let reference = validate_git_ref(reference)?;
    // A configured origin must confirm a mutable WIP ref. A failed refresh
    // cannot turn an older cached commit into the final saved checkpoint.
    let fetched = fetch_origin_ref(repo_root, reference, credentials)
        .await
        .map_err(|_| {
        ServerError::conflict_kind(
            "workspace_sandbox_unavailable",
            "The saved sandbox checkpoint could not be refreshed from its repository. Check repository access or try again.",
        )
    })?;
    let resolved_ref = if fetched {
        format!("refs/remotes/origin/{reference}^{{commit}}")
    } else {
        format!("{reference}^{{commit}}")
    };
    git_stdout(repo_root, &["rev-parse", "--verify", &resolved_ref])
        .await
        .map_err(|_| checkpoint_missing())
}

/// Fetch one ref through the same bounded git path workspace creation uses
/// (`git fetch --no-tags --no-write-fetch-head`). Only repositories without
/// an origin may resolve a local ref without refreshing it.
///
/// A private origin refuses the anonymous fetch. The fetch then runs once
/// more with a borrowed credential, handed to that one git process through
/// the one-shot helper: nothing reaches the repository's configuration,
/// argv, or a log line.
async fn fetch_origin_ref(
    repo_root: &Path,
    reference: &str,
    credentials: Option<&dyn OriginCredentials>,
) -> Result<bool, String> {
    let remotes = git_stdout(repo_root, &["remote"]).await?;
    if !remotes.lines().any(|remote| remote == "origin") {
        return Ok(false);
    }
    let tracking = format!("refs/remotes/origin/{reference}");
    let refspec = format!("+refs/heads/{reference}:{tracking}");
    let args = [
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "--",
        "origin",
        refspec.as_str(),
    ];
    let anonymous = match git_bytes(repo_root, &args, git_runner::STDOUT_BYTES, false).await {
        Ok(_) => return Ok(true),
        Err(error) => error,
    };
    let Some(credential) = (match credentials {
        Some(credentials) => credentials.borrow().await,
        None => None,
    }) else {
        return Err(anonymous);
    };
    git_bytes_with_credential(
        repo_root,
        &args,
        git_runner::STDOUT_BYTES,
        false,
        Some(&credential),
    )
    .await?;
    Ok(true)
}

async fn merge_base_oid(
    repo_root: &Path,
    base_ref: &str,
    tip: &str,
    credentials: Option<&dyn OriginCredentials>,
) -> Result<String, ServerError> {
    let Ok(base_ref) = validate_git_ref(base_ref) else {
        return Err(unavailable_base());
    };
    match classify_base_ref(repo_root, base_ref).await {
        Some(branch) => {
            if branch.is_empty() {
                return Err(unavailable_base());
            }
            match fetch_origin_ref(repo_root, &branch, credentials).await {
                Ok(true) => {
                    // A confirmed remote base must win. Trying the local branch first
                    // lets a stale host `main` become merge-base and inflate the
                    // checkpoint diff with unrelated history.
                    merge_base_with(repo_root, &format!("refs/remotes/origin/{branch}"), tip).await
                }
                Ok(false) => merge_base_with(repo_root, base_ref, tip).await,
                Err(_) => Err(unavailable_base()),
            }
        }
        None => {
            // Tags and commit IDs stay pinned. Fetching them as origin branches
            // fails when the name is not a branch, even if the object is local.
            merge_base_with(repo_root, base_ref, tip).await
        }
    }
}

/// Mutable origin branches return a fetchable name. Tags and commit IDs return
/// `None` so they stay pinned, matching workspace creation.
///
/// Local symbolic success is not required: a branch that exists only on origin
/// still needs a bounded fetch. A resolved `refs/heads/` name is used as-is so
/// a local `origin/feature` branch is not rewritten to `feature`.
async fn classify_base_ref(repo_root: &Path, base_ref: &str) -> Option<String> {
    match git_stdout(
        repo_root,
        &["rev-parse", "--symbolic-full-name", "--verify", base_ref],
    )
    .await
    {
        Ok(resolved) => {
            // A resolved commit has no symbolic name. Tags and other remotes
            // also stay pinned; only origin branches need a refresh.
            resolved
                .strip_prefix("refs/heads/")
                .or_else(|| resolved.strip_prefix("refs/remotes/origin/"))
                .filter(|branch| !branch.is_empty())
                .map(str::to_owned)
        }
        Err(_) => {
            if base_ref.starts_with("refs/")
                && !base_ref.starts_with("refs/heads/")
                && !base_ref.starts_with("refs/remotes/origin/")
            {
                return None;
            }
            let branch = origin_branch_name(base_ref);
            (!branch.is_empty()).then(|| branch.to_owned())
        }
    }
}

async fn merge_base_with(
    repo_root: &Path,
    candidate: &str,
    tip: &str,
) -> Result<String, ServerError> {
    if let Ok(oid) = git_stdout(repo_root, &["merge-base", candidate, tip]).await {
        if !oid.is_empty() {
            return Ok(oid);
        }
    }
    Err(unavailable_base())
}

/// Branch name used for `origin` fetch/tracking, accepting common base_ref shapes.
fn origin_branch_name(base_ref: &str) -> &str {
    base_ref
        .strip_prefix("refs/remotes/origin/")
        .or_else(|| base_ref.strip_prefix("refs/heads/"))
        .or_else(|| base_ref.strip_prefix("origin/"))
        .unwrap_or(base_ref)
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
    limit.clamp(1, MAX_TREE_LIMIT) as usize
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
        "The sandbox has not saved a checkpoint yet. Its first live checkpoint lands within about a minute of the turn starting. Open the transcript meanwhile.",
    )
}

fn unavailable_base() -> ServerError {
    ServerError::conflict_kind(
        "sandbox_checkpoint_unavailable_base",
        "The retained checkpoint's base revision is not available, so Tidebreak cannot show a diff without inventing an empty one.",
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
    git_bytes_with_credential(cwd, args, max_bytes, accept_truncated, None).await
}

/// A git command that lends `credential`, when given, to this one process.
///
/// The credential rides the environment into the one-shot helper, which
/// first resets any configured helper so none can store it, and answers
/// only for the forge host over https.
fn credentialed_git_command(
    cwd: &Path,
    credential: Option<&GitCredential>,
) -> tokio::process::Command {
    let mut command = git_runner::git_command(Some(cwd));
    if let Some(credential) = credential {
        command.args(super::gh::GIT_CREDENTIAL_CONFIG_ARGS);
        command
            .env(super::gh::GIT_CREDENTIAL_USERNAME_ENV, &credential.username)
            .env(super::gh::GIT_CREDENTIAL_SECRET_ENV, &credential.secret)
            .env(
                super::gh::GIT_CREDENTIAL_HOST_ENV,
                super::gh::GIT_CREDENTIAL_FORGE_HOST,
            );
    }
    command
}

async fn git_bytes_with_credential(
    cwd: &Path,
    args: &[&str],
    max_bytes: usize,
    accept_truncated: bool,
    credential: Option<&GitCredential>,
) -> Result<(Vec<u8>, bool), String> {
    require_local_git_dir(cwd).map_err(|err| err.message().to_owned())?;
    let mut command = credentialed_git_command(cwd, credential);
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
        let to_oid = resolve_commit(&repo, git_ref, None).await.unwrap();
        let from_oid = merge_base_oid(&repo, "main", &to_oid, None).await.unwrap();
        RemoteCheckout {
            repo_root: repo,
            source: WorkspaceContentSource {
                revision: WorkspaceContentRevision::Retained,
                revision_ref: Some(git_ref.to_owned()),
                saved_at: None,
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

    fn git_rev_parse(dir: &Path, spec: &str) -> Option<String> {
        let output = StdCommand::new("git")
            .args(["rev-parse", "--verify", spec])
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn clone_repo(src: &Path, dst: &Path) {
        run(
            src.parent().unwrap(),
            &[
                "git",
                "clone",
                "--quiet",
                src.to_str().unwrap(),
                dst.to_str().unwrap(),
            ],
        );
        run(dst, &["git", "config", "user.email", "dev@example.com"]);
        run(dst, &["git", "config", "user.name", "Dev"]);
        run(dst, &["git", "config", "commit.gpgsign", "false"]);
        run(dst, &["git", "config", "core.autocrlf", "false"]);
    }

    #[tokio::test]
    async fn resolve_commit_fetches_a_wip_ref_from_origin_and_refreshes_later_pushes() {
        let dir = TempDir::new().unwrap();
        let seed = dir.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        run(&seed, &["git", "init", "-b", "main"]);
        run(&seed, &["git", "config", "user.email", "dev@example.com"]);
        run(&seed, &["git", "config", "user.name", "Dev"]);
        run(&seed, &["git", "config", "commit.gpgsign", "false"]);
        run(&seed, &["git", "config", "core.autocrlf", "false"]);
        std::fs::write(seed.join("README.md"), "hello\n").unwrap();
        run(&seed, &["git", "add", "README.md"]);
        run(&seed, &["git", "commit", "-m", "init"]);

        let origin = dir.path().join("origin.git");
        run(
            dir.path(),
            &[
                "git",
                "clone",
                "--bare",
                "--quiet",
                seed.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        let host = dir.path().join("host");
        let sandbox = dir.path().join("sandbox");
        clone_repo(&origin, &host);
        clone_repo(&origin, &sandbox);
        // Narrow fetch so ordinary `git fetch` cannot hide the missing raw ref.
        run(
            &host,
            &[
                "git",
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        );

        std::fs::write(sandbox.join("proof.txt"), "first push\n").unwrap();
        run(&sandbox, &["git", "add", "proof.txt"]);
        run(&sandbox, &["git", "commit", "-m", "wip one"]);
        run(
            &sandbox,
            &["git", "push", "origin", "HEAD:refs/heads/mg-wip/proof-i1"],
        );
        let first = git_rev_parse(&sandbox, "HEAD").unwrap();

        assert!(git_rev_parse(&host, "mg-wip/proof-i1").is_none());
        assert!(git_rev_parse(&host, "refs/remotes/origin/mg-wip/proof-i1").is_none());
        run(&host, &["git", "fetch", "--quiet", "origin"]);
        assert!(
            git_rev_parse(&host, "mg-wip/proof-i1").is_none(),
            "raw ref must stay unresolved after ordinary fetch"
        );
        assert!(
            git_rev_parse(&host, "refs/remotes/origin/mg-wip/proof-i1").is_none(),
            "narrow ordinary fetch must not take the wip ref"
        );

        let resolved = resolve_commit(&host, "mg-wip/proof-i1", None)
            .await
            .unwrap();
        assert_eq!(resolved, first);
        assert!(git_rev_parse(&host, "mg-wip/proof-i1").is_none());
        assert_eq!(
            git_rev_parse(&host, "refs/remotes/origin/mg-wip/proof-i1").as_deref(),
            Some(first.as_str())
        );

        std::fs::write(sandbox.join("proof.txt"), "second push\n").unwrap();
        run(&sandbox, &["git", "add", "proof.txt"]);
        run(&sandbox, &["git", "commit", "-m", "wip two"]);
        run(
            &sandbox,
            &[
                "git",
                "push",
                "--force",
                "origin",
                "HEAD:refs/heads/mg-wip/proof-i1",
            ],
        );
        let second = git_rev_parse(&sandbox, "HEAD").unwrap();
        assert_ne!(first, second);

        let refreshed = resolve_commit(&host, "mg-wip/proof-i1", None)
            .await
            .unwrap();
        assert_eq!(refreshed, second);
        let from_oid = merge_base_oid(&host, "main", &refreshed, None)
            .await
            .unwrap();
        let checkout = RemoteCheckout {
            repo_root: host,
            source: WorkspaceContentSource {
                revision: WorkspaceContentRevision::Retained,
                revision_ref: Some("mg-wip/proof-i1".into()),
                saved_at: None,
            },
            from_oid,
            to_oid: refreshed,
        };
        let blob = read_checkout_blob(&checkout, "proof.txt").await.unwrap();
        assert_eq!(blob.content, "second push\n");

        // Keep the cached ref, then make origin unreachable. Returning that
        // cached commit would silently substitute an unverified checkpoint.
        let missing_origin = dir.path().join("unreachable.git");
        run(
            &checkout.repo_root,
            &[
                "git",
                "remote",
                "set-url",
                "origin",
                missing_origin.to_str().unwrap(),
            ],
        );
        assert!(
            git_rev_parse(&checkout.repo_root, "refs/remotes/origin/mg-wip/proof-i1").is_some()
        );
        let error = resolve_commit(&checkout.repo_root, "mg-wip/proof-i1", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_sandbox_unavailable");
    }

    fn seed_bare_origin(dir: &Path) -> PathBuf {
        let seed = dir.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        run(&seed, &["git", "init", "-b", "main"]);
        run(&seed, &["git", "config", "user.email", "dev@example.com"]);
        run(&seed, &["git", "config", "user.name", "Dev"]);
        run(&seed, &["git", "config", "commit.gpgsign", "false"]);
        run(&seed, &["git", "config", "core.autocrlf", "false"]);
        std::fs::write(seed.join("README.md"), "hello\n").unwrap();
        run(&seed, &["git", "add", "README.md"]);
        run(&seed, &["git", "commit", "-m", "init"]);
        let origin = dir.join("origin.git");
        run(
            dir,
            &[
                "git",
                "clone",
                "--bare",
                "--quiet",
                seed.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        origin
    }

    async fn changed_paths(repo: &Path, from: &str, to: &str) -> Vec<String> {
        list_changed_files(repo, from, to, DiffBounds::default())
            .await
            .unwrap()
            .files
            .into_iter()
            .map(|file| file.path.to_wire())
            .collect()
    }

    #[test]
    fn origin_branch_name_accepts_common_base_ref_shapes() {
        assert_eq!(origin_branch_name("main"), "main");
        assert_eq!(origin_branch_name("origin/main"), "main");
        assert_eq!(origin_branch_name("refs/heads/main"), "main");
        assert_eq!(origin_branch_name("refs/remotes/origin/main"), "main");
        assert_eq!(origin_branch_name("release/1.2"), "release/1.2");
    }

    #[tokio::test]
    async fn merge_base_oid_uses_refreshed_origin_not_stale_local_main() {
        let dir = TempDir::new().unwrap();
        let origin = seed_bare_origin(dir.path());
        let host = dir.path().join("host");
        let sandbox = dir.path().join("sandbox");
        clone_repo(&origin, &host);

        let advance = dir.path().join("advance");
        clone_repo(&origin, &advance);
        for i in 0..8 {
            let name = format!("history-{i}.txt");
            std::fs::write(advance.join(&name), format!("{i}\n")).unwrap();
            run(&advance, &["git", "add", &name]);
            run(&advance, &["git", "commit", "-m", &format!("history {i}")]);
        }
        run(&advance, &["git", "push", "origin", "main"]);
        clone_repo(&origin, &sandbox);

        std::fs::write(sandbox.join("marker.txt"), "only this\n").unwrap();
        run(&sandbox, &["git", "add", "marker.txt"]);
        run(&sandbox, &["git", "commit", "-m", "marker"]);
        run(
            &sandbox,
            &["git", "push", "origin", "HEAD:refs/heads/mg-wip/marker-i1"],
        );

        let tip = resolve_commit(&host, "mg-wip/marker-i1", None)
            .await
            .unwrap();
        let stale_local = git_rev_parse(&host, "main").unwrap();
        let origin_main = git_rev_parse(&sandbox, "origin/main").unwrap();
        assert_ne!(stale_local, origin_main);
        assert_ne!(origin_main, tip);

        for base in [
            "main",
            "origin/main",
            "refs/heads/main",
            "refs/remotes/origin/main",
        ] {
            let from_oid = merge_base_oid(&host, base, &tip, None).await.unwrap();
            assert_eq!(from_oid, origin_main, "base_ref {base}");
            let paths = changed_paths(&host, &from_oid, &tip).await;
            assert_eq!(paths, vec!["marker.txt".to_owned()], "base_ref {base}");
        }
    }

    #[tokio::test]
    async fn merge_base_oid_is_unavailable_when_origin_base_refresh_fails() {
        let dir = TempDir::new().unwrap();
        let origin = seed_bare_origin(dir.path());
        let host = dir.path().join("host");
        clone_repo(&origin, &host);
        let tip = git_rev_parse(&host, "HEAD").unwrap();
        let missing_origin = dir.path().join("unreachable.git");
        run(
            &host,
            &[
                "git",
                "remote",
                "set-url",
                "origin",
                missing_origin.to_str().unwrap(),
            ],
        );
        let error = merge_base_oid(&host, "main", &tip, None).await.unwrap_err();
        assert_eq!(error.kind(), "sandbox_checkpoint_unavailable_base");
    }

    #[tokio::test]
    async fn merge_base_oid_keeps_tag_and_commit_bases_pinned_without_origin_fetch() {
        let dir = TempDir::new().unwrap();
        let origin = seed_bare_origin(dir.path());
        let host = dir.path().join("host");
        clone_repo(&origin, &host);
        let pinned = git_rev_parse(&host, "HEAD").unwrap();
        run(&host, &["git", "tag", "v1"]);
        let short = pinned.chars().take(12).collect::<String>();

        let advance = dir.path().join("advance");
        clone_repo(&origin, &advance);
        std::fs::write(advance.join("later.txt"), "later\n").unwrap();
        run(&advance, &["git", "add", "later.txt"]);
        run(&advance, &["git", "commit", "-m", "later"]);
        run(&advance, &["git", "push", "origin", "main"]);

        std::fs::write(host.join("marker.txt"), "only this\n").unwrap();
        run(&host, &["git", "add", "marker.txt"]);
        run(&host, &["git", "commit", "-m", "marker"]);
        let tip = git_rev_parse(&host, "HEAD").unwrap();

        let missing_origin = dir.path().join("unreachable.git");
        run(
            &host,
            &[
                "git",
                "remote",
                "set-url",
                "origin",
                missing_origin.to_str().unwrap(),
            ],
        );

        for base in ["v1", "refs/tags/v1", pinned.as_str(), short.as_str()] {
            let from_oid = merge_base_oid(&host, base, &tip, None).await.unwrap();
            assert_eq!(from_oid, pinned, "base_ref {base}");
        }

        let error = merge_base_oid(&host, "main", &tip, None).await.unwrap_err();
        assert_eq!(error.kind(), "sandbox_checkpoint_unavailable_base");
    }

    #[tokio::test]
    async fn merge_base_oid_fetches_origin_branch_absent_from_host_refs() {
        let dir = TempDir::new().unwrap();
        let origin = seed_bare_origin(dir.path());
        let host = dir.path().join("host");
        clone_repo(&origin, &host);
        run(
            &host,
            &[
                "git",
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        );

        let advance = dir.path().join("advance");
        clone_repo(&origin, &advance);
        std::fs::write(advance.join("history.txt"), "later base\n").unwrap();
        run(&advance, &["git", "add", "history.txt"]);
        run(&advance, &["git", "commit", "-m", "later base"]);
        run(
            &advance,
            &["git", "push", "origin", "HEAD:refs/heads/deadbeef"],
        );

        let sandbox = dir.path().join("sandbox");
        clone_repo(&origin, &sandbox);
        run(&sandbox, &["git", "checkout", "-q", "deadbeef"]);
        std::fs::write(sandbox.join("marker.txt"), "only this\n").unwrap();
        run(&sandbox, &["git", "add", "marker.txt"]);
        run(&sandbox, &["git", "commit", "-m", "marker"]);
        run(
            &sandbox,
            &["git", "push", "origin", "HEAD:refs/heads/mg-wip/fresh-i1"],
        );

        let tip = resolve_commit(&host, "mg-wip/fresh-i1", None)
            .await
            .unwrap();
        assert!(git_rev_parse(&host, "deadbeef").is_none());
        assert!(git_rev_parse(&host, "refs/heads/deadbeef").is_none());
        assert!(git_rev_parse(&host, "refs/remotes/origin/deadbeef").is_none());

        for base in ["deadbeef", "refs/heads/deadbeef"] {
            let from_oid = merge_base_oid(&host, base, &tip, None).await.unwrap();
            let paths = changed_paths(&host, &from_oid, &tip).await;
            assert_eq!(paths, vec!["marker.txt".to_owned()], "base_ref {base}");
        }
    }

    #[tokio::test]
    async fn merge_base_oid_preserves_local_origin_feature_branch_name() {
        let dir = TempDir::new().unwrap();
        let origin = seed_bare_origin(dir.path());
        let host = dir.path().join("host");
        clone_repo(&origin, &host);

        let feature = dir.path().join("feature");
        clone_repo(&origin, &feature);
        std::fs::write(feature.join("wrong.txt"), "stale feature\n").unwrap();
        run(&feature, &["git", "add", "wrong.txt"]);
        run(&feature, &["git", "commit", "-m", "stale feature"]);
        run(
            &feature,
            &["git", "push", "origin", "HEAD:refs/heads/feature"],
        );

        let named = dir.path().join("named");
        clone_repo(&origin, &named);
        run(&named, &["git", "checkout", "-q", "-b", "origin/feature"]);
        std::fs::write(named.join("history.txt"), "named base\n").unwrap();
        run(&named, &["git", "add", "history.txt"]);
        run(&named, &["git", "commit", "-m", "named base"]);
        run(
            &named,
            &["git", "push", "origin", "HEAD:refs/heads/origin/feature"],
        );

        run(
            &host,
            &[
                "git",
                "fetch",
                "origin",
                "+refs/heads/origin/feature:refs/heads/origin/feature",
            ],
        );
        let stale_named = git_rev_parse(&host, "refs/heads/origin/feature").unwrap();

        std::fs::write(named.join("history.txt"), "refreshed named\n").unwrap();
        run(&named, &["git", "add", "history.txt"]);
        run(&named, &["git", "commit", "-m", "refresh named"]);
        run(
            &named,
            &[
                "git",
                "push",
                "--force",
                "origin",
                "HEAD:refs/heads/origin/feature",
            ],
        );
        std::fs::write(named.join("marker.txt"), "only this\n").unwrap();
        run(&named, &["git", "add", "marker.txt"]);
        run(&named, &["git", "commit", "-m", "marker"]);
        run(
            &named,
            &["git", "push", "origin", "HEAD:refs/heads/mg-wip/named-i1"],
        );

        let tip = resolve_commit(&host, "mg-wip/named-i1", None)
            .await
            .unwrap();
        assert_eq!(
            git_rev_parse(&host, "refs/heads/origin/feature").as_deref(),
            Some(stale_named.as_str())
        );

        let from_oid = merge_base_oid(&host, "refs/heads/origin/feature", &tip, None)
            .await
            .unwrap();
        let paths = changed_paths(&host, &from_oid, &tip).await;
        assert_eq!(paths, vec!["marker.txt".to_owned()]);
        assert_ne!(from_oid, stale_named);
        assert!(git_rev_parse(&host, "refs/remotes/origin/origin/feature").is_some());
    }

    /// Stands in for the gateway lender. `grants_origin`, when set, is the
    /// origin a borrowed credential opens: the borrow repoints the host's
    /// `origin` there, which is what the forge accepting the credential
    /// looks like to git.
    struct TestCredentials {
        host: PathBuf,
        grants_origin: Option<PathBuf>,
        borrowed: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl OriginCredentials for TestCredentials {
        async fn borrow(&self) -> Option<GitCredential> {
            self.borrowed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let origin = self.grants_origin.as_ref()?;
            run(
                &self.host,
                &[
                    "git",
                    "remote",
                    "set-url",
                    "origin",
                    origin.to_str().unwrap(),
                ],
            );
            Some(GitCredential {
                username: "x-access-token".to_owned(),
                secret: "borrowed-secret".to_owned(),
            })
        }
    }

    impl TestCredentials {
        fn borrowed(&self) -> usize {
            self.borrowed.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// A host clone whose origin refuses it, plus a checkpoint pushed to the
    /// real origin that only a credentialed fetch can reach.
    fn private_origin_host(dir: &Path) -> (PathBuf, PathBuf, String) {
        let origin = seed_bare_origin(dir);
        let host = dir.join("host");
        let sandbox = dir.join("sandbox");
        clone_repo(&origin, &host);
        clone_repo(&origin, &sandbox);
        std::fs::write(sandbox.join("private.txt"), "saved in the sandbox\n").unwrap();
        run(&sandbox, &["git", "add", "private.txt"]);
        run(&sandbox, &["git", "commit", "-m", "wip"]);
        run(
            &sandbox,
            &["git", "push", "origin", "HEAD:refs/heads/mg-wip/private-i1"],
        );
        let tip = git_rev_parse(&sandbox, "HEAD").unwrap();
        let refused = dir.join("refuses-anonymous.git");
        run(
            &host,
            &[
                "git",
                "remote",
                "set-url",
                "origin",
                refused.to_str().unwrap(),
            ],
        );
        (origin, host, tip)
    }

    #[tokio::test]
    async fn a_refused_origin_fetch_retries_once_with_a_borrowed_credential() {
        let dir = TempDir::new().unwrap();
        let (origin, host, tip) = private_origin_host(dir.path());
        let credentials = TestCredentials {
            host: host.clone(),
            grants_origin: Some(origin),
            borrowed: Default::default(),
        };

        let resolved = resolve_commit(&host, "mg-wip/private-i1", Some(&credentials))
            .await
            .unwrap();
        assert_eq!(resolved, tip);
        assert_eq!(credentials.borrowed(), 1);
        let from_oid = merge_base_oid(&host, "main", &resolved, Some(&credentials))
            .await
            .unwrap();
        assert_eq!(
            changed_paths(&host, &from_oid, &resolved).await,
            vec!["private.txt".to_owned()]
        );
        // The origin now answers anonymously, so the base fetch borrowed
        // nothing. A public origin never asks the lender.
        assert_eq!(credentials.borrowed(), 1);

        // Nothing about the credential reached the repository's config.
        let config = std::fs::read_to_string(host.join(".git/config")).unwrap();
        assert!(!config.contains("borrowed-secret"), "{config}");
        assert!(!config.contains("credential"), "{config}");
    }

    #[tokio::test]
    async fn a_refused_origin_without_a_credential_stays_unavailable() {
        let dir = TempDir::new().unwrap();
        let (_origin, host, _tip) = private_origin_host(dir.path());
        let credentials = TestCredentials {
            host: host.clone(),
            grants_origin: None,
            borrowed: Default::default(),
        };
        let error = resolve_commit(&host, "mg-wip/private-i1", Some(&credentials))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_sandbox_unavailable");
        assert_eq!(credentials.borrowed(), 1);

        let error = resolve_commit(&host, "mg-wip/private-i1", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_sandbox_unavailable");

        let tip = git_rev_parse(&host, "HEAD").unwrap();
        let error = merge_base_oid(&host, "main", &tip, Some(&credentials))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "sandbox_checkpoint_unavailable_base");
        assert_eq!(credentials.borrowed(), 2);
    }

    #[test]
    fn a_borrowed_credential_rides_the_environment_not_argv() {
        let credential = GitCredential {
            username: "x-access-token".to_owned(),
            secret: "borrowed-secret".to_owned(),
        };
        let command = credentialed_git_command(Path::new("/tmp"), Some(&credential));
        let command = command.as_std();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().all(|arg| !arg.contains("borrowed-secret")));
        // The helper list is reset before the one-shot helper answers, so no
        // configured helper can store the credential.
        assert_eq!(
            args[..2],
            ["-c".to_owned(), "credential.helper=".to_owned()]
        );
        let envs = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            envs.get(super::super::gh::GIT_CREDENTIAL_SECRET_ENV)
                .map(String::as_str),
            Some("borrowed-secret")
        );
        assert_eq!(
            envs.get(super::super::gh::GIT_CREDENTIAL_HOST_ENV)
                .map(String::as_str),
            Some(super::super::gh::GIT_CREDENTIAL_FORGE_HOST)
        );

        let anonymous = credentialed_git_command(Path::new("/tmp"), None);
        assert_eq!(anonymous.as_std().get_args().count(), 0);
    }

    #[tokio::test]
    async fn merge_base_oid_is_unavailable_when_the_base_is_missing() {
        let (_dir, repo) = init_repo();
        std::fs::write(repo.join("changed.txt"), "changed\n").unwrap();
        run(&repo, &["git", "add", "changed.txt"]);
        run(&repo, &["git", "commit", "-m", "wip"]);
        let tip = git_rev_parse(&repo, "HEAD").unwrap();
        let error = merge_base_oid(&repo, "does-not-exist", &tip, None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "sandbox_checkpoint_unavailable_base");
        // Falling back to the tip would compare the tree with itself.
        let files = list_changed_files(
            &repo,
            &tip,
            &tip,
            crate::code::checkpoint::DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(
            files.files.is_empty(),
            "a tip-vs-tip range hides changed files"
        );
    }
}

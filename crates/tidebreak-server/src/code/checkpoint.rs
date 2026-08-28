//! Per-turn worktree checkpoints via a temporary git index.
//!
//! A turn that ends — completed, failed, or interrupted — records the
//! worktree's full state, tracked changes and untracked files alike, as a
//! synthetic commit created through a temporary index file. The user's index,
//! `HEAD`, and reflog are untouched. The commit is referenced only by a hidden
//! ref `refs/tidebreak/checkpoints/<workspace>/<session>/<ordinal>`.
//!
//! A failed or interrupted turn is checkpointed for the same reason a
//! completed one is: the engine may have rewritten files before it died, and
//! edits outside the chain are edits the per-turn diff and any future restore
//! cannot see. They would otherwise land in the next turn's checkpoint, under
//! the wrong turn.
//!
//! The session segment is load-bearing. A workspace holds several sessions
//! (decision 0055) and `next_turn_ordinal` counts per session, so every
//! session reaches turn 1; a workspace-keyed ref let one sibling's snapshot
//! overwrite another's.
//!
//! Ordinal 0 is the session's start baseline: the worktree as it stood when
//! that session was created. A first turn diffs against the baseline rather
//! than against the repo's base ref, so whatever a sibling session already
//! changed in the shared worktree stays out of this session's turn 1.
//!
//! Diffs are produced here, bounded in bytes and file count, with truncation
//! marked on the payload. The renderer never runs git.

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use tidebreak_harness::{spawn_process_tree, BoundedProcessOutput, OutputBudget};
use tokio::process::Command;
use tokio::time::{timeout, timeout_at, Instant};
use tracing::warn;

use tidebreak_core::db::code::{
    append_event, get_session, get_turn, get_workspace, list_turns, save_turn,
};
use tidebreak_core::{
    CodeEvent, CodeSession, CodeSessionId, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWorkspace,
    DbStore, Diffstat, FileChangeKind, HarnessNoticeLevel, SequencedCodeEvent, WorkspaceId,
};

use super::bus::CodeEventBus;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(120);
const GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const GIT_OUTPUT_LINES: usize = 200_000;
const GIT_ERROR_BYTES: usize = 64 * 1024;
const GIT_ERROR_LINES: usize = 2_048;
const MAX_DIFF_LINES: usize = 32_768;

/// Default bound on a unified-diff body.
pub(crate) const MAX_DIFF_BYTES: usize = 256 * 1024;
/// Default bound on how many files a files/diff payload includes in full.
pub(crate) const MAX_DIFF_FILES: usize = 64;

const REF_PREFIX: &str = "refs/tidebreak/checkpoints";
const GIT_PATH_WIRE_PREFIX: &str = "tidebreak-path:v1:";

/// Ordinal of a session's start baseline, one below its first turn.
///
/// Numbering the baseline keeps it in the same ref path as the session's
/// checkpoints, so [`previous_checkpoint_oid`] resolves `ordinal - 1` for
/// turn 1 the way it does for every later turn, and
/// [`delete_workspace_refs`] reaps it by prefix with the workspace's refs.
const BASELINE_ORDINAL: i64 = 0;

/// Byte and file-count caps for a produced diff or file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiffBounds {
    pub max_bytes: usize,
    pub max_files: usize,
}

impl Default for DiffBounds {
    fn default() -> Self {
        Self {
            max_bytes: MAX_DIFF_BYTES,
            max_files: MAX_DIFF_FILES,
        }
    }
}

/// One Git path, kept as bytes until the route serializes it.
///
/// UTF-8 paths keep their existing wire value unless they start with the
/// reserved prefix. Every other path uses a canonical URL-safe base64 value,
/// which the file-diff query must return unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct GitPath(Vec<u8>);

impl GitPath {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    fn from_wire(value: &str) -> Result<Self, CheckpointError> {
        let Some(encoded) = value.strip_prefix(GIT_PATH_WIRE_PREFIX) else {
            return Ok(Self(value.as_bytes().to_vec()));
        };
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CheckpointError::user("file path identifier is invalid"))?;
        let path = Self(bytes);
        if path.to_wire() != value {
            return Err(CheckpointError::user(
                "file path identifier is not canonical",
            ));
        }
        Ok(path)
    }

    pub(crate) fn to_wire(&self) -> String {
        match std::str::from_utf8(&self.0) {
            Ok(path) if !path.starts_with(GIT_PATH_WIRE_PREFIX) => path.to_owned(),
            _ => format!("{GIT_PATH_WIRE_PREFIX}{}", URL_SAFE_NO_PAD.encode(&self.0)),
        }
    }

    fn to_os_string(&self) -> Result<OsString, String> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(OsString::from_vec(self.0.clone()))
        }
        #[cfg(not(unix))]
        {
            String::from_utf8(self.0.clone())
                .map(OsString::from)
                .map_err(|_| "Git returned a path that this platform cannot represent".to_owned())
        }
    }
}

/// One file in a bounded workspace or turn file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub path: GitPath,
    pub kind: FileChangeKind,
    pub insertions: u32,
    pub deletions: u32,
    pub previous_path: Option<GitPath>,
}

/// Bounded file list for `GET /code/workspaces/{id}/files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedFiles {
    pub files: Vec<ChangedFile>,
    pub truncated: bool,
    pub stat: Diffstat,
}

/// Bounded unified diff for `GET /code/workspaces/{id}/diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedDiff {
    pub diff: String,
    pub truncated: bool,
    pub stat: Diffstat,
}

/// A recorded checkpoint: hidden ref name and the turn-scoped diffstat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedCheckpoint {
    pub checkpoint_ref: String,
    pub diffstat: Diffstat,
}

/// Failure from a checkpoint or diff operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CheckpointError {
    #[error("{0}")]
    User(String),
    #[error("{0}")]
    Internal(String),
}

impl CheckpointError {
    fn user(message: impl Into<String>) -> Self {
        Self::User(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum GitCommandError {
    #[error("git {description} timed out")]
    TimedOut { description: String },
    #[error("{0}")]
    Failed(String),
}

impl GitCommandError {
    fn timed_out(description: impl Into<String>) -> Self {
        Self::TimedOut {
            description: description.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReusableIndexFailure {
    Locked,
    Corrupt,
}

#[async_trait]
trait SnapshotGit: Send + Sync {
    async fn checkpoint_index_path(
        &self,
        worktree: &Path,
        deadline: Instant,
    ) -> Result<Option<PathBuf>, GitCommandError>;

    async fn snapshot_tree_with_index(
        &self,
        worktree: &Path,
        index_path: &Path,
        reset_from_head: bool,
        deadline: Instant,
    ) -> Result<String, GitCommandError>;
}

struct ProcessSnapshotGit;

#[async_trait]
impl SnapshotGit for ProcessSnapshotGit {
    async fn checkpoint_index_path(
        &self,
        worktree: &Path,
        deadline: Instant,
    ) -> Result<Option<PathBuf>, GitCommandError> {
        checkpoint_index_path_before(worktree, deadline).await
    }

    async fn snapshot_tree_with_index(
        &self,
        worktree: &Path,
        index_path: &Path,
        reset_from_head: bool,
        deadline: Instant,
    ) -> Result<String, GitCommandError> {
        snapshot_tree_with_index_before(worktree, index_path, reset_from_head, deadline).await
    }
}

/// Hidden ref for one turn of one session.
///
/// Keyed on the session as well as the workspace. Ordinals come from
/// `next_turn_ordinal`, which counts per session, so two sessions sharing a
/// workspace both reach turn 1. The workspace stays first in the path so
/// [`delete_workspace_refs`] can match every session by prefix.
pub(crate) fn checkpoint_ref(
    workspace_id: WorkspaceId,
    session_id: CodeSessionId,
    ordinal: i64,
) -> String {
    format!("{REF_PREFIX}/{workspace_id}/{session_id}/{ordinal}")
}

/// Hidden ref for where one session started.
pub(crate) fn session_baseline_ref(workspace_id: WorkspaceId, session_id: CodeSessionId) -> String {
    checkpoint_ref(workspace_id, session_id, BASELINE_ORDINAL)
}

/// Record where a session starts: the worktree as it stood at that moment.
///
/// A workspace holds several sessions over one worktree (decision 0055), so
/// the base branch is the wrong `from` for a session's first turn — it credits
/// this session with every edit a sibling made before it existed. The baseline
/// is that `from`.
///
/// Call this before the session can take a turn. Failure is not fatal: a
/// session with no baseline falls back to `merge_base(base_ref)`, which is
/// what every first turn used to diff against and is still the same tree for
/// the only session in a fresh workspace.
pub(crate) async fn record_session_baseline(
    worktree: &Path,
    workspace_id: WorkspaceId,
    session_id: CodeSessionId,
) -> Result<String, CheckpointError> {
    let r#ref = session_baseline_ref(workspace_id, session_id);
    write_snapshot_ref(worktree, &r#ref, None, "session baseline").await?;
    Ok(r#ref)
}

/// After a turn reaches any terminal status, snapshot the worktree and
/// journal.
///
/// Completed, failed, and interrupted turns are all checkpointed: a turn that
/// died mid-edit still changed the worktree, and only a ref of its own keeps
/// those edits in the chain instead of folding them into the next turn. A
/// `Running` turn is skipped — the worktree is still moving — and a turn that
/// already holds a ref is left alone.
///
/// Checkpoint failure does not fail the turn: a [`CodeEvent::HarnessNotice`]
/// is journaled and the already-recorded work stands.
pub(crate) async fn after_turn_ended(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &CodeSession,
    turn: &mut CodeTurn,
) {
    let terminal = matches!(
        turn.status,
        CodeTurnStatus::Completed | CodeTurnStatus::Failed | CodeTurnStatus::Interrupted
    );
    if !terminal || turn.checkpoint_ref.is_some() {
        return;
    }
    match record_for_turn(db, session, turn).await {
        Ok(recorded) => {
            turn.checkpoint_ref = Some(recorded.checkpoint_ref.clone());
            turn.diffstat = Some(recorded.diffstat.clone());
            if let Err(err) = save_turn(db, &session.owner, turn).await {
                warn!(
                    session = %session.id,
                    turn = %turn.id,
                    error = %err,
                    "failed to persist checkpoint on the turn row"
                );
            }
            let _ = journal(
                db,
                bus,
                session,
                CodeEvent::CheckpointRecorded {
                    turn_id: turn.id,
                    diffstat: recorded.diffstat,
                },
            )
            .await;
        }
        Err(err) => {
            let message = truncate_notice(format!("checkpoint failed: {err}"));
            warn!(
                session = %session.id,
                turn = %turn.id,
                error = %err,
                "checkpoint failed; turn is kept"
            );
            let _ = journal(
                db,
                bus,
                session,
                CodeEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    message,
                },
            )
            .await;
        }
    }
}

async fn record_for_turn(
    db: &DbStore,
    session: &CodeSession,
    turn: &CodeTurn,
) -> Result<RecordedCheckpoint, CheckpointError> {
    let workspace = get_workspace(db, &session.owner, session.workspace_id)
        .await
        .map_err(|err| CheckpointError::internal(err.to_string()))?
        .ok_or_else(|| CheckpointError::user("workspace not found"))?;
    let worktree = PathBuf::from(&workspace.worktree_path);
    let previous = previous_checkpoint_oid(&worktree, &workspace, db, turn).await?;
    record_checkpoint(
        &worktree,
        workspace.id,
        session.id,
        turn.ordinal,
        turn.status,
        previous.as_deref(),
        &workspace.base_ref,
    )
    .await
}

/// Snapshot the worktree into a hidden checkpoint ref.
///
/// Uses a temporary index file. The user's index and `HEAD` are not written.
/// `status` is the turn's terminal status, recorded in the commit message.
pub(crate) async fn record_checkpoint(
    worktree: &Path,
    workspace_id: WorkspaceId,
    session_id: CodeSessionId,
    ordinal: i64,
    status: CodeTurnStatus,
    previous_oid: Option<&str>,
    base_ref: &str,
) -> Result<RecordedCheckpoint, CheckpointError> {
    let r#ref = checkpoint_ref(workspace_id, session_id, ordinal);
    // `checkpoint turn <n>` stays the leading shape; the status is appended so
    // `git log` on the hidden refs distinguishes a completed turn from one
    // that failed or was interrupted. Nothing parses this message.
    let message = format!("checkpoint turn {ordinal} ({})", status.as_str());
    let commit = write_snapshot_ref(worktree, &r#ref, previous_oid, &message).await?;

    let from = match previous_oid {
        Some(oid) => oid.to_owned(),
        None => merge_base(worktree, base_ref).await?,
    };
    let files = collect_changes(worktree, &from, &commit, DiffBounds::default()).await?;
    Ok(RecordedCheckpoint {
        checkpoint_ref: r#ref,
        diffstat: files.stat,
    })
}

/// Snapshot the worktree, commit it, and move `r#ref` onto the commit.
///
/// The commit's parent is `parent_oid`, or `HEAD` when there is none, so a
/// session's refs form a chain: baseline, then one commit per turn. Returns
/// the commit oid.
async fn write_snapshot_ref(
    worktree: &Path,
    r#ref: &str,
    parent_oid: Option<&str>,
    message: &str,
) -> Result<String, CheckpointError> {
    let tree = snapshot_tree(worktree).await?;
    let parent = match parent_oid {
        Some(oid) => oid.to_owned(),
        None => git_text(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?,
    };
    let commit = git_text(
        worktree,
        &["commit-tree", &tree, "-p", &parent, "-m", message],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    git_text(
        worktree,
        &["update-ref", "--no-deref", r#ref, &commit],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    Ok(commit)
}

/// Changed files between two trees or a tree and the live worktree snapshot.
pub(crate) async fn list_changed_files(
    worktree: &Path,
    from: &str,
    to: &str,
    bounds: DiffBounds,
) -> Result<BoundedFiles, CheckpointError> {
    collect_changes(worktree, from, to, bounds).await
}

/// Bounded unified diff between two trees (or a live snapshot oid).
pub(crate) async fn produce_diff(
    worktree: &Path,
    from: &str,
    to: &str,
    file: Option<&str>,
    bounds: DiffBounds,
) -> Result<BoundedDiff, CheckpointError> {
    if let Some(path) = file {
        let path = GitPath::from_wire(path)?;
        let paths = std::slice::from_ref(&path);
        let (raw, read_truncated) = git_bytes_with_literal_paths_bounded(
            worktree,
            &["diff", "--find-renames", from, to, "--"],
            paths,
            GIT_SNAPSHOT_TIMEOUT,
            OutputBudget::head(bounds.max_bytes, MAX_DIFF_LINES),
        )
        .await
        .map_err(CheckpointError::internal)?;
        let (diff, decode_truncated) = truncate_bytes(&raw, bounds.max_bytes);
        let body_truncated = read_truncated || decode_truncated;
        let selected = collect_changes_for_paths(worktree, from, to, paths).await?;
        let stat = Diffstat {
            files: selected.stat.files.max(u32::from(!diff.is_empty())),
            insertions: selected.stat.insertions,
            deletions: selected.stat.deletions,
            truncated: body_truncated,
        };
        return Ok(BoundedDiff {
            diff,
            truncated: body_truncated,
            stat,
        });
    }

    let listed = collect_changes(worktree, from, to, bounds).await?;
    // One `git diff` for every included path rather than one spawn per file.
    // `collect_changes` has already capped `listed.files` at `max_files` and
    // set `listed.truncated` when it did, so passing those paths as a single
    // pathspec preserves both bounds and the output order (git sorts by path,
    // as does `--name-status -z`) while paying one process instead of N.
    let mut truncated = listed.truncated;
    if listed.files.is_empty() {
        return Ok(BoundedDiff {
            diff: String::new(),
            truncated,
            stat: Diffstat {
                truncated,
                ..listed.stat
            },
        });
    }
    let paths: Vec<_> = listed
        .files
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let (raw, read_truncated) = git_bytes_with_literal_paths_bounded(
        worktree,
        &["diff", "--find-renames", from, to, "--"],
        &paths,
        GIT_SNAPSHOT_TIMEOUT,
        OutputBudget::head(bounds.max_bytes, MAX_DIFF_LINES),
    )
    .await
    .map_err(CheckpointError::internal)?;
    let (body, decode_truncated) = truncate_bytes(&raw, bounds.max_bytes);
    let body_truncated = read_truncated || decode_truncated;
    truncated |= body_truncated;
    Ok(BoundedDiff {
        diff: body,
        truncated,
        stat: Diffstat {
            truncated,
            ..listed.stat
        },
    })
}

/// Snapshot the current worktree (tracked + untracked) as a tree oid.
///
/// The user's index is never opened. A temporary index file is used and
/// deleted before this returns.
pub(crate) async fn snapshot_tree(worktree: &Path) -> Result<String, CheckpointError> {
    snapshot_tree_with_git(worktree, GIT_SNAPSHOT_TIMEOUT, &ProcessSnapshotGit).await
}

async fn snapshot_tree_with_git(
    worktree: &Path,
    limit: Duration,
    git: &impl SnapshotGit,
) -> Result<String, CheckpointError> {
    let deadline = Instant::now() + limit;
    // Reuse one index per worktree so git's stat cache survives between turns
    // and `add -A` re-hashes only what changed. A cold index re-hashes the
    // whole worktree every time: ~0.85s versus ~0.20s on a 20k-file tree.
    let reusable = before_checkpoint_deadline(
        deadline,
        "rev-parse --git-path tidebreak-checkpoint-index",
        git.checkpoint_index_path(worktree, deadline),
    )
    .await
    .map_err(|err| CheckpointError::internal(err.to_string()))?;
    if let Some(index_path) = reusable {
        let reset_from_head = !index_path.exists();
        match before_checkpoint_deadline(
            deadline,
            "checkpoint snapshot",
            git.snapshot_tree_with_index(worktree, &index_path, reset_from_head, deadline),
        )
        .await
        {
            Ok(tree) => return Ok(tree),
            Err(err) => match reusable_index_failure(&err, &index_path) {
                Some(failure) => {
                    if failure == ReusableIndexFailure::Corrupt {
                        let _ = tokio::fs::remove_file(&index_path).await;
                    }
                    // A concurrent snapshot holding `<index>.lock`, or a
                    // corrupt reusable index, can use a private cold index.
                    // Every other failure returns without repeating `add -A`.
                    warn!(
                        error = %err,
                        "reusable checkpoint index unusable; falling back to a temporary one"
                    );
                }
                None => return Err(CheckpointError::internal(err.to_string())),
            },
        }
    }

    let temp = tempfile::NamedTempFile::new().map_err(|err| {
        CheckpointError::internal(format!("could not create temporary index: {err}"))
    })?;
    let index_path = temp.path().to_path_buf();
    // `git read-tree` wants to create the index; an empty file can confuse it.
    drop(temp);
    let _ = tokio::fs::remove_file(&index_path).await;

    let result = before_checkpoint_deadline(
        deadline,
        "checkpoint snapshot",
        git.snapshot_tree_with_index(worktree, &index_path, true, deadline),
    )
    .await;
    let _ = tokio::fs::remove_file(&index_path).await;
    result.map_err(|err| CheckpointError::internal(err.to_string()))
}

async fn before_checkpoint_deadline<T>(
    deadline: Instant,
    description: &str,
    future: impl Future<Output = Result<T, GitCommandError>>,
) -> Result<T, GitCommandError> {
    timeout_at(deadline, future)
        .await
        .map_err(|_| GitCommandError::timed_out(description))?
}

fn reusable_index_failure(
    error: &GitCommandError,
    index_path: &Path,
) -> Option<ReusableIndexFailure> {
    let GitCommandError::Failed(message) = error else {
        return None;
    };
    let message = message.to_ascii_lowercase();
    let index_name = index_path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let lock_name = format!("{index_name}.lock");
    if !index_name.is_empty()
        && message.contains(&lock_name)
        && (message.contains("file exists")
            || message.contains("already exists")
            || message.contains("another git process"))
    {
        return Some(ReusableIndexFailure::Locked);
    }

    const CORRUPTION_MARKERS: &[&str] = &[
        "index file corrupt",
        "index file smaller than expected",
        "index file is too small",
        "bad signature 0x",
        "unknown index entry format",
        "unsupported index version",
        "index version is not supported",
        "invalid index",
        "malformed index",
    ];
    CORRUPTION_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
        .then_some(ReusableIndexFailure::Corrupt)
}

/// Path of this worktree's reusable checkpoint index.
///
/// `--git-path` resolves into the worktree's own git dir, so linked worktrees
/// each get their own file and none of them is the user's `index`. Returns
/// `None` outside a repository, where the caller starts with a temp index.
#[cfg(test)]
async fn checkpoint_index_path(worktree: &Path) -> Option<PathBuf> {
    checkpoint_index_path_before(worktree, Instant::now() + GIT_TIMEOUT)
        .await
        .ok()
        .flatten()
}

async fn checkpoint_index_path_before(
    worktree: &Path,
    deadline: Instant,
) -> Result<Option<PathBuf>, GitCommandError> {
    let raw = match git_text_env_before(
        worktree,
        &["rev-parse", "--git-path", "tidebreak-checkpoint-index"],
        &[],
        deadline.min(Instant::now() + GIT_TIMEOUT),
    )
    .await
    {
        Ok(raw) => raw,
        Err(GitCommandError::Failed(message))
            if message
                .to_ascii_lowercase()
                .contains("not a git repository") =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(trimmed);
    Ok(Some(if path.is_absolute() {
        path
    } else {
        worktree.join(path)
    }))
}

/// Snapshot the worktree through `index_path`.
///
/// `reset_from_head` seeds a fresh index with `read-tree HEAD`. A reused index
/// must skip that: `read-tree` overwrites the index wholesale and throws away
/// the stat cache, which is the entire reason for keeping the file. `add -A`
/// reconciles whatever the index already holds against the worktree — adding,
/// updating, and staging deletions — so the resulting tree is the same either
/// way, and it stays the same after `HEAD` moves.
#[cfg(test)]
async fn snapshot_tree_with_index(
    worktree: &Path,
    index_path: &Path,
    reset_from_head: bool,
) -> Result<String, CheckpointError> {
    snapshot_tree_with_index_before(
        worktree,
        index_path,
        reset_from_head,
        Instant::now() + GIT_SNAPSHOT_TIMEOUT,
    )
    .await
    .map_err(|err| CheckpointError::internal(err.to_string()))
}

async fn snapshot_tree_with_index_before(
    worktree: &Path,
    index_path: &Path,
    reset_from_head: bool,
    deadline: Instant,
) -> Result<String, GitCommandError> {
    let index = index_path.to_string_lossy();
    if reset_from_head {
        git_text_env_before(
            worktree,
            &["read-tree", "HEAD"],
            &[("GIT_INDEX_FILE", index.as_ref())],
            deadline.min(Instant::now() + GIT_TIMEOUT),
        )
        .await?;
    }
    git_text_env_before(
        worktree,
        &["add", "-A"],
        &[("GIT_INDEX_FILE", index.as_ref())],
        deadline,
    )
    .await?;
    git_text_env_before(
        worktree,
        &["write-tree"],
        &[("GIT_INDEX_FILE", index.as_ref())],
        deadline.min(Instant::now() + GIT_TIMEOUT),
    )
    .await
}

/// Resolve `merge-base(base_ref, HEAD)`, falling back to `base_ref`.
pub(crate) async fn merge_base(worktree: &Path, base_ref: &str) -> Result<String, CheckpointError> {
    match git_text(worktree, &["merge-base", base_ref, "HEAD"], GIT_TIMEOUT).await {
        Ok(oid) if !oid.is_empty() => Ok(oid),
        _ => git_text(worktree, &["rev-parse", base_ref], GIT_TIMEOUT)
            .await
            .map_err(|err| {
                CheckpointError::user(format!("could not resolve base {base_ref}: {err}"))
            }),
    }
}

/// Resolve the base shown by the workspace diff.
///
/// A pull request can rebase onto `origin/main` while the local `main` ref
/// stays behind. When a pull request exists, follow its remote base so the
/// workspace pane matches the host. The read path never fetches. If the remote
/// ref is absent, keep the workspace's configured base.
async fn workspace_merge_base(
    worktree: &Path,
    configured_base: &str,
    pull_request_base: Option<&str>,
) -> Result<String, CheckpointError> {
    if let Some(remote_ref) = pull_request_base.and_then(remote_tracking_ref) {
        let commit_ref = format!("{remote_ref}^{{commit}}");
        if git_text(
            worktree,
            &["rev-parse", "--verify", "--quiet", &commit_ref],
            GIT_TIMEOUT,
        )
        .await
        .is_ok()
        {
            return merge_base(worktree, &remote_ref).await;
        }
    }
    merge_base(worktree, configured_base).await
}

fn remote_tracking_ref(base_branch: &str) -> Option<String> {
    let base_branch = base_branch.trim();
    if base_branch.is_empty() {
        return None;
    }
    if base_branch.starts_with("refs/remotes/") {
        return Some(base_branch.to_owned());
    }
    let branch = base_branch
        .strip_prefix("refs/heads/")
        .or_else(|| base_branch.strip_prefix("origin/"))
        .unwrap_or(base_branch);
    if branch.starts_with("refs/") {
        return None;
    }
    Some(format!("refs/remotes/origin/{branch}"))
}

fn workspace_pull_request_base(workspace: &CodeWorkspace) -> Option<&str> {
    workspace.pr.as_ref().map(|pull_request| {
        pull_request
            .base_branch
            .as_deref()
            .unwrap_or(&workspace.base_ref)
    })
}

/// Delete every checkpoint ref belonging to one workspace.
///
/// The workspace stays first in the ref path, so one prefix covers every
/// session's turns and its start baseline.
pub(crate) async fn delete_workspace_refs(
    repo_root: &Path,
    workspace_id: WorkspaceId,
) -> Result<usize, CheckpointError> {
    let prefix = format!("{REF_PREFIX}/{workspace_id}/");
    let refs = list_checkpoint_refs(repo_root).await?;
    let mut removed = 0usize;
    for r#ref in refs {
        if r#ref.starts_with(&prefix) {
            delete_ref(repo_root, &r#ref).await?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub(crate) async fn list_checkpoint_refs(repo_root: &Path) -> Result<Vec<String>, CheckpointError> {
    let out = git_text(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("{REF_PREFIX}/"),
        ],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Resolve the trees a files/diff query should compare.
pub(crate) async fn resolve_diff_range(
    db: &DbStore,
    workspace: &CodeWorkspace,
    turn_id: Option<CodeTurnId>,
) -> Result<(PathBuf, String, String, Option<CodeTurnId>), CheckpointError> {
    let worktree = PathBuf::from(&workspace.worktree_path);
    if !worktree.exists() {
        return Err(CheckpointError::user("workspace worktree is gone"));
    }
    match turn_id {
        None => {
            let from = workspace_merge_base(
                &worktree,
                &workspace.base_ref,
                workspace_pull_request_base(workspace),
            )
            .await?;
            let to = snapshot_tree(&worktree).await?;
            Ok((worktree, from, to, None))
        }
        Some(id) => {
            let turn = get_turn(db, &workspace.owner, id)
                .await
                .map_err(|err| CheckpointError::internal(err.to_string()))?
                .ok_or_else(|| CheckpointError::user("turn not found"))?;
            let session = get_session(db, &workspace.owner, turn.session_id)
                .await
                .map_err(|err| CheckpointError::internal(err.to_string()))?
                .ok_or_else(|| CheckpointError::user("session not found"))?;
            if session.workspace_id != workspace.id {
                return Err(CheckpointError::user(
                    "turn does not belong to this workspace",
                ));
            }
            let to = turn
                .checkpoint_ref
                .clone()
                .ok_or_else(|| CheckpointError::user("this turn has no checkpoint"))?;
            let from = previous_checkpoint_oid(&worktree, workspace, db, &turn)
                .await?
                .unwrap_or(
                    workspace_merge_base(
                        &worktree,
                        &workspace.base_ref,
                        workspace_pull_request_base(workspace),
                    )
                    .await?,
                );
            Ok((worktree, from, to, Some(id)))
        }
    }
}

/// The oid a turn chains from: its diff base and its checkpoint's parent.
///
/// Turn `n` resolves turn `n - 1` in the same session, and turn 1 resolves
/// ordinal 0 — the session's start baseline — so its diff holds what this
/// session changed rather than everything sitting in the shared worktree.
///
/// `None` means no ref answered and the caller falls back to
/// `merge_base(base_ref)`: a session created before baselines were recorded
/// has none, and for the only session in a fresh workspace the baseline and
/// the merge base are the same tree.
async fn previous_checkpoint_oid(
    worktree: &Path,
    workspace: &CodeWorkspace,
    db: &DbStore,
    turn: &CodeTurn,
) -> Result<Option<String>, CheckpointError> {
    if turn.ordinal <= BASELINE_ORDINAL {
        return Ok(None);
    }
    let previous_ref = checkpoint_ref(workspace.id, turn.session_id, turn.ordinal - 1);
    if let Some(oid) = resolve_checkpoint_oid(worktree, &previous_ref).await {
        return Ok(Some(oid));
    }
    // A cleanup or older build may have removed one ref while its database row
    // remains. Walk earlier rows, but only return a ref Git can still resolve.
    let turns = list_turns(db, &workspace.owner, turn.session_id)
        .await
        .map_err(|err| CheckpointError::internal(err.to_string()))?;
    for candidate in turns.into_iter().rev() {
        if candidate.ordinal >= turn.ordinal {
            continue;
        }
        let Some(r#ref) = candidate.checkpoint_ref else {
            continue;
        };
        if let Some(oid) = resolve_checkpoint_oid(worktree, &r#ref).await {
            return Ok(Some(oid));
        }
    }
    let baseline = session_baseline_ref(workspace.id, turn.session_id);
    Ok(resolve_checkpoint_oid(worktree, &baseline).await)
}

async fn resolve_checkpoint_oid(worktree: &Path, r#ref: &str) -> Option<String> {
    git_text(worktree, &["rev-parse", "--verify", r#ref], GIT_TIMEOUT)
        .await
        .ok()
        .filter(|oid| !oid.is_empty())
}

async fn collect_changes(
    worktree: &Path,
    from: &str,
    to: &str,
    bounds: DiffBounds,
) -> Result<BoundedFiles, CheckpointError> {
    collect_changes_inner(worktree, from, to, bounds, None).await
}

async fn collect_changes_for_paths(
    worktree: &Path,
    from: &str,
    to: &str,
    paths: &[GitPath],
) -> Result<BoundedFiles, CheckpointError> {
    collect_changes_inner(
        worktree,
        from,
        to,
        DiffBounds {
            max_bytes: usize::MAX,
            max_files: usize::MAX,
        },
        Some(paths),
    )
    .await
}

async fn collect_changes_inner(
    worktree: &Path,
    from: &str,
    to: &str,
    bounds: DiffBounds,
    paths: Option<&[GitPath]>,
) -> Result<BoundedFiles, CheckpointError> {
    let name_status_args = [
        "diff",
        "--name-status",
        "-z",
        "--find-renames",
        from,
        to,
        "--",
    ];
    let numstat_args = ["diff", "--numstat", "-z", "--find-renames", from, to, "--"];
    let name_status = match paths {
        Some(paths) => {
            git_bytes_with_literal_paths(worktree, &name_status_args, paths, GIT_TIMEOUT).await
        }
        None => {
            git_bytes(
                worktree,
                &name_status_args[..name_status_args.len() - 1],
                GIT_TIMEOUT,
            )
            .await
        }
    }
    .map_err(CheckpointError::internal)?;
    let numstat = match paths {
        Some(paths) => {
            git_bytes_with_literal_paths(worktree, &numstat_args, paths, GIT_TIMEOUT).await
        }
        None => {
            git_bytes(
                worktree,
                &numstat_args[..numstat_args.len() - 1],
                GIT_TIMEOUT,
            )
            .await
        }
    }
    .map_err(CheckpointError::internal)?;
    let stats = parse_numstat(&numstat);
    let mut files = parse_name_status(&name_status);
    for file in &mut files {
        if let Some((insertions, deletions)) = stats
            .get(&file.path)
            .or_else(|| file.previous_path.as_ref().and_then(|prev| stats.get(prev)))
        {
            file.insertions = *insertions;
            file.deletions = *deletions;
        }
    }
    let total_files = files.len();
    let insertions = files
        .iter()
        .map(|file| file.insertions)
        .fold(0u32, u32::saturating_add);
    let deletions = files
        .iter()
        .map(|file| file.deletions)
        .fold(0u32, u32::saturating_add);
    let truncated = total_files > bounds.max_files;
    if truncated {
        files.truncate(bounds.max_files);
    }
    Ok(BoundedFiles {
        files,
        truncated,
        stat: Diffstat {
            files: u32::try_from(total_files).unwrap_or(u32::MAX),
            insertions,
            deletions,
            truncated,
        },
    })
}

fn parse_name_status(raw: &[u8]) -> Vec<ChangedFile> {
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());
    let mut files = Vec::new();
    while let Some(status) = parts.next() {
        let code = status.first().copied().unwrap_or(b'M');
        match code {
            b'R' | b'C' => {
                let previous = parts.next().unwrap_or_default();
                let path = parts.next().unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                files.push(ChangedFile {
                    path: GitPath::from_bytes(path),
                    kind: if code == b'R' {
                        FileChangeKind::Renamed
                    } else {
                        FileChangeKind::Modified
                    },
                    insertions: 0,
                    deletions: 0,
                    previous_path: (!previous.is_empty()).then(|| GitPath::from_bytes(previous)),
                });
            }
            other => {
                let path = parts.next().unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                let kind = match other {
                    b'A' => FileChangeKind::Added,
                    b'D' => FileChangeKind::Deleted,
                    _ => FileChangeKind::Modified,
                };
                files.push(ChangedFile {
                    path: GitPath::from_bytes(path),
                    kind,
                    insertions: 0,
                    deletions: 0,
                    previous_path: None,
                });
            }
        }
    }
    files
}

/// Parse `git diff --numstat -z`, keyed by post-image path.
///
/// `-z` is what makes the keys line up with `--name-status -z`: without it git
/// quotes and C-escapes any path outside ASCII, and collapses a rename into a
/// single `old => new` field. Neither form ever matches a name-status path, so
/// those files would silently carry zero insertions and deletions.
///
/// The NUL-delimited record is `<added>\t<deleted>\t<path>\0`, except for a
/// rename or copy, where the path column is empty and the pre-image and
/// post-image paths follow as two further NUL-terminated fields.
fn parse_numstat(raw: &[u8]) -> std::collections::HashMap<GitPath, (u32, u32)> {
    let mut out = std::collections::HashMap::new();
    let mut parts = raw.split(|byte| *byte == 0).filter(|part| !part.is_empty());
    while let Some(record) = parts.next() {
        // A path may itself contain a tab, so only split off the two counts.
        let mut cols = record.splitn(3, |byte| *byte == b'\t');
        let insertions = parse_stat_count(cols.next().unwrap_or(b"0"));
        let deletions = parse_stat_count(cols.next().unwrap_or(b"0"));
        let path = cols.next().unwrap_or_default();
        if path.is_empty() {
            let (Some(_previous), Some(current)) = (parts.next(), parts.next()) else {
                break;
            };
            out.insert(GitPath::from_bytes(current), (insertions, deletions));
        } else {
            out.insert(GitPath::from_bytes(path), (insertions, deletions));
        }
    }
    out
}

fn parse_stat_count(value: &[u8]) -> u32 {
    if value == b"-" {
        0
    } else {
        std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }
}

fn truncate_bytes(raw: &[u8], max: usize) -> (String, bool) {
    let text = String::from_utf8_lossy(raw);
    if text.len() <= max {
        return (text.into_owned(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

async fn delete_ref(repo_root: &Path, r#ref: &str) -> Result<(), CheckpointError> {
    git_text(repo_root, &["update-ref", "-d", r#ref], GIT_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(CheckpointError::internal)
}

async fn journal(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
    event: CodeEvent,
) -> Result<(), tidebreak_core::db::code::CodeJournalError> {
    let seq = append_event(db, &session.owner, session.id, session.spawn_epoch, &event).await?;
    bus.publish(session.id, SequencedCodeEvent { seq, event });
    Ok(())
}

fn truncate_notice(message: String) -> String {
    const MAX: usize = tidebreak_core::MAX_NOTICE_CHARS;
    if message.chars().count() <= MAX {
        return message;
    }
    message.chars().take(MAX).collect()
}

async fn git_text(cwd: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    git_text_env(cwd, args, &[], limit).await
}

async fn git_text_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    limit: Duration,
) -> Result<String, String> {
    let bytes = git_bytes_env(cwd, args, env, limit).await?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

async fn git_text_env_before(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: Instant,
) -> Result<String, GitCommandError> {
    let bytes = git_bytes_env_before(cwd, args, env, deadline).await?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

async fn git_bytes(cwd: &Path, args: &[&str], limit: Duration) -> Result<Vec<u8>, String> {
    git_bytes_env(cwd, args, &[], limit).await
}

async fn git_bytes_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let mut command = git_command(cwd);
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    run_git_command(command, args.join(" "), limit).await
}

async fn git_bytes_env_before(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: Instant,
) -> Result<Vec<u8>, GitCommandError> {
    let description = args.join(" ");
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| GitCommandError::timed_out(&description))?;
    let mut command = git_command(cwd);
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let (stdout, truncated) = run_git_command_bounded_typed(
        command,
        description.clone(),
        remaining,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
        false,
    )
    .await?;
    if truncated {
        Err(GitCommandError::Failed(format!(
            "git {description} output exceeded its limit"
        )))
    } else {
        Ok(stdout)
    }
}

async fn git_bytes_with_literal_paths(
    cwd: &Path,
    args: &[&str],
    paths: &[GitPath],
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let mut command = git_command(cwd);
    command.arg("--literal-pathspecs").args(args);
    for path in paths {
        command.arg(path.to_os_string()?);
    }
    run_git_command(
        command,
        format!(
            "--literal-pathspecs {} <{} paths>",
            args.join(" "),
            paths.len()
        ),
        limit,
    )
    .await
}

async fn git_bytes_with_literal_paths_bounded(
    cwd: &Path,
    args: &[&str],
    paths: &[GitPath],
    limit: Duration,
    stdout_budget: OutputBudget,
) -> Result<(Vec<u8>, bool), String> {
    let mut command = git_command(cwd);
    command.arg("--literal-pathspecs").args(args);
    for path in paths {
        command.arg(path.to_os_string()?);
    }
    run_git_command_bounded(
        command,
        format!(
            "--literal-pathspecs {} <{} paths>",
            args.join(" "),
            paths.len()
        ),
        limit,
        stdout_budget,
        true,
    )
    .await
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "Tidebreak")
        .env("GIT_AUTHOR_EMAIL", "tidebreak@localhost")
        .env("GIT_COMMITTER_NAME", "Tidebreak")
        .env("GIT_COMMITTER_EMAIL", "tidebreak@localhost");
    command
}

async fn run_git_command(
    command: Command,
    description: String,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let (stdout, truncated) = run_git_command_bounded(
        command,
        description.clone(),
        limit,
        OutputBudget::head(GIT_OUTPUT_BYTES, GIT_OUTPUT_LINES),
        false,
    )
    .await?;
    if truncated {
        Err(format!("git {description} output exceeded its limit"))
    } else {
        Ok(stdout)
    }
}

async fn run_git_command_bounded(
    command: Command,
    description: String,
    limit: Duration,
    stdout_budget: OutputBudget,
    accept_truncated_stdout: bool,
) -> Result<(Vec<u8>, bool), String> {
    run_git_command_bounded_typed(
        command,
        description,
        limit,
        stdout_budget,
        accept_truncated_stdout,
    )
    .await
    .map_err(|err| err.to_string())
}

async fn run_git_command_bounded_typed(
    mut command: Command,
    description: String,
    limit: Duration,
    stdout_budget: OutputBudget,
    accept_truncated_stdout: bool,
) -> Result<(Vec<u8>, bool), GitCommandError> {
    let child = spawn_process_tree(&mut command)
        .map_err(|err| GitCommandError::Failed(format!("failed to spawn git: {err}")))?;
    let output = timeout(
        limit,
        child.wait_with_bounded_output(
            stdout_budget,
            OutputBudget::tail(GIT_ERROR_BYTES, GIT_ERROR_LINES),
            true,
        ),
    )
    .await
    .map_err(|_| GitCommandError::timed_out(&description))?
    .map_err(|err| GitCommandError::Failed(format!("git {description} failed: {err}")))?;
    finish_git_output(output, &description, accept_truncated_stdout)
        .map_err(GitCommandError::Failed)
}

fn finish_git_output(
    output: BoundedProcessOutput,
    description: &str,
    accept_truncated_stdout: bool,
) -> Result<(Vec<u8>, bool), String> {
    let stdout_truncated = output.stdout.truncated;
    let stderr_truncated = output.stderr.truncated;
    let stderr_empty = output.stderr.bytes.is_empty();
    if output.status.success() && !output.terminated_for_output {
        return if stdout_truncated && !accept_truncated_stdout {
            Err(format!("git {description} output exceeded its limit"))
        } else {
            Ok((output.stdout.bytes, stdout_truncated))
        };
    }
    if output.terminated_for_output
        && stdout_truncated
        && !stderr_truncated
        && stderr_empty
        && accept_truncated_stdout
    {
        return Ok((output.stdout.bytes, true));
    }

    let stdout = output.stdout.into_marked_text().trim().to_owned();
    let stderr = output.stderr.into_marked_text().trim().to_owned();
    if output.terminated_for_output {
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(if detail.is_empty() {
            format!("git {description} output exceeded its limit")
        } else {
            format!("git {description} output exceeded its limit: {detail}")
        });
    }
    Err(if stderr.is_empty() { stdout } else { stderr })
}

/// Fingerprint of the user's `HEAD` and index, used to prove a checkpoint
/// does not touch either.
#[cfg(test)]
pub(crate) async fn user_git_fingerprint(
    worktree: &Path,
) -> Result<(String, Vec<u8>), CheckpointError> {
    let head = git_text(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    let index_path = git_text(worktree, &["rev-parse", "--git-path", "index"], GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    let path = worktree.join(index_path);
    let bytes = tokio::fs::read(&path).await.unwrap_or_default();
    Ok((head, bytes))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as StdCommand;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use tidebreak_core::db::code::{
        insert_repo, insert_session, insert_turn, insert_workspace, list_events, MAX_REPLAY_EVENTS,
    };
    use tidebreak_core::{
        Attention, AttentionSource, CodeRepo, CodeSessionKind, CodeSessionLifecycle, CodeTurnId,
        CodeWorkspaceStatus, HarnessKind, OwnerId, PermissionMode, RepoId,
    };
    use tokio::sync::Notify;

    #[derive(Debug)]
    enum SnapshotStep {
        Return(Result<String, GitCommandError>),
        After(Duration, Result<String, GitCommandError>),
        Pending,
    }

    #[derive(Debug, Clone)]
    struct SnapshotCall {
        index_path: PathBuf,
        reset_from_head: bool,
        deadline: Instant,
        remaining: Duration,
    }

    #[derive(Debug)]
    struct ScriptedSnapshotGit {
        reusable_index: PathBuf,
        steps: Mutex<VecDeque<SnapshotStep>>,
        calls: Mutex<Vec<SnapshotCall>>,
        started: Notify,
    }

    impl ScriptedSnapshotGit {
        fn new(reusable_index: PathBuf, steps: Vec<SnapshotStep>) -> Self {
            Self {
                reusable_index,
                steps: Mutex::new(steps.into()),
                calls: Mutex::new(Vec::new()),
                started: Notify::new(),
            }
        }

        fn calls(&self) -> Vec<SnapshotCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SnapshotGit for ScriptedSnapshotGit {
        async fn checkpoint_index_path(
            &self,
            _worktree: &Path,
            _deadline: Instant,
        ) -> Result<Option<PathBuf>, GitCommandError> {
            Ok(Some(self.reusable_index.clone()))
        }

        async fn snapshot_tree_with_index(
            &self,
            _worktree: &Path,
            index_path: &Path,
            reset_from_head: bool,
            deadline: Instant,
        ) -> Result<String, GitCommandError> {
            self.calls.lock().unwrap().push(SnapshotCall {
                index_path: index_path.to_path_buf(),
                reset_from_head,
                deadline,
                remaining: deadline.saturating_duration_since(Instant::now()),
            });
            self.started.notify_one();
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted snapshot step");
            match step {
                SnapshotStep::Return(result) => result,
                SnapshotStep::After(delay, result) => {
                    tokio::time::sleep(delay).await;
                    result
                }
                SnapshotStep::Pending => std::future::pending().await,
            }
        }
    }

    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("origin");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["git", "init", "-b", "main"]);
        run(&repo, &["git", "config", "user.email", "dev@example.com"]);
        run(&repo, &["git", "config", "user.name", "Dev"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        std::fs::write(repo.join("keep.txt"), "keep\n").unwrap();
        run(&repo, &["git", "add", "README.md", "keep.txt"]);
        run(&repo, &["git", "commit", "-m", "init"]);
        (dir, repo)
    }

    fn add_worktree(repo: &Path, label: &str) -> PathBuf {
        let path = repo.parent().unwrap().join(label);
        run(
            repo,
            &[
                "git",
                "worktree",
                "add",
                "-b",
                &format!("tidebreak/{label}"),
                path.to_str().unwrap(),
                "main",
            ],
        );
        path
    }

    fn run(cwd: &Path, args: &[&str]) {
        let status = StdCommand::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "{args:?} failed in {}", cwd.display());
    }

    fn ws() -> WorkspaceId {
        WorkspaceId::new()
    }

    fn sess() -> CodeSessionId {
        CodeSessionId::new()
    }

    #[tokio::test]
    async fn checkpoint_captures_untracked_renames_and_mode_changes() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "cap");
        std::fs::write(tree.join("README.md"), "hello world\n").unwrap();
        std::fs::write(tree.join("new.txt"), "untracked\n").unwrap();
        std::fs::rename(tree.join("keep.txt"), tree.join("kept.txt")).unwrap();
        let mut perms = std::fs::metadata(tree.join("README.md"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(tree.join("README.md"), perms).unwrap();

        let before = user_git_fingerprint(&tree).await.unwrap();
        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let after = user_git_fingerprint(&tree).await.unwrap();
        assert_eq!(before, after, "user index and HEAD must be byte-identical");
        assert!(recorded.checkpoint_ref.starts_with(REF_PREFIX));
        assert!(recorded.diffstat.files >= 2);

        let head = git_text(&tree, &["rev-parse", "--abbrev-ref", "HEAD"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(head, "tidebreak/cap");
        let visible = git_text(&tree, &["log", "--oneline", "-1"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert!(
            !visible.contains("checkpoint"),
            "checkpoint must not appear on the branch: {visible}"
        );

        let from = merge_base(&tree, "main").await.unwrap();
        let files = list_changed_files(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            DiffBounds::default(),
        )
        .await
        .unwrap();
        let paths: Vec<_> = files.files.iter().map(|file| file.path.to_wire()).collect();
        assert!(paths.iter().any(|path| path == "new.txt"), "{paths:?}");
        assert!(
            files
                .files
                .iter()
                .any(|file| file.kind == FileChangeKind::Renamed
                    && (file.path.to_wire() == "kept.txt"
                        || file
                            .previous_path
                            .as_ref()
                            .is_some_and(|path| path.to_wire() == "keep.txt"))),
            "{:#?}",
            files.files
        );
        assert!(
            files
                .files
                .iter()
                .any(|file| file.path.to_wire() == "README.md"
                    && file.kind == FileChangeKind::Modified),
            "{:#?}",
            files.files
        );

        let diff = produce_diff(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            None,
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(diff.diff.contains("untracked"), "{}", diff.diff);
        assert!(
            diff.diff.contains("hello world") || diff.diff.contains("README"),
            "{}",
            diff.diff
        );
    }

    #[tokio::test]
    async fn line_counts_survive_renames_and_non_ascii_paths() {
        let (_dir, repo) = init_repo();
        let body: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/alpha.rs"), &body).unwrap();
        run(&repo, &["git", "add", "src/alpha.rs"]);
        run(&repo, &["git", "commit", "-m", "alpha"]);

        let tree = add_worktree(&repo, "numstat");
        // A rename with one added line, and a path outside ASCII. Non-`-z`
        // numstat renders the first as `src/{alpha.rs => beta.rs}` and quotes
        // and escapes the second, so neither matched its name-status entry.
        std::fs::rename(tree.join("src/alpha.rs"), tree.join("src/beta.rs")).unwrap();
        std::fs::write(tree.join("src/beta.rs"), format!("{body}line 13\n")).unwrap();
        std::fs::write(tree.join("café.txt"), "un\ndeux\n").unwrap();

        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let files = list_changed_files(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            DiffBounds::default(),
        )
        .await
        .unwrap();

        let renamed = files
            .files
            .iter()
            .find(|file| file.path.to_wire() == "src/beta.rs")
            .unwrap_or_else(|| panic!("{:#?}", files.files));
        assert_eq!(renamed.kind, FileChangeKind::Renamed);
        assert_eq!(
            renamed
                .previous_path
                .as_ref()
                .map(GitPath::to_wire)
                .as_deref(),
            Some("src/alpha.rs")
        );
        assert_eq!((renamed.insertions, renamed.deletions), (1, 0));

        // Matched by kind rather than by name: macOS and Linux disagree on the
        // Unicode normalization of the path, but not on its line counts.
        let accented = files
            .files
            .iter()
            .find(|file| file.kind == FileChangeKind::Added)
            .unwrap_or_else(|| panic!("{:#?}", files.files));
        let accented_path = accented.path.to_wire();
        assert!(accented_path.contains("caf"), "{accented_path}");
        assert!(
            !accented_path.starts_with('"'),
            "path must not be quoted: {accented_path}"
        );
        assert_eq!((accented.insertions, accented.deletions), (2, 0));

        assert_eq!(files.stat.insertions, 3);
        assert_eq!(files.stat.deletions, 0);
    }

    #[test]
    fn non_utf8_paths_do_not_collide_in_parsers_or_wire_values() {
        let first_name = b"collision-\x80.txt";
        let second_name = b"collision-\x81.txt";
        assert_eq!(
            String::from_utf8_lossy(first_name),
            String::from_utf8_lossy(second_name),
            "the old lossy representation collapsed these paths"
        );
        let name_status = b"A\0collision-\x80.txt\0A\0collision-\x81.txt\0";
        let numstat = b"1\t0\tcollision-\x80.txt\x001\t0\tcollision-\x81.txt\0";
        let stats = parse_numstat(numstat);
        let mut files = parse_name_status(name_status);
        for file in &mut files {
            let (insertions, deletions) = stats.get(&file.path).copied().unwrap();
            file.insertions = insertions;
            file.deletions = deletions;
        }

        assert_eq!(files.len(), 2);
        assert_ne!(files[0].path, files[1].path);
        let first = files[0].path.to_wire();
        let second = files[1].path.to_wire();
        assert_ne!(first, second);
        assert_eq!(GitPath::from_wire(&first).unwrap(), files[0].path);
        assert_eq!(GitPath::from_wire(&second).unwrap(), files[1].path);
        assert_eq!((files[0].insertions, files[0].deletions), (1, 0));
        assert_eq!((files[1].insertions, files[1].deletions), (1, 0));
    }

    #[test]
    fn reserved_wire_prefix_is_encoded_without_colliding() {
        let literal = GitPath::from_bytes(b"tidebreak-path:v1:YQ");
        let literal_wire = literal.to_wire();

        assert_ne!(literal_wire, "tidebreak-path:v1:YQ");
        assert_eq!(GitPath::from_wire(&literal_wire).unwrap(), literal);
        assert!(GitPath::from_wire("tidebreak-path:v1:YQ").is_err());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn non_utf8_wire_identity_addresses_exact_file_diff() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "non-utf8");
        let first_name = b"collision-\x80.txt";
        let second_name = b"collision-\x81.txt";
        std::fs::write(
            tree.join(OsString::from_vec(first_name.to_vec())),
            "first\n",
        )
        .unwrap();
        std::fs::write(
            tree.join(OsString::from_vec(second_name.to_vec())),
            "second\n",
        )
        .unwrap();

        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let files = list_changed_files(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            DiffBounds::default(),
        )
        .await
        .unwrap();

        let first = files
            .files
            .iter()
            .find(|file| file.path.0.as_slice() == first_name)
            .map(|file| file.path.to_wire())
            .expect("first byte path is listed");
        let second = files
            .files
            .iter()
            .find(|file| file.path.0.as_slice() == second_name)
            .map(|file| file.path.to_wire())
            .expect("second byte path is listed");
        assert_ne!(first, second);
        assert_eq!(GitPath::from_wire(&first).unwrap().0.as_slice(), first_name);
        assert_eq!(
            GitPath::from_wire(&second).unwrap().0.as_slice(),
            second_name
        );

        let first_diff = produce_diff(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            Some(&first),
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(first_diff.diff.contains("+first"), "{}", first_diff.diff);
        assert!(!first_diff.diff.contains("+second"), "{}", first_diff.diff);
        assert_eq!(
            (
                first_diff.stat.files,
                first_diff.stat.insertions,
                first_diff.stat.deletions
            ),
            (1, 1, 0)
        );

        let second_diff = produce_diff(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            Some(&second),
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(second_diff.diff.contains("+second"), "{}", second_diff.diff);
        assert!(!second_diff.diff.contains("+first"), "{}", second_diff.diff);
        assert_eq!(
            (
                second_diff.stat.files,
                second_diff.stat.insertions,
                second_diff.stat.deletions
            ),
            (1, 1, 0)
        );
    }

    #[tokio::test]
    async fn file_diff_treats_pathspec_magic_as_a_literal_path() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "literal-pathspec");
        let literal = ":(glob)*.txt";
        std::fs::write(tree.join(literal), "literal magic\n").unwrap();
        std::fs::write(tree.join("victim.txt"), "must stay out\n").unwrap();

        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let diff = produce_diff(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            Some(literal),
            DiffBounds::default(),
        )
        .await
        .unwrap();

        assert!(diff.diff.contains("+literal magic"), "{}", diff.diff);
        assert!(!diff.diff.contains("must stay out"), "{}", diff.diff);
        assert_eq!(
            (diff.stat.files, diff.stat.insertions, diff.stat.deletions),
            (1, 1, 0)
        );
    }

    #[tokio::test]
    async fn selected_file_stats_do_not_depend_on_the_file_list_cap() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "selected-stat");
        std::fs::write(tree.join("a-first.txt"), "first\n").unwrap();
        std::fs::write(tree.join("b-second.txt"), "second\n").unwrap();
        std::fs::write(tree.join("z-selected.txt"), "one\ntwo\n").unwrap();

        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let bounds = DiffBounds {
            max_bytes: MAX_DIFF_BYTES,
            max_files: 1,
        };
        let listed = list_changed_files(&tree, &from, &recorded.checkpoint_ref, bounds)
            .await
            .unwrap();
        assert!(listed.truncated);
        assert!(listed
            .files
            .iter()
            .all(|file| file.path.to_wire() != "z-selected.txt"));

        let diff = produce_diff(
            &tree,
            &from,
            &recorded.checkpoint_ref,
            Some("z-selected.txt"),
            bounds,
        )
        .await
        .unwrap();
        assert!(diff.diff.contains("+two"), "{}", diff.diff);
        assert_eq!(
            (diff.stat.files, diff.stat.insertions, diff.stat.deletions),
            (1, 2, 0)
        );
        assert!(!diff.stat.truncated);
    }

    #[tokio::test]
    async fn turn_diff_is_the_range_between_checkpoints() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "turns");
        let id = ws();
        let session = sess();
        std::fs::write(tree.join("a.txt"), "one\n").unwrap();
        let first = record_checkpoint(
            &tree,
            id,
            session,
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        std::fs::write(tree.join("b.txt"), "two\n").unwrap();
        let first_oid = git_text(&tree, &["rev-parse", &first.checkpoint_ref], GIT_TIMEOUT)
            .await
            .unwrap();
        let second = record_checkpoint(
            &tree,
            id,
            session,
            2,
            CodeTurnStatus::Completed,
            Some(&first_oid),
            "main",
        )
        .await
        .unwrap();

        let turn2 = produce_diff(
            &tree,
            &first.checkpoint_ref,
            &second.checkpoint_ref,
            None,
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(turn2.diff.contains("b.txt"), "{}", turn2.diff);
        assert!(
            !turn2.diff.contains("a.txt"),
            "turn 2 must not include turn 1: {}",
            turn2.diff
        );

        let turn1 = produce_diff(
            &tree,
            &merge_base(&tree, "main").await.unwrap(),
            &first.checkpoint_ref,
            None,
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(turn1.diff.contains("a.txt"), "{}", turn1.diff);
        assert!(!turn1.diff.contains("b.txt"), "{}", turn1.diff);
    }

    #[tokio::test]
    async fn truncation_is_marked_when_caps_are_exceeded() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "caps");
        for i in 0..8 {
            std::fs::write(
                tree.join(format!("f{i}.txt")),
                format!("{i}\n{}", "x".repeat(80)),
            )
            .unwrap();
        }
        let recorded = record_checkpoint(
            &tree,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let from = merge_base(&tree, "main").await.unwrap();
        let tight = DiffBounds {
            max_bytes: 40,
            max_files: 2,
        };
        let files = list_changed_files(&tree, &from, &recorded.checkpoint_ref, tight)
            .await
            .unwrap();
        assert!(files.truncated, "file-count cap must mark truncation");
        assert_eq!(files.files.len(), 2);
        assert!(files.stat.truncated);
        assert!(files.stat.files >= 8);

        let diff = produce_diff(&tree, &from, &recorded.checkpoint_ref, None, tight)
            .await
            .unwrap();
        assert!(diff.truncated);
        assert!(diff.diff.len() <= tight.max_bytes, "{}", diff.diff.len());
    }

    #[tokio::test]
    async fn workspace_diff_follows_the_pull_requests_remote_base() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "remote-base");

        std::fs::write(tree.join("base-change.txt"), "landed on main\n").unwrap();
        run(&tree, &["git", "add", "base-change.txt"]);
        run(&tree, &["git", "commit", "-m", "advance remote main"]);
        let remote_base = git_text(&tree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
            .await
            .unwrap();
        run(
            &tree,
            &[
                "git",
                "update-ref",
                "refs/remotes/origin/main",
                &remote_base,
            ],
        );

        std::fs::write(tree.join("workspace-change.txt"), "pull request change\n").unwrap();
        run(&tree, &["git", "add", "workspace-change.txt"]);
        run(&tree, &["git", "commit", "-m", "change the workspace"]);

        let from = workspace_merge_base(&tree, "main", Some("main"))
            .await
            .unwrap();
        let to = snapshot_tree(&tree).await.unwrap();
        let files = list_changed_files(&tree, &from, &to, DiffBounds::default())
            .await
            .unwrap();
        let paths: Vec<_> = files.files.iter().map(|file| file.path.to_wire()).collect();

        assert_eq!(paths, vec!["workspace-change.txt".to_owned()]);
        assert_eq!(files.stat.files, 1);
    }

    #[tokio::test]
    async fn archive_removes_only_the_target_workspaces_refs() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "gone");
        let live = ws();
        let dead = ws();
        std::fs::write(tree.join("x.txt"), "x\n").unwrap();
        record_checkpoint(
            &tree,
            live,
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        record_checkpoint(
            &tree,
            dead,
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        let listed = list_checkpoint_refs(&repo).await.unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");

        let removed = delete_workspace_refs(&repo, dead).await.unwrap();
        assert_eq!(removed, 1);
        let listed = list_checkpoint_refs(&repo).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].contains(&live.to_string()));

        assert_eq!(
            list_checkpoint_refs(&repo).await.unwrap().len(),
            1,
            "another workspace's refs must survive"
        );
    }

    /// Two sessions share a workspace and both reach turn 1, because
    /// `next_turn_ordinal` counts per session. A workspace-keyed ref made the
    /// second `update-ref` silently overwrite the first, orphaning its commit
    /// and leaving the first session's turn row pointing at the other
    /// session's tree.
    #[tokio::test]
    async fn sibling_sessions_do_not_share_a_turn_one_checkpoint() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "siblings");
        let workspace = ws();
        let first_session = sess();
        let second_session = sess();

        std::fs::write(tree.join("first.txt"), "first\n").unwrap();
        let first = record_checkpoint(
            &tree,
            workspace,
            first_session,
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();
        std::fs::write(tree.join("second.txt"), "second\n").unwrap();
        let second = record_checkpoint(
            &tree,
            workspace,
            second_session,
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap();

        assert_ne!(
            first.checkpoint_ref, second.checkpoint_ref,
            "sibling sessions must not share a ref"
        );
        let listed = list_checkpoint_refs(&repo).await.unwrap();
        assert_eq!(listed.len(), 2, "both checkpoints survive: {listed:?}");

        // The first session's ref still resolves, and to its own snapshot:
        // `first.txt` was there, `second.txt` had not been written yet.
        let first_tree = git_text(
            &tree,
            &["ls-tree", "--name-only", &first.checkpoint_ref],
            GIT_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(first_tree.contains("first.txt"), "{first_tree}");
        assert!(!first_tree.contains("second.txt"), "{first_tree}");

        // Archiving the workspace still reaps both, since the workspace stays
        // first in the ref path.
        let removed = delete_workspace_refs(&repo, workspace).await.unwrap();
        assert_eq!(removed, 2);
    }

    #[tokio::test]
    async fn reusable_index_lock_failure_runs_one_cold_fallback() {
        let dir = TempDir::new().unwrap();
        let reusable = dir.path().join("tidebreak-checkpoint-index");
        std::fs::write(&reusable, b"warm").unwrap();
        let error = GitCommandError::Failed(format!(
            "fatal: Unable to create '{}.lock': File exists.",
            reusable.display()
        ));
        let git = ScriptedSnapshotGit::new(
            reusable.clone(),
            vec![
                SnapshotStep::Return(Err(error)),
                SnapshotStep::Return(Ok("cold-tree".into())),
            ],
        );

        let tree = snapshot_tree_with_git(dir.path(), Duration::from_secs(1), &git)
            .await
            .unwrap();
        let calls = git.calls();

        assert_eq!(tree, "cold-tree");
        assert_eq!(calls.len(), 2, "one warm and one cold attempt");
        assert_eq!(calls[0].index_path, reusable);
        assert!(!calls[0].reset_from_head);
        assert_ne!(calls[1].index_path, calls[0].index_path);
        assert!(calls[1].reset_from_head);
    }

    #[tokio::test]
    async fn reusable_index_corruption_removes_it_and_runs_one_cold_fallback() {
        let dir = TempDir::new().unwrap();
        let reusable = dir.path().join("tidebreak-checkpoint-index");
        std::fs::write(&reusable, b"corrupt").unwrap();
        let git = ScriptedSnapshotGit::new(
            reusable.clone(),
            vec![
                SnapshotStep::Return(Err(GitCommandError::Failed(
                    "fatal: index file corrupt".into(),
                ))),
                SnapshotStep::Return(Ok("cold-tree".into())),
            ],
        );

        let tree = snapshot_tree_with_git(dir.path(), Duration::from_secs(1), &git)
            .await
            .unwrap();

        assert_eq!(tree, "cold-tree");
        assert_eq!(git.calls().len(), 2, "one warm and one cold attempt");
        assert!(
            !reusable.exists(),
            "the corrupt reusable index is discarded"
        );
    }

    #[tokio::test]
    async fn reusable_index_timeout_returns_without_a_cold_fallback() {
        let dir = TempDir::new().unwrap();
        let reusable = dir.path().join("tidebreak-checkpoint-index");
        std::fs::write(&reusable, b"warm").unwrap();
        let git = ScriptedSnapshotGit::new(
            reusable,
            vec![SnapshotStep::Return(Err(GitCommandError::timed_out(
                "add -A",
            )))],
        );

        let error = snapshot_tree_with_git(dir.path(), Duration::from_secs(1), &git)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("timed out"), "{error}");
        assert_eq!(
            git.calls().len(),
            1,
            "timeout must not start a cold attempt"
        );
    }

    #[tokio::test]
    async fn cancelling_a_reusable_index_attempt_never_starts_cold_fallback() {
        let dir = TempDir::new().unwrap();
        let reusable = dir.path().join("tidebreak-checkpoint-index");
        std::fs::write(&reusable, b"warm").unwrap();
        let git = Arc::new(ScriptedSnapshotGit::new(
            reusable,
            vec![SnapshotStep::Pending],
        ));
        let started = git.started.notified();
        let task_git = Arc::clone(&git);
        let worktree = dir.path().to_path_buf();
        let task = tokio::spawn(async move {
            snapshot_tree_with_git(&worktree, Duration::from_secs(30), task_git.as_ref()).await
        });
        started.await;

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::task::yield_now().await;
        assert_eq!(
            git.calls().len(),
            1,
            "cancellation must not start a cold attempt"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cold_fallback_consumes_only_the_outer_deadlines_remaining_time() {
        let dir = TempDir::new().unwrap();
        let reusable = dir.path().join("tidebreak-checkpoint-index");
        std::fs::write(&reusable, b"warm").unwrap();
        let error = GitCommandError::Failed(format!(
            "fatal: Unable to create '{}.lock': File exists.",
            reusable.display()
        ));
        let git = ScriptedSnapshotGit::new(
            reusable,
            vec![
                SnapshotStep::After(Duration::from_secs(40), Err(error)),
                SnapshotStep::Pending,
            ],
        );
        let started_at = Instant::now();

        let error = snapshot_tree_with_git(dir.path(), Duration::from_secs(120), &git)
            .await
            .unwrap_err();
        let calls = git.calls();

        assert!(error.to_string().contains("timed out"), "{error}");
        assert_eq!(Instant::now() - started_at, Duration::from_secs(120));
        assert_eq!(calls.len(), 2, "the fallback starts once");
        assert_eq!(calls[0].deadline, calls[1].deadline);
        assert_eq!(calls[1].remaining, Duration::from_secs(80));
    }

    /// The reused index keeps git's stat cache, which is only safe if it
    /// still yields the tree a cold index would. Check that after every kind
    /// of mutation, including one that moves HEAD underneath it.
    #[tokio::test]
    async fn reused_index_matches_a_cold_snapshot_through_every_mutation() {
        let (_dir, repo) = init_repo();
        std::fs::write(repo.join("tracked-then-ignored.txt"), "still tracked\n").unwrap();
        run(&repo, &["git", "add", "tracked-then-ignored.txt"]);
        run(&repo, &["git", "commit", "-m", "add tracked file"]);
        std::fs::write(repo.join(".gitignore"), "tracked-then-ignored.txt\n").unwrap();
        run(&repo, &["git", "add", ".gitignore"]);
        run(&repo, &["git", "commit", "-m", "ignore tracked file"]);
        let tree = add_worktree(&repo, "reuse");

        async fn cold(worktree: &Path) -> String {
            let temp = TempDir::new().unwrap();
            let index = temp.path().join("cold-index");
            snapshot_tree_with_index(worktree, &index, true)
                .await
                .unwrap()
        }

        type Mutation = (&'static str, Box<dyn Fn(&Path)>);
        let mutate: Vec<Mutation> = vec![
            (
                "add a file",
                Box::new(|t: &Path| std::fs::write(t.join("a.txt"), "one\n").unwrap()),
            ),
            (
                "add a second file",
                Box::new(|t: &Path| std::fs::write(t.join("b.txt"), "two\n").unwrap()),
            ),
            (
                "modify a tracked file",
                Box::new(|t: &Path| std::fs::write(t.join("a.txt"), "one changed\n").unwrap()),
            ),
            (
                "delete a file",
                Box::new(|t: &Path| std::fs::remove_file(t.join("b.txt")).unwrap()),
            ),
            (
                "move HEAD out from under the index",
                Box::new(|t: &Path| {
                    std::fs::write(t.join("c.txt"), "three\n").unwrap();
                    run(t, &["git", "add", "c.txt"]);
                    run(t, &["git", "commit", "-m", "commit c"]);
                }),
            ),
        ];

        for (label, apply) in mutate {
            apply(&tree);
            let warm = snapshot_tree(&tree).await.unwrap();
            let cold = cold(&tree).await;
            assert_eq!(warm, cold, "reused index diverged after: {label}");
        }

        // The reusable index lives in the worktree's git dir and is not the
        // user's index.
        let path = checkpoint_index_path(&tree).await.unwrap();
        assert!(path.is_absolute(), "{path:?}");
        assert!(
            path.to_string_lossy()
                .contains("tidebreak-checkpoint-index"),
            "{path:?}"
        );
        let user_index = git_text(&tree, &["rev-parse", "--git-path", "index"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert_ne!(path.to_string_lossy().trim(), user_index.trim());
    }

    /// A database, bus, and session bound to a real worktree.
    ///
    /// [`after_turn_ended`] reads the workspace row to find the worktree, so
    /// a checkpoint test needs the rows, not just the git tree.
    async fn seed_session(
        repo: &Path,
        worktree: &Path,
    ) -> (Arc<DbStore>, Arc<CodeEventBus>, CodeSession) {
        let db = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                repo.parent().unwrap().join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let owner = OwnerId::local();
        let repo_id = RepoId::new();
        insert_repo(
            &db,
            &CodeRepo {
                id: repo_id,
                owner: owner.clone(),
                root_path: repo.display().to_string(),
                display_name: "example".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: chrono::Utc::now(),
                removed_at: None,
                cloned_from: None,
                origin_host: None,
                origin_owner: None,
                origin_name: None,
            },
        )
        .await
        .unwrap();
        let workspace_id = WorkspaceId::new();
        insert_workspace(
            &db,
            &CodeWorkspace {
                id: workspace_id,
                owner: owner.clone(),
                repo_id,
                title: "first".into(),
                worktree_path: worktree.display().to_string(),
                branch_name: "tidebreak/first".into(),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: chrono::Utc::now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
        )
        .await
        .unwrap();
        let session = CodeSession {
            id: CodeSessionId::new(),
            owner,
            workspace_id,
            kind: CodeSessionKind::Interactive,
            harness_kind: HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Running,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: chrono::Utc::now(),
        };
        insert_session(&db, &session).await.unwrap();
        (db, Arc::new(CodeEventBus::default()), session)
    }

    async fn seed_turn(
        db: &Arc<DbStore>,
        session: &CodeSession,
        ordinal: i64,
        status: CodeTurnStatus,
    ) -> CodeTurn {
        let turn = CodeTurn {
            id: CodeTurnId::new(),
            session_id: session.id,
            ordinal,
            status,
            model: session.model.clone(),
            fast_mode: session.fast_mode,
            user_input: "edit the tree".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
        };
        insert_turn(db, &session.owner, &turn).await.unwrap();
        turn
    }

    async fn recorded_events(db: &Arc<DbStore>, session: &CodeSession) -> Vec<CodeEvent> {
        list_events(db, &session.owner, session.id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events
            .into_iter()
            .map(|framed| framed.event)
            .collect()
    }

    /// An interrupted turn had already rewritten the worktree. Without a ref
    /// of its own those edits fall outside the chain: the turn's diff is
    /// empty and the next turn's checkpoint absorbs them.
    #[tokio::test]
    async fn an_interrupted_turn_records_its_edits() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "interrupted");
        std::fs::write(tree.join("half-done.txt"), "written before the stop\n").unwrap();
        let (db, bus, session) = seed_session(&repo, &tree).await;
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Interrupted).await;

        after_turn_ended(&db, &bus, &session, &mut turn).await;

        let r#ref = turn
            .checkpoint_ref
            .clone()
            .expect("an interrupted turn must own a checkpoint");
        let listed = git_text(&tree, &["ls-tree", "--name-only", &r#ref], GIT_TIMEOUT)
            .await
            .unwrap();
        assert!(listed.contains("half-done.txt"), "{listed}");
        assert_eq!(turn.diffstat.as_ref().unwrap().files, 1);

        let stored = get_turn(&db, &session.owner, turn.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.checkpoint_ref.as_deref(), Some(r#ref.as_str()));
        assert!(
            recorded_events(&db, &session)
                .await
                .iter()
                .any(|event| matches!(event, CodeEvent::CheckpointRecorded { .. })),
            "an interrupted turn must journal its checkpoint"
        );

        // The hidden ref is the only record a human has of this turn.
        let subject = git_text(&tree, &["log", "-1", "--format=%s", &r#ref], GIT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(subject, "checkpoint turn 1 (interrupted)");
    }

    #[tokio::test]
    async fn a_failed_turn_records_its_edits() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "failed");
        std::fs::write(tree.join("README.md"), "rewritten before the failure\n").unwrap();
        let (db, bus, session) = seed_session(&repo, &tree).await;
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Failed).await;

        after_turn_ended(&db, &bus, &session, &mut turn).await;

        let r#ref = turn
            .checkpoint_ref
            .clone()
            .expect("a failed turn must own a checkpoint");
        let diff = produce_diff(
            &tree,
            &merge_base(&tree, "main").await.unwrap(),
            &r#ref,
            None,
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert!(
            diff.diff.contains("rewritten before the failure"),
            "{}",
            diff.diff
        );
        assert!(
            recorded_events(&db, &session)
                .await
                .iter()
                .any(|event| matches!(event, CodeEvent::CheckpointRecorded { .. })),
            "a failed turn must journal its checkpoint"
        );
    }

    /// A running turn is still moving the worktree, so it gets nothing.
    #[tokio::test]
    async fn a_running_turn_is_not_checkpointed() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "running");
        std::fs::write(tree.join("in-flight.txt"), "mid-edit\n").unwrap();
        let (db, bus, session) = seed_session(&repo, &tree).await;
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Running).await;

        after_turn_ended(&db, &bus, &session, &mut turn).await;

        assert!(turn.checkpoint_ref.is_none());
        assert!(turn.diffstat.is_none());
        assert!(
            list_checkpoint_refs(&repo).await.unwrap().is_empty(),
            "a running turn must not write a ref"
        );
        assert!(recorded_events(&db, &session).await.is_empty());
    }

    /// The failure posture is unchanged on the new paths: a checkpoint that
    /// cannot be taken warns and leaves the turn as it was.
    #[tokio::test]
    async fn a_failed_checkpoint_keeps_a_failed_turn() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "unrecoverable");
        let (db, bus, session) = seed_session(&repo, &tree).await;
        // Replace the checkout with a plain directory so the snapshot fails.
        std::fs::remove_dir_all(&tree).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("orphan.txt"), "still here\n").unwrap();
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Failed).await;

        after_turn_ended(&db, &bus, &session, &mut turn).await;

        assert!(turn.checkpoint_ref.is_none());
        assert_eq!(turn.status, CodeTurnStatus::Failed);
        assert!(
            recorded_events(&db, &session)
                .await
                .iter()
                .any(|event| matches!(
                    event,
                    CodeEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Warning,
                        ..
                    }
                )),
            "a checkpoint failure must warn instead of failing the turn"
        );
    }

    /// A failed turn owning a ref must not break the ordinal chain: turn 2
    /// parents off turn 1 and its diff covers only its own edits.
    #[tokio::test]
    async fn a_later_turn_chains_off_a_failed_turns_checkpoint() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "chain");
        let (db, bus, session) = seed_session(&repo, &tree).await;

        std::fs::write(tree.join("from-failed.txt"), "one\n").unwrap();
        let mut failed = seed_turn(&db, &session, 1, CodeTurnStatus::Failed).await;
        after_turn_ended(&db, &bus, &session, &mut failed).await;
        let first = failed.checkpoint_ref.clone().unwrap();

        std::fs::write(tree.join("from-completed.txt"), "two\n").unwrap();
        let mut completed = seed_turn(&db, &session, 2, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut completed).await;
        let second = completed.checkpoint_ref.clone().unwrap();

        let parent = git_text(&tree, &["rev-parse", &format!("{second}^")], GIT_TIMEOUT)
            .await
            .unwrap();
        let first_oid = git_text(&tree, &["rev-parse", &first], GIT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(parent, first_oid, "turn 2 must parent off turn 1");

        assert_eq!(completed.diffstat.as_ref().unwrap().files, 1);
        let diff = produce_diff(&tree, &first, &second, None, DiffBounds::default())
            .await
            .unwrap();
        assert!(diff.diff.contains("from-completed.txt"), "{}", diff.diff);
        assert!(
            !diff.diff.contains("from-failed.txt"),
            "the failed turn's edits belong to the failed turn: {}",
            diff.diff
        );
    }

    /// A second session over the same worktree, the shape record 55 allows.
    async fn seed_sibling_session(db: &Arc<DbStore>, sibling: &CodeSession) -> CodeSession {
        let session = CodeSession {
            id: CodeSessionId::new(),
            harness_kind: HarnessKind::Codex,
            created_at: chrono::Utc::now(),
            ..sibling.clone()
        };
        insert_session(db, &session).await.unwrap();
        session
    }

    async fn oid_of(worktree: &Path, revision: &str) -> String {
        git_text(worktree, &["rev-parse", revision], GIT_TIMEOUT)
            .await
            .unwrap()
    }

    /// A workspace holds several sessions over one worktree (record 55), so a
    /// session's first turn cannot mean "the worktree against the base
    /// branch": that credits this session with whatever a sibling already
    /// changed. Measured before the baseline existed — a session that edited
    /// one file reported `{files: 2, insertions: 11, deletions: 1}`.
    #[tokio::test]
    async fn a_siblings_edits_stay_out_of_a_later_sessions_first_turn() {
        let (_dir, repo) = init_repo();
        std::fs::create_dir_all(repo.join("receipts")).unwrap();
        std::fs::write(repo.join("receipts/__init__.py"), "").unwrap();
        std::fs::write(
            repo.join("receipts/parser.py"),
            "def parse(line):\n    return line\n",
        )
        .unwrap();
        run(&repo, &["git", "add", "receipts"]);
        run(&repo, &["git", "commit", "-m", "receipts"]);
        let tree = add_worktree(&repo, "shared");
        let (db, bus, earlier) = seed_session(&repo, &tree).await;
        record_session_baseline(&tree, earlier.workspace_id, earlier.id)
            .await
            .unwrap();

        // The session already in the workspace takes a turn: one line.
        std::fs::write(
            tree.join("receipts/__init__.py"),
            "from .parser import parse\n",
        )
        .unwrap();
        let mut theirs = seed_turn(&db, &earlier, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &earlier, &mut theirs).await;

        // A second session starts on the worktree the first one has edited.
        let later = seed_sibling_session(&db, &earlier).await;
        record_session_baseline(&tree, later.workspace_id, later.id)
            .await
            .unwrap();

        // Its first turn rewrites one file: ten lines in, one out.
        let body: String = (1..=10).map(|i| format!("    step {i}\n")).collect();
        std::fs::write(
            tree.join("receipts/parser.py"),
            format!("def parse(line):\n{body}"),
        )
        .unwrap();
        let mut mine = seed_turn(&db, &later, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &later, &mut mine).await;

        let stat = mine.diffstat.clone().expect("turn 1 records a diffstat");
        assert_eq!(
            (stat.files, stat.insertions, stat.deletions),
            (1, 10, 1),
            "turn 1 must count only this session's edit: {stat:?}"
        );

        // The read path agrees: `code diff --turn` resolves the same range.
        let workspace = get_workspace(&db, &later.owner, later.workspace_id)
            .await
            .unwrap()
            .unwrap();
        let (_, from, to, _) = resolve_diff_range(&db, &workspace, Some(mine.id))
            .await
            .unwrap();
        let diff = produce_diff(&tree, &from, &to, None, DiffBounds::default())
            .await
            .unwrap();
        assert!(diff.diff.contains("receipts/parser.py"), "{}", diff.diff);
        assert!(
            !diff.diff.contains("__init__.py"),
            "the sibling's file belongs to the sibling: {}",
            diff.diff
        );

        // The sibling keeps its own turn, unchanged.
        let theirs = theirs.diffstat.clone().unwrap();
        assert_eq!((theirs.files, theirs.insertions), (1, 1), "{theirs:?}");
    }

    /// The only session in a fresh workspace is unchanged: its baseline and
    /// the merge base are the same tree, so turn 1 still reports everything
    /// between the base branch and the worktree.
    #[tokio::test]
    async fn a_lone_sessions_first_turn_still_covers_the_workspace() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "lone");
        let (db, bus, session) = seed_session(&repo, &tree).await;
        record_session_baseline(&tree, session.workspace_id, session.id)
            .await
            .unwrap();

        std::fs::write(tree.join("README.md"), "hello world\n").unwrap();
        std::fs::write(tree.join("new.txt"), "untracked\n").unwrap();
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut turn).await;

        let stat = turn.diffstat.clone().unwrap();
        let against_base = list_changed_files(
            &tree,
            &merge_base(&tree, "main").await.unwrap(),
            turn.checkpoint_ref.as_ref().unwrap(),
            DiffBounds::default(),
        )
        .await
        .unwrap();
        assert_eq!(stat, against_base.stat, "{stat:?}");
        assert_eq!(stat.files, 2);
    }

    /// A session created before baselines were recorded has none. Turn 1
    /// falls back to the base ref rather than failing.
    #[tokio::test]
    async fn a_session_without_a_baseline_falls_back_to_the_base_ref() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "no-baseline");
        let (db, bus, session) = seed_session(&repo, &tree).await;

        std::fs::write(tree.join("a.txt"), "one\n").unwrap();
        let mut turn = seed_turn(&db, &session, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut turn).await;

        let r#ref = turn.checkpoint_ref.clone().expect("turn 1 keeps its ref");
        let baseline = session_baseline_ref(session.workspace_id, session.id);
        assert!(
            git_text(&tree, &["rev-parse", "--verify", &baseline], GIT_TIMEOUT)
                .await
                .is_err(),
            "this session has no baseline"
        );
        assert_eq!(turn.diffstat.clone().unwrap().files, 1);
        assert_eq!(
            oid_of(&tree, &format!("{ref}^")).await,
            oid_of(&tree, "HEAD").await,
            "with no baseline the checkpoint parents off HEAD"
        );
    }

    /// Turns after the first are untouched: each one chains off the previous
    /// turn's checkpoint, which itself chains off the baseline.
    #[tokio::test]
    async fn later_turns_chain_off_the_previous_checkpoint() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "chain-from-baseline");
        let (db, bus, session) = seed_session(&repo, &tree).await;
        let baseline = record_session_baseline(&tree, session.workspace_id, session.id)
            .await
            .unwrap();

        std::fs::write(tree.join("one.txt"), "one\n").unwrap();
        let mut first = seed_turn(&db, &session, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut first).await;
        let first_ref = first.checkpoint_ref.clone().unwrap();

        std::fs::write(tree.join("two.txt"), "two\n").unwrap();
        let mut second = seed_turn(&db, &session, 2, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut second).await;
        let second_ref = second.checkpoint_ref.clone().unwrap();

        assert_eq!(second.diffstat.clone().unwrap().files, 1);
        let diff = produce_diff(&tree, &first_ref, &second_ref, None, DiffBounds::default())
            .await
            .unwrap();
        assert!(diff.diff.contains("two.txt"), "{}", diff.diff);
        assert!(!diff.diff.contains("one.txt"), "{}", diff.diff);

        assert_eq!(
            oid_of(&tree, &format!("{second_ref}^")).await,
            oid_of(&tree, &first_ref).await,
            "turn 2 parents off turn 1"
        );
        assert_eq!(
            oid_of(&tree, &format!("{first_ref}^")).await,
            oid_of(&tree, &baseline).await,
            "turn 1 parents off the baseline"
        );
    }

    /// A missing hidden ref must not poison every later turn. The database row
    /// can outlive the ref after cleanup, so the next checkpoint resumes from
    /// the session baseline and includes the edits that lost their checkpoint.
    #[tokio::test]
    async fn a_missing_previous_checkpoint_falls_back_to_the_session_baseline() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "missing-previous");
        let (db, bus, session) = seed_session(&repo, &tree).await;
        let baseline = record_session_baseline(&tree, session.workspace_id, session.id)
            .await
            .unwrap();

        std::fs::write(tree.join("one.txt"), "one\n").unwrap();
        let mut first = seed_turn(&db, &session, 1, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut first).await;
        let first_ref = first.checkpoint_ref.clone().unwrap();
        delete_ref(&repo, &first_ref).await.unwrap();

        std::fs::write(tree.join("two.txt"), "two\n").unwrap();
        let mut second = seed_turn(&db, &session, 2, CodeTurnStatus::Completed).await;
        after_turn_ended(&db, &bus, &session, &mut second).await;
        let second_ref = second
            .checkpoint_ref
            .clone()
            .expect("turn 2 still records a checkpoint");

        assert_eq!(
            oid_of(&tree, &format!("{second_ref}^")).await,
            oid_of(&tree, &baseline).await,
            "the surviving baseline restarts the checkpoint chain"
        );
        assert_eq!(
            second.diffstat.as_ref().map(|stat| stat.files),
            Some(2),
            "the recovered checkpoint includes both turns' live edits"
        );
        assert!(
            recorded_events(&db, &session)
                .await
                .iter()
                .all(|event| !matches!(
                    event,
                    CodeEvent::HarnessNotice {
                        level: HarnessNoticeLevel::Warning,
                        ..
                    }
                )),
            "a stale row must not produce another checkpoint warning"
        );
    }

    #[tokio::test]
    async fn snapshot_failure_does_not_write_a_ref() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&missing).unwrap();
        let err = record_checkpoint(
            &missing,
            ws(),
            sess(),
            1,
            CodeTurnStatus::Completed,
            None,
            "main",
        )
        .await
        .unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

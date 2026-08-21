//! Per-turn worktree checkpoints via a temporary git index.
//!
//! A completed turn records the worktree's full state — tracked changes and
//! untracked files — as a synthetic commit created through a temporary index
//! file. The user's index, `HEAD`, and reflog are untouched. The commit is
//! referenced only by a hidden ref
//! `refs/tidebreak/checkpoints/<workspace>/<session>/<ordinal>`.
//!
//! The session segment is load-bearing. A workspace holds several sessions
//! (decision 0055) and `next_turn_ordinal` counts per session, so every
//! session reaches turn 1; a workspace-keyed ref let one sibling's snapshot
//! overwrite another's.
//!
//! Diffs are produced here, bounded in bytes and file count, with truncation
//! marked on the payload. The renderer never runs git.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;
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

/// Default bound on a unified-diff body.
pub(crate) const MAX_DIFF_BYTES: usize = 256 * 1024;
/// Default bound on how many files a files/diff payload includes in full.
pub(crate) const MAX_DIFF_FILES: usize = 64;

const REF_PREFIX: &str = "refs/tidebreak/checkpoints";

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

/// One file in a bounded workspace or turn file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub path: String,
    pub kind: FileChangeKind,
    pub insertions: u32,
    pub deletions: u32,
    pub previous_path: Option<String>,
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

/// Hidden ref for one turn of one session.
///
/// Keyed on the session as well as the workspace. Ordinals come from
/// `next_turn_ordinal`, which counts per session, so two sessions sharing a
/// workspace both reach turn 1. The workspace stays first in the path so
/// [`delete_workspace_refs`] and [`sweep_orphaned_refs`] keep matching by
/// prefix.
pub(crate) fn checkpoint_ref(
    workspace_id: WorkspaceId,
    session_id: CodeSessionId,
    ordinal: i64,
) -> String {
    format!("{REF_PREFIX}/{workspace_id}/{session_id}/{ordinal}")
}

/// After a turn reaches `Completed`, snapshot the worktree and journal.
///
/// Checkpoint failure does not fail the turn: a [`CodeEvent::HarnessNotice`]
/// is journaled and the already-recorded work stands.
pub(crate) async fn after_turn_completed(
    db: &Arc<DbStore>,
    bus: &Arc<CodeEventBus>,
    session: &CodeSession,
    turn: &mut CodeTurn,
) {
    if turn.status != CodeTurnStatus::Completed || turn.checkpoint_ref.is_some() {
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
        previous.as_deref(),
        &workspace.base_ref,
    )
    .await
}

/// Snapshot the worktree into a hidden checkpoint ref.
///
/// Uses a temporary index file. The user's index and `HEAD` are not written.
pub(crate) async fn record_checkpoint(
    worktree: &Path,
    workspace_id: WorkspaceId,
    session_id: CodeSessionId,
    ordinal: i64,
    previous_oid: Option<&str>,
    base_ref: &str,
) -> Result<RecordedCheckpoint, CheckpointError> {
    let r#ref = checkpoint_ref(workspace_id, session_id, ordinal);
    let tree = snapshot_tree(worktree).await?;
    let parent = match previous_oid {
        Some(oid) => oid.to_owned(),
        None => git_text(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?,
    };
    let message = format!("checkpoint turn {ordinal}");
    let commit = git_text(
        worktree,
        &["commit-tree", &tree, "-p", &parent, "-m", &message],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    git_text(
        worktree,
        &["update-ref", "--no-deref", &r#ref, &commit],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;

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
    let listed = collect_changes(worktree, from, to, bounds).await?;
    if let Some(path) = file {
        let raw = git_bytes(
            worktree,
            &["diff", "--find-renames", from, to, "--", path],
            GIT_SNAPSHOT_TIMEOUT,
        )
        .await
        .map_err(CheckpointError::internal)?;
        let (diff, body_truncated) = truncate_bytes(&raw, bounds.max_bytes);
        let file_stat = listed
            .files
            .iter()
            .find(|entry| entry.path == path || entry.previous_path.as_deref() == Some(path));
        let stat = Diffstat {
            files: u32::from(file_stat.is_some() || !diff.is_empty()),
            insertions: file_stat.map(|entry| entry.insertions).unwrap_or(0),
            deletions: file_stat.map(|entry| entry.deletions).unwrap_or(0),
            truncated: body_truncated,
        };
        return Ok(BoundedDiff {
            diff,
            truncated: body_truncated,
            stat,
        });
    }

    let mut body = String::new();
    let mut truncated = listed.truncated;
    for (included, entry) in listed.files.iter().enumerate() {
        if included >= bounds.max_files {
            truncated = true;
            break;
        }
        let remaining = bounds.max_bytes.saturating_sub(body.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let raw = git_bytes(
            worktree,
            &["diff", "--find-renames", from, to, "--", &entry.path],
            GIT_SNAPSHOT_TIMEOUT,
        )
        .await
        .map_err(CheckpointError::internal)?;
        let (chunk, chunk_truncated) = truncate_bytes(&raw, remaining);
        if !body.is_empty() && !chunk.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&chunk);
        if chunk_truncated {
            truncated = true;
            break;
        }
    }
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
    let temp = tempfile::NamedTempFile::new().map_err(|err| {
        CheckpointError::internal(format!("could not create temporary index: {err}"))
    })?;
    let index_path = temp.path().to_path_buf();
    // `git read-tree` wants to create the index; an empty file can confuse it.
    drop(temp);
    let _ = tokio::fs::remove_file(&index_path).await;

    let result = snapshot_tree_with_index(worktree, &index_path).await;
    let _ = tokio::fs::remove_file(&index_path).await;
    result
}

async fn snapshot_tree_with_index(
    worktree: &Path,
    index_path: &Path,
) -> Result<String, CheckpointError> {
    let index = index_path.to_string_lossy();
    git_text_env(
        worktree,
        &["read-tree", "HEAD"],
        &[("GIT_INDEX_FILE", index.as_ref())],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    git_text_env(
        worktree,
        &["add", "-A"],
        &[("GIT_INDEX_FILE", index.as_ref())],
        GIT_SNAPSHOT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    git_text_env(
        worktree,
        &["write-tree"],
        &[("GIT_INDEX_FILE", index.as_ref())],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)
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

/// Delete every checkpoint ref belonging to one workspace.
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

/// Drop checkpoint refs whose workspace is no longer live (crash / leftover).
pub(crate) async fn sweep_orphaned_refs(
    repo_root: &Path,
    live_workspace_ids: &[WorkspaceId],
) -> Result<usize, CheckpointError> {
    let live: std::collections::HashSet<String> =
        live_workspace_ids.iter().map(ToString::to_string).collect();
    let refs = list_checkpoint_refs(repo_root).await?;
    let mut removed = 0usize;
    for r#ref in refs {
        let Some(workspace) = workspace_id_from_ref(&r#ref) else {
            continue;
        };
        if !live.contains(&workspace) {
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
            let from = merge_base(&worktree, &workspace.base_ref).await?;
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
                .unwrap_or(merge_base(&worktree, &workspace.base_ref).await?);
            Ok((worktree, from, to, Some(id)))
        }
    }
}

async fn previous_checkpoint_oid(
    worktree: &Path,
    workspace: &CodeWorkspace,
    db: &DbStore,
    turn: &CodeTurn,
) -> Result<Option<String>, CheckpointError> {
    if turn.ordinal <= 1 {
        return Ok(None);
    }
    let previous_ref = checkpoint_ref(workspace.id, turn.session_id, turn.ordinal - 1);
    if let Ok(oid) = git_text(
        worktree,
        &["rev-parse", "--verify", &previous_ref],
        GIT_TIMEOUT,
    )
    .await
    {
        if !oid.is_empty() {
            return Ok(Some(oid));
        }
    }
    let turns = list_turns(db, &workspace.owner, turn.session_id)
        .await
        .map_err(|err| CheckpointError::internal(err.to_string()))?;
    Ok(turns
        .into_iter()
        .rev()
        .find(|candidate| candidate.ordinal < turn.ordinal && candidate.checkpoint_ref.is_some())
        .and_then(|candidate| candidate.checkpoint_ref))
}

async fn collect_changes(
    worktree: &Path,
    from: &str,
    to: &str,
    bounds: DiffBounds,
) -> Result<BoundedFiles, CheckpointError> {
    let name_status = git_bytes(
        worktree,
        &["diff", "--name-status", "-z", "--find-renames", from, to],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    let numstat = git_bytes(
        worktree,
        &["diff", "--numstat", "-z", "--find-renames", from, to],
        GIT_TIMEOUT,
    )
    .await
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
    let text = String::from_utf8_lossy(raw);
    let mut parts = text.split('\0').filter(|part| !part.is_empty());
    let mut files = Vec::new();
    while let Some(status) = parts.next() {
        let code = status.chars().next().unwrap_or('M');
        match code {
            'R' | 'C' => {
                let previous = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                if path.is_empty() {
                    continue;
                }
                files.push(ChangedFile {
                    path,
                    kind: if code == 'R' {
                        FileChangeKind::Renamed
                    } else {
                        FileChangeKind::Modified
                    },
                    insertions: 0,
                    deletions: 0,
                    previous_path: Some(previous).filter(|value| !value.is_empty()),
                });
            }
            other => {
                let path = parts.next().unwrap_or("").to_owned();
                if path.is_empty() {
                    continue;
                }
                let kind = match other {
                    'A' => FileChangeKind::Added,
                    'D' => FileChangeKind::Deleted,
                    _ => FileChangeKind::Modified,
                };
                files.push(ChangedFile {
                    path,
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
fn parse_numstat(raw: &[u8]) -> std::collections::HashMap<String, (u32, u32)> {
    let text = String::from_utf8_lossy(raw);
    let mut out = std::collections::HashMap::new();
    let mut parts = text.split('\0').filter(|part| !part.is_empty());
    while let Some(record) = parts.next() {
        // A path may itself contain a tab, so only split off the two counts.
        let mut cols = record.splitn(3, '\t');
        let insertions = parse_stat_count(cols.next().unwrap_or("0"));
        let deletions = parse_stat_count(cols.next().unwrap_or("0"));
        let path = cols.next().unwrap_or("");
        if path.is_empty() {
            let (Some(_previous), Some(current)) = (parts.next(), parts.next()) else {
                break;
            };
            out.insert(current.to_owned(), (insertions, deletions));
        } else {
            out.insert(path.to_owned(), (insertions, deletions));
        }
    }
    out
}

fn parse_stat_count(value: &str) -> u32 {
    if value == "-" {
        0
    } else {
        value.parse().unwrap_or(0)
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

fn workspace_id_from_ref(r#ref: &str) -> Option<String> {
    let rest = r#ref.strip_prefix(&format!("{REF_PREFIX}/"))?;
    let (workspace, _ordinal) = rest.split_once('/')?;
    if workspace.is_empty() {
        None
    } else {
        Some(workspace.to_owned())
    }
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

async fn git_bytes(cwd: &Path, args: &[&str], limit: Duration) -> Result<Vec<u8>, String> {
    git_bytes_env(cwd, args, &[], limit).await
}

async fn git_bytes_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .args(args)
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
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|err| format!("git {} failed: {err}", args.join(" ")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
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
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

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
        let recorded = record_checkpoint(&tree, ws(), sess(), 1, None, "main")
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
        let paths: Vec<_> = files.files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"new.txt"), "{paths:?}");
        assert!(
            files
                .files
                .iter()
                .any(|file| file.kind == FileChangeKind::Renamed
                    && (file.path == "kept.txt"
                        || file.previous_path.as_deref() == Some("keep.txt"))),
            "{:#?}",
            files.files
        );
        assert!(
            files
                .files
                .iter()
                .any(|file| file.path == "README.md" && file.kind == FileChangeKind::Modified),
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

        let recorded = record_checkpoint(&tree, ws(), sess(), 1, None, "main")
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
            .find(|file| file.path == "src/beta.rs")
            .unwrap_or_else(|| panic!("{:#?}", files.files));
        assert_eq!(renamed.kind, FileChangeKind::Renamed);
        assert_eq!(renamed.previous_path.as_deref(), Some("src/alpha.rs"));
        assert_eq!((renamed.insertions, renamed.deletions), (1, 0));

        // Matched by kind rather than by name: macOS and Linux disagree on the
        // Unicode normalization of the path, but not on its line counts.
        let accented = files
            .files
            .iter()
            .find(|file| file.kind == FileChangeKind::Added)
            .unwrap_or_else(|| panic!("{:#?}", files.files));
        assert!(accented.path.contains("caf"), "{}", accented.path);
        assert!(
            !accented.path.starts_with('"'),
            "path must not be quoted: {}",
            accented.path
        );
        assert_eq!((accented.insertions, accented.deletions), (2, 0));

        assert_eq!(files.stat.insertions, 3);
        assert_eq!(files.stat.deletions, 0);
    }

    #[tokio::test]
    async fn turn_diff_is_the_range_between_checkpoints() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "turns");
        let id = ws();
        let session = sess();
        std::fs::write(tree.join("a.txt"), "one\n").unwrap();
        let first = record_checkpoint(&tree, id, session, 1, None, "main")
            .await
            .unwrap();
        std::fs::write(tree.join("b.txt"), "two\n").unwrap();
        let first_oid = git_text(&tree, &["rev-parse", &first.checkpoint_ref], GIT_TIMEOUT)
            .await
            .unwrap();
        let second = record_checkpoint(&tree, id, session, 2, Some(&first_oid), "main")
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
        let recorded = record_checkpoint(&tree, ws(), sess(), 1, None, "main")
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
        assert!(diff.diff.len() <= 80, "{}", diff.diff.len());
    }

    #[tokio::test]
    async fn archive_removes_workspace_refs_and_sweep_drops_orphans() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "gone");
        let live = ws();
        let dead = ws();
        std::fs::write(tree.join("x.txt"), "x\n").unwrap();
        record_checkpoint(&tree, live, sess(), 1, None, "main")
            .await
            .unwrap();
        record_checkpoint(&tree, dead, sess(), 1, None, "main")
            .await
            .unwrap();
        let listed = list_checkpoint_refs(&repo).await.unwrap();
        assert_eq!(listed.len(), 2, "{listed:?}");

        let removed = delete_workspace_refs(&repo, dead).await.unwrap();
        assert_eq!(removed, 1);
        let listed = list_checkpoint_refs(&repo).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].contains(&live.to_string()));

        let swept = sweep_orphaned_refs(&repo, &[]).await.unwrap();
        assert_eq!(swept, 1);
        assert!(list_checkpoint_refs(&repo).await.unwrap().is_empty());
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
        let first = record_checkpoint(&tree, workspace, first_session, 1, None, "main")
            .await
            .unwrap();
        std::fs::write(tree.join("second.txt"), "second\n").unwrap();
        let second = record_checkpoint(&tree, workspace, second_session, 1, None, "main")
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
    async fn snapshot_failure_does_not_write_a_ref() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-a-repo");
        std::fs::create_dir_all(&missing).unwrap();
        let err = record_checkpoint(&missing, ws(), sess(), 1, None, "main")
            .await
            .unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}

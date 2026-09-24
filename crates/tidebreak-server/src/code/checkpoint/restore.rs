//! Put a workspace's worktree back to an earlier checkpoint.
//!
//! A restore makes every file the checkpoint snapshot sees — tracked changes
//! and untracked files alike — match the target checkpoint's tree. `HEAD`, the
//! branch, and the user's index are untouched: the files go back, and whatever
//! the branch committed since then shows as uncommitted changes that undo it.
//!
//! Before any file changes, the state being replaced is committed to a hidden
//! ref of its own, so a restore can itself be undone by restoring that state.
//! A restore never touches what that state does not hold: an ignored or
//! excluded file, a folder holding one, or a nested repository that stands
//! where the restore would write refuses it instead, naming it (see
//! [`super::worktree`]).
//!
//! A restore runs in two steps, so the caller can journal it in between:
//! [`prepare_restore`] checks and saves, and [`PreparedRestore::apply`] moves
//! the files one at a time. When a path cannot move, the paths already moved
//! go back, and the error says whether that was verified.

use std::collections::HashMap;
use std::path::Path;

use tidebreak_core::{CodeRestoreId, SessionId, WorkspaceId};

use super::worktree::{
    apply, inspect, refuse_sparse_checkout, tree_entries, tree_of, tree_of_entries, ApplyFailure,
    Plan, PrivateIndex, TreeEntry,
};
use super::{
    checkpoint_ref, collect_changes, git_text, snapshot_tree, BoundedFiles, CheckpointError,
    DiffBounds, GitPath, BASELINE_ORDINAL, GIT_TIMEOUT, REF_PREFIX,
};

/// Hidden ref that holds the worktree as it stood just before one restore.
///
/// It sits under the session whose transcript records the restore, so an undo
/// knows where to journal without a second record. The workspace stays first
/// in the path, so archive reaps it with every other checkpoint ref.
pub fn restore_point_ref(
    workspace_id: WorkspaceId,
    session_id: SessionId,
    restore_id: CodeRestoreId,
) -> String {
    format!("{REF_PREFIX}/{workspace_id}/{session_id}/restore/{restore_id}")
}

/// Hidden ref that a session's next turn diffs from, when a restore moved the
/// worktree after that session's turn `ordinal` ended.
///
/// Without it, the next turn's diff would start at turn `ordinal`'s own
/// checkpoint and credit the turn with undoing everything the restore undid.
/// The next turn also reads it to tell the engine what moved under it.
pub fn chain_resume_ref(workspace_id: WorkspaceId, session_id: SessionId, ordinal: i64) -> String {
    format!("{REF_PREFIX}/{workspace_id}/{session_id}/after/{ordinal}")
}

/// Find the state saved just before one restore: the session whose transcript
/// records that restore, and the commit that holds the state.
pub async fn find_restore_point(
    worktree: &Path,
    workspace_id: WorkspaceId,
    restore_id: CodeRestoreId,
) -> Result<Option<(SessionId, String)>, CheckpointError> {
    let prefix = format!("{REF_PREFIX}/{workspace_id}/");
    let suffix = format!("/restore/{restore_id}");
    let listed = git_text(
        worktree,
        &["for-each-ref", "--format=%(objectname) %(refname)", &prefix],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    for line in listed.lines() {
        let Some((oid, name)) = line.trim().split_once(' ') else {
            continue;
        };
        let Some(session) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(&suffix))
        else {
            continue;
        };
        if let Ok(session) = session.parse::<SessionId>() {
            return Ok(Some((session, oid.to_owned())));
        }
    }
    Ok(None)
}

/// When a checkpoint commit was written, in seconds since the epoch.
pub async fn commit_time(worktree: &Path, commit: &str) -> Result<i64, CheckpointError> {
    let raw = git_text(
        worktree,
        &["show", "-s", "--format=%ct", commit, "--"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    raw.trim()
        .parse()
        .map_err(|_| CheckpointError::internal(format!("unreadable commit time {raw:?}")))
}

/// What a restore would change, read before anything moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreview {
    /// The live worktree's snapshot tree. A restore that names it refuses to
    /// run over a worktree that changed after this preview.
    pub current_tree: String,
    /// Everything that changed since the target state. The restore undoes all
    /// of it, whoever made it.
    pub files: BoundedFiles,
    /// What the restore would overwrite or remove although no saved state
    /// could bring it back: ignored and excluded files, and nested
    /// repositories. While any are listed, the restore refuses.
    pub blocked: Vec<GitPath>,
}

/// Read what restoring `target` would undo, without changing anything.
pub async fn preview_restore(
    worktree: &Path,
    target: &str,
) -> Result<RestorePreview, CheckpointError> {
    refuse_sparse_checkout(worktree).await?;
    let current_tree = snapshot_tree(worktree).await?;
    let target_tree = tree_of(worktree, target).await?;
    let files =
        collect_changes(worktree, &target_tree, &current_tree, DiffBounds::default()).await?;
    let blocked = inspect(worktree, &current_tree, &target_tree)
        .await?
        .in_the_way();
    Ok(RestorePreview {
        current_tree,
        files,
        blocked,
    })
}

/// A restore that has landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedRestore {
    /// The commit that holds the state the restore replaced.
    pub saved_oid: String,
    /// The tree the worktree now matches.
    pub restored_tree: String,
    /// What the restore changed, from the replaced state to the restored one.
    pub files: BoundedFiles,
}

/// A restore that has been checked and saved, and has changed no file yet.
pub struct PreparedRestore {
    plan: Plan,
    /// The tree the worktree will match.
    pub restored_tree: String,
    /// What the restore changes, from the replaced state to the restored one.
    pub files: BoundedFiles,
    /// The commit that holds the state the restore replaces.
    pub saved_oid: String,
}

/// Why a prepared restore did not land.
#[derive(Debug)]
pub enum RestoreApplyError {
    /// A path could not move, every path already moved went back, and each
    /// of them was then verified to hold what it held before. Nothing
    /// changed. `changed` says a path changed after the check.
    NothingChanged { reason: String, changed: bool },
    /// Some paths could not go back, or did not verify. The saved state
    /// holds everything the restore replaced, so undoing the restore puts it
    /// back.
    Partial(String),
}

/// Check that restoring `target` is safe, and save what it will replace.
///
/// `expected_current` is the `current_tree` a preview returned. When it is
/// given and the worktree no longer snapshots to it, nothing changes: the
/// person confirmed a list of losses that is no longer the whole list. The
/// restore also refuses while anything it would overwrite or remove is not in
/// the snapshot. The saved state lands on `saved_ref`, which must not exist
/// yet. It holds every path the restore will touch, exactly as the check found
/// it.
pub async fn prepare_restore(
    worktree: &Path,
    target: &str,
    expected_current: Option<&str>,
    saved_ref: &str,
    saved_message: &str,
) -> Result<PreparedRestore, CheckpointError> {
    refuse_sparse_checkout(worktree).await?;
    let index = PrivateIndex::new(worktree).await?;
    let current_tree = index.snapshot(worktree).await?;
    drop(index);
    if expected_current.is_some_and(|expected| expected != current_tree) {
        return Err(CheckpointError::conflict(
            "worktree_changed",
            "The workspace changed after you reviewed this restore. Review it again.",
        ));
    }
    let restored_tree = tree_of(worktree, target).await?;
    if restored_tree == current_tree {
        return Err(CheckpointError::conflict(
            "nothing_to_restore",
            "The workspace already matches that checkpoint.",
        ));
    }
    let mut plan = inspect(worktree, &current_tree, &restored_tree)
        .await?
        .into_plan("restore_blocked", "The restore")?;
    // A saved state comes back as the exact bytes it kept, not as what a
    // clean filter made of them.
    plan.write_exact(exact_bytes_kept_by(worktree, target).await?);
    let files = collect_changes(
        worktree,
        &current_tree,
        &restored_tree,
        DiffBounds::default(),
    )
    .await?;
    // Save what is about to be replaced before any file moves, so even a
    // restore that stops halfway leaves a way back.
    let head = git_text(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    let saved_oid = save_state(
        worktree,
        &current_tree,
        &head,
        saved_message,
        &plan.exact_bytes_the_snapshot_lacks(),
    )
    .await?;
    git_text(
        worktree,
        &["update-ref", "--no-deref", saved_ref, &saved_oid, ""],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    Ok(PreparedRestore {
        plan,
        restored_tree,
        files,
        saved_oid,
    })
}

impl PreparedRestore {
    /// Move the files, one path at a time. When a path cannot move, the
    /// paths already moved go back.
    pub async fn apply(self, worktree: &Path) -> Result<AppliedRestore, RestoreApplyError> {
        match apply(worktree, &self.plan).await {
            Ok(()) => Ok(AppliedRestore {
                saved_oid: self.saved_oid,
                restored_tree: self.restored_tree,
                files: self.files,
            }),
            Err(ApplyFailure::RolledBack { reason, changed }) => {
                Err(RestoreApplyError::NothingChanged { reason, changed })
            }
            Err(ApplyFailure::Partial { reason }) => Err(RestoreApplyError::Partial(reason)),
        }
    }

    /// Move the paths before step `killed_at`, then stop the way a killed
    /// process stops: nothing rolls back and nothing is reported.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn apply_until_killed(self, worktree: &Path, killed_at: usize) {
        super::worktree::apply_until_killed(worktree, &self.plan, killed_at).await;
    }
}

/// Prepare and apply in one step, for a caller with nothing to journal.
pub async fn restore_worktree(
    worktree: &Path,
    target: &str,
    expected_current: Option<&str>,
    saved_ref: &str,
    saved_message: &str,
) -> Result<AppliedRestore, CheckpointError> {
    prepare_restore(worktree, target, expected_current, saved_ref, saved_message)
        .await?
        .apply(worktree)
        .await
        .map_err(|err| match err {
            RestoreApplyError::NothingChanged { reason, changed } => CheckpointError::conflict(
                if changed {
                    "worktree_changed"
                } else {
                    "restore_failed"
                },
                format!("The restore stopped, and nothing changed. {reason}"),
            ),
            RestoreApplyError::Partial(reason) => CheckpointError::internal(reason),
        })
}

/// Where a restore put the worktree, as its chain commit records it for the
/// next turn's note to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoredTo {
    /// Before turn `ordinal` of `session_id`.
    BeforeTurn { session_id: SessionId, ordinal: i64 },
    /// Back to where an earlier restore started.
    BeforeRestore,
}

const RESTORED_TO_TRAILER: &str = "Tidebreak-Restored-To:";

/// The message of the commit a restore leaves on each session's chain.
pub fn chain_commit_message(restore_id: CodeRestoreId, to: RestoredTo) -> String {
    let target = match to {
        RestoredTo::BeforeTurn {
            session_id,
            ordinal,
        } => format!("before-turn {ordinal} {session_id}"),
        RestoredTo::BeforeRestore => "before-restore".to_owned(),
    };
    format!("restore {restore_id}\n\n{RESTORED_TO_TRAILER} {target}\n")
}

fn parse_restored_to(message: &str) -> Option<RestoredTo> {
    let value = message
        .lines()
        .find_map(|line| line.trim().strip_prefix(RESTORED_TO_TRAILER))?
        .trim();
    if value == "before-restore" {
        return Some(RestoredTo::BeforeRestore);
    }
    let mut parts = value.strip_prefix("before-turn ")?.split(' ');
    let ordinal = parts.next()?.parse().ok()?;
    let session_id = parts.next()?.parse().ok()?;
    Some(RestoredTo::BeforeTurn {
        session_id,
        ordinal,
    })
}

/// Point each session's chain at the restored state.
///
/// One commit holds the restored tree on top of the saved state, so `git log`
/// on the hidden refs reads as what happened. Each ref in `resume_refs` is
/// a [`chain_resume_ref`] for a session's newest turn.
pub async fn continue_chains_after_restore(
    worktree: &Path,
    applied: &AppliedRestore,
    message: &str,
    resume_refs: &[String],
) -> Result<(), CheckpointError> {
    continue_chains_to(
        worktree,
        &applied.restored_tree,
        &applied.saved_oid,
        message,
        resume_refs,
    )
    .await
}

/// Point each session's chain at `tree`, on top of the state a restore
/// saved. A restore that did not finish passes the files as they stand, so
/// the next turn diffs from what is really there.
pub async fn continue_chains_to(
    worktree: &Path,
    tree: &str,
    saved_oid: &str,
    message: &str,
    resume_refs: &[String],
) -> Result<(), CheckpointError> {
    if resume_refs.is_empty() {
        return Ok(());
    }
    let restored = commit_tree(worktree, tree, &[saved_oid], message).await?;
    for r#ref in resume_refs {
        git_text(
            worktree,
            &["update-ref", "--no-deref", r#ref, &restored],
            GIT_TIMEOUT,
        )
        .await
        .map_err(CheckpointError::internal)?;
    }
    Ok(())
}

/// How many changed files the note to the engine names.
const NOTED_FILES: usize = 20;

/// What to tell a session's engine before its turn `ordinal`, when a restore
/// moved the worktree after its previous turn.
///
/// The engine still remembers the turns the restore undid, so without this
/// it would keep working from files that are gone. `None` when no restore ran
/// since that turn, or it left this session's files as the engine last saw
/// them.
pub async fn restore_note(
    worktree: &Path,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    ordinal: i64,
) -> Result<Option<String>, CheckpointError> {
    let previous = ordinal - 1;
    if previous <= BASELINE_ORDINAL {
        // A first turn starts from the files as they are.
        return Ok(None);
    }
    let resume = chain_resume_ref(workspace_id, session_id, previous);
    let Ok(resumed) = git_text(worktree, &["rev-parse", "--verify", &resume], GIT_TIMEOUT).await
    else {
        return Ok(None);
    };
    let last_seen = checkpoint_ref(workspace_id, session_id, previous);
    let Ok(last_seen) = git_text(
        worktree,
        &["rev-parse", "--verify", &last_seen],
        GIT_TIMEOUT,
    )
    .await
    else {
        return Ok(None);
    };
    let files = collect_changes(
        worktree,
        &last_seen,
        &resumed,
        DiffBounds {
            max_files: NOTED_FILES,
            ..DiffBounds::default()
        },
    )
    .await?;
    if files.files.is_empty() {
        return Ok(None);
    }
    let message = git_text(
        worktree,
        &["show", "-s", "--format=%B", &resumed, "--"],
        GIT_TIMEOUT,
    )
    .await
    .unwrap_or_default();
    let point = match parse_restored_to(&message) {
        Some(RestoredTo::BeforeTurn {
            session_id: target,
            ordinal,
        }) if target == session_id => format!("to how they were before your turn {ordinal}"),
        Some(RestoredTo::BeforeTurn { ordinal, .. }) => {
            format!("to how they were before turn {ordinal} of another agent in this workspace")
        }
        Some(RestoredTo::BeforeRestore) => "to how they were before an earlier restore".to_owned(),
        None => "to an earlier checkpoint".to_owned(),
    };
    let mut listed: Vec<String> = files
        .files
        .iter()
        .map(|file| match file.kind {
            tidebreak_core::FileChangeKind::Added => format!("- {} (back)", file.path.to_wire()),
            tidebreak_core::FileChangeKind::Deleted => {
                format!("- {} (gone)", file.path.to_wire())
            }
            _ => format!("- {}", file.path.to_wire()),
        })
        .collect();
    let total = usize::try_from(files.stat.files).unwrap_or(usize::MAX);
    if total > listed.len() {
        listed.push(format!("- and {} more", total - listed.len()));
    }
    Ok(Some(format!(
        "Note from Tidebreak: since your last turn, the person restored this workspace's files \
         {point}. These files no longer match what you last saw, so any edits you remember \
         making to them may be gone. Read a file again before you rely on it.\n{}",
        listed.join("\n")
    )))
}

/// The trailer on a saved state that names the commit holding the exact
/// bytes of files a clean filter changed.
const EXACT_BYTES_TRAILER: &str = "Tidebreak-Exact-Bytes:";

/// Commit the state a restore replaces, on top of `head`.
///
/// A snapshot holds each file as git's clean filters made it. When a filter
/// dropped part of a file the restore replaces, such as notebook output, the
/// file's exact bytes go in a second commit: the saved state's second parent,
/// which keeps them reachable, and which its message names. Undoing the
/// restore writes those bytes back as they were.
async fn save_state(
    worktree: &Path,
    tree: &str,
    head: &str,
    message: &str,
    exact: &[(GitPath, TreeEntry)],
) -> Result<String, CheckpointError> {
    if exact.is_empty() {
        return commit_tree(worktree, tree, &[head], message).await;
    }
    let exact_tree = tree_of_entries(worktree, exact).await?;
    let exact_commit = commit_tree(
        worktree,
        &exact_tree,
        &[],
        "exact bytes of the files git's filters change",
    )
    .await?;
    commit_tree(
        worktree,
        tree,
        &[head, &exact_commit],
        &format!("{message}\n\n{EXACT_BYTES_TRAILER} {exact_commit}\n"),
    )
    .await
}

/// The exact bytes a saved state kept for files a clean filter changed, by
/// path. Empty for a turn's checkpoint, which keeps none.
async fn exact_bytes_kept_by(
    worktree: &Path,
    target: &str,
) -> Result<HashMap<GitPath, TreeEntry>, CheckpointError> {
    let message = git_text(
        worktree,
        &["show", "-s", "--format=%B", target, "--"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    let Some(commit) = message
        .lines()
        .find_map(|line| line.trim().strip_prefix(EXACT_BYTES_TRAILER))
        .map(str::trim)
    else {
        return Ok(HashMap::new());
    };
    tree_entries(worktree, commit).await
}

async fn commit_tree(
    worktree: &Path,
    tree: &str,
    parents: &[&str],
    message: &str,
) -> Result<String, CheckpointError> {
    let mut args = vec!["commit-tree", tree];
    for parent in parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    git_text(worktree, &args, GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{add_worktree, git_stdout, init_repo, run, strip_output_filter};
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn ws() -> WorkspaceId {
        WorkspaceId::new()
    }

    /// Snapshot the worktree as a commit on a hidden ref, the way a turn's
    /// checkpoint does, and return the commit.
    async fn checkpoint(worktree: &Path, name: &str) -> String {
        let tree = snapshot_tree(worktree).await.unwrap();
        let head = git_stdout(worktree, &["rev-parse", "HEAD"]);
        let commit = git_stdout(worktree, &["commit-tree", &tree, "-p", &head, "-m", name]);
        run(
            worktree,
            &[
                "git",
                "update-ref",
                &format!("{REF_PREFIX}/test/{name}"),
                &commit,
            ],
        );
        commit
    }

    fn read(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn saved_ref() -> String {
        format!("{REF_PREFIX}/test/saved")
    }

    fn ref_exists(worktree: &Path, r#ref: &str) -> bool {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", r#ref])
            .current_dir(worktree)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[tokio::test]
    async fn a_restore_puts_tracked_and_untracked_files_back_and_saves_what_it_replaced() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "restore");
        std::fs::write(tree.join(".gitignore"), "build/\n").unwrap();
        run(&tree, &["git", "add", ".gitignore"]);
        run(&tree, &["git", "commit", "-q", "-m", "ignore build"]);
        // The state before the turn: one edit and one untracked file.
        std::fs::write(tree.join("README.md"), "before the turn\n").unwrap();
        std::fs::write(tree.join("notes.txt"), "untracked before\n").unwrap();
        let before = checkpoint(&tree, "before").await;

        // What the turn and a person did after that.
        std::fs::write(tree.join("README.md"), "the turn's version\n").unwrap();
        std::fs::remove_file(tree.join("keep.txt")).unwrap();
        std::fs::remove_file(tree.join("notes.txt")).unwrap();
        std::fs::create_dir_all(tree.join("src/deep")).unwrap();
        std::fs::write(tree.join("src/deep/new.rs"), "fn main() {}\n").unwrap();
        std::fs::create_dir_all(tree.join("build")).unwrap();
        std::fs::write(tree.join("build/out.bin"), "ignored output\n").unwrap();
        let script = tree.join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let head_before = git_stdout(&tree, &["rev-parse", "HEAD"]);
        let index_before = std::fs::read(git_index_path(&tree)).unwrap();
        let replaced_tree = snapshot_tree(&tree).await.unwrap();

        let preview = preview_restore(&tree, &before).await.unwrap();
        assert_eq!(preview.current_tree, replaced_tree);
        assert!(preview.blocked.is_empty());
        let mut listed: Vec<String> = preview
            .files
            .files
            .iter()
            .map(|file| file.path.to_wire())
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            [
                "README.md",
                "keep.txt",
                "notes.txt",
                "run.sh",
                "src/deep/new.rs"
            ],
            "the preview names every change the restore undoes"
        );

        let applied = restore_worktree(
            &tree,
            &before,
            Some(&preview.current_tree),
            &saved_ref(),
            "state before restore",
        )
        .await
        .unwrap();

        assert_eq!(
            read(&tree.join("README.md")).as_deref(),
            Some("before the turn\n")
        );
        assert_eq!(read(&tree.join("keep.txt")).as_deref(), Some("keep\n"));
        assert_eq!(
            read(&tree.join("notes.txt")).as_deref(),
            Some("untracked before\n"),
            "an untracked file the target held comes back"
        );
        assert!(
            !tree.join("src/deep/new.rs").exists(),
            "a file added since is removed"
        );
        assert!(!tree.join("run.sh").exists());
        assert_eq!(
            read(&tree.join("build/out.bin")).as_deref(),
            Some("ignored output\n"),
            "ignored files are no checkpoint's business"
        );
        assert_eq!(git_stdout(&tree, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            std::fs::read(git_index_path(&tree)).unwrap(),
            index_before,
            "the user's index is byte-identical"
        );
        assert_eq!(snapshot_tree(&tree).await.unwrap(), applied.restored_tree);
        assert_eq!(applied.files.stat.files, 5);

        // The replaced state is saved, and restoring it undoes the restore.
        assert_eq!(
            git_stdout(&tree, &["rev-parse", &format!("{}^{{tree}}", saved_ref())]),
            replaced_tree
        );
        let undo_ref = format!("{REF_PREFIX}/test/undo");
        restore_worktree(
            &tree,
            &applied.saved_oid,
            None,
            &undo_ref,
            "state before undo",
        )
        .await
        .unwrap();
        assert_eq!(snapshot_tree(&tree).await.unwrap(), replaced_tree);
        assert_eq!(
            read(&tree.join("src/deep/new.rs")).as_deref(),
            Some("fn main() {}\n")
        );
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "the executable bit comes back with the file"
        );
    }

    #[tokio::test]
    async fn an_unchanged_file_is_not_rewritten() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "untouched");
        let before = checkpoint(&tree, "before").await;
        std::fs::write(tree.join("README.md"), "changed\n").unwrap();
        let untouched = std::fs::metadata(tree.join("keep.txt"))
            .unwrap()
            .modified()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        restore_worktree(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();

        assert_eq!(read(&tree.join("README.md")).as_deref(), Some("hello\n"));
        assert_eq!(
            std::fs::metadata(tree.join("keep.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            untouched,
            "a restore touches only the files that move, so a watcher rebuilds nothing else"
        );
    }

    #[tokio::test]
    async fn a_worktree_that_changed_after_the_preview_is_left_alone() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "moved");
        let before = checkpoint(&tree, "before").await;
        std::fs::write(tree.join("README.md"), "reviewed\n").unwrap();
        let preview = preview_restore(&tree, &before).await.unwrap();
        std::fs::write(tree.join("README.md"), "edited after the review\n").unwrap();

        let err = restore_worktree(
            &tree,
            &before,
            Some(&preview.current_tree),
            &saved_ref(),
            "state before restore",
        )
        .await
        .unwrap_err();

        assert!(
            matches!(
                err,
                CheckpointError::Conflict {
                    kind: "worktree_changed",
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(
            read(&tree.join("README.md")).as_deref(),
            Some("edited after the review\n")
        );
        assert!(
            !ref_exists(&tree, &saved_ref()),
            "a refused restore saves nothing"
        );
    }

    #[tokio::test]
    async fn restoring_the_state_the_worktree_already_holds_is_refused() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "same");
        let before = checkpoint(&tree, "before").await;
        let err = restore_worktree(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                CheckpointError::Conflict {
                    kind: "nothing_to_restore",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    /// An ignored `.env` that was an untracked file when the checkpoint was
    /// taken, and an ignored file inside a folder that the checkpoint holds as
    /// a file: no snapshot holds either, so the restore refuses and names
    /// them, and both stay exactly as they are.
    #[tokio::test]
    async fn a_restore_refuses_to_overwrite_or_remove_ignored_files() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "ignored");
        std::fs::write(tree.join(".env"), "PLACEHOLDER=1\n").unwrap();
        std::fs::write(tree.join("utils"), "a file once\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        // Start the next snapshot cold, the way a fresh machine or a rebuilt
        // index does: an ignored file is then in no snapshot at all.
        let reusable = git_stdout(
            &tree,
            &["rev-parse", "--git-path", "tidebreak-checkpoint-index"],
        );
        let _ = std::fs::remove_file(tree.join(reusable));

        // The turn ignores `.env` and turns `utils` into a folder that holds
        // an ignored file of the person's own.
        std::fs::write(tree.join(".gitignore"), ".env\n*.local\n").unwrap();
        std::fs::write(tree.join(".env"), "the real value\n").unwrap();
        std::fs::remove_file(tree.join("utils")).unwrap();
        std::fs::create_dir_all(tree.join("utils")).unwrap();
        std::fs::write(tree.join("utils/helpers.rs"), "pub fn help() {}\n").unwrap();
        std::fs::write(tree.join("utils/settings.local"), "mine\n").unwrap();

        let preview = preview_restore(&tree, &before).await.unwrap();
        let named: Vec<String> = preview.blocked.iter().map(GitPath::to_wire).collect();
        assert_eq!(named, [".env", "utils/settings.local"]);

        let err = restore_worktree(
            &tree,
            &before,
            Some(&preview.current_tree),
            &saved_ref(),
            "state before restore",
        )
        .await
        .unwrap_err();
        let CheckpointError::Conflict {
            kind: "restore_blocked",
            message,
        } = err
        else {
            panic!("{err:?}");
        };
        assert!(
            message.contains(".env") && message.contains("utils/settings.local"),
            "{message}"
        );
        assert_eq!(
            read(&tree.join(".env")).as_deref(),
            Some("the real value\n")
        );
        assert_eq!(
            read(&tree.join("utils/settings.local")).as_deref(),
            Some("mine\n")
        );
        assert_eq!(
            read(&tree.join("utils/helpers.rs")).as_deref(),
            Some("pub fn help() {}\n")
        );
        assert!(!ref_exists(&tree, &saved_ref()));
    }

    /// A file that appears after the restore checked the worktree, where the
    /// target puts a file of its own, is never overwritten.
    #[tokio::test]
    async fn a_file_that_appears_after_the_check_is_not_overwritten() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "race");
        std::fs::write(tree.join("draft.md"), "the checkpoint's draft\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        std::fs::remove_file(tree.join("draft.md")).unwrap();
        std::fs::write(tree.join("README.md"), "later\n").unwrap();

        let prepared = prepare_restore(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();
        std::fs::write(tree.join("draft.md"), "typed a moment later\n").unwrap();
        let err = prepared.apply(&tree).await.unwrap_err();

        assert!(
            matches!(err, RestoreApplyError::NothingChanged { changed: true, .. }),
            "{err:?}"
        );
        assert_eq!(
            read(&tree.join("draft.md")).as_deref(),
            Some("typed a moment later\n")
        );
        assert_eq!(read(&tree.join("README.md")).as_deref(), Some("later\n"));
    }

    /// A clone inside the worktree is saved only as the commit it points at.
    /// A restore to a checkpoint that holds a plain file there refuses,
    /// naming it, and the clone, its history, and its uncommitted work stay.
    #[tokio::test]
    async fn a_restore_never_removes_a_nested_repository() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "nested-restore");
        std::fs::write(tree.join("vendor"), "a placeholder file\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        std::fs::remove_file(tree.join("vendor")).unwrap();
        let vendor = tree.join("vendor");
        std::fs::create_dir_all(&vendor).unwrap();
        run(&vendor, &["git", "init", "-q", "-b", "main"]);
        run(&vendor, &["git", "config", "user.email", "dev@example.com"]);
        run(&vendor, &["git", "config", "user.name", "Dev"]);
        run(&vendor, &["git", "config", "commit.gpgsign", "false"]);
        std::fs::write(vendor.join("lib.rs"), "committed\n").unwrap();
        run(&vendor, &["git", "add", "lib.rs"]);
        run(&vendor, &["git", "commit", "-q", "-m", "vendored"]);
        std::fs::write(vendor.join("lib.rs"), "uncommitted work\n").unwrap();

        let preview = preview_restore(&tree, &before).await.unwrap();
        assert_eq!(
            preview
                .blocked
                .iter()
                .map(GitPath::to_wire)
                .collect::<Vec<_>>(),
            ["vendor"]
        );
        let err = restore_worktree(&tree, &before, None, &saved_ref(), "saved")
            .await
            .unwrap_err();

        assert!(
            matches!(
                err,
                CheckpointError::Conflict {
                    kind: "restore_blocked",
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(vendor.join(".git").is_dir());
        assert_eq!(
            read(&vendor.join("lib.rs")).as_deref(),
            Some("uncommitted work\n")
        );
    }

    /// An ignored `.env` typed between the check and the checkout, where the
    /// checkpoint puts a `.env` of its own, is never overwritten.
    #[tokio::test]
    async fn an_ignored_file_that_appears_after_the_check_is_not_overwritten() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "ignored-race");
        std::fs::write(tree.join(".env"), "PLACEHOLDER=1\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        std::fs::remove_file(tree.join(".env")).unwrap();
        std::fs::write(tree.join(".gitignore"), ".env\n").unwrap();

        let prepared = prepare_restore(&tree, &before, None, &saved_ref(), "saved")
            .await
            .unwrap();
        std::fs::write(tree.join(".env"), "the real value\n").unwrap();
        let err = prepared.apply(&tree).await.unwrap_err();

        assert!(
            matches!(err, RestoreApplyError::NothingChanged { changed: true, .. }),
            "{err:?}"
        );
        assert_eq!(
            read(&tree.join(".env")).as_deref(),
            Some("the real value\n")
        );
        assert!(tree.join(".gitignore").exists());
    }

    /// A folder that refuses a removal stops the restore. The restore reports
    /// that, never success, and the worktree is exactly as it was.
    #[tokio::test]
    async fn a_removal_a_read_only_folder_refuses_is_reported_and_rolled_back() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "read-only-restore");
        let before = checkpoint(&tree, "before").await;
        std::fs::write(tree.join("README.md"), "current readme\n").unwrap();
        std::fs::create_dir_all(tree.join("sealed")).unwrap();
        std::fs::write(tree.join("sealed/added.txt"), "added since\n").unwrap();
        let current = snapshot_tree(&tree).await.unwrap();

        let prepared = prepare_restore(&tree, &before, None, &saved_ref(), "saved")
            .await
            .unwrap();
        std::fs::set_permissions(tree.join("sealed"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let err = prepared.apply(&tree).await.unwrap_err();
        std::fs::set_permissions(tree.join("sealed"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(
                err,
                RestoreApplyError::NothingChanged { changed: false, .. }
            ),
            "{err:?}"
        );
        assert_eq!(snapshot_tree(&tree).await.unwrap(), current);
    }

    /// The restore stops partway, at a folder it cannot write in: the file it
    /// already wrote goes back, so the worktree is as it was and nothing is
    /// half-restored.
    #[tokio::test]
    async fn a_restore_that_stops_partway_is_rolled_back() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "partway");
        std::fs::create_dir_all(tree.join("locked")).unwrap();
        std::fs::write(tree.join("locked/kept.txt"), "kept\n").unwrap();
        std::fs::write(tree.join("README.md"), "the checkpoint's readme\n").unwrap();
        std::fs::write(tree.join("locked/later.txt"), "the checkpoint's file\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        std::fs::write(tree.join("README.md"), "current readme\n").unwrap();
        std::fs::remove_file(tree.join("locked/later.txt")).unwrap();

        let prepared = prepare_restore(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();
        // The restore can rewrite README.md, but cannot create a file in
        // `locked/`.
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let err = prepared.apply(&tree).await.unwrap_err();
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(
                err,
                RestoreApplyError::NothingChanged { changed: false, .. }
            ),
            "{err:?}"
        );
        assert_eq!(
            read(&tree.join("README.md")).as_deref(),
            Some("current readme\n")
        );
        assert!(!tree.join("locked/later.txt").exists());
        assert!(
            ref_exists(&tree, &saved_ref()),
            "the saved state stays, so the restore can still be undone"
        );
    }

    /// A restore killed partway, the way a crash stops it, leaves every file
    /// whole, in its version before or after. Its Undo still brings back
    /// exactly the state it replaced, because that state was saved before
    /// any file moved.
    #[tokio::test]
    async fn a_restore_killed_partway_can_still_be_undone_exactly() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "killed");
        for n in 0..4 {
            std::fs::write(tree.join(format!("f{n}.txt")), format!("checkpoint {n}\n")).unwrap();
        }
        let target = checkpoint(&tree, "target").await;
        for n in 0..4 {
            std::fs::write(tree.join(format!("f{n}.txt")), format!("current {n}\n")).unwrap();
        }
        std::fs::write(tree.join("new.txt"), "made after the checkpoint\n").unwrap();
        let before = snapshot_tree(&tree).await.unwrap();

        // Five paths move, so a kill at step 5 is a restore that finished.
        for killed_at in 0..=5 {
            let saved = format!("{REF_PREFIX}/test/saved-{killed_at}");
            let prepared = prepare_restore(&tree, &target, None, &saved, "state before restore")
                .await
                .unwrap();
            super::super::worktree::apply_until_killed(&tree, &prepared.plan, killed_at).await;

            for n in 0..4 {
                let now = read(&tree.join(format!("f{n}.txt")));
                assert!(
                    now == Some(format!("current {n}\n"))
                        || now == Some(format!("checkpoint {n}\n")),
                    "f{n}.txt is whole after a kill at step {killed_at}: {now:?}"
                );
            }
            assert!(ref_exists(&tree, &saved), "the Undo outlives the kill");

            // Undo, as the transcript's button runs it: restore the saved state.
            let undo_saved = format!("{REF_PREFIX}/test/undo-{killed_at}");
            match prepare_restore(&tree, &saved, None, &undo_saved, "state before undo").await {
                Ok(undo) => {
                    undo.apply(&tree).await.unwrap();
                }
                Err(CheckpointError::Conflict {
                    kind: "nothing_to_restore",
                    ..
                }) => assert_eq!(killed_at, 0, "only a kill before any path moved"),
                Err(err) => panic!("undo after a kill at step {killed_at}: {err:?}"),
            }
            assert_eq!(
                snapshot_tree(&tree).await.unwrap(),
                before,
                "the state before comes back exactly after a kill at step {killed_at}"
            );
        }
    }

    /// A clean filter drops `OUTPUT` lines, so no snapshot holds them. The
    /// restore saves the file's exact bytes, so its Undo brings the output
    /// back, and so does undoing that Undo.
    #[tokio::test]
    async fn a_restore_and_its_undo_keep_what_a_filter_strips() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "lossy-restore");
        strip_output_filter(&tree);
        std::fs::write(tree.join("nb.ipynb"), "code v1\nOUTPUT 1\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        let with_output = "code v2\nOUTPUT 42\n";
        std::fs::write(tree.join("nb.ipynb"), with_output).unwrap();

        restore_worktree(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();
        // The checkpoint never held its output; the restore writes what it has.
        assert_eq!(read(&tree.join("nb.ipynb")).as_deref(), Some("code v1\n"));

        let undo = format!("{REF_PREFIX}/test/undo");
        restore_worktree(&tree, &saved_ref(), None, &undo, "state before undo")
            .await
            .unwrap();
        assert_eq!(read(&tree.join("nb.ipynb")).as_deref(), Some(with_output));

        let redo = format!("{REF_PREFIX}/test/redo");
        restore_worktree(&tree, &undo, None, &redo, "state before redo")
            .await
            .unwrap();
        assert_eq!(read(&tree.join("nb.ipynb")).as_deref(), Some("code v1\n"));
        restore_worktree(
            &tree,
            &redo,
            None,
            &format!("{REF_PREFIX}/test/again"),
            "again",
        )
        .await
        .unwrap();
        assert_eq!(read(&tree.join("nb.ipynb")).as_deref(), Some(with_output));
    }

    /// A restore that stops partway puts a file back as its exact bytes,
    /// not as what the filter kept of it.
    #[tokio::test]
    async fn a_restore_that_stops_partway_puts_back_what_a_filter_strips() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "lossy-rollback");
        strip_output_filter(&tree);
        std::fs::create_dir_all(tree.join("locked")).unwrap();
        std::fs::write(tree.join("locked/kept.txt"), "kept\n").unwrap();
        std::fs::write(tree.join("a.ipynb"), "code v1\n").unwrap();
        std::fs::write(tree.join("locked/later.txt"), "the checkpoint's file\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        let with_output = "code v2\nOUTPUT 42\n";
        std::fs::write(tree.join("a.ipynb"), with_output).unwrap();
        std::fs::remove_file(tree.join("locked/later.txt")).unwrap();

        let prepared = prepare_restore(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();
        // `a.ipynb` is written first; the file in `locked/` cannot be.
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let err = prepared.apply(&tree).await.unwrap_err();
        std::fs::set_permissions(tree.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        assert!(
            matches!(
                err,
                RestoreApplyError::NothingChanged { changed: false, .. }
            ),
            "{err:?}"
        );
        assert_eq!(read(&tree.join("a.ipynb")).as_deref(), Some(with_output));
    }

    /// A file whose name is as long as the disk allows: a restore removes it
    /// and rewrites another, and its Undo writes both back, because each new
    /// version is written under a short name first.
    #[tokio::test]
    async fn a_long_file_name_survives_a_restore_and_its_undo() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "long-restore");
        let added = format!("{}.txt", "a".repeat(246));
        let edited = format!("{}.txt", "e".repeat(246));
        std::fs::write(tree.join(&edited), "v1\n").unwrap();
        let before = checkpoint(&tree, "before").await;
        std::fs::write(tree.join(&edited), "v2\n").unwrap();
        std::fs::write(tree.join(&added), "made after the checkpoint\n").unwrap();

        restore_worktree(&tree, &before, None, &saved_ref(), "state before restore")
            .await
            .unwrap();
        assert!(!tree.join(&added).exists());
        assert_eq!(read(&tree.join(&edited)).as_deref(), Some("v1\n"));

        let undo = format!("{REF_PREFIX}/test/undo");
        restore_worktree(&tree, &saved_ref(), None, &undo, "state before undo")
            .await
            .unwrap();
        assert_eq!(
            read(&tree.join(&added)).as_deref(),
            Some("made after the checkpoint\n")
        );
        assert_eq!(read(&tree.join(&edited)).as_deref(), Some("v2\n"));
    }

    #[tokio::test]
    async fn a_restore_point_is_found_by_its_id_and_names_its_session() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "find");
        let workspace = ws();
        let session = SessionId::new();
        let restore = CodeRestoreId::new();
        let commit = checkpoint(&tree, "point").await;
        run(
            &tree,
            &[
                "git",
                "update-ref",
                &restore_point_ref(workspace, session, restore),
                &commit,
            ],
        );
        // A resume ref under the same session must not be mistaken for it.
        run(
            &tree,
            &[
                "git",
                "update-ref",
                &chain_resume_ref(workspace, session, 3),
                &commit,
            ],
        );

        assert_eq!(
            find_restore_point(&tree, workspace, restore).await.unwrap(),
            Some((session, commit.clone()))
        );
        assert_eq!(
            find_restore_point(&tree, workspace, CodeRestoreId::new())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            find_restore_point(&tree, ws(), restore).await.unwrap(),
            None,
            "a restore belongs to its own workspace"
        );
    }

    #[tokio::test]
    async fn the_next_turn_hears_which_files_a_restore_moved() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "note");
        let workspace = ws();
        let session = SessionId::new();
        let first = checkpoint(&tree, "first").await;
        std::fs::write(tree.join("README.md"), "turn two wrote this\n").unwrap();
        std::fs::write(tree.join("added.rs"), "fn added() {}\n").unwrap();
        let second = checkpoint(&tree, "second").await;
        run(
            &tree,
            &[
                "git",
                "update-ref",
                &checkpoint_ref(workspace, session, 2),
                &second,
            ],
        );
        assert_eq!(
            restore_note(&tree, workspace, session, 3).await.unwrap(),
            None,
            "no restore, no note"
        );

        let applied = restore_worktree(&tree, &first, None, &saved_ref(), "saved")
            .await
            .unwrap();
        continue_chains_after_restore(
            &tree,
            &applied,
            &chain_commit_message(
                CodeRestoreId::new(),
                RestoredTo::BeforeTurn {
                    session_id: session,
                    ordinal: 2,
                },
            ),
            &[chain_resume_ref(workspace, session, 2)],
        )
        .await
        .unwrap();

        let note = restore_note(&tree, workspace, session, 3)
            .await
            .unwrap()
            .expect("the engine is told");
        assert!(note.contains("before your turn 2"), "{note}");
        assert!(note.contains("- README.md\n"), "{note}");
        assert!(note.contains("- added.rs (gone)"), "{note}");
        assert_eq!(
            restore_note(&tree, workspace, session, 4).await.unwrap(),
            None,
            "only the turn right after the restore hears about it"
        );
    }

    fn git_index_path(worktree: &Path) -> PathBuf {
        let path = git_stdout(worktree, &["rev-parse", "--git-path", "index"]);
        worktree.join(path)
    }
}

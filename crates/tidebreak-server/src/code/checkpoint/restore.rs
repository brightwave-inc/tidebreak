//! Put a workspace's worktree back to an earlier checkpoint.
//!
//! A restore makes every file the checkpoint snapshot sees — tracked changes
//! and untracked files alike — match the target checkpoint's tree. Ignored
//! files stay where they are, because no checkpoint records them. `HEAD`, the
//! branch, and the user's index are untouched: the files go back, and whatever
//! the branch committed since then shows as uncommitted changes that undo it.
//!
//! Before any file changes, the state being replaced is committed to a hidden
//! ref of its own, so a restore can itself be undone by restoring that state.
//! The checkout itself runs through a private index, so the user's staging
//! area never moves and no checkout hook fires.

use std::path::{Path, PathBuf};

use tokio::time::Instant;

use tidebreak_core::{CodeRestoreId, SessionId, WorkspaceId};

use super::{
    checkpoint_index_path_before, collect_changes, git_text, git_text_env, snapshot_tree,
    BoundedFiles, CheckpointError, DiffBounds, GIT_SNAPSHOT_TIMEOUT, GIT_TIMEOUT, REF_PREFIX,
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

/// What a restore would change, read before anything moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreview {
    /// The live worktree's snapshot tree. A restore that names it refuses to
    /// run over a worktree that changed after this preview.
    pub current_tree: String,
    /// Everything that changed since the target state. The restore undoes all
    /// of it, whoever made it.
    pub files: BoundedFiles,
}

/// Read what restoring `target` would undo, without changing anything.
pub async fn preview_restore(
    worktree: &Path,
    target: &str,
) -> Result<RestorePreview, CheckpointError> {
    let current_tree = snapshot_tree(worktree).await?;
    let target_tree = tree_of(worktree, target).await?;
    let files =
        collect_changes(worktree, &target_tree, &current_tree, DiffBounds::default()).await?;
    Ok(RestorePreview {
        current_tree,
        files,
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

/// Make the worktree match `target`'s tree, after saving what it replaces.
///
/// `expected_current` is the `current_tree` a preview returned. When it is
/// given and the worktree no longer snapshots to it, nothing changes: the
/// person confirmed a list of losses that is no longer the whole list.
/// The saved state lands on `saved_ref`, which must not exist yet.
pub async fn restore_worktree(
    worktree: &Path,
    target: &str,
    expected_current: Option<&str>,
    saved_ref: &str,
    saved_message: &str,
) -> Result<AppliedRestore, CheckpointError> {
    let index = PrivateIndex::new(worktree).await?;
    let current_tree = index.snapshot(worktree).await?;
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
    let files = collect_changes(
        worktree,
        &current_tree,
        &restored_tree,
        DiffBounds::default(),
    )
    .await?;
    // Save what is about to be replaced before any file moves, so even a
    // checkout that stops halfway leaves a way back.
    let head = git_text(worktree, &["rev-parse", "HEAD"], GIT_TIMEOUT)
        .await
        .map_err(CheckpointError::internal)?;
    let saved_oid = commit_tree(worktree, &current_tree, &head, saved_message).await?;
    git_text(
        worktree,
        &["update-ref", "--no-deref", saved_ref, &saved_oid, ""],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)?;
    index.check_out(worktree, &restored_tree).await?;
    Ok(AppliedRestore {
        saved_oid,
        restored_tree,
        files,
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
    if resume_refs.is_empty() {
        return Ok(());
    }
    let restored = commit_tree(
        worktree,
        &applied.restored_tree,
        &applied.saved_oid,
        message,
    )
    .await?;
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

/// The tree a checkpoint commit holds.
async fn tree_of(worktree: &Path, commit: &str) -> Result<String, CheckpointError> {
    let spec = format!("{commit}^{{tree}}");
    git_text(
        worktree,
        &["rev-parse", "--verify", "--quiet", &spec],
        GIT_TIMEOUT,
    )
    .await
    .ok()
    .filter(|tree| !tree.is_empty())
    .ok_or_else(|| CheckpointError::not_found("That checkpoint is no longer in the repository."))
}

async fn commit_tree(
    worktree: &Path,
    tree: &str,
    parent: &str,
    message: &str,
) -> Result<String, CheckpointError> {
    git_text(
        worktree,
        &["commit-tree", tree, "-p", parent, "-m", message],
        GIT_TIMEOUT,
    )
    .await
    .map_err(CheckpointError::internal)
}

/// An index file of the restore's own, deleted when it drops.
///
/// It starts as a copy of the reusable checkpoint index when there is one, so
/// the snapshot re-hashes only what changed since the last checkpoint. The copy
/// is safe to take while another snapshot writes the original: git replaces an
/// index by renaming a finished file over it, and it marks an entry it could
/// not trust by its stat data, so a copied entry is re-read, never believed.
struct PrivateIndex {
    path: PathBuf,
}

impl PrivateIndex {
    async fn new(worktree: &Path) -> Result<Self, CheckpointError> {
        let temp = tempfile::NamedTempFile::new().map_err(|err| {
            CheckpointError::internal(format!("could not create a temporary index: {err}"))
        })?;
        let index = Self {
            path: temp.path().to_path_buf(),
        };
        // Git wants to create the index itself; an empty file reads as corrupt.
        drop(temp);
        let _ = tokio::fs::remove_file(&index.path).await;
        if let Ok(Some(reusable)) =
            checkpoint_index_path_before(worktree, Instant::now() + GIT_TIMEOUT).await
        {
            if tokio::fs::copy(&reusable, &index.path).await.is_err() {
                let _ = tokio::fs::remove_file(&index.path).await;
            }
        }
        Ok(index)
    }

    /// Snapshot the worktree into this index and return the tree.
    async fn snapshot(&self, worktree: &Path) -> Result<String, CheckpointError> {
        match self.try_snapshot(worktree).await {
            Ok(tree) => Ok(tree),
            // A seed git cannot read costs only its stat cache. Start cold.
            Err(_) if self.path.exists() => {
                let _ = tokio::fs::remove_file(&self.path).await;
                self.try_snapshot(worktree).await
            }
            Err(err) => Err(err),
        }
    }

    async fn try_snapshot(&self, worktree: &Path) -> Result<String, CheckpointError> {
        let index = self.path.to_string_lossy();
        let env = [("GIT_INDEX_FILE", index.as_ref())];
        if !self.path.exists() {
            git_text_env(worktree, &["read-tree", "HEAD"], &env, GIT_TIMEOUT)
                .await
                .map_err(CheckpointError::internal)?;
        }
        git_text_env(worktree, &["add", "-A"], &env, GIT_SNAPSHOT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)?;
        git_text_env(worktree, &["write-tree"], &env, GIT_TIMEOUT)
            .await
            .map_err(CheckpointError::internal)
    }

    /// Make the worktree match `tree`, starting from the snapshot this index
    /// holds.
    ///
    /// A one-tree reset writes each file whose content differs, removes each
    /// file the target lacks, and leaves an unchanged file alone, stat data and
    /// all, so a watcher sees only the files that really moved. Git checks
    /// every path before it writes one, and refuses rather than overwrite an
    /// untracked file that appeared after the snapshot.
    async fn check_out(&self, worktree: &Path, tree: &str) -> Result<(), CheckpointError> {
        let index = self.path.to_string_lossy();
        git_text_env(
            worktree,
            &["read-tree", "--reset", "-u", tree],
            &[("GIT_INDEX_FILE", index.as_ref())],
            GIT_SNAPSHOT_TIMEOUT,
        )
        .await
        .map(|_| ())
        .map_err(|err| {
            CheckpointError::conflict(
                "restore_blocked",
                format!("Git could not restore the worktree: {err}"),
            )
        })
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testing::{add_worktree, git_stdout, init_repo, run};
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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

        let saved_ref = format!("{REF_PREFIX}/test/saved");
        let applied = restore_worktree(
            &tree,
            &before,
            Some(&preview.current_tree),
            &saved_ref,
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
            git_stdout(&tree, &["rev-parse", &format!("{saved_ref}^{{tree}}")]),
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

        restore_worktree(
            &tree,
            &before,
            None,
            &format!("{REF_PREFIX}/test/saved"),
            "state before restore",
        )
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

        let saved_ref = format!("{REF_PREFIX}/test/saved");
        let err = restore_worktree(
            &tree,
            &before,
            Some(&preview.current_tree),
            &saved_ref,
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
            git_text(
                &tree,
                &["rev-parse", "--verify", "--quiet", &saved_ref],
                GIT_TIMEOUT
            )
            .await
            .is_err(),
            "a refused restore saves nothing"
        );
    }

    #[tokio::test]
    async fn restoring_the_state_the_worktree_already_holds_is_refused() {
        let (_dir, repo) = init_repo();
        let tree = add_worktree(&repo, "same");
        let before = checkpoint(&tree, "before").await;
        let err = restore_worktree(
            &tree,
            &before,
            None,
            &format!("{REF_PREFIX}/test/saved"),
            "state before restore",
        )
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

    fn git_index_path(worktree: &Path) -> PathBuf {
        let path = git_stdout(worktree, &["rev-parse", "--git-path", "index"]);
        worktree.join(path)
    }
}

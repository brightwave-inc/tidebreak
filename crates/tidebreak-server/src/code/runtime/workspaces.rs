//! Workspace lifecycle: create, archive, release, restore, storage, and file reads.

use super::*;

impl CodeRuntime {
    pub(crate) async fn create_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
        base_ref: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        let branch = branch_name(&repo.branch_prefix, &title, id.0.as_u128());
        let existing = list_workspaces(&self.db, owner, Some(repo_id)).await?;
        if existing
            .iter()
            .any(|workspace| workspace.branch_name == branch)
        {
            return Err(ServerError::conflict_kind(
                "branch_collision",
                format!("branch {branch} already exists on this repository"),
            ));
        }
        let repo_slug = {
            let from_name = slugify(&repo.display_name);
            if from_name.is_empty() {
                slugify(&repo.root_path)
            } else {
                from_name
            }
        };
        let workspace_slug = {
            let from_title = slugify(&title);
            if from_title.is_empty() {
                worktree::two_word_name(id.0.as_u128())
            } else {
                from_title
            }
        };
        // Resolved per creation, not cached: the root is a setting an operator
        // can change while the process runs, and it decides only where the
        // *next* worktree lands. Existing workspaces keep the absolute path on
        // their row (`crate::code::worktree_root`).
        let root = self.owner_worktree_root(owner).await?;
        let path = worktree_dir(&root, id, &repo_slug, &workspace_slug);
        let display_title = if title.is_empty() {
            workspace_slug.clone()
        } else {
            title
        };
        let base = base_ref
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| repo.default_base_ref.clone());
        let mut workspace = CodeWorkspace {
            id,
            owner: owner.clone(),
            repo_id,
            title: display_title,
            worktree_path: path.display().to_string(),
            branch_name: branch.clone(),
            base_ref: base.clone(),
            status: CodeWorkspaceStatus::Creating,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        };
        insert_workspace(&self.db, &workspace).await?;
        let operation =
            match create_worktree(std::path::Path::new(&repo.root_path), &path, &branch, &base)
                .await
            {
                Ok(operation) => operation,
                Err(err) => {
                    let _ = delete_workspace(&self.db, owner, id).await;
                    return Err(map_worktree(err));
                }
            };
        // Before the setup script, which may itself commit: from here on,
        // anything this workspace commits should already carry the right
        // name.
        self.name_workspace_author(owner, &path).await;
        match run_setup_script(
            &path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await
        {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                match self.save_workspace_final(&workspace).await {
                    Ok(true) => operation.complete().await,
                    Ok(false) => {
                        operation.rollback().await;
                        return Err(ServerError::not_found(format!(
                            "workspace {} not found",
                            workspace.id
                        )));
                    }
                    Err(error) => {
                        operation.rollback().await;
                        return Err(error);
                    }
                }
                gh::run_auto_create_actions(&path, &repo.quick_actions).await;
                Ok(workspace)
            }
            Err(err) => {
                workspace.status = CodeWorkspaceStatus::SetupFailed;
                match self.save_workspace_final(&workspace).await {
                    Ok(true) => operation.complete().await,
                    Ok(false) => {
                        operation.rollback().await;
                        return Err(ServerError::not_found(format!(
                            "workspace {} not found",
                            workspace.id
                        )));
                    }
                    Err(error) => {
                        operation.rollback().await;
                        return Err(error);
                    }
                }
                Err(ServerError::unprocessable_kind(
                    "setup_failed",
                    err.to_string(),
                ))
            }
        }
    }

    pub(super) async fn save_workspace_final(
        &self,
        workspace: &CodeWorkspace,
    ) -> Result<bool, ServerError> {
        #[cfg(test)]
        if self
            .fail_next_workspace_final_save
            .swap(false, Ordering::SeqCst)
        {
            return Err(ServerError::internal(
                "injected workspace lifecycle persistence failure",
            ));
        }
        Ok(save_workspace(&self.db, workspace).await?)
    }

    pub(crate) async fn list_workspaces(
        &self,
        owner: &OwnerId,
        repo_id: Option<RepoId>,
    ) -> Result<Vec<CodeWorkspace>, ServerError> {
        Ok(list_workspaces(&self.db, owner, repo_id).await?)
    }

    pub(crate) async fn get_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        get_workspace(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("workspace {id} not found")))
    }

    pub(crate) async fn save_workspace(
        &self,
        workspace: &CodeWorkspace,
    ) -> Result<(), ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(workspace.id);
        let _lifecycle_guard = lifecycle.lock().await;
        let current = self.get_workspace(&workspace.owner, workspace.id).await?;
        if current.status != workspace.status {
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                format!(
                    "workspace became {} before the update was saved",
                    current.status.as_str()
                ),
            ));
        }
        if !save_workspace(&self.db, workspace).await? {
            return Err(ServerError::not_found(format!(
                "workspace {} not found",
                workspace.id
            )));
        }
        crate::code::attention::emit_workspace_digests(
            &self.db,
            &self.bus,
            &workspace.owner,
            workspace.id,
        )
        .await;
        Ok(())
    }

    /// Rename the untouched placeholder branch after background titling names
    /// the workspace. Every guard fails closed and leaves the original branch.
    pub(crate) async fn rename_generated_workspace_branch(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        title: &str,
    ) -> Result<bool, ServerError> {
        if !naming_settings::auto_rename_branches(&*self.db, owner).await? {
            return Ok(false);
        }
        let turn = self.worktree_turn_lock(id);
        let _turn_guard = turn.lock().await;
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, id).await?;
        if workspace.is_remote()
            || workspace.status != CodeWorkspaceStatus::Active
            || workspace.pr.is_some()
            || workspace.title != title
        {
            return Ok(false);
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        let expected = branch_name(&repo.branch_prefix, "", id.0.as_u128());
        if workspace.branch_name != expected {
            return Ok(false);
        }
        let next = branch_name(&repo.branch_prefix, title, id.0.as_u128());
        if next == expected {
            return Ok(false);
        }
        let path = std::path::Path::new(&workspace.worktree_path);
        if !rename_local_only_branch(path, &expected, &next)
            .await
            .map_err(map_worktree)?
        {
            return Ok(false);
        }
        if !set_workspace_branch_if(&self.db, owner, id, title, &expected, &next).await? {
            if let Err(error) = rename_local_only_branch(path, &next, &expected).await {
                tracing::error!(
                    workspace = %id,
                    error = %error,
                    "could not restore a branch after its workspace update lost the race"
                );
            }
            return Ok(false);
        }
        crate::code::attention::emit_workspace_digests(&self.db, &self.bus, owner, id).await;
        Ok(true)
    }

    pub(crate) async fn archive_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
        force: bool,
        terminals: &crate::code::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Archived {
            return Ok(workspace);
        }
        let path = std::path::PathBuf::from(&workspace.worktree_path);
        if workspace.status == CodeWorkspaceStatus::Archiving && !path.exists() {
            let sessions = list_sessions_for_workspace(&self.db, owner, id).await?;
            if sessions.iter().any(|session| {
                matches!(
                    session.lifecycle,
                    CodeSessionLifecycle::Running | CodeSessionLifecycle::Fenced
                )
            }) {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    "a workspace worker may still be running",
                ));
            }
            let repo = self.get_repo(owner, workspace.repo_id).await?;
            let archived = self.finalize_removed_workspace(workspace, &repo).await?;
            self.forget_workspace_turn_lock(archived.id);
            return Ok(archived);
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        // Blockers first: a refused archive must leave the workspace exactly as
        // it was, and running the hook script is not "exactly as it was".
        self.refuse_running_sessions(owner, id, force).await?;
        if path.exists() && !force {
            if let Some(block) = archive_blockers(&path, &workspace.base_ref)
                .await
                .map_err(map_worktree)?
            {
                return Err(ServerError::conflict_kind(
                    block.as_str(),
                    "workspace has uncommitted or unpushed work; pass force to discard it",
                ));
            }
        }
        {
            let lifecycle = self.workspace_lifecycle_lock(id);
            let _lifecycle_guard = lifecycle.lock().await;
            workspace = self.get_workspace(owner, id).await?;
            if workspace.status != CodeWorkspaceStatus::Active {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    format!("workspace is {}", workspace.status.as_str()),
                ));
            }
            self.refuse_running_sessions(owner, id, force).await?;
            if !compare_and_set_workspace_status(
                &self.db,
                owner,
                id,
                CodeWorkspaceStatus::Active,
                CodeWorkspaceStatus::Archiving,
            )
            .await?
            {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_busy",
                    "another workspace lifecycle operation started first",
                ));
            }
        }
        workspace.status = CodeWorkspaceStatus::Archiving;

        let archived = self
            .archive_workspace_exclusive(owner, workspace, repo, force, terminals)
            .await;
        if archived
            .as_ref()
            .is_err_and(|error| archive_failure_can_reopen(error) && path.exists())
            && !compare_and_set_workspace_status(
                &self.db,
                owner,
                id,
                CodeWorkspaceStatus::Archiving,
                CodeWorkspaceStatus::Active,
            )
            .await?
        {
            tracing::warn!(
                workspace = %id,
                "code-mode: failed to restore Active after a refused archive"
            );
        }
        archived
    }

    pub(super) async fn archive_workspace_exclusive(
        &self,
        owner: &OwnerId,
        workspace: CodeWorkspace,
        repo: CodeRepo,
        force: bool,
        terminals: &crate::code::terminal::TerminalHub,
    ) -> Result<CodeWorkspace, ServerError> {
        if !terminals.close_workspace_and_wait(workspace.id).await {
            return Err(ServerError::conflict_kind(
                "terminal_shutdown_timeout",
                "a workspace terminal did not stop; the checkout was preserved",
            ));
        }
        let workers_stopped = self.end_workspace_sessions(owner, workspace.id).await?;
        #[cfg(test)]
        let workers_stopped =
            workers_stopped && !self.archive_shutdown_timeout.load(Ordering::SeqCst);
        if !workers_stopped {
            return Err(ServerError::conflict_kind(
                "workspace_shutdown_timeout",
                "a workspace worker did not stop; the checkout was preserved",
            ));
        }

        if workspace.is_remote() {
            let archived_at = Utc::now();
            if !complete_workspace_archive(&self.db, owner, workspace.id, archived_at).await? {
                let current = self.get_workspace(owner, workspace.id).await?;
                if current.status == CodeWorkspaceStatus::Archived {
                    return Ok(current);
                }
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_changed",
                    "the workspace row changed before archive completed",
                ));
            }
            let mut workspace = workspace;
            workspace.status = CodeWorkspaceStatus::Archived;
            workspace.archived_at = Some(archived_at);
            crate::code::attention::emit_workspace_digests(
                &self.db,
                &self.bus,
                owner,
                workspace.id,
            )
            .await;
            return Ok(workspace);
        }

        let turn = self.worktree_turn_lock(workspace.id);
        let _turn_guard = turn.lock().await;
        let current = self.get_workspace(owner, workspace.id).await?;
        if current.status != CodeWorkspaceStatus::Archiving {
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                format!(
                    "workspace became {} during archive",
                    current.status.as_str()
                ),
            ));
        }

        let path = std::path::Path::new(&workspace.worktree_path);
        if path.exists() {
            // Decision 0032: the archive script obeys the same
            // failure-preserves rule as setup. Lifecycle exclusion starts
            // before the hook so no in-process writer can overlap it.
            if let Err(err) = run_archive_script(
                path,
                std::path::Path::new(&repo.root_path),
                &workspace.title,
                repo.archive_script.as_deref(),
            )
            .await
            {
                return Err(ServerError::unprocessable_kind(
                    "archive_script_failed",
                    err.to_string(),
                ));
            }
            if !force {
                if let Some(block) = archive_blockers(path, &workspace.base_ref)
                    .await
                    .map_err(map_worktree)?
                {
                    return Err(ServerError::conflict_kind(
                        block.as_str(),
                        "workspace changed during archive; the checkout was preserved",
                    ));
                }
            }
        }

        remove_worktree(std::path::Path::new(&repo.root_path), path)
            .await
            .map_err(map_worktree)?;
        let archived = self.finalize_removed_workspace(workspace, &repo).await?;
        drop(_turn_guard);
        self.forget_workspace_turn_lock(archived.id);
        Ok(archived)
    }

    pub(super) async fn finalize_removed_workspace(
        &self,
        mut workspace: CodeWorkspace,
        repo: &CodeRepo,
    ) -> Result<CodeWorkspace, ServerError> {
        let _ = prune_worktrees(std::path::Path::new(&repo.root_path)).await;
        if let Err(error) =
            delete_workspace_refs(std::path::Path::new(&repo.root_path), workspace.id).await
        {
            tracing::warn!(
                workspace = %workspace.id,
                "code-mode: could not delete checkpoint refs on archive: {error}"
            );
        }
        let archived_at = workspace.archived_at.unwrap_or_else(Utc::now);
        if !workspace.is_remote() {
            workspace = self
                .release_workspace_after_removal(workspace, repo)
                .await?;
        } else {
            workspace.status = CodeWorkspaceStatus::Archived;
            workspace.archived_at = Some(archived_at);
            if !complete_workspace_archive(&self.db, &workspace.owner, workspace.id, archived_at)
                .await?
            {
                return Err(ServerError::conflict_kind(
                    "workspace_lifecycle_changed",
                    "the workspace row changed before archive completed",
                ));
            }
            crate::code::attention::emit_workspace_digests(
                &self.db,
                &self.bus,
                &workspace.owner,
                workspace.id,
            )
            .await;
            return Ok(workspace);
        }
        #[cfg(test)]
        if self
            .fail_next_workspace_release_metadata
            .swap(false, Ordering::SeqCst)
        {
            return Err(ServerError::conflict_kind(
                "workspace_release_metadata_failed",
                "injected workspace release metadata failure",
            ));
        }
        if !complete_workspace_release(
            &self.db,
            &workspace.owner,
            workspace.id,
            workspace.released_at.unwrap_or(archived_at),
            workspace.released_tip.clone(),
            workspace.bundle_bytes,
        )
        .await?
        {
            return Err(ServerError::conflict_kind(
                "workspace_lifecycle_changed",
                "the workspace row changed before release completed",
            ));
        }
        let repo_root = std::path::Path::new(&repo.root_path);
        if worktree::branch_exists(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?
        {
            worktree::delete_branch(repo_root, &workspace.branch_name)
                .await
                .map_err(map_worktree)?;
        }
        crate::code::attention::emit_workspace_digests(
            &self.db,
            &self.bus,
            &workspace.owner,
            workspace.id,
        )
        .await;
        Ok(workspace)
    }

    /// Bundle a local branch after archive removes its checkout.
    pub(super) async fn release_workspace_after_removal(
        &self,
        mut workspace: CodeWorkspace,
        repo: &CodeRepo,
    ) -> Result<CodeWorkspace, ServerError> {
        let repo_root = std::path::Path::new(&repo.root_path);
        if worktree::branch_exists(repo_root, &workspace.branch_name)
            .await
            .map_err(map_worktree)?
        {
            let tip = worktree::branch_tip(repo_root, &workspace.branch_name)
                .await
                .map_err(map_worktree)?;
            let bundle = worktree::bundle_path(&self.data_dir, &workspace.id.0);
            let bytes = worktree::create_bundle(
                repo_root,
                &workspace.base_ref,
                &workspace.branch_name,
                &bundle,
            )
            .await
            .map_err(map_worktree)?;
            if bytes == 0 {
                workspace.released_tip = Some(tip);
                workspace.bundle_bytes = None;
            } else {
                workspace.released_tip = Some(tip);
                workspace.bundle_bytes = Some(i64::try_from(bytes).unwrap_or(i64::MAX));
            }
        } else {
            workspace.released_tip = None;
            workspace.bundle_bytes = None;
        }

        let archived_at = workspace.archived_at.unwrap_or_else(Utc::now);
        workspace.status = CodeWorkspaceStatus::Released;
        workspace.archived_at = Some(archived_at);
        workspace.released_at = Some(archived_at);
        Ok(workspace)
    }

    pub(super) fn forget_workspace_turn_lock(&self, workspace_id: WorkspaceId) {
        self.worktree_turns
            .lock()
            .expect("worktree turn locks")
            .remove(&workspace_id);
    }

    /// Restore a saved workspace. A local archive records the exact tip,
    /// bundles commits beyond the base when needed, and drops the ref. Restore
    /// rebuilds the branch from that recovery data. A remote archive has no
    /// host checkout or branch and keeps its remote recovery path.
    /// Drop a restored workspace's release bookkeeping.
    ///
    /// The bundle stays until the row carrying these cleared fields is durable.
    pub(super) fn clear_release(workspace: &mut CodeWorkspace) {
        if workspace.released_at.is_none() && workspace.bundle_bytes.is_none() {
            return;
        }
        workspace.released_at = None;
        workspace.released_tip = None;
        workspace.bundle_bytes = None;
    }

    /// Remove a restored workspace's bundle after its final row is durable.
    pub(super) fn remove_release_bundle(workspace: &CodeWorkspace, data_dir: &std::path::Path) {
        let bundle = worktree::bundle_path(data_dir, &workspace.id.0);
        if let Err(error) = std::fs::remove_file(&bundle) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    workspace = %workspace.id,
                    "code-mode: could not remove restored bundle {}: {error}",
                    bundle.display()
                );
            }
        }
    }

    pub(crate) async fn restore_workspace(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        let released = workspace.status == CodeWorkspaceStatus::Released;
        if !released && workspace.status != CodeWorkspaceStatus::Archived {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        let repo_root = std::path::Path::new(&repo.root_path);
        let path = std::path::Path::new(&workspace.worktree_path);
        let operation = if released {
            let released_tip = workspace.released_tip.as_deref().ok_or_else(|| {
                ServerError::conflict_kind(
                    "released_tip_missing",
                    "the released workspace has no recorded commit; its bundle was preserved",
                )
            })?;
            if workspace.bundle_bytes.is_some() {
                let bundle = worktree::bundle_path(&self.data_dir, &workspace.id.0);
                worktree::restore_released_worktree(
                    repo_root,
                    path,
                    &workspace.branch_name,
                    &bundle,
                    released_tip,
                )
                .await
                .map_err(map_worktree)?
            } else {
                worktree::restore_released_tip_worktree(
                    repo_root,
                    path,
                    &workspace.branch_name,
                    released_tip,
                )
                .await
                .map_err(map_worktree)?
            }
        } else {
            if !worktree::branch_exists(repo_root, &workspace.branch_name)
                .await
                .map_err(map_worktree)?
            {
                return Err(ServerError::conflict_kind(
                    "branch_missing",
                    format!(
                        "branch {} no longer exists; create a new workspace instead",
                        workspace.branch_name
                    ),
                ));
            }
            worktree::restore_worktree(repo_root, path, &workspace.branch_name)
                .await
                .map_err(map_worktree)?
        };
        // Mirror create's tail exactly: setup decides between Active and
        // SetupFailed, and a failing script preserves the checkout
        // (Decision 0032's failure-preserves rule). One vocabulary for both
        // paths — a reader debugging "setup_failed" should not need to know
        // whether the workspace was created or restored.
        let setup = run_setup_script(
            path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await;
        workspace.status = if setup.is_ok() {
            CodeWorkspaceStatus::Active
        } else {
            CodeWorkspaceStatus::SetupFailed
        };
        workspace.archived_at = None;
        if released {
            Self::clear_release(&mut workspace);
        }
        match self.save_workspace_final(&workspace).await {
            Ok(true) => {}
            Ok(false) => {
                operation.rollback().await;
                return Err(ServerError::not_found(format!(
                    "workspace {} not found",
                    workspace.id
                )));
            }
            Err(error) => {
                operation.rollback().await;
                return Err(error);
            }
        }
        operation.complete().await;
        if released {
            Self::remove_release_bundle(&workspace, &self.data_dir);
        }
        match setup {
            Ok(()) => Ok(workspace),
            Err(error) => Err(ServerError::unprocessable_kind(
                "setup_failed",
                error.to_string(),
            )),
        }
    }

    /// Re-run the setup script on a worktree that already exists.
    ///
    /// A failed setup keeps the checkout (Decision 0032), but every other
    /// route refuses a `setup_failed` workspace, so the state has no exit
    /// short of archiving the work. This is that exit: fix the script, run it
    /// again, and the workspace goes Active without cutting a second worktree.
    ///
    /// Both outcomes match create's tail — Active on success, still
    /// `SetupFailed` and a 422 `setup_failed` on failure.
    pub(crate) async fn retry_workspace_setup(
        &self,
        owner: &OwnerId,
        id: WorkspaceId,
    ) -> Result<CodeWorkspace, ServerError> {
        let lifecycle = self.workspace_lifecycle_lock(id);
        let _lifecycle_guard = lifecycle.lock().await;
        let mut workspace = self.get_workspace(owner, id).await?;
        if workspace.status == CodeWorkspaceStatus::Active {
            return Ok(workspace);
        }
        if workspace.status != CodeWorkspaceStatus::SetupFailed {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let path = std::path::PathBuf::from(&workspace.worktree_path);
        if !path.exists() {
            return Err(ServerError::conflict_kind(
                "worktree_missing",
                "the worktree is gone; archive this workspace and restore it instead",
            ));
        }
        let repo = self.get_repo(owner, workspace.repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        match run_setup_script(
            &path,
            std::path::Path::new(&repo.root_path),
            &workspace.title,
            repo.setup_script.as_deref(),
        )
        .await
        {
            Ok(()) => {
                workspace.status = CodeWorkspaceStatus::Active;
                if !self.save_workspace_final(&workspace).await? {
                    return Err(ServerError::not_found(format!(
                        "workspace {} not found",
                        workspace.id
                    )));
                }
                gh::run_auto_create_actions(&path, &repo.quick_actions).await;
                Ok(workspace)
            }
            Err(error) => Err(ServerError::unprocessable_kind(
                "setup_failed",
                error.to_string(),
            )),
        }
    }

    /// Download the workspace pull request's failing job logs into private
    /// storage, and report where they landed.
    ///
    /// The fix-errors action calls this before it sends its prompt, so the
    /// agent opens a file instead of working out which job failed and asking
    /// GitHub for it. The digest is read fresh: fixing against the logs of a
    /// head that has already been superseded is worse than not attaching any.
    pub(crate) async fn workspace_check_logs(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<(Option<String>, ci_logs::WrittenCheckLogs), ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        let status = self.refresh_workspace_pr(owner, workspace_id).await?;
        let Some(pr) = status.pr else {
            return Err(ServerError::not_found(
                "no pull request found for this workspace",
            ));
        };
        let gh = gh::observe_gh(self.gh_search_path_owned().as_deref()).await;
        let binary = gh::require_gh_binary(&gh).map_err(map_gh)?;
        let private_root = crate::code::scratch::workspace_root(&self.data_dir, workspace.id)
            .map_err(|err| {
                ServerError::internal(format!("could not open private storage: {err}"))
            })?;
        let written = ci_logs::write_failing_check_logs(
            &private_root,
            &binary,
            pr.checks.as_deref().unwrap_or(&[]),
            pr.head_sha.as_deref(),
        )
        .await
        .map_err(|err| ServerError::internal(format!("could not write the check logs: {err}")))?;
        Ok((pr.head_sha, written))
    }

    pub(crate) async fn workspace_tree(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        query: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<String>, bool), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
        worktree::list_tree_paths(
            std::path::Path::new(&workspace.worktree_path),
            query,
            limit.unwrap_or(worktree::DEFAULT_TREE_LIMIT),
        )
        .await
        .map_err(map_worktree)
    }

    pub(crate) async fn workspace_search(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        query: &str,
        include: &str,
        exclude: &str,
        limit: Option<u32>,
    ) -> Result<(Vec<worktree::WorktreeSearchMatch>, bool), ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        worktree::search_worktree_contents(
            std::path::Path::new(&workspace.worktree_path),
            query,
            include,
            exclude,
            limit.unwrap_or(worktree::DEFAULT_SEARCH_LIMIT),
        )
        .await
        .map_err(map_worktree)
    }

    pub(crate) async fn workspace_files(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
    ) -> Result<(Vec<ChangedFile>, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let listed = list_changed_files(&worktree, &from, &to, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((listed.files, listed.truncated, listed.stat, turn))
    }

    pub(crate) async fn workspace_blob(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        path: &str,
    ) -> Result<worktree::WorktreeBlob, ServerError> {
        let workspace = self.require_live_workspace(owner, workspace_id).await?;
        worktree::read_worktree_file(std::path::Path::new(&workspace.worktree_path), path)
            .await
            .map_err(map_worktree)
    }

    pub(crate) async fn workspace_diff(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        turn_id: Option<CodeTurnId>,
        file: Option<&str>,
    ) -> Result<(String, bool, Diffstat, Option<CodeTurnId>), ServerError> {
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_remote",
                "this workspace's engine runs in a remote sandbox; there is no host worktree",
            ));
        }
        let (worktree, from, to, turn) = resolve_diff_range(&self.db, &workspace, turn_id)
            .await
            .map_err(map_checkpoint)?;
        let produced = produce_diff(&worktree, &from, &to, file, DiffBounds::default())
            .await
            .map_err(map_checkpoint)?;
        Ok((produced.diff, produced.truncated, produced.stat, turn))
    }

    pub(super) async fn refuse_running_sessions(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
        allow_running: bool,
    ) -> Result<(), ServerError> {
        if allow_running {
            return Ok(());
        }
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
        if sessions
            .iter()
            .any(|session| session.lifecycle == CodeSessionLifecycle::Running)
        {
            return Err(ServerError::conflict_kind(
                "session_running",
                "a session is still running in this workspace; pass force to end it",
            ));
        }
        Ok(())
    }

    /// End every session in a workspace.
    ///
    /// Returns whether every worker confirmed it stopped. A `false` means at
    /// least one is still running somewhere with its own handle on the
    /// workspace's turn lock.
    pub(super) async fn end_workspace_sessions(
        &self,
        owner: &OwnerId,
        workspace_id: WorkspaceId,
    ) -> Result<bool, ServerError> {
        let mut all_stopped = true;
        let sessions = list_sessions_for_workspace(&self.db, owner, workspace_id).await?;
        for mut session in sessions {
            if session.lifecycle == CodeSessionLifecycle::Ended {
                continue;
            }
            let handle = self
                .workers
                .lock()
                .expect("code workers")
                .remove(&session.id);
            let decision_gate = handle
                .as_ref()
                .map(|handle| handle.approval_decisions.clone());
            let _decision_guard = match decision_gate {
                Some(gate) => Some(gate.lock_owned().await),
                None => None,
            };
            self.revoke_browser_session(&session);
            // Mark the row ended before asking the worker to stop. A worker
            // interrupted mid-turn re-reads the row on its way round the loop
            // and leaves on its own when it finds the session ended, so one
            // `Shutdown` is enough however busy it was.
            session.lifecycle = CodeSessionLifecycle::Ended;
            session.child_pid = None;
            session.child_process_identity = None;
            session.fence_reason = None;
            crate::code::attention::persist_session(&self.db, &self.bus, &session).await?;
            let stopped = match handle {
                Some(handle) => Self::shut_down_worker(session.id, handle).await,
                None => true,
            };
            all_stopped &= stopped;
            // The outgoing worker still holds this epoch, so a persist during
            // the wait can overwrite Ended. Re-assert from a fresh load.
            let mut current = self.get_session(owner, session.id).await?;
            current.lifecycle = CodeSessionLifecycle::Ended;
            current.child_pid = None;
            current.child_process_identity = None;
            current.fence_reason = None;
            if !crate::code::attention::persist_session(&self.db, &self.bus, &current).await? {
                return Err(ServerError::conflict_kind(
                    "session_not_ended",
                    "the session did not stay ended after the worker stopped",
                ));
            }
            if stopped {
                crate::code::approval_sweep::abandon_for_restart(
                    &self.db,
                    &self.bus,
                    owner,
                    current.id,
                    current.spawn_epoch,
                )
                .await;
            } else {
                crate::code::approval_sweep::abandon_for_ended_session(
                    &self.db,
                    &self.bus,
                    owner,
                    current.id,
                    current.spawn_epoch,
                )
                .await;
            }
        }
        Ok(all_stopped)
    }

    /// The workspace a session binds, when it binds one.
    pub(crate) async fn session_workspace(
        &self,
        session: &CodeSession,
    ) -> Result<Option<CodeWorkspace>, ServerError> {
        match session.workspace_id {
            Some(workspace_id) => self
                .get_workspace(&session.owner, workspace_id)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    /// The workspace id a checkout-bound operation needs, or the conflict a
    /// session without one answers.
    pub(crate) fn require_workspace_id(session: &CodeSession) -> Result<WorkspaceId, ServerError> {
        session.workspace_id.ok_or_else(|| {
            ServerError::conflict_kind(
                "session_has_no_workspace",
                "this session runs without a workspace, so there is no checkout to act on",
            )
        })
    }

    /// The turn lock for a workspace, minted on first use.
    ///
    /// Every session in the workspace hands the same `Arc` to its worker, so
    /// the lock outlives any one session and a worker recovered after a
    /// restart rejoins the same queue. See record 55.
    pub(in crate::code) fn worktree_turn_lock(
        &self,
        workspace_id: WorkspaceId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.worktree_turns
            .lock()
            .expect("worktree turn locks")
            .entry(workspace_id)
            .or_default()
            .clone()
    }

    pub(crate) fn workspace_write_lock(
        &self,
        workspace_id: WorkspaceId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.workspace_lifecycle_lock(workspace_id)
    }

    pub(super) fn workspace_lifecycle_lock(
        &self,
        workspace_id: WorkspaceId,
    ) -> Arc<tokio::sync::Mutex<()>> {
        self.workspace_lifecycles
            .lock()
            .expect("workspace lifecycle locks")
            .entry(workspace_id)
            .or_default()
            .clone()
    }
}

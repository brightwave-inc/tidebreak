//! Registered repositories: register, read, save, and remove.

use super::*;

impl CodeRuntime {
    pub(crate) async fn register_repo(
        &self,
        owner: &OwnerId,
        root_path: PathBuf,
        metadata: RepoRegistration,
    ) -> Result<CodeRepo, ServerError> {
        let RepoRegistration {
            cloned_from,
            display_name,
            default_base_ref,
            branch_prefix,
            setup_script,
            archive_script,
            quick_actions,
        } = metadata;
        let validated = validate_repo_path(&root_path).await.map_err(map_worktree)?;
        let toplevel = validated.toplevel.display().to_string();
        let exact = get_repo_by_root_path(&self.db, owner, &toplevel).await?;
        #[cfg(windows)]
        let existing = match exact {
            Some(repo) => Some(repo),
            None => list_repos(&self.db, owner).await?.into_iter().find(|repo| {
                repo_paths_equivalent(std::path::Path::new(&repo.root_path), &validated.toplevel)
            }),
        };
        #[cfg(not(windows))]
        let existing = exact;
        if let Some(existing) = existing {
            return Err(ServerError::conflict_kind(
                "repo_already_registered",
                format!(
                    "repository {} is already registered as {}",
                    toplevel, existing.id
                ),
            ));
        }
        // Nested registrations of the same toplevel are already collapsed by
        // canonicalize + unique root_path. A path inside another registered
        // repo would resolve to the same toplevel.
        let name = display_name
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                validated
                    .toplevel
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "repo".into())
            });
        let default_branch_prefix = self.default_branch_prefix(owner).await;
        let configured_base = default_base_ref
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let default_base_ref = match worktree::resolve_default_base_ref(
            &validated.toplevel,
            configured_base.as_deref(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(WorktreeError::MissingBaseRef { .. }) => {
                configured_base.unwrap_or_else(|| "main".into())
            }
            Err(other) => return Err(map_worktree(other)),
        };
        let repo = CodeRepo {
            id: RepoId::new(),
            owner: owner.clone(),
            root_path: toplevel,
            display_name: name,
            default_base_ref,
            branch_prefix: branch_prefix
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(default_branch_prefix),
            setup_script,
            archive_script,
            quick_actions,
            created_at: Utc::now(),
            removed_at: None,
            cloned_from,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        };
        insert_repo(&self.db, &repo).await?;
        self.delivery_cache.invalidate_owner(owner);
        Ok(repo)
    }

    pub(crate) async fn list_repos(&self, owner: &OwnerId) -> Result<Vec<CodeRepo>, ServerError> {
        Ok(list_repos(&self.db, owner).await?)
    }

    pub async fn get_repo(&self, owner: &OwnerId, id: RepoId) -> Result<CodeRepo, ServerError> {
        get_repo(&self.db, owner, id)
            .await?
            .ok_or_else(|| ServerError::not_found(format!("repo {id} not found")))
    }

    pub(crate) async fn save_repo(&self, repo: &CodeRepo) -> Result<(), ServerError> {
        if !save_repo(&self.db, repo).await? {
            return Err(ServerError::not_found(format!(
                "repo {} not found",
                repo.id
            )));
        }
        Ok(())
    }

    /// Refuse work that materializes a checkout for a removed registration.
    ///
    /// Reading a removed repository stays allowed: its archived workspaces and
    /// their transcripts resolve through it, which is the reason the row is
    /// kept at all.
    pub(super) fn refuse_removed_repo(repo: &CodeRepo) -> Result<(), ServerError> {
        if repo.removed_at.is_some() {
            return Err(ServerError::conflict_kind(
                "repo_removed",
                "this repository registration was removed; register it again to start new work",
            ));
        }
        Ok(())
    }

    /// Remove a repository registration, keeping every archived workspace and
    /// transcript that hangs off it.
    ///
    /// The row survives on purpose. Hard-deleting it strands that history: on
    /// SQLite the workspace foreign key is not enforced, so the rows stay
    /// behind with nothing to reach them through, and on PostgreSQL it is
    /// enforced, so the delete fails against the very archived workspaces this
    /// path requires. Reclaiming the checkout on disk is a separate act with
    /// its own confirmation.
    pub(crate) async fn remove_repo(
        &self,
        owner: &OwnerId,
        id: RepoId,
        reclaim_checkout: bool,
    ) -> Result<(), ServerError> {
        let repo = self.get_repo(owner, id).await?;
        let workspaces = list_workspaces(&self.db, owner, Some(id)).await?;
        if workspaces.iter().any(|workspace| {
            !matches!(
                workspace.status,
                CodeWorkspaceStatus::Archived | CodeWorkspaceStatus::Released
            )
        }) {
            return Err(ServerError::conflict_kind(
                "repo_has_workspaces",
                "archive every workspace before removing the repository",
            ));
        }
        if reclaim_checkout {
            // Only a checkout Tidebreak cloned is Tidebreak's to delete. A
            // registration names a directory the user already had, and the
            // clone parent is a setting that moves, so there is no path test
            // that stays honest — the recorded origin is the only safe
            // signal.
            if repo.cloned_from.is_none() {
                return Err(ServerError::conflict_kind(
                    "checkout_not_reclaimable",
                    "Tidebreak did not clone this repository, so it will not delete the directory; \
                     remove the registration and delete the checkout yourself",
                ));
            }
            let root = PathBuf::from(&repo.root_path);
            // Re-validate rather than trusting the stored path: a row written
            // long ago must not turn into a recursive delete of whatever
            // occupies that path now.
            match validate_repo_path(&root).await {
                // `root_path` was stored as the canonical toplevel, so a
                // repository still rooted there resolves back to the same
                // path. Anything else — a nested checkout, a different repo
                // moved in — compares unequal and is left alone.
                Ok(validated) if validated.toplevel == root => {
                    tokio::fs::remove_dir_all(&root).await.map_err(|error| {
                        ServerError::internal(format!(
                            "could not remove the cloned checkout {}: {error}",
                            root.display()
                        ))
                    })?;
                    tracing::info!(repo = %repo.id, "code-mode: reclaimed a cloned checkout");
                }
                _ => {
                    return Err(ServerError::conflict_kind(
                        "checkout_not_a_repository",
                        format!(
                            "{} is no longer the git repository Tidebreak cloned; \
                             it was left alone",
                            root.display()
                        ),
                    ));
                }
            }
        }
        if !mark_repo_removed(&self.db, owner, id, Utc::now()).await? {
            return Err(ServerError::not_found(format!("repo {id} not found")));
        }
        self.delivery_cache.invalidate_owner(owner);
        Ok(())
    }
}

//! Where new code-mode worktrees land: one deployment setting, resolved.
//!
//! A worktree is user work — uncommitted code on a real branch — so it does
//! not belong in the disposable app-data directory the database and logs live
//! in. The root is therefore a setting an operator can point anywhere, with a
//! visible default supplied by the embedding
//! ([`tidebreak_core::Config::code_worktree_root_default`]) and the old
//! `<data_dir>/code/worktrees` as the fallback for headless deployments.
//!
//! The setting decides where the *next* workspace is created. Every existing
//! workspace keeps the absolute `worktree_path` recorded on its row: a git
//! worktree stores absolute paths in both its own `.git` file and the repo's
//! `.git/worktrees/*` entry, so moving one is a repair pass, not a rename.
//! Changing the root never touches a tree already on disk.

use std::path::{Path, PathBuf};

use tidebreak_core::{OwnerId, Store};

use super::clone::owner_dir;
use super::runtime::CodeRuntime;
use super::worktree::data_dir_worktree_root;
use crate::error::ServerError;
use crate::routes::code::CodeWorktreeRoot;

/// Deployment-wide setting naming the root. Absent means "use the default".
pub(crate) const WORKTREE_ROOT_SETTING: &str = "code_worktree_root";

impl CodeRuntime {
    /// The root the next workspace on this deployment is created under, before
    /// the per-owner segment.
    pub(crate) async fn worktree_root(&self) -> Result<PathBuf, ServerError> {
        Ok(match read_worktree_root(&*self.db).await? {
            Some(root) => PathBuf::from(root),
            None => self.default_worktree_root(),
        })
    }

    /// The root one owner's worktrees land under.
    ///
    /// The setting is one deployment-wide path, so a multi-user machine needs
    /// the same per-owner segment clones take ([`owner_dir`]) — otherwise two
    /// users' repositories of the same name share a folder. The local profile
    /// has exactly one owner and keeps the root itself. Existing workspaces do
    /// not use this derivation: they keep the absolute path stored on the row.
    pub(crate) async fn owner_worktree_root(
        &self,
        owner: &OwnerId,
    ) -> Result<PathBuf, ServerError> {
        Ok(owner_dir(&self.worktree_root().await?, owner))
    }

    /// What the root falls back to with no setting stored: the visible
    /// location the embedding named, or the app-data directory it always used.
    pub(crate) fn default_worktree_root(&self) -> PathBuf {
        self.worktree_root_default
            .clone()
            .unwrap_or_else(|| data_dir_worktree_root(&self.data_dir))
    }

    /// `GET /code/worktree-root`.
    pub(crate) async fn worktree_root_snapshot(&self) -> Result<CodeWorktreeRoot, ServerError> {
        let stored = read_worktree_root(&*self.db).await?;
        let default_root = self.default_worktree_root().display().to_string();
        Ok(CodeWorktreeRoot {
            effective_root: stored.clone().unwrap_or_else(|| default_root.clone()),
            root: stored,
            default_root,
        })
    }

    /// `PUT /code/worktree-root`. An absent or blank root clears the setting,
    /// returning the deployment to its default.
    pub(crate) async fn set_worktree_root(
        &self,
        root: Option<&str>,
    ) -> Result<CodeWorktreeRoot, ServerError> {
        match root.map(str::trim).filter(|value| !value.is_empty()) {
            None => self.db.delete_setting(WORKTREE_ROOT_SETTING).await?,
            Some(root) => {
                let root = PathBuf::from(root);
                validate_worktree_root(&root).await?;
                self.db
                    .set_setting(
                        WORKTREE_ROOT_SETTING,
                        &serde_json::json!(root.display().to_string()),
                    )
                    .await?;
            }
        }
        self.worktree_root_snapshot().await
    }
}

/// Refuse a root the deployment cannot create worktrees under, at the moment
/// it is set rather than at the first workspace that fails.
///
/// The directory need not exist yet — creating it is part of accepting it, so
/// the folder shows up where the user asked for it instead of appearing with
/// the first workspace.
async fn validate_worktree_root(root: &Path) -> Result<(), ServerError> {
    if !root.is_absolute() {
        return Err(ServerError::bad_request_kind(
            "worktree_root_relative",
            format!("worktree root {} must be an absolute path", root.display()),
        ));
    }
    match tokio::fs::metadata(root).await {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(ServerError::bad_request_kind(
            "worktree_root_not_dir",
            format!("worktree root {} is not a directory", root.display()),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(root).await.map_err(|err| {
                ServerError::bad_request_kind(
                    "worktree_root_unusable",
                    format!("could not create worktree root {}: {err}", root.display()),
                )
            })
        }
        Err(err) => Err(ServerError::bad_request_kind(
            "worktree_root_unusable",
            format!("could not read worktree root {}: {err}", root.display()),
        )),
    }
}

async fn read_worktree_root(store: &dyn Store) -> Result<Option<String>, ServerError> {
    Ok(store
        .get_setting(WORKTREE_ROOT_SETTING)
        .await?
        .and_then(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|root| !root.is_empty())
                .map(ToOwned::to_owned)
        }))
}

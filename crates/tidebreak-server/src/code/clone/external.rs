//! Start an owner-isolated checkout with the external grant’s GitHub identity.

use super::*;
use crate::obo_gateway::{GitCredentialLender, GitForgeAttributionRequest};
use std::sync::Arc;

impl CodeRuntime {
    /// Resolve the GitHub origin used by the instance's configured forge.
    pub fn workspace_repository_origin(
        repo: &tidebreak_core::CodeRepo,
    ) -> Result<String, ServerError> {
        let unknown = || {
            ServerError::conflict_kind(
                "repo_origin_unknown",
                "workspace repository access requires a recorded github.com origin",
            )
        };
        if !repo
            .origin_host
            .as_deref()
            .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
        {
            return Err(unknown());
        }
        let (Some(owner), Some(name)) = (repo.origin_owner.as_deref(), repo.origin_name.as_deref())
        else {
            return Err(unknown());
        };
        Self::canonical_external_repository(&format!("{owner}/{name}"))
    }

    /// Check the instance's GitHub App authority before admitting repository
    /// work, including a checkout that another channel already registered.
    pub async fn require_workspace_repository_access(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        origin: &str,
    ) -> Result<(), ServerError> {
        let grant = tidebreak_core::db::code::get_external_grant(&self.db, owner, grant_id)
            .await?
            .filter(|grant| grant.kind.is_workspace() && grant.revoked_at.is_none())
            .ok_or_else(|| ServerError::unauthorized("The workspace connection was revoked."))?;
        let origin = Self::canonical_external_repository(origin)?;
        let lender: Arc<dyn GitCredentialLender> = if let Some(external) = self
            .harness_llm()
            .and_then(|relay| relay.external_delegations().cloned())
        {
            external
                .for_grant(owner, grant.id)
                .await
                .map_err(ServerError::from)?
        } else {
            self.git_credentials().cloned().ok_or_else(|| {
                ServerError::conflict_kind(
                    "git_forge_refused",
                    git_forge_refusal_message(&GitForgeError::NoGitForge),
                )
            })?
        };
        // The gateway checks the selected installation for this exact repository.
        // Do not use the discovery list as an allowlist: large lists can be bounded.
        // The credential is discarded here; each git operation borrows its own.
        lender
            .git_credential(owner, &origin, GitForgeAttributionRequest::Installation)
            .await
            .map_err(|error| {
                ServerError::conflict_kind("git_forge_refused", git_forge_refusal_message(&error))
            })?;
        Ok(())
    }

    /// Resolve a registered repository or start its bounded clone with the
    /// adapter grant's identity. A later retry observes the completed job.
    pub async fn prepare_external_repository(
        self: &Arc<Self>,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        origin: &str,
        attribution: GitForgeAttributionRequest,
    ) -> Result<tidebreak_core::CodeRepo, ServerError> {
        if tidebreak_core::db::code::get_external_grant(&self.db, owner, grant_id)
            .await?
            .is_some_and(|grant| grant.kind.is_workspace())
        {
            self.require_workspace_repository_access(owner, grant_id, origin)
                .await?;
        }
        let _creation = self.clone_jobs.external_start_lock.lock().await;
        match self.repo_by_origin(owner, origin).await {
            Ok(repo) => return Ok(repo),
            Err(error) if error.kind() == "repo_unknown" => {}
            Err(error) => return Err(error),
        }
        if !self.chooses_clone_destination().await? {
            return self.repo_by_origin(owner, origin).await;
        }
        let origin = origin.to_ascii_lowercase();
        let previous = {
            let mut jobs = self.clone_jobs.jobs.lock().expect("clone jobs");
            prune_completed_jobs(&mut jobs, Instant::now());
            jobs.values()
                .find(|job| &job.owner == owner && job.external_origin.as_deref() == Some(&origin))
                .cloned()
        };
        if let Some(job) = previous {
            if let Some(error) = job.error {
                return Err(ServerError::conflict_kind("repository_clone_failed", error));
            }
            if job.done {
                return Err(ServerError::conflict_kind("repo_removed", "This repository registration was removed. Register it again before starting a session."));
            }
            return Err(preparing());
        }
        let lender: Option<Arc<dyn GitCredentialLender>> = if let Some(external) = self
            .harness_llm()
            .as_ref()
            .and_then(|relay| relay.external_delegations().cloned())
        {
            Some(
                external
                    .for_grant(owner, grant_id)
                    .await
                    .map_err(ServerError::from)?,
            )
        } else {
            self.git_credentials().cloned()
        };
        self.start_clone_as(
            owner,
            CloneRequest {
                url: None,
                github: Some(origin.clone()),
                parent_dir: None,
                name: Some(origin.replace('/', "--")),
            },
            Some(origin),
            attribution,
            lender,
        )
        .await?;
        Err(preparing())
    }
}

fn preparing() -> ServerError {
    ServerError::conflict_kind("repository_preparing", "Tidebreak is cloning this repository for the session owner. Your request resumes when the checkout is ready.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obo_gateway::test_support::FakeLender;

    #[tokio::test]
    async fn external_clone_is_owner_scoped_and_uses_the_requested_forge() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            tidebreak_core::DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let lender = Arc::new(FakeLender::refusing(GitForgeError::RepositoryNotInstalled));
        let runtime = Arc::new(
            CodeRuntime::new(db, dir.path().into(), None, None, None, None, None, None)
                .with_git_credentials(lender.clone())
                .with_clone_parent_default(dir.path().into()),
        );
        let owner = OwnerId::new("user:slack-service").unwrap();
        let grant = tidebreak_core::CodeGrantId::new();
        let first = runtime
            .prepare_external_repository(
                &owner,
                grant,
                "acme/tools",
                GitForgeAttributionRequest::Installation,
            )
            .await
            .unwrap_err();
        assert_eq!(first.kind(), "repository_preparing");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let error = runtime
                .prepare_external_repository(
                    &owner,
                    grant,
                    "ACME/Tools",
                    GitForgeAttributionRequest::Installation,
                )
                .await
                .unwrap_err();
            if error.kind() == "repository_clone_failed" {
                break;
            }
            assert!(Instant::now() < deadline, "clone did not finish");
            tokio::task::yield_now().await;
        }
        assert_eq!(lender.minted(), vec!["acme/tools"]);
        assert_eq!(
            lender.asked(),
            vec![GitForgeAttributionRequest::Installation]
        );
        assert!(runtime.list_repos(&owner).await.unwrap().is_empty());
        let other = OwnerId::new("user:another-person").unwrap();
        let error = runtime
            .prepare_external_repository(
                &other,
                grant,
                "acme/tools",
                GitForgeAttributionRequest::Person,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.kind(),
            "repository_preparing",
            "one owner's failed job must not be reused by another owner"
        );
        assert_eq!(runtime.clone_jobs.jobs.lock().unwrap().len(), 2);
    }
}

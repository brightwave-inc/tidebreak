//! Deployment-token git lending on a standalone self-host machine.
//!
//! A gateway-authenticated machine borrows per operation through
//! [`super::OboGateway`]. A standalone machine has no gateway, and used to
//! hand the process `GH_TOKEN` to every engine child. This lender holds that
//! token in the server and answers the same [`super::GitCredentialLender`]
//! seam, so clone, delivery, and the session loopback route borrow it and
//! the child never sees it.

use async_trait::async_trait;
use tidebreak_core::OwnerId;

use super::{
    GitCredential, GitCredentialLender, GitForgeAttribution, GitForgeAttributionRequest,
    GitForgeError, GitForgeIdentity, GitHubRepository,
};

/// Display name the add-repository sentence and the git card read when this
/// lender answers. Distinct from a gateway App name so the UI does not call
/// the token an App.
pub const STANDALONE_FORGE_APP_NAME: &str = "this machine";

/// What the UI names when the operator did not set `TIDEBREAK_GIT_BOT_LOGIN`.
pub const STANDALONE_BOT_LOGIN_FALLBACK: &str = "the deployment's GitHub account";

/// One standing GitHub token, lent per operation as `x-access-token`.
pub struct StaticGitCredentialLender {
    token: String,
    bot_login: Option<String>,
}

impl std::fmt::Debug for StaticGitCredentialLender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StaticGitCredentialLender")
            .field("bot_login", &self.bot_login)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl StaticGitCredentialLender {
    /// Hold `token` and, when set, the login the token belongs to.
    #[must_use]
    pub fn new(token: String, bot_login: Option<String>) -> Self {
        Self { token, bot_login }
    }

    /// Build from `GH_TOKEN` or `GITHUB_TOKEN`, plus optional
    /// `TIDEBREAK_GIT_BOT_LOGIN`. `None` when no token is set, so a
    /// standalone machine without one keeps today's uncredentialed git.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("GH_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .ok()
            .filter(|value| !value.is_empty())?;
        let bot_login = std::env::var("TIDEBREAK_GIT_BOT_LOGIN")
            .ok()
            .filter(|value| !value.is_empty());
        Some(Self::new(token, bot_login))
    }

    fn bot_login(&self) -> String {
        self.bot_login
            .clone()
            .unwrap_or_else(|| STANDALONE_BOT_LOGIN_FALLBACK.to_owned())
    }

    fn refuse_person(attribution: GitForgeAttributionRequest) -> Result<(), GitForgeError> {
        match attribution {
            GitForgeAttributionRequest::Person => Err(GitForgeError::PersonNotOffered),
            GitForgeAttributionRequest::Installation => Ok(()),
        }
    }
}

#[async_trait]
impl GitCredentialLender for StaticGitCredentialLender {
    async fn git_forge_identity(
        &self,
        _owner: &OwnerId,
        attribution: GitForgeAttributionRequest,
    ) -> Result<GitForgeIdentity, GitForgeError> {
        Self::refuse_person(attribution)?;
        Ok(GitForgeIdentity {
            app_name: STANDALONE_FORGE_APP_NAME.to_owned(),
            attribution: GitForgeAttribution::Bot {
                bot_login: Some(self.bot_login()),
            },
        })
    }

    async fn git_credential(
        &self,
        _owner: &OwnerId,
        _repository: &str,
        attribution: GitForgeAttributionRequest,
    ) -> Result<GitCredential, GitForgeError> {
        Self::refuse_person(attribution)?;
        Ok(GitCredential {
            username: "x-access-token".to_owned(),
            secret: self.token.clone(),
        })
    }

    async fn list_repositories(
        &self,
        _owner: &OwnerId,
        _attribution: GitForgeAttributionRequest,
    ) -> Result<Vec<GitHubRepository>, GitForgeError> {
        Err(GitForgeError::NoGitForge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner() -> OwnerId {
        OwnerId::new("user:alice").unwrap()
    }

    #[tokio::test]
    async fn identity_names_the_configured_login() {
        let lender = StaticGitCredentialLender::new("tok".into(), Some("ship-bot".into()));
        let identity = lender
            .git_forge_identity(&owner(), GitForgeAttributionRequest::Installation)
            .await
            .unwrap();
        assert_eq!(identity.app_name, STANDALONE_FORGE_APP_NAME);
        assert_eq!(
            identity.attribution,
            GitForgeAttribution::Bot {
                bot_login: Some("ship-bot".into()),
            }
        );
    }

    #[tokio::test]
    async fn identity_falls_back_when_no_login_is_set() {
        let lender = StaticGitCredentialLender::new("tok".into(), None);
        let identity = lender
            .git_forge_identity(&owner(), GitForgeAttributionRequest::Installation)
            .await
            .unwrap();
        assert_eq!(
            identity.attribution,
            GitForgeAttribution::Bot {
                bot_login: Some(STANDALONE_BOT_LOGIN_FALLBACK.into()),
            }
        );
    }

    #[tokio::test]
    async fn a_person_request_is_not_offered() {
        let lender = StaticGitCredentialLender::new("tok".into(), Some("ship-bot".into()));
        let identity = lender
            .git_forge_identity(&owner(), GitForgeAttributionRequest::Person)
            .await
            .unwrap_err();
        assert_eq!(identity, GitForgeError::PersonNotOffered);
        let credential = lender
            .git_credential(&owner(), "acme/demo", GitForgeAttributionRequest::Person)
            .await
            .unwrap_err();
        assert_eq!(credential, GitForgeError::PersonNotOffered);
    }

    #[tokio::test]
    async fn an_installation_borrow_returns_the_token_username_pair() {
        let lender = StaticGitCredentialLender::new("tok".into(), None);
        let credential = lender
            .git_credential(
                &owner(),
                "acme/demo",
                GitForgeAttributionRequest::Installation,
            )
            .await
            .unwrap();
        assert_eq!(credential.username, "x-access-token");
        assert_eq!(credential.secret, "tok");
    }

    #[tokio::test]
    async fn the_repository_list_keeps_the_local_path() {
        let lender = StaticGitCredentialLender::new("tok".into(), None);
        let listed = lender
            .list_repositories(&owner(), GitForgeAttributionRequest::Installation)
            .await
            .unwrap_err();
        assert_eq!(listed, GitForgeError::NoGitForge);
    }
}

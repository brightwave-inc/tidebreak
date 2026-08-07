//! ChatGPT subscription OAuth runtime for the OpenAI provider.
//!
//! Lighter than [`crate::gateway_runtime`]: one pending sign-in at a time,
//! vault-backed tokens, no model sync. Completing sign-in writes the
//! `ProviderCredential::Oauth` marker and enables the OpenAI provider.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use openwave_connectors::{
    ChatGptAuth, ChatGptAuthConfig, ChatGptConnection, ChatGptCredentialVault,
};
use openwave_core::{Result, SecretProvider, Store};
use openwave_router::BearerTokenSource;
use tokio::sync::Mutex;

use crate::error::ServerError;
use crate::providers::{self, ProviderCredential, ProviderKind};

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum SignInProgress {
    #[default]
    Idle,
    Pending {
        authorization_url: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Default)]
struct SignInState {
    progress: SignInProgress,
    waiter: Option<tokio::task::JoinHandle<()>>,
}

/// Process-local ChatGPT OAuth handle shared by provider routes and routing.
pub struct ChatGptRuntime {
    connection: Arc<ChatGptConnection>,
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    sign_in: Mutex<SignInState>,
    sign_in_generation: AtomicU64,
}

impl ChatGptRuntime {
    pub fn new(store: Arc<dyn Store>, secrets: Arc<dyn SecretProvider>) -> Result<Self> {
        let auth = ChatGptAuth::new(ChatGptAuthConfig::production())?;
        let vault = ChatGptCredentialVault::new(secrets.clone());
        Ok(Self {
            connection: Arc::new(ChatGptConnection::new(auth, vault)),
            store,
            secrets,
            sign_in: Mutex::new(SignInState::default()),
            sign_in_generation: AtomicU64::new(0),
        })
    }

    /// Route auth for the OpenAI ChatGPT OAuth route, or `None` when no
    /// session is stored.
    ///
    /// The token source borrows this runtime's connection rather than opening
    /// a second one over the same vault entry: refresh rotation and sign-out
    /// are serialized per connection, so two of them racing can push a stale
    /// refresh token at OpenAI or resurrect a session that sign-out just
    /// cleared.
    pub async fn route_auth(&self) -> Option<(Arc<dyn BearerTokenSource>, String)> {
        let account_id = self.connection.account_id().await.ok().flatten()?;
        let source: Arc<dyn BearerTokenSource> =
            Arc::new(ChatGptTokenSource(self.connection.clone()));
        Some((source, account_id))
    }

    /// Start browser sign-in; returns the URL to open.
    ///
    /// Callers must refuse managed profiles before invoking this — BYO OpenAI
    /// credentials (key or ChatGPT OAuth) are locked out there.
    pub async fn begin_sign_in(self: &Arc<Self>) -> Result<String, ServerError> {
        // A previous attempt still owns the fixed callback port, so it has to
        // be torn down before the new listener can bind.
        self.cancel_pending().await;
        let pending = self
            .connection
            .auth()
            .start_sign_in()
            .await
            .map_err(ServerError::from)?;
        let authorization_url = pending.authorization_url().to_string();

        let mut sign_in = self.sign_in.lock().await;
        let generation = self.sign_in_generation.fetch_add(1, Ordering::SeqCst) + 1;
        sign_in.progress = SignInProgress::Pending {
            authorization_url: authorization_url.clone(),
        };
        let runtime = self.clone();
        sign_in.waiter = Some(tokio::spawn(async move {
            let finished = pending.finish(SIGN_IN_TIMEOUT).await;
            let mut sign_in = runtime.sign_in.lock().await;
            if runtime.sign_in_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            sign_in.progress = match finished {
                Ok(session) => match runtime.persist_session(&session).await {
                    Ok(()) => SignInProgress::Idle,
                    Err(error) => SignInProgress::Failed {
                        message: error.message().to_string(),
                    },
                },
                Err(error) => SignInProgress::Failed {
                    message: error.to_string(),
                },
            };
        }));

        Ok(authorization_url)
    }

    /// Abandon any in-flight sign-in and wait for its callback listener to go
    /// away. Aborting alone is not enough: the port is only freed once the
    /// task has actually been dropped.
    async fn cancel_pending(&self) {
        self.sign_in_generation.fetch_add(1, Ordering::SeqCst);
        let mut sign_in = self.sign_in.lock().await;
        sign_in.progress = SignInProgress::Idle;
        if let Some(waiter) = sign_in.waiter.take() {
            waiter.abort();
            let _ = waiter.await;
        }
    }

    async fn persist_session(
        &self,
        session: &openwave_connectors::ChatGptAuthorizedSession,
    ) -> Result<(), ServerError> {
        self.connection
            .store_session(session)
            .await
            .map_err(ServerError::from)?;
        // Mutual exclusivity: OAuth marker replaces any API key. If either
        // follow-up write fails, clear the vault so `status` and routing never
        // see tokens without the Oauth marker that makes them usable.
        if let Err(error) = self.finish_persisted_session().await {
            let _ = self.connection.sign_out().await;
            return Err(error);
        }
        Ok(())
    }

    async fn finish_persisted_session(&self) -> Result<(), ServerError> {
        providers::write_credential(
            &*self.secrets,
            ProviderKind::Openai,
            &ProviderCredential::Oauth {},
        )
        .await?;
        let mut config = providers::read_config(&*self.store, ProviderKind::Openai).await?;
        config.enabled = true;
        providers::write_config(&*self.store, ProviderKind::Openai, &config).await?;
        Ok(())
    }

    /// Revoke best-effort, clear vault and Oauth marker.
    pub async fn sign_out(&self) -> Result<(), ServerError> {
        self.cancel_pending().await;
        self.connection
            .sign_out()
            .await
            .map_err(ServerError::from)?;
        if matches!(
            providers::read_credential(&*self.secrets, ProviderKind::Openai).await?,
            Some(ProviderCredential::Oauth {})
        ) {
            providers::delete_credential(&*self.secrets, ProviderKind::Openai).await?;
        }
        Ok(())
    }

    /// Pending / failed status for the OpenAI Providers row.
    pub async fn status(&self) -> ChatGptSignInStatus {
        let progress = self.sign_in.lock().await.progress.clone();
        // Same bar as routing: vault tokens alone are not a usable session.
        let signed_in = matches!(
            providers::read_credential(&*self.secrets, ProviderKind::Openai)
                .await
                .ok()
                .flatten(),
            Some(ProviderCredential::Oauth {})
        ) && openwave_connectors::has_stored_chatgpt_credentials(&*self.secrets)
            .await;
        match progress {
            SignInProgress::Pending { authorization_url } => ChatGptSignInStatus {
                signed_in,
                pending_authorization_url: Some(authorization_url),
                error: None,
            },
            SignInProgress::Failed { message } => ChatGptSignInStatus {
                signed_in,
                pending_authorization_url: None,
                error: Some(message),
            },
            SignInProgress::Idle => ChatGptSignInStatus {
                signed_in,
                pending_authorization_url: None,
                error: None,
            },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
pub struct ChatGptSignInStatus {
    pub signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending_authorization_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

struct ChatGptTokenSource(Arc<ChatGptConnection>);

#[async_trait::async_trait]
impl BearerTokenSource for ChatGptTokenSource {
    async fn bearer_token(&self) -> openwave_core::Result<String> {
        self.0.access_token().await
    }
}

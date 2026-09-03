//! ChatGPT subscription OAuth runtime for the OpenAI provider.
//!
//! Lighter than [`crate::gateway_runtime`]: one pending sign-in at a time,
//! vault-backed tokens, no model sync. Completing sign-in writes the
//! `ProviderCredential::Oauth` marker and enables the OpenAI provider.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::connectors::{
    ChatGptAuth, ChatGptAuthConfig, ChatGptConnection, ChatGptCredentialVault,
};
use tidebreak_core::{Result, SecretProvider, Store};
use tidebreak_router::BearerTokenSource;
use tokio::sync::Mutex;

use crate::error::ServerError;
use crate::providers::{self, ProviderCredential, ProviderKind};

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
const RECONNECT_REQUIRED_MESSAGE: &str =
    "Your ChatGPT session is no longer valid. Sign in with ChatGPT again.";

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
    credential_motion: Arc<Mutex<()>>,
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
            credential_motion: Arc::new(Mutex::new(())),
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
        match providers::chatgpt_reconnect_required(&*self.store).await {
            Ok(false) => {}
            Ok(true) => return None,
            Err(error) => {
                tracing::warn!(%error, "could not read ChatGPT credential health");
                return None;
            }
        }
        let account_id = self.connection.account_id().await.ok().flatten()?;
        let source: Arc<dyn BearerTokenSource> = Arc::new(ChatGptTokenSource {
            connection: self.connection.clone(),
            store: self.store.clone(),
            credential_motion: self.credential_motion.clone(),
        });
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
    pub(crate) async fn cancel_pending(&self) {
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
        session: &crate::connectors::ChatGptAuthorizedSession,
    ) -> Result<(), ServerError> {
        let _guard = self.credential_motion.lock().await;
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
        providers::clear_chatgpt_reconnect_required(&*self.store).await?;
        Ok(())
    }

    /// Revoke best-effort, clear vault and Oauth marker.
    pub async fn sign_out(&self) -> Result<(), ServerError> {
        self.cancel_pending().await;
        let _guard = self.credential_motion.lock().await;
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
        providers::clear_chatgpt_reconnect_required(&*self.store).await?;
        Ok(())
    }

    /// Pending / failed status for the OpenAI Providers row.
    pub async fn status(&self) -> ChatGptSignInStatus {
        let progress = self.sign_in.lock().await.progress.clone();
        let reconnect_required = match providers::chatgpt_reconnect_required(&*self.store).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error, "could not read ChatGPT credential health");
                true
            }
        };
        // Same bar as routing: vault tokens alone are not a usable session.
        let signed_in = matches!(
            providers::read_credential(&*self.secrets, ProviderKind::Openai)
                .await
                .ok()
                .flatten(),
            Some(ProviderCredential::Oauth {})
        ) && crate::connectors::has_stored_chatgpt_credentials(&*self.secrets)
            .await
            && !reconnect_required;
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
                error: reconnect_required.then(|| RECONNECT_REQUIRED_MESSAGE.to_owned()),
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

struct ChatGptTokenSource {
    connection: Arc<ChatGptConnection>,
    store: Arc<dyn Store>,
    credential_motion: Arc<Mutex<()>>,
}

#[async_trait::async_trait]
impl BearerTokenSource for ChatGptTokenSource {
    async fn bearer_token(&self) -> tidebreak_core::Result<String> {
        let _guard = self.credential_motion.lock().await;
        if providers::chatgpt_reconnect_required(&*self.store).await? {
            return Err(tidebreak_core::AgentError::SignInRequired(
                RECONNECT_REQUIRED_MESSAGE.to_owned(),
            ));
        }
        self.connection.access_token().await
    }

    async fn authentication_rejected(&self, bearer: &str) {
        let _guard = self.credential_motion.lock().await;
        match self.connection.stored_access_token_matches(bearer).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(%error, "could not compare the rejected ChatGPT credential");
                return;
            }
        }
        if let Err(error) = providers::mark_chatgpt_reconnect_required(&*self.store).await {
            tracing::warn!(%error, "could not mark the ChatGPT credential for reconnection");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};

    use tidebreak_core::{DbStore, SecretProvider};

    use super::*;

    #[derive(Default)]
    struct TestSecrets(StdMutex<HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        async fn set_secret(&self, key: &str, value: &str) -> tidebreak_core::Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        async fn delete_secret(&self, key: &str) -> tidebreak_core::Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_provider_rejection_requires_reconnect_without_deleting_the_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("chatgpt-health.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets = Arc::new(TestSecrets::default());
        providers::write_credential(
            &*secrets,
            ProviderKind::Openai,
            &ProviderCredential::Oauth {},
        )
        .await
        .unwrap();
        secrets
            .set_secret(
                crate::connectors::CHATGPT_SECRET_KEY,
                &serde_json::json!({
                    "access_token": "access",
                    "refresh_token": "refresh",
                    "account_id": "acct-test",
                    "expires_at_unix": 4_102_444_800_u64,
                })
                .to_string(),
            )
            .await
            .unwrap();
        providers::write_config(
            &*store,
            ProviderKind::Openai,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
        let runtime = ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap();
        let (source, _) = runtime
            .route_auth()
            .await
            .expect("the session routes first");

        source.authentication_rejected("access").await;

        assert!(runtime.route_auth().await.is_none());
        let status = runtime.status().await;
        assert!(!status.signed_in);
        assert_eq!(status.error.as_deref(), Some(RECONNECT_REQUIRED_MESSAGE));
        assert!(crate::connectors::has_stored_chatgpt_credentials(&*secrets).await);
        assert!(matches!(
            providers::read_credential(&*secrets, ProviderKind::Openai)
                .await
                .unwrap(),
            Some(ProviderCredential::Oauth {})
        ));
    }

    #[tokio::test]
    async fn a_stale_rejection_cannot_disable_a_replacement_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("chatgpt-stale-health.db").display()
            ))
            .await
            .unwrap(),
        );
        let secrets = Arc::new(TestSecrets::default());
        providers::write_credential(
            &*secrets,
            ProviderKind::Openai,
            &ProviderCredential::Oauth {},
        )
        .await
        .unwrap();
        let credentials = |access_token: &str| {
            serde_json::json!({
                "access_token": access_token,
                "refresh_token": "refresh",
                "account_id": "acct-test",
                "expires_at_unix": 4_102_444_800_u64,
            })
            .to_string()
        };
        secrets
            .set_secret(
                crate::connectors::CHATGPT_SECRET_KEY,
                &credentials("old-access"),
            )
            .await
            .unwrap();
        let runtime = ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap();
        let (source, _) = runtime.route_auth().await.expect("the old session routes");
        secrets
            .set_secret(
                crate::connectors::CHATGPT_SECRET_KEY,
                &credentials("new-access"),
            )
            .await
            .unwrap();

        source.authentication_rejected("old-access").await;

        assert!(!providers::chatgpt_reconnect_required(&*store)
            .await
            .unwrap());
        assert!(runtime.route_auth().await.is_some());
    }
}

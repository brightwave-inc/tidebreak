//! Re-home stored credentials so the running binary owns their keychain items.
//!
//! macOS grants keychain access on the code signature of the process that
//! *created* an item. An item created by a binary carrying the dev designated
//! requirement (`identifier "openwave-dev" and certificate leaf = H"…"`) stays
//! readable after a rebuild and from a binary at another path. An item created
//! by some earlier build is a different matter: the access prompt returns, and
//! because the dev certificate is self-signed with no team identifier, the
//! approval given at that prompt is pinned to the binary's cdhash — the next
//! rebuild invalidates it. Credentials stored before a machine had a stable dev
//! identity therefore prompt once per credential on every launch, forever.
//!
//! Rewriting each value from a signed binary is the durable repair: delete the
//! item, then store the value again so the new item belongs to the current
//! signature. Read the value first and hold it in memory, so a failure to
//! delete leaves the credential in place.

use crate::web_search::WebSearchProviderKind;
use openwave_code_execution::{DAYTONA_CREDENTIAL_KEY, E2B_CREDENTIAL_KEY};
use openwave_core::connected_app::ConnectedAppKind;
use openwave_core::{Result, SecretProvider, Store};

use crate::providers::{ProviderKind, LEGACY_ANTHROPIC_API_KEY};

/// Credential keys for the web-search providers. `credential_key` is a `const
/// fn`, so the keys resolve here rather than being spelled out again.
const WEB_SEARCH_CREDENTIAL_KEYS: &[&str] = &[
    web_search_credential_key(WebSearchProviderKind::Exa),
    web_search_credential_key(WebSearchProviderKind::Tavily),
    web_search_credential_key(WebSearchProviderKind::Brave),
];

/// One provider's fixed credential key, in const context.
///
/// A credential-free provider (a self-hosted instance) stores nothing and has
/// nothing to re-home, so listing one above is a compile-time error.
const fn web_search_credential_key(kind: WebSearchProviderKind) -> &'static str {
    match kind.credential_key() {
        Some(key) => key,
        None => panic!("web-search provider stores no credential"),
    }
}

/// Credential keys for the code-execution providers that take one (`Local`
/// runs in the host sandbox and has none).
const CODE_EXECUTION_CREDENTIAL_KEYS: &[&str] = &[E2B_CREDENTIAL_KEY, DAYTONA_CREDENTIAL_KEY];

/// What happened to one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RehomeOutcome {
    /// Nothing is stored under this key.
    Absent,
    /// The value was rewritten and read back unchanged.
    Rehomed,
    /// The value is still stored, and still owned by whatever created it.
    Skipped(String),
    /// The item was deleted but could not be written back: the credential is
    /// gone and has to be entered again.
    Lost(String),
}

/// Every key the application may store a credential under: the fixed
/// per-feature keys, plus one dynamic `connected_app.{id}.credential` key per
/// stored `rest_api` record that references a credential.
///
/// The dynamic keys are why this reads the store: they exist only as records,
/// so a static list cannot name them, and a re-home pass that skipped them
/// would silently leave every REST credential owned by the old signature. A
/// store that cannot be read fails the enumeration rather than shrinking it —
/// an incomplete key list is exactly the silent loss this exists to prevent.
pub async fn stored_secret_keys(store: &dyn Store) -> Result<Vec<String>> {
    let mut keys = static_secret_keys();
    for record in store.list_connected_apps().await? {
        if record.kind != ConnectedAppKind::RestApi {
            continue;
        }
        // Lenient on purpose: a definition written by a future shape fails
        // the closed parse, but its credential reference must still be
        // re-homed — so the reference is read out of the raw JSON instead.
        let Some(secret_name) = record
            .definition
            .get("credential")
            .and_then(|credential| credential.get("secret_name"))
            .and_then(|name| name.as_str())
        else {
            continue;
        };
        keys.push(secret_name.to_string());
    }
    Ok(keys)
}

/// The fixed keys features store credentials under, independent of profile
/// state.
fn static_secret_keys() -> Vec<String> {
    let mut keys: Vec<String> = ProviderKind::ALL
        .iter()
        .map(|kind| kind.credential_key())
        .collect();
    keys.push(LEGACY_ANTHROPIC_API_KEY.to_string());
    keys.extend(
        WEB_SEARCH_CREDENTIAL_KEYS
            .iter()
            .chain(CODE_EXECUTION_CREDENTIAL_KEYS)
            .map(|key| (*key).to_string()),
    );
    keys
}

/// Re-home every stored credential, reporting each key in the order of
/// [`stored_secret_keys`].
pub async fn rehome_secrets(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<Vec<(String, RehomeOutcome)>> {
    Ok(rehome_keys(secrets, stored_secret_keys(store).await?).await)
}

async fn rehome_keys(
    secrets: &dyn SecretProvider,
    keys: Vec<String>,
) -> Vec<(String, RehomeOutcome)> {
    let mut outcomes = Vec::new();
    for key in keys {
        let outcome = rehome_one(secrets, &key).await;
        outcomes.push((key, outcome));
    }
    outcomes
}

async fn rehome_one(secrets: &dyn SecretProvider, key: &str) -> RehomeOutcome {
    let value = match secrets.get_secret(key).await {
        Ok(Some(value)) => value,
        Ok(None) => return RehomeOutcome::Absent,
        Err(error) => return RehomeOutcome::Skipped(format!("could not read it: {error}")),
    };
    if let Err(error) = secrets.delete_secret(key).await {
        return RehomeOutcome::Skipped(format!("could not remove the old item: {error}"));
    }
    if let Err(error) = secrets.set_secret(key, &value).await {
        return RehomeOutcome::Lost(format!("could not store it again: {error}"));
    }
    match secrets.get_secret(key).await {
        Ok(Some(stored)) if stored == value => RehomeOutcome::Rehomed,
        Ok(_) => RehomeOutcome::Lost("it did not read back unchanged".to_string()),
        Err(error) => RehomeOutcome::Lost(format!("it could not be read back: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use openwave_core::Result;

    use super::*;

    #[derive(Default)]
    struct RecordingSecrets {
        values: Mutex<BTreeMap<String, String>>,
        ops: Mutex<Vec<String>>,
        fail_delete: bool,
    }

    #[async_trait]
    impl SecretProvider for RecordingSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.values.lock().unwrap().get(key).cloned())
        }

        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.ops.lock().unwrap().push(format!("set {key}"));
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.ops.lock().unwrap().push(format!("delete {key}"));
            if self.fail_delete {
                return Err(openwave_core::AgentError::Secret("denied".into()));
            }
            self.values.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// The repair only works if the item is removed before the value is stored
    /// again — an in-place update keeps the original item, and with it the
    /// ownership that makes macOS prompt.
    #[tokio::test]
    async fn stored_values_are_deleted_then_written_back_intact() {
        let key = ProviderKind::Anthropic.credential_key();
        let secrets = RecordingSecrets::default();
        secrets.set_secret(&key, "sk-ant-123").await.unwrap();
        secrets.ops.lock().unwrap().clear();

        let outcomes = rehome_keys(&secrets, static_secret_keys()).await;

        let ops = secrets.ops.lock().unwrap().clone();
        assert_eq!(ops, vec![format!("delete {key}"), format!("set {key}")]);
        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("sk-ant-123")
        );
        assert_eq!(
            outcomes
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, outcome)| outcome),
            Some(&RehomeOutcome::Rehomed)
        );
        assert!(outcomes
            .iter()
            .filter(|(candidate, _)| *candidate != key)
            .all(|(_, outcome)| *outcome == RehomeOutcome::Absent));
    }

    /// A credential we cannot remove is left alone rather than reported as
    /// repaired: the prompt will return, which is better than losing the value.
    #[tokio::test]
    async fn an_undeletable_credential_is_kept_and_reported() {
        let key = ProviderKind::Anthropic.credential_key();
        let secrets = RecordingSecrets {
            fail_delete: true,
            ..RecordingSecrets::default()
        };
        secrets.set_secret(&key, "sk-ant-123").await.unwrap();

        let outcomes = rehome_keys(&secrets, static_secret_keys()).await;

        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("sk-ant-123")
        );
        let outcome = outcomes
            .into_iter()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, outcome)| outcome)
            .unwrap();
        assert!(matches!(outcome, RehomeOutcome::Skipped(_)), "{outcome:?}");
    }

    /// A provider whose credential key is missing here would keep prompting
    /// with no way to repair it. `ProviderKind::ALL` and
    /// `WebSearchProviderKind::ALL` cover every variant, so adding a kind fails
    /// this test until its key is listed.
    #[test]
    fn every_credentialed_provider_kind_is_covered() {
        let keys = static_secret_keys();

        for kind in ProviderKind::ALL {
            assert!(keys.contains(&kind.credential_key()), "{kind:?}");
        }
        for kind in WebSearchProviderKind::ALL {
            // A credential-free provider stores nothing to re-home.
            let Some(expected) = kind.credential_key() else {
                continue;
            };
            assert!(keys.iter().any(|key| key == expected), "{kind:?}");
        }
        // `CodeExecutionProviderKind` is `#[non_exhaustive]`, so a match here
        // cannot stand in for coverage; assert the credentialed kinds' keys
        // directly (`Local` runs in the host sandbox and stores nothing).
        for expected in [E2B_CREDENTIAL_KEY, DAYTONA_CREDENTIAL_KEY] {
            assert!(keys.iter().any(|key| key == expected), "{expected}");
        }
        assert!(keys.iter().any(|key| key == LEGACY_ANTHROPIC_API_KEY));
    }
}

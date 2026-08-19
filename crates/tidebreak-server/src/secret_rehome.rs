//! Re-home stored credentials so the running binary owns their keychain items.
//!
//! macOS grants keychain access on the code signature of the process that
//! *created* an item. An item created by a binary carrying the dev designated
//! requirement (`identifier "tidebreak-dev" and certificate leaf = H"…"`) stays
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
//!
//! Two entry points run that rewrite. Server boot calls
//! [`rehome_once_per_binary`], which re-homes once per binary (an app update
//! always replaces the binary, so that is once per update) before any
//! credential consumer is constructed. And `tidebreak rehome-secrets` runs
//! [`rehome_secrets`] on demand — the manual repair for a dev machine whose
//! signing identity changed without a binary stamp change.

use crate::web_search::WebSearchProviderKind;
use tidebreak_code_execution::{DAYTONA_CREDENTIAL_KEY, E2B_CREDENTIAL_KEY};
use tidebreak_core::connected_app::ConnectedAppKind;
use tidebreak_core::{Result, SecretProvider, Store};

use crate::connectors::{CHATGPT_SECRET_KEY, GATEWAY_SECRET_KEY};
use crate::mcp_config::env_secret_key;
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
/// per-feature keys, plus the dynamic per-record keys — one
/// `connected_app.{id}.credential` per stored `rest_api` record that
/// references a credential, and one `mcp.{id}.env_v1` per stored
/// `mcp_server` record (present or not; a record with no stored values
/// simply reports [`RehomeOutcome::Absent`]).
///
/// The dynamic keys are why this reads the store: they exist only as records,
/// so a static list cannot name them, and a re-home pass that skipped them
/// would silently leave every REST credential and MCP server environment
/// owned by the old signature. A store that cannot be read fails the
/// enumeration rather than shrinking it — an incomplete key list is exactly
/// the silent loss this exists to prevent.
pub async fn stored_secret_keys(store: &dyn Store) -> Result<Vec<String>> {
    let mut keys = static_secret_keys();
    for record in store.list_connected_apps().await? {
        if record.kind == ConnectedAppKind::McpServer {
            keys.push(env_secret_key(record.id));
            continue;
        }
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
    // The OAuth sessions: the model gateway's `gateway.credentials_v1` and
    // the ChatGPT sign-in. These are the credentials an app update strands
    // most painfully — an unreadable session reads as "not connected" and
    // the user re-pairs — so they must not be missed here.
    keys.push(GATEWAY_SECRET_KEY.to_string());
    keys.push(CHATGPT_SECRET_KEY.to_string());
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

/// The settings key recording which binary last completed a re-home pass.
const REHOME_STAMP_SETTING: &str = "secret_rehome.binary_v1";

/// Identify the running binary for the once-per-binary stamp: the Cargo
/// version plus the executable's size and modification time. Keychain
/// ownership is pinned to the code signature, and every app update or dev
/// rebuild replaces the binary, so the artifact's own metadata is the
/// stamp that rolls exactly when ownership breaks — the workspace version
/// stays `0.0.0` outside tagged release builds and cannot serve alone.
fn binary_stamp() -> String {
    let exe = std::env::current_exe();
    let metadata = exe
        .as_ref()
        .ok()
        .and_then(|exe| std::fs::metadata(exe).ok());
    let (len, mtime) = metadata
        .and_then(|metadata| {
            let mtime = metadata.modified().ok()?;
            let mtime = mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .ok()?;
            Some((metadata.len(), mtime.as_secs()))
        })
        .unwrap_or((0, 0));
    format!("{}:{len}:{mtime}", env!("CARGO_PKG_VERSION"))
}

/// Re-home the stored credentials once per binary, at server boot.
///
/// macOS pins a keychain item's access to the code signature of the binary
/// that created it, so an app update strands every credential the previous
/// binary wrote — reads prompt or fail, and a gateway session that fails to
/// read presents as "not connected". Rewriting each value from the new
/// binary (what [`rehome_secrets`] does) restores ownership. This runs the
/// pass inline, before boot constructs any credential consumer, for two
/// reasons: the repair then takes effect in the same launch rather than the
/// next one, and it cannot interleave with a token refresh — a re-home that
/// raced a refresh could write back a pre-refresh session, which the
/// gateway's refresh-token reuse detection would read as a sign-out.
///
/// The pass is once per binary (see [`binary_stamp`]) and best-effort: a
/// failure — the store unreadable, a credential the user declined to
/// unlock — is logged, never surfaced, and leaves the stamp unwritten so
/// the next launch retries. A pass where every key resolved (stored or
/// not) stamps the binary so later boots of the same binary skip the work.
///
/// Returns whether the pass ran (`false` when this binary is already
/// stamped).
pub(crate) async fn rehome_once_per_binary(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<bool> {
    let stamp = binary_stamp();
    if store
        .get_setting(REHOME_STAMP_SETTING)
        .await?
        .as_ref()
        .and_then(serde_json::Value::as_str)
        == Some(stamp.as_str())
    {
        return Ok(false);
    }
    let outcomes = rehome_secrets(store, secrets).await?;
    let mut rehomed = 0usize;
    let mut unresolved: Vec<String> = Vec::new();
    for (key, outcome) in &outcomes {
        match outcome {
            RehomeOutcome::Absent => {}
            RehomeOutcome::Rehomed => rehomed += 1,
            RehomeOutcome::Skipped(reason) => {
                unresolved.push(format!("{key} (skipped: {reason})"));
            }
            RehomeOutcome::Lost(reason) => {
                unresolved.push(format!("{key} (LOST: {reason})"));
            }
        }
    }
    if rehomed > 0 {
        tracing::info!(
            "re-homed {rehomed} stored credential(s) so this binary owns their keychain items"
        );
    }
    if unresolved.is_empty() {
        store
            .set_setting(REHOME_STAMP_SETTING, &serde_json::Value::String(stamp))
            .await?;
    } else {
        // No stamp: the next launch retries. A skipped credential is usually
        // a dismissed keychain prompt — exactly the state this exists to
        // repair — so it gets another chance rather than prompting forever.
        tracing::warn!(
            "could not re-home every stored credential (retrying next launch): {}",
            unresolved.join(", ")
        );
    }
    Ok(true)
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
    use tidebreak_core::Result;

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
                return Err(tidebreak_core::AgentError::Secret("denied".into()));
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
        secrets
            .set_secret(&key, "test-anthropic-key")
            .await
            .unwrap();
        secrets.ops.lock().unwrap().clear();

        let outcomes = rehome_keys(&secrets, static_secret_keys()).await;

        let ops = secrets.ops.lock().unwrap().clone();
        assert_eq!(ops, vec![format!("delete {key}"), format!("set {key}")]);
        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("test-anthropic-key")
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
        secrets
            .set_secret(&key, "test-anthropic-key")
            .await
            .unwrap();

        let outcomes = rehome_keys(&secrets, static_secret_keys()).await;

        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("test-anthropic-key")
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

    /// The OAuth sessions are the credentials an app update strands most
    /// painfully — an unreadable session reads as "not connected" — so the
    /// gateway and ChatGPT session keys must be in the re-home list.
    #[test]
    fn connector_session_keys_are_covered() {
        let keys = static_secret_keys();
        assert!(keys.iter().any(|key| key == GATEWAY_SECRET_KEY));
        assert!(keys.iter().any(|key| key == CHATGPT_SECRET_KEY));
    }

    /// MCP server environment values live under per-record `mcp.{id}.env_v1`
    /// keys; a re-home pass that skipped them would leave every MCP
    /// credential owned by the old signature.
    #[tokio::test]
    async fn mcp_server_env_keys_are_enumerated() {
        let directory = tempfile::tempdir().unwrap();
        let store = tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("rehome.db").display()
        ))
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let record = tidebreak_core::connected_app::ConnectedApp {
            id: tidebreak_core::id::ConnectedAppId::new(),
            name: "docs".to_string(),
            kind: ConnectedAppKind::McpServer,
            definition: serde_json::json!({}),
            created_at: now,
            updated_at: now,
        };
        let expected = env_secret_key(record.id);
        store
            .replace_connected_apps(ConnectedAppKind::McpServer, &[record])
            .await
            .unwrap();

        let keys = stored_secret_keys(&store).await.unwrap();
        assert!(keys.contains(&expected), "{keys:?}");
    }

    /// Re-homing is idempotent: a second pass over an already re-homed value
    /// rewrites it again (the implementation cannot tell "already owned"
    /// apart cheaply, so it rewrites unconditionally) and the value reads
    /// back unchanged both times.
    #[tokio::test]
    async fn rehoming_twice_keeps_the_value_intact() {
        let key = ProviderKind::Anthropic.credential_key();
        let secrets = RecordingSecrets::default();
        secrets
            .set_secret(&key, "test-anthropic-key")
            .await
            .unwrap();

        for _ in 0..2 {
            let outcomes = rehome_keys(&secrets, static_secret_keys()).await;
            assert_eq!(
                outcomes
                    .iter()
                    .find(|(candidate, _)| *candidate == key)
                    .map(|(_, outcome)| outcome),
                Some(&RehomeOutcome::Rehomed)
            );
            assert_eq!(
                secrets.get_secret(&key).await.unwrap().as_deref(),
                Some("test-anthropic-key")
            );
        }
    }

    /// Boot runs the pass once per binary: the first call re-homes and
    /// stamps, and a later call for the same binary is a no-op.
    #[tokio::test]
    async fn boot_rehome_runs_once_then_skips() {
        let directory = tempfile::tempdir().unwrap();
        let store = tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("rehome.db").display()
        ))
        .await
        .unwrap();
        let key = ProviderKind::Anthropic.credential_key();
        let secrets = RecordingSecrets::default();
        secrets
            .set_secret(&key, "test-anthropic-key")
            .await
            .unwrap();

        assert!(rehome_once_per_binary(&store, &secrets).await.unwrap());
        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("test-anthropic-key")
        );

        secrets.ops.lock().unwrap().clear();
        assert!(!rehome_once_per_binary(&store, &secrets).await.unwrap());
        assert!(secrets.ops.lock().unwrap().is_empty());
    }

    /// A credential that could not be re-homed leaves the binary unstamped,
    /// so the next launch retries rather than leaving it stranded until the
    /// next update.
    #[tokio::test]
    async fn boot_rehome_retries_after_a_skipped_credential() {
        let directory = tempfile::tempdir().unwrap();
        let store = tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("rehome.db").display()
        ))
        .await
        .unwrap();
        let key = ProviderKind::Anthropic.credential_key();
        let secrets = RecordingSecrets {
            fail_delete: true,
            ..RecordingSecrets::default()
        };
        secrets
            .set_secret(&key, "test-anthropic-key")
            .await
            .unwrap();

        assert!(rehome_once_per_binary(&store, &secrets).await.unwrap());
        // Not stamped: the very next boot takes the pass again.
        secrets.ops.lock().unwrap().clear();
        assert!(rehome_once_per_binary(&store, &secrets).await.unwrap());
        assert!(!secrets.ops.lock().unwrap().is_empty());
    }
}

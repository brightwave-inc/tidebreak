//! A [`SecretProvider`] decorator that keeps every key in **one** stored item.
//!
//! macOS grants keychain access on the code signature of the binary that
//! *created* an item, so an app update strands every credential the previous
//! binary wrote. `secret_rehome` in `tidebreak-server` repairs that by
//! rewriting each value from the new binary — but the repair is per item, and
//! so is the prompt. A profile holding four credentials therefore cost roughly
//! eight approvals on every update, one pair per item, forever.
//!
//! [`CachingSecretProvider`](crate::secret_cache::CachingSecretProvider)
//! already removes *repeat* reads within a process. What it cannot remove is
//! the first read of each distinct item — that is one prompt per item, by
//! construction. The only way past it is to stop having many items.
//!
//! So the whole profile lives in one item ([`BUNDLE_KEY`]) holding a JSON
//! object of key → value. One item is one approval, whatever the profile
//! stores. Everything above this layer still names logical keys
//! (`provider.anthropic.credential`) and never learns where they sit.
//!
//! **Reading a key the bundle does not hold falls through to the per-key item
//! of the same name.** Migration is a boot-time pass
//! ([`BundledSecretProvider::absorb_legacy_items`]), and the shell's remote
//! attachment reads its token before that pass can have run — without the
//! fallback, one launch after an update would read as "not attached" and
//! silently drop the user back onto this computer. The fallback makes the
//! order stop mattering; migration then becomes cleanup rather than a
//! prerequisite.
//!
//! **Writes re-read before they merge.** The desktop builds two providers over
//! the same item — the server's and the shell's remote attachment — and
//! `tidebreak rehome-secrets` deliberately runs without the instance lock, so
//! a blind read-modify-write could drop a credential another writer had just
//! stored. Every write takes a process-wide lock, re-reads the item, applies
//! its change, and confirms the result read back; a store that changed
//! underneath is merged again rather than overwritten.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use futures::lock::Mutex as AsyncMutex;

use crate::error::{AgentError, Result};
use crate::storage::SecretProvider;

/// The single item every logical key is stored inside.
///
/// Namespaced so it cannot collide with a logical key. Those are
/// `provider.{kind}.credential`, `gateway.credentials_v1`,
/// `mcp.{uuid}.env_v1`, `connected_app.{uuid}.credential`, and friends — none
/// of which start with `tidebreak.`.
pub const BUNDLE_KEY: &str = "tidebreak.secret_bundle_v1";

/// The desktop shell's stored bearer for a machine attached with a static
/// token.
///
/// Declared here rather than beside its only writer (`tidebreak-desktop`'s
/// `remote` module) so the server's credential enumeration can name it. The
/// shell depends on the server, not the other way round, and a key the
/// enumeration cannot name is a key that never gets re-homed — which is
/// exactly what happened to this one.
pub const DESKTOP_REMOTE_MACHINE_TOKEN_KEY: &str = "desktop.remote-machine.token";

/// How many times a write re-merges before giving up.
///
/// Only a writer outside this process can force a retry, and only by landing
/// between this one's read and its read-back. Three attempts is far past what
/// that window can plausibly produce; exhausting them means something is
/// rewriting the item continuously, and failing loudly beats overwriting it.
const WRITE_ATTEMPTS: usize = 3;

/// Serializes bundle writes across every provider in this process.
///
/// One item, several providers over it: the server's and the shell's remote
/// attachment both wrap the same keychain service. Per-instance locking would
/// let those two interleave a read-modify-write and lose a key.
fn write_lock() -> &'static AsyncMutex<()> {
    // `futures`, not `tokio`: this module is available in every feature
    // configuration of the crate, and `tokio` is optional here.
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// What happened to one per-key item during
/// [`BundledSecretProvider::absorb_legacy_items`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsorbOutcome {
    /// No per-key item under this name; nothing to move.
    Absent,
    /// The value is in the bundle, and the per-key item is gone.
    Absorbed,
    /// The value is in the bundle, but the per-key item could not be removed.
    /// Harmless — the bundle wins on read — and retried next launch.
    CopiedNotRemoved(String),
    /// The value could not be read or could not be stored. It is still
    /// wherever it was; nothing was removed.
    Skipped(String),
}

/// What happened to the bundle item during
/// [`BundledSecretProvider::rehome_bundle_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RehomeItemOutcome {
    /// This profile stores nothing yet, so there is no item to re-home.
    Absent,
    /// The item was rewritten by this binary and read back unchanged.
    Rehomed,
    /// The item is still stored, and still owned by whatever created it. The
    /// prompt will return, which is better than losing the credentials.
    Skipped(String),
    /// The item was removed but could not be written back. Every credential in
    /// it has to be entered again.
    Lost(String),
}

/// Wraps a [`SecretProvider`], keeping every key inside one stored item.
pub struct BundledSecretProvider {
    inner: Arc<dyn SecretProvider>,
    /// The decoded item. `None` until first read.
    cache: Mutex<Option<BTreeMap<String, String>>>,
}

impl BundledSecretProvider {
    /// Store `inner`'s keys in a single item.
    #[must_use]
    pub fn new(inner: Arc<dyn SecretProvider>) -> Self {
        Self {
            inner,
            cache: Mutex::new(None),
        }
    }

    /// The decoded bundle, read through on first use.
    async fn load(&self) -> Result<BTreeMap<String, String>> {
        if let Some(hit) = self.cache.lock().unwrap().clone() {
            return Ok(hit);
        }
        let fresh = self.read_through().await?;
        *self.cache.lock().unwrap() = Some(fresh.clone());
        Ok(fresh)
    }

    /// Read and decode the item, ignoring the cache.
    async fn read_through(&self) -> Result<BTreeMap<String, String>> {
        let Some(raw) = self.inner.get_secret(BUNDLE_KEY).await? else {
            return Ok(BTreeMap::new());
        };
        serde_json::from_str(&raw).map_err(|error| {
            // Refuse rather than start over: an unreadable bundle is every
            // credential at once, and silently treating it as empty would
            // hand the reader a profile that looks freshly installed and then
            // overwrite the item they could have recovered from.
            AgentError::Secret(format!(
                "the stored credential bundle could not be read: {error}"
            ))
        })
    }

    /// Apply one change to the item, merging against whatever is stored now.
    async fn write(&self, change: impl Fn(&mut BTreeMap<String, String>)) -> Result<()> {
        let _guard = write_lock().lock().await;
        let mut last_seen = None;
        for _ in 0..WRITE_ATTEMPTS {
            // Deliberately not `load`: a write merges against the store, not
            // against whatever this process last happened to read.
            let stored = self.read_through().await?;
            let mut merged = stored.clone();
            change(&mut merged);
            if merged == stored {
                // A delete of a key that is not there, or a set of the value
                // already stored. Writing would only churn the item — and on
                // an empty profile it would create one that need not exist.
                *self.cache.lock().unwrap() = Some(merged);
                return Ok(());
            }
            let encoded = serde_json::to_string(&merged)
                .map_err(|error| AgentError::Secret(error.to_string()))?;
            self.inner.set_secret(BUNDLE_KEY, &encoded).await?;
            let confirmed = self.read_through().await?;
            if confirmed == merged {
                *self.cache.lock().unwrap() = Some(confirmed);
                return Ok(());
            }
            // Something wrote between the read and the read-back. Merging
            // again re-applies this change on top of theirs, which converges;
            // overwriting would drop whatever they stored.
            last_seen = Some(confirmed);
        }
        // Whatever the store holds now is the truth for the next reader, not
        // the value this call was trying to reach.
        *self.cache.lock().unwrap() = last_seen;
        Err(AgentError::Secret(format!(
            "another writer kept changing the credential bundle; \
             after {WRITE_ATTEMPTS} attempts this change could not be confirmed \
             as stored, and no other credential was overwritten"
        )))
    }

    /// Move any per-key items named by `keys` into the bundle, then remove
    /// them.
    ///
    /// The order is what makes this safe: a value is stored in the bundle and
    /// read back before its old item is touched. Today's per-item re-home
    /// deletes first and has a "deleted, then could not store it again" state
    /// where the credential is simply gone; this cannot reach that state. If
    /// the removal fails the value lives in both places, the bundle wins on
    /// read, and the next launch tries the removal again.
    pub async fn absorb_legacy_items(&self, keys: &[String]) -> Vec<(String, AbsorbOutcome)> {
        let mut outcomes = Vec::with_capacity(keys.len());
        for key in keys {
            outcomes.push((key.clone(), self.absorb_one(key).await));
        }
        outcomes
    }

    async fn absorb_one(&self, key: &str) -> AbsorbOutcome {
        if key == BUNDLE_KEY {
            return AbsorbOutcome::Absent;
        }
        let value = match self.inner.get_secret(key).await {
            Ok(Some(value)) => value,
            Ok(None) => return AbsorbOutcome::Absent,
            Err(error) => return AbsorbOutcome::Skipped(format!("could not read it: {error}")),
        };
        if let Err(error) = self
            .write(|bundle| {
                bundle.insert(key.to_string(), value.clone());
            })
            .await
        {
            return AbsorbOutcome::Skipped(format!("could not store it in the bundle: {error}"));
        }
        match self.read_through().await {
            Ok(bundle) if bundle.get(key) == Some(&value) => {}
            Ok(_) => {
                return AbsorbOutcome::Skipped(
                    "it did not read back from the bundle unchanged".to_string(),
                )
            }
            Err(error) => {
                return AbsorbOutcome::Skipped(format!(
                    "the bundle could not be read back: {error}"
                ))
            }
        }
        match self.inner.delete_secret(key).await {
            Ok(()) => AbsorbOutcome::Absorbed,
            Err(error) => AbsorbOutcome::CopiedNotRemoved(error.to_string()),
        }
    }

    /// Rewrite the bundle item so the running binary owns it.
    ///
    /// Delete-then-store is the whole point: an in-place update keeps the
    /// original item, and with it the ownership that makes macOS prompt. The
    /// value is held in memory across the gap, so a failed delete leaves the
    /// credentials exactly where they were.
    pub async fn rehome_bundle_item(&self) -> RehomeItemOutcome {
        let _guard = write_lock().lock().await;
        let stored = match self.inner.get_secret(BUNDLE_KEY).await {
            Ok(Some(stored)) => stored,
            Ok(None) => return RehomeItemOutcome::Absent,
            Err(error) => return RehomeItemOutcome::Skipped(format!("could not read it: {error}")),
        };
        if let Err(error) = self.inner.delete_secret(BUNDLE_KEY).await {
            return RehomeItemOutcome::Skipped(format!("could not remove the old item: {error}"));
        }
        if let Err(error) = self.inner.set_secret(BUNDLE_KEY, &stored).await {
            return RehomeItemOutcome::Lost(format!("could not store it again: {error}"));
        }
        match self.inner.get_secret(BUNDLE_KEY).await {
            Ok(Some(read_back)) if read_back == stored => RehomeItemOutcome::Rehomed,
            Ok(_) => RehomeItemOutcome::Lost("it did not read back unchanged".to_string()),
            Err(error) => RehomeItemOutcome::Lost(format!("it could not be read back: {error}")),
        }
    }

    /// Forget the decoded item, so the next read asks the store.
    #[cfg(test)]
    fn forget(&self) {
        *self.cache.lock().unwrap() = None;
    }
}

#[async_trait]
impl SecretProvider for BundledSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        if key == BUNDLE_KEY {
            return self.inner.get_secret(key).await;
        }
        if let Some(hit) = self.load().await?.get(key) {
            return Ok(Some(hit.clone()));
        }
        // Absent from the decoded item. Re-read before believing it: a value
        // can land in the item without this provider observing the write —
        // the other provider in this process, another process, or a sign-in
        // completing beside a boot-time read. Reading an item that is already
        // approved for this process raises no prompt.
        let fresh = self.read_through().await?;
        *self.cache.lock().unwrap() = Some(fresh.clone());
        if let Some(hit) = fresh.get(key) {
            return Ok(Some(hit.clone()));
        }
        // Still nothing: fall through to the per-key item this key used to
        // live in. See the module docs — this is what lets migration be
        // cleanup rather than a prerequisite.
        self.inner.get_secret(key).await
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        if key == BUNDLE_KEY {
            return Err(reserved_key());
        }
        self.write(|bundle| {
            bundle.insert(key.to_string(), value.to_string());
        })
        .await
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        if key == BUNDLE_KEY {
            return Err(reserved_key());
        }
        self.write(|bundle| {
            bundle.remove(key);
        })
        .await?;
        // The per-key item may still exist on a profile this pass has not
        // migrated yet. Leaving it would resurrect the credential through the
        // read fallback, which is the opposite of what a delete means.
        self.inner.delete_secret(key).await
    }
}

fn reserved_key() -> AgentError {
    AgentError::Secret(format!(
        "{BUNDLE_KEY} is where credentials are stored; it is not itself a credential key"
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// A stand-in for the OS credential store, counting the items it holds so
    /// a test can assert the thing this module exists for: one item, not many.
    #[derive(Default)]
    struct FakeStore {
        items: Mutex<HashMap<String, String>>,
    }

    impl FakeStore {
        fn keys(&self) -> Vec<String> {
            let mut keys: Vec<String> = self.items.lock().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        }
    }

    #[async_trait]
    impl SecretProvider for FakeStore {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.items.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.items
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.items.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn bundled() -> (Arc<FakeStore>, BundledSecretProvider) {
        let store = Arc::new(FakeStore::default());
        let provider = BundledSecretProvider::new(store.clone());
        (store, provider)
    }

    /// The reason this module exists: whatever the profile stores, the OS
    /// holds one item, so an update costs one approval rather than one per
    /// credential.
    #[tokio::test]
    async fn every_credential_lands_in_a_single_item() {
        let (store, secrets) = bundled();

        secrets
            .set_secret("provider.anthropic.credential", "key-a")
            .await
            .unwrap();
        secrets
            .set_secret("provider.openai.credential", "key-o")
            .await
            .unwrap();
        secrets
            .set_secret("gateway.credentials_v1", "session")
            .await
            .unwrap();

        assert_eq!(store.keys(), vec![BUNDLE_KEY.to_string()]);
        assert_eq!(
            secrets
                .get_secret("provider.openai.credential")
                .await
                .unwrap()
                .as_deref(),
            Some("key-o")
        );
    }

    #[tokio::test]
    async fn a_key_round_trips_and_a_delete_leaves_the_others() {
        let (_store, secrets) = bundled();
        secrets.set_secret("a", "1").await.unwrap();
        secrets.set_secret("b", "2").await.unwrap();

        secrets.delete_secret("a").await.unwrap();

        assert_eq!(secrets.get_secret("a").await.unwrap(), None);
        assert_eq!(secrets.get_secret("b").await.unwrap().as_deref(), Some("2"));
        // Deleting what is not there is a no-op, as on the real thing.
        secrets.delete_secret("a").await.unwrap();
    }

    /// An empty profile must not gain an item just because something asked
    /// about a key it does not have.
    #[tokio::test]
    async fn a_no_op_write_stores_nothing() {
        let (store, secrets) = bundled();
        secrets.delete_secret("never-stored").await.unwrap();
        assert!(store.keys().is_empty());
    }

    /// The shell's remote attachment and the server both wrap the same item.
    /// A write through one has to be visible to the other, or the second one
    /// to write clobbers the first.
    #[tokio::test]
    async fn two_providers_over_one_item_do_not_lose_each_others_keys() {
        let store = Arc::new(FakeStore::default());
        let server = BundledSecretProvider::new(store.clone());
        let shell = BundledSecretProvider::new(store.clone());

        // Both decode the (empty) item first, so each holds a stale view.
        assert_eq!(server.get_secret("a").await.unwrap(), None);
        assert_eq!(shell.get_secret("b").await.unwrap(), None);

        server.set_secret("a", "1").await.unwrap();
        shell.set_secret("b", "2").await.unwrap();

        // The write merged against the store rather than against the stale
        // read, so the first key survived the second write.
        assert_eq!(server.get_secret("a").await.unwrap().as_deref(), Some("1"));
        assert_eq!(server.get_secret("b").await.unwrap().as_deref(), Some("2"));
        assert_eq!(store.keys(), vec![BUNDLE_KEY.to_string()]);
    }

    /// A session written outside this provider — by another instance, or by a
    /// sign-in racing a boot-time read — is seen without a restart.
    #[tokio::test]
    async fn a_miss_re_reads_so_a_later_write_is_seen() {
        let (store, secrets) = bundled();
        assert_eq!(
            secrets.get_secret("gateway.credentials_v1").await.unwrap(),
            None
        );

        store
            .set_secret(BUNDLE_KEY, r#"{"gateway.credentials_v1":"session"}"#)
            .await
            .unwrap();

        assert_eq!(
            secrets
                .get_secret("gateway.credentials_v1")
                .await
                .unwrap()
                .as_deref(),
            Some("session")
        );
    }

    /// Migration runs at server boot, and the shell reads its remote-machine
    /// token before that. Without the fallback that launch reads as "not
    /// attached" and drops the reader back onto this computer.
    #[tokio::test]
    async fn a_key_still_in_its_own_item_reads_before_migration_runs() {
        let (store, secrets) = bundled();
        store
            .set_secret(DESKTOP_REMOTE_MACHINE_TOKEN_KEY, "legacy-token")
            .await
            .unwrap();

        assert_eq!(
            secrets
                .get_secret(DESKTOP_REMOTE_MACHINE_TOKEN_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("legacy-token")
        );
    }

    /// The bundle is the newer truth: a migration that stored the value and
    /// then failed to remove the old item must not serve the stale one.
    #[tokio::test]
    async fn the_bundle_wins_over_a_leftover_item() {
        let (store, secrets) = bundled();
        store.set_secret("k", "old").await.unwrap();
        secrets.set_secret("k", "new").await.unwrap();

        assert_eq!(
            secrets.get_secret("k").await.unwrap().as_deref(),
            Some("new")
        );
    }

    /// A delete has to reach the per-key item too, or the read fallback
    /// resurrects the credential the reader just removed.
    #[tokio::test]
    async fn a_delete_removes_the_leftover_item_as_well() {
        let (store, secrets) = bundled();
        store.set_secret("k", "old").await.unwrap();
        secrets.set_secret("k", "new").await.unwrap();

        secrets.delete_secret("k").await.unwrap();

        assert_eq!(secrets.get_secret("k").await.unwrap(), None);
        assert_eq!(store.keys(), vec![BUNDLE_KEY.to_string()]);
    }

    #[tokio::test]
    async fn absorbing_moves_each_item_into_the_bundle_and_removes_it() {
        let (store, secrets) = bundled();
        store
            .set_secret("provider.anthropic.credential", "key-a")
            .await
            .unwrap();
        store
            .set_secret("gateway.credentials_v1", "session")
            .await
            .unwrap();

        let outcomes = secrets
            .absorb_legacy_items(&[
                "provider.anthropic.credential".to_string(),
                "gateway.credentials_v1".to_string(),
                "provider.openai.credential".to_string(),
            ])
            .await;

        assert_eq!(
            outcomes,
            vec![
                (
                    "provider.anthropic.credential".to_string(),
                    AbsorbOutcome::Absorbed
                ),
                (
                    "gateway.credentials_v1".to_string(),
                    AbsorbOutcome::Absorbed
                ),
                (
                    "provider.openai.credential".to_string(),
                    AbsorbOutcome::Absent
                ),
            ]
        );
        assert_eq!(store.keys(), vec![BUNDLE_KEY.to_string()]);
        assert_eq!(
            secrets
                .get_secret("provider.anthropic.credential")
                .await
                .unwrap()
                .as_deref(),
            Some("key-a")
        );
    }

    /// Absorbing twice is what a retried launch does. It must not double-move
    /// or report a second pass as work.
    #[tokio::test]
    async fn absorbing_again_finds_nothing_left_to_move() {
        let (store, secrets) = bundled();
        store.set_secret("k", "v").await.unwrap();
        let keys = vec!["k".to_string()];

        assert_eq!(
            secrets.absorb_legacy_items(&keys).await,
            vec![("k".to_string(), AbsorbOutcome::Absorbed)]
        );
        assert_eq!(
            secrets.absorb_legacy_items(&keys).await,
            vec![("k".to_string(), AbsorbOutcome::Absent)]
        );
        assert_eq!(secrets.get_secret("k").await.unwrap().as_deref(), Some("v"));
    }

    /// The store the item lands in is the one the caller wrapped, so a value
    /// survives a process that forgot everything it had decoded.
    #[tokio::test]
    async fn the_item_outlives_the_decoded_copy() {
        let (_store, secrets) = bundled();
        secrets.set_secret("k", "v").await.unwrap();
        secrets.forget();
        assert_eq!(secrets.get_secret("k").await.unwrap().as_deref(), Some("v"));
    }

    /// Re-homing is delete-then-store: an in-place update keeps the original
    /// item, and with it the ownership that makes macOS prompt.
    #[tokio::test]
    async fn re_homing_rewrites_the_item_without_changing_what_it_holds() {
        let (store, secrets) = bundled();
        secrets.set_secret("k", "v").await.unwrap();

        assert_eq!(
            secrets.rehome_bundle_item().await,
            RehomeItemOutcome::Rehomed
        );

        assert_eq!(store.keys(), vec![BUNDLE_KEY.to_string()]);
        assert_eq!(secrets.get_secret("k").await.unwrap().as_deref(), Some("v"));
    }

    #[tokio::test]
    async fn re_homing_an_empty_profile_reports_nothing_to_do() {
        let (_store, secrets) = bundled();
        assert_eq!(
            secrets.rehome_bundle_item().await,
            RehomeItemOutcome::Absent
        );
    }

    /// An unreadable item is every credential at once. Reporting it as an
    /// empty profile would look like a fresh install and then overwrite the
    /// one thing the reader could have recovered from.
    #[tokio::test]
    async fn an_undecodable_item_is_refused_rather_than_treated_as_empty() {
        let (store, secrets) = bundled();
        store.set_secret(BUNDLE_KEY, "not json").await.unwrap();

        let error = secrets.get_secret("k").await.unwrap_err();
        assert!(
            error.to_string().contains("could not be read"),
            "unexpected error: {error}"
        );
        assert!(secrets.set_secret("k", "v").await.is_err());
        // And nothing was overwritten.
        assert_eq!(
            store.get_secret(BUNDLE_KEY).await.unwrap().as_deref(),
            Some("not json")
        );
    }

    #[tokio::test]
    async fn the_bundle_key_is_not_a_credential_key() {
        let (_store, secrets) = bundled();
        assert!(secrets.set_secret(BUNDLE_KEY, "{}").await.is_err());
        assert!(secrets.delete_secret(BUNDLE_KEY).await.is_err());
    }
}

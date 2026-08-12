//! A [`SecretProvider`] decorator that reads each key at most once.
//!
//! Credential reads sit on the per-turn path: the resolver rebuilds its route
//! set for every turn, and building a route reads that provider's credential.
//! Against the OS keychain each of those is a round-trip to the platform
//! credential store — and on macOS, where an item's access approval is tied to
//! the code signature of the binary that created it, a read whose approval no
//! longer holds raises a visible prompt. Reading once per process instead of
//! once per turn takes both costs off the hot path.
//!
//! Reads are memoized, misses included. Writes and deletes pass through and
//! then drop the key's cached entry — whether or not they succeeded, so a
//! failed write can't leave a stale value behind. A write deliberately does
//! *not* seed the cache: the read that follows is a real one, which keeps
//! read-back verification (see `secret_rehome` in `openwave-server`) honest.
//!
//! The tradeoff is that edits made outside this process — Keychain Access, the
//! CLI, another running instance — are not observed until the process restarts.
//! For most keys that is the right call, but a few are session state with a
//! lifecycle this process does not fully own: the model-gateway session, say,
//! can appear in the keychain between a boot-time miss and the read that
//! would serve it — a read in flight while sign-in completes resolves to the
//! old `NoEntry` and lands in the cache *after* the write's invalidation, and
//! a session written by another instance never invalidates this cache at all.
//! [`with_miss_passthrough`](CachingSecretProvider::with_miss_passthrough)
//! marks such keys: their hits still memoize, but a miss is never stored, so
//! the next read re-asks the store. That is cheap for exactly these keys —
//! reading an *absent* item answers `NoEntry` without raising an ACL prompt;
//! only an existing item with a stale approval does.
//!
//! [`with_miss_passthrough`]: CachingSecretProvider::with_miss_passthrough

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::Result;
use crate::storage::SecretProvider;

/// Wraps a [`SecretProvider`], serving repeat reads of a key from memory.
pub struct CachingSecretProvider {
    inner: Arc<dyn SecretProvider>,
    /// Keys a miss is never memoized for — see the module docs. Hits on these
    /// keys still cache; only the confirmed-absent entry is skipped, so a
    /// value that appears after process start is seen on the next read.
    miss_passthrough: HashSet<String>,
    /// Key → last known value. An entry of `None` records a confirmed miss.
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl CachingSecretProvider {
    /// Cache reads against `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn SecretProvider>) -> Self {
        Self {
            inner,
            miss_passthrough: HashSet::new(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Never memoize a miss for `keys`: each absent read re-asks the store.
    ///
    /// For keys whose value can appear without this process performing the
    /// write — a session completed by a flow racing a boot-time read, or
    /// written by another instance entirely — a cached `None` would hide the
    /// value until restart. Repeat misses cost one store round-trip each, and
    /// reading an absent item never raises an ACL prompt, so this is for keys
    /// that are absent by default and read on slow paths (status surfaces,
    /// background ticks), not for per-turn resolver probes.
    #[must_use]
    pub fn with_miss_passthrough<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Self {
        self.miss_passthrough
            .extend(keys.into_iter().map(str::to_string));
        self
    }

    fn cached(&self, key: &str) -> Option<Option<String>> {
        self.cache.lock().unwrap().get(key).cloned()
    }

    fn store(&self, key: &str, value: Option<String>) {
        self.cache.lock().unwrap().insert(key.to_string(), value);
    }

    fn invalidate(&self, key: &str) {
        self.cache.lock().unwrap().remove(key);
    }
}

impl std::fmt::Debug for CachingSecretProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the values.
        f.debug_struct("CachingSecretProvider")
            .field("cached_keys", &self.cache.lock().unwrap().len())
            .finish()
    }
}

#[async_trait]
impl SecretProvider for CachingSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        // Two concurrent misses on one key both reach `inner`; they resolve to
        // the same value, so the only cost is the duplicate read.
        if let Some(hit) = self.cached(key) {
            return Ok(hit);
        }
        let value = self.inner.get_secret(key).await?;
        if value.is_some() || !self.miss_passthrough.contains(key) {
            self.store(key, value.clone());
        }
        Ok(value)
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        let result = self.inner.set_secret(key, value).await;
        self.invalidate(key);
        result
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        let result = self.inner.delete_secret(key).await;
        self.invalidate(key);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Counts reads so a test can tell a cache hit from a fetch.
    #[derive(Default)]
    struct CountingSecrets {
        value: Mutex<Option<String>>,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl SecretProvider for CountingSecrets {
        async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.value.lock().unwrap().clone())
        }

        async fn set_secret(&self, _key: &str, value: &str) -> Result<()> {
            *self.value.lock().unwrap() = Some(value.to_string());
            Ok(())
        }

        async fn delete_secret(&self, _key: &str) -> Result<()> {
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    #[tokio::test]
    async fn repeat_reads_are_served_from_memory_until_a_write_lands() {
        let inner = Arc::new(CountingSecrets::default());
        let secrets = CachingSecretProvider::new(inner.clone());
        let key = "provider.anthropic.credential";

        // A miss is cached too — the resolver asks about every provider kind,
        // including the ones nothing is stored for.
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 1);

        secrets.set_secret(key, "sk-123").await.unwrap();
        assert_eq!(
            secrets.get_secret(key).await.unwrap().as_deref(),
            Some("sk-123")
        );
        assert_eq!(
            secrets.get_secret(key).await.unwrap().as_deref(),
            Some("sk-123")
        );
        assert_eq!(inner.reads.load(Ordering::SeqCst), 2);

        secrets.delete_secret(key).await.unwrap();
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_passthrough_miss_is_reread_so_a_later_write_is_seen() {
        let inner = Arc::new(CountingSecrets::default());
        let secrets =
            CachingSecretProvider::new(inner.clone()).with_miss_passthrough(["some.session_v1"]);
        let key = "some.session_v1";

        // Every miss re-asks the store — the session key's reason for being
        // here is that a value can appear without this cache observing the
        // write (a racing read landing after the write's invalidation, or a
        // writer outside this process).
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 2);

        // A write landing behind the cache's back — straight into the inner
        // store — is visible on the very next read, with no restart.
        *inner.value.lock().unwrap() = Some("session-1".to_string());
        assert_eq!(
            secrets.get_secret(key).await.unwrap().as_deref(),
            Some("session-1")
        );

        // A hit still memoizes: the passthrough costs reads only while absent.
        assert_eq!(
            secrets.get_secret(key).await.unwrap().as_deref(),
            Some("session-1")
        );
        assert_eq!(inner.reads.load(Ordering::SeqCst), 3);

        // And a delete through the cache still invalidates the hit.
        secrets.delete_secret(key).await.unwrap();
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        assert_eq!(inner.reads.load(Ordering::SeqCst), 4);
    }
}

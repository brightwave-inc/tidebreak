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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::Result;
use crate::storage::SecretProvider;

/// Wraps a [`SecretProvider`], serving repeat reads of a key from memory.
pub struct CachingSecretProvider {
    inner: Arc<dyn SecretProvider>,
    /// Key → last known value. An entry of `None` records a confirmed miss.
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl CachingSecretProvider {
    /// Cache reads against `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn SecretProvider>) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
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
        self.store(key, value.clone());
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
}

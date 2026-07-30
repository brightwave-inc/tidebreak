//! Durable source-blob retirement execution.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use openwave_core::{AgentError, BlobRetirement, BlobRetirementStatus, BlobStore, Result, Store};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::state::BlobWriteGuard;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BlobRetirementWorkerConfig {
    lease: Duration,
    heartbeat: Duration,
    retry_base: Duration,
    retry_cap: Duration,
    idle_min: Duration,
    idle_cap: Duration,
    failure_delay: Duration,
}

impl Default for BlobRetirementWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(60),
            heartbeat: Duration::from_secs(15),
            retry_base: Duration::from_secs(5),
            retry_cap: Duration::from_secs(5 * 60),
            idle_min: Duration::from_millis(250),
            idle_cap: Duration::from_secs(5),
            failure_delay: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobRetirementWorkerOutcome {
    Idle,
    Completed(Uuid),
    RetryScheduled(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}

#[derive(Clone)]
pub(crate) struct BlobRetirementWorker {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    wake: Arc<Notify>,
    blob_writes: Arc<BlobWriteGuard>,
    config: BlobRetirementWorkerConfig,
}

enum Supervised<T> {
    Completed(T),
    LeaseLost,
}

impl BlobRetirementWorker {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blobs: Arc<dyn BlobStore>,
        wake: Arc<Notify>,
        blob_writes: Arc<BlobWriteGuard>,
        config: BlobRetirementWorkerConfig,
    ) -> Self {
        assert!(!config.lease.is_zero());
        assert!(!config.heartbeat.is_zero());
        assert!(config.heartbeat < config.lease);
        Self {
            store,
            blobs,
            wake,
            blob_writes,
            config,
        }
    }

    pub(crate) async fn run(self) {
        let mut idle_delay = self.config.idle_min;
        loop {
            match self.run_once().await {
                Ok(BlobRetirementWorkerOutcome::Idle) => {
                    tokio::select! {
                        _ = tokio::time::sleep(idle_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                    idle_delay = idle_delay.saturating_mul(2).min(self.config.idle_cap);
                }
                Ok(_) => idle_delay = self.config.idle_min,
                Err(error) => {
                    eprintln!("openwave: blob retirement worker iteration failed: {error}");
                    tokio::select! {
                        _ = tokio::time::sleep(self.config.failure_delay) => {}
                        _ = self.wake.notified() => {}
                    }
                }
            }
        }
    }

    pub(crate) async fn run_once(&self) -> Result<BlobRetirementWorkerOutcome> {
        let now = Utc::now();
        let lease_expires_at = now + chrono_duration(self.config.lease)?;
        let Some(retirement) = self
            .store
            .claim_blob_retirement(now, lease_expires_at)
            .await?
        else {
            return Ok(BlobRetirementWorkerOutcome::Idle);
        };
        self.process(retirement).await
    }

    async fn process(&self, retirement: BlobRetirement) -> Result<BlobRetirementWorkerOutcome> {
        if retirement.status != BlobRetirementStatus::Running {
            return Err(AgentError::msg(format!(
                "claimed blob retirement {} has an invalid execution state",
                retirement.blob_id
            )));
        }
        let lease_token = retirement.lease_token.ok_or_else(|| {
            AgentError::msg(format!(
                "claimed blob retirement {} has no lease token",
                retirement.blob_id
            ))
        })?;

        let permit = match self
            .supervise(
                &retirement,
                lease_token,
                self.blob_writes.acquire(retirement.blob_id),
            )
            .await
        {
            Supervised::LeaseLost => {
                return Ok(BlobRetirementWorkerOutcome::LeaseLost(retirement.blob_id));
            }
            Supervised::Completed(Ok(permit)) => permit,
            Supervised::Completed(Err(error)) => {
                return self
                    .record_failure(
                        &retirement,
                        lease_token,
                        "blob_lock_failed",
                        &error.to_string(),
                    )
                    .await;
            }
        };

        // Source publication takes this same cross-process guard around both
        // blob publication and catalog acceptance. Once the exact lease and
        // authoritative references are revalidated here, no new reference can
        // appear until deletion and lease-fenced completion have finished.
        if !self
            .store
            .validate_blob_retirement_lease(retirement.blob_id, lease_token, Utc::now())
            .await?
        {
            return Ok(BlobRetirementWorkerOutcome::LeaseLost(retirement.blob_id));
        }

        let blobs = Arc::clone(&self.blobs);
        let blob_id = retirement.blob_id;
        let deletion = self
            .supervise(&retirement, lease_token, async move {
                tokio::task::spawn_blocking(move || {
                    let result = blobs.delete(blob_id);
                    (permit, result)
                })
                .await
                .map_err(|error| AgentError::Store(format!("blob delete task failed: {error}")))
            })
            .await;
        match deletion {
            Supervised::LeaseLost => {
                // Dropping the join future detaches the blocking operation, but
                // that operation owns the file-lock permit until it exits.
                Ok(BlobRetirementWorkerOutcome::LeaseLost(retirement.blob_id))
            }
            Supervised::Completed(Err(error)) => {
                self.record_failure(
                    &retirement,
                    lease_token,
                    "blob_delete_failed",
                    &error.to_string(),
                )
                .await
            }
            Supervised::Completed(Ok((permit, result))) => {
                let outcome = match result {
                    Err(error) => {
                        self.record_failure(
                            &retirement,
                            lease_token,
                            "blob_delete_failed",
                            &error.to_string(),
                        )
                        .await?
                    }
                    Ok(()) => {
                        if self
                            .store
                            .complete_blob_retirement(retirement.blob_id, lease_token, Utc::now())
                            .await?
                        {
                            BlobRetirementWorkerOutcome::Completed(retirement.blob_id)
                        } else {
                            BlobRetirementWorkerOutcome::LeaseLost(retirement.blob_id)
                        }
                    }
                };
                drop(permit);
                Ok(outcome)
            }
        }
    }

    async fn supervise<T>(
        &self,
        retirement: &BlobRetirement,
        lease_token: Uuid,
        future: impl Future<Output = T>,
    ) -> Supervised<T> {
        tokio::pin!(future);
        let mut heartbeat = tokio::time::interval(self.config.heartbeat);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                result = &mut future => return Supervised::Completed(result),
                _ = heartbeat.tick() => {
                    if !self.prove_lease(retirement, lease_token).await {
                        return Supervised::LeaseLost;
                    }
                }
            }
        }
    }

    async fn prove_lease(&self, retirement: &BlobRetirement, lease_token: Uuid) -> bool {
        let now = Utc::now();
        let Ok(lease) = chrono_duration(self.config.lease) else {
            return false;
        };
        matches!(
            self.store
                .heartbeat_blob_retirement(retirement.blob_id, lease_token, now, now + lease,)
                .await,
            Ok(true)
        )
    }

    async fn record_failure(
        &self,
        retirement: &BlobRetirement,
        lease_token: Uuid,
        code: &str,
        detail: &str,
    ) -> Result<BlobRetirementWorkerOutcome> {
        let failed_at = Utc::now();
        let retry_at = if retirement.attempt_count < retirement.max_attempts {
            Some(failed_at + chrono_duration(self.retry_delay(retirement))?)
        } else {
            None
        };
        let detail = truncate_detail(detail);
        let status = self
            .store
            .record_blob_retirement_failure(
                retirement.blob_id,
                lease_token,
                failed_at,
                retry_at,
                code,
                Some(&detail),
            )
            .await?;
        Ok(match status {
            Some(BlobRetirementStatus::RetryWait) => {
                BlobRetirementWorkerOutcome::RetryScheduled(retirement.blob_id)
            }
            Some(BlobRetirementStatus::Failed) => {
                BlobRetirementWorkerOutcome::Failed(retirement.blob_id)
            }
            Some(_) | None => BlobRetirementWorkerOutcome::LeaseLost(retirement.blob_id),
        })
    }

    fn retry_delay(&self, retirement: &BlobRetirement) -> Duration {
        let exponent = retirement.attempt_count.saturating_sub(1).clamp(0, 20) as u32;
        self.config
            .retry_base
            .saturating_mul(2_u32.saturating_pow(exponent))
            .min(self.config.retry_cap)
    }
}

fn truncate_detail(detail: &str) -> String {
    detail
        .chars()
        .take(BlobRetirement::MAX_ERROR_DETAIL_LEN)
        .collect()
}

fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid blob-worker duration: {error}")))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex as StdMutex};

    use async_trait::async_trait;
    use openwave_core::{
        DbStore, DocumentId, DocumentSourceBlob, DocumentSourceUpsert, FsBlobStore,
    };

    use super::*;

    fn config() -> BlobRetirementWorkerConfig {
        BlobRetirementWorkerConfig {
            lease: Duration::from_millis(500),
            heartbeat: Duration::from_millis(25),
            retry_base: Duration::from_millis(5),
            retry_cap: Duration::from_millis(20),
            idle_min: Duration::from_millis(1),
            idle_cap: Duration::from_millis(5),
            failure_delay: Duration::from_millis(1),
        }
    }

    async fn store(dir: &tempfile::TempDir) -> Arc<DbStore> {
        Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("worker.db").display()
            ))
            .await
            .unwrap(),
        )
    }

    fn source(id: DocumentId, blob: DocumentSourceBlob) -> DocumentSourceUpsert {
        DocumentSourceUpsert {
            id,
            chat_id: None,
            project_id: None,
            source_uri: Some(format!("file:///{id}.bin")),
            media_type: "application/octet-stream".into(),
            title: None,
            source_blob: blob,
            canonical_text: String::new(),
            updated_at: Utc::now(),
        }
    }

    fn worker(
        store: Arc<DbStore>,
        blobs: Arc<dyn BlobStore>,
        blob_writes: Arc<BlobWriteGuard>,
    ) -> BlobRetirementWorker {
        BlobRetirementWorker::new(store, blobs, Arc::new(Notify::new()), blob_writes, config())
    }

    #[tokio::test]
    async fn worker_deletes_and_completes_an_unreferenced_blob() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(dir.path().join("blobs")));
        let blob_writes = Arc::new(BlobWriteGuard::new(dir.path().join("blob-locks")));
        let worker = worker(store.clone(), blobs.clone(), blob_writes);
        let bytes = b"retire this source".to_vec();
        let descriptor = DocumentSourceBlob::from_bytes(&bytes);
        blobs.put(descriptor.id, bytes).await.unwrap();
        let source = source(DocumentId::new(), descriptor.clone());
        store.accept_document_source(&source).await.unwrap();
        store.delete_document(source.id).await.unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            BlobRetirementWorkerOutcome::Completed(descriptor.id)
        );
        assert_eq!(blobs.get(descriptor.id).await.unwrap(), None);
        assert_eq!(
            store
                .get_blob_retirement(descriptor.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            BlobRetirementStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn source_publication_guard_fences_retirement_before_catalog_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(dir.path().join("blobs")));
        let blob_writes = Arc::new(BlobWriteGuard::new(dir.path().join("blob-locks")));
        let worker = worker(store.clone(), blobs.clone(), blob_writes.clone());
        let bytes = b"shared source survives".to_vec();
        let descriptor = DocumentSourceBlob::from_bytes(&bytes);
        blobs.put(descriptor.id, bytes.clone()).await.unwrap();
        let retired_source = source(DocumentId::new(), descriptor.clone());
        store.accept_document_source(&retired_source).await.unwrap();
        store.delete_document(retired_source.id).await.unwrap();

        // A publisher holds this lock from immutable put through catalog
        // acceptance. The worker may claim meanwhile, but cannot validate or
        // delete until the publisher's reference write has cancelled its lease.
        let publisher = blob_writes.acquire(descriptor.id).await.unwrap();
        let task = tokio::spawn({
            let worker = worker.clone();
            async move { worker.run_once().await.unwrap() }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store
                    .get_blob_retirement(descriptor.id)
                    .await
                    .unwrap()
                    .is_some_and(|retirement| retirement.status == BlobRetirementStatus::Running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker did not claim the retirement");
        blobs.put(descriptor.id, bytes.clone()).await.unwrap();
        let live_source = source(DocumentId::new(), descriptor.clone());
        store.accept_document_source(&live_source).await.unwrap();
        drop(publisher);

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            BlobRetirementWorkerOutcome::LeaseLost(descriptor.id)
        );
        assert_eq!(blobs.get(descriptor.id).await.unwrap(), Some(bytes));
        assert_eq!(
            store
                .get_blob_retirement(descriptor.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            BlobRetirementStatus::Cancelled
        );
    }

    struct FailOnceBlobStore {
        inner: FsBlobStore,
        fail_delete: AtomicBool,
    }

    #[async_trait]
    impl BlobStore for FailOnceBlobStore {
        async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<()> {
            self.inner.put(id, bytes).await
        }

        async fn get(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
            self.inner.get(id).await
        }

        fn delete(&self, id: Uuid) -> Result<()> {
            if self.fail_delete.swap(false, Ordering::SeqCst) {
                Err(AgentError::Store("injected blob delete failure".into()))
            } else {
                self.inner.delete(id)
            }
        }
    }

    #[tokio::test]
    async fn worker_retries_a_transient_delete_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let blobs: Arc<dyn BlobStore> = Arc::new(FailOnceBlobStore {
            inner: FsBlobStore::new(dir.path().join("blobs")),
            fail_delete: AtomicBool::new(true),
        });
        let blob_writes = Arc::new(BlobWriteGuard::new(dir.path().join("blob-locks")));
        let worker = worker(store.clone(), blobs.clone(), blob_writes);
        let bytes = b"retry deleting this source".to_vec();
        let descriptor = DocumentSourceBlob::from_bytes(&bytes);
        blobs.put(descriptor.id, bytes).await.unwrap();
        let source = source(DocumentId::new(), descriptor.clone());
        store.accept_document_source(&source).await.unwrap();
        store.delete_document(source.id).await.unwrap();

        assert_eq!(
            worker.run_once().await.unwrap(),
            BlobRetirementWorkerOutcome::RetryScheduled(descriptor.id)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            BlobRetirementWorkerOutcome::Completed(descriptor.id)
        );
        assert_eq!(blobs.get(descriptor.id).await.unwrap(), None);
    }

    struct DeleteGate {
        entered: Notify,
        released: StdMutex<bool>,
        release: Condvar,
    }

    struct BlockingDeleteBlobStore {
        inner: FsBlobStore,
        gate: Arc<DeleteGate>,
    }

    #[async_trait]
    impl BlobStore for BlockingDeleteBlobStore {
        async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<()> {
            self.inner.put(id, bytes).await
        }

        async fn get(&self, id: Uuid) -> Result<Option<Vec<u8>>> {
            self.inner.get(id).await
        }

        fn delete(&self, id: Uuid) -> Result<()> {
            self.gate.entered.notify_one();
            let mut released = self.gate.released.lock().unwrap();
            while !*released {
                released = self.gate.release.wait(released).unwrap();
            }
            drop(released);
            self.inner.delete(id)
        }
    }

    #[tokio::test]
    async fn lease_loss_cannot_release_the_guard_before_blocking_delete_exits() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir).await;
        let gate = Arc::new(DeleteGate {
            entered: Notify::new(),
            released: StdMutex::new(false),
            release: Condvar::new(),
        });
        let blobs: Arc<dyn BlobStore> = Arc::new(BlockingDeleteBlobStore {
            inner: FsBlobStore::new(dir.path().join("blobs")),
            gate: gate.clone(),
        });
        let blob_writes = Arc::new(BlobWriteGuard::new(dir.path().join("blob-locks")));
        let worker = worker(store.clone(), blobs.clone(), blob_writes.clone());
        let bytes = b"blocking retirement".to_vec();
        let descriptor = DocumentSourceBlob::from_bytes(&bytes);
        blobs.put(descriptor.id, bytes).await.unwrap();
        let source = source(DocumentId::new(), descriptor.clone());
        store.accept_document_source(&source).await.unwrap();
        store.delete_document(source.id).await.unwrap();

        let task = tokio::spawn({
            let worker = worker.clone();
            async move { worker.run_once().await.unwrap() }
        });
        tokio::time::timeout(Duration::from_secs(1), gate.entered.notified())
            .await
            .expect("worker did not enter blocking delete");
        let running = store
            .get_blob_retirement(descriptor.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .record_blob_retirement_failure(
                    descriptor.id,
                    running.lease_token.unwrap(),
                    Utc::now(),
                    None,
                    "lease_revoked_for_test",
                    None,
                )
                .await
                .unwrap(),
            Some(BlobRetirementStatus::Failed)
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("worker did not observe lease loss")
                .unwrap(),
            BlobRetirementWorkerOutcome::LeaseLost(descriptor.id)
        );

        let publisher = tokio::spawn({
            let blob_writes = blob_writes.clone();
            async move { blob_writes.acquire(descriptor.id).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!publisher.is_finished());
        {
            let mut released = gate.released.lock().unwrap();
            *released = true;
            gate.release.notify_all();
        }
        let _publisher = tokio::time::timeout(Duration::from_secs(1), publisher)
            .await
            .expect("publisher remained blocked after delete exited")
            .unwrap();
    }

    #[tokio::test]
    async fn independent_blob_guards_serialize_on_the_same_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let first_guard = Arc::new(BlobWriteGuard::new(dir.path()));
        let second_guard = Arc::new(BlobWriteGuard::new(dir.path()));
        let blob_id = Uuid::new_v4();
        let first = first_guard.acquire(blob_id).await.unwrap();
        let waiting = tokio::spawn(async move { second_guard.acquire(blob_id).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(!waiting.is_finished());
        drop(first);
        let _second = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("independent guard did not acquire after release")
            .unwrap();
    }
}

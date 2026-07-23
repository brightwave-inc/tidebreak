//! Conservative grace-period discovery of unreferenced source blobs.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use openwave_core::{AgentError, Result, Store};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use crate::state::BlobWriteGuard;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BlobOrphanAuditorConfig {
    grace_period: Duration,
    interval: Duration,
    drain_delay: Duration,
    failure_delay: Duration,
    batch_size: usize,
}

impl Default for BlobOrphanAuditorConfig {
    fn default() -> Self {
        Self {
            grace_period: Duration::from_secs(24 * 60 * 60),
            interval: Duration::from_secs(6 * 60 * 60),
            drain_delay: Duration::from_millis(10),
            failure_delay: Duration::from_secs(60),
            batch_size: 128,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct BlobOrphanAuditReport {
    pub scanned: usize,
    pub eligible: usize,
    pub enqueued: usize,
    pub skipped: usize,
    pub remaining: usize,
    pub failures: Vec<BlobOrphanAuditFailure>,
}

#[derive(Debug)]
pub(crate) struct BlobOrphanAuditFailure {
    pub blob_id: Option<Uuid>,
    pub error: String,
}

#[derive(Clone)]
pub(crate) struct BlobOrphanAuditor {
    store: Arc<dyn Store>,
    blob_root: Arc<PathBuf>,
    blob_writes: Arc<BlobWriteGuard>,
    wake: Arc<Notify>,
    audit: Arc<Mutex<()>>,
    inventory: Arc<Mutex<VecDeque<Uuid>>>,
    config: BlobOrphanAuditorConfig,
}

struct ScanResult {
    scanned: usize,
    eligible: usize,
    candidates: VecDeque<Uuid>,
    failures: Vec<BlobOrphanAuditFailure>,
}

impl BlobOrphanAuditor {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        blob_root: impl Into<PathBuf>,
        blob_writes: Arc<BlobWriteGuard>,
        wake: Arc<Notify>,
        config: BlobOrphanAuditorConfig,
    ) -> Self {
        assert!(config.batch_size > 0);
        Self {
            store,
            blob_root: Arc::new(blob_root.into()),
            blob_writes,
            wake,
            audit: Arc::new(Mutex::new(())),
            inventory: Arc::new(Mutex::new(VecDeque::new())),
            config,
        }
    }

    pub(crate) async fn run(self) {
        loop {
            let delay = match self.audit_once().await {
                Ok(report) => {
                    if report.enqueued > 0 || !report.failures.is_empty() {
                        eprintln!(
                            "openwave: blob orphan audit scanned {}, eligible {}, enqueued {}, skipped {}, remaining {}, errors {}",
                            report.scanned,
                            report.eligible,
                            report.enqueued,
                            report.skipped,
                            report.remaining,
                            report.failures.len()
                        );
                    }
                    for failure in &report.failures {
                        match failure.blob_id {
                            Some(blob_id) => eprintln!(
                                "openwave: blob {blob_id} orphan audit failed: {}",
                                failure.error
                            ),
                            None => {
                                eprintln!("openwave: blob orphan scan failed: {}", failure.error)
                            }
                        }
                    }
                    next_delay(self.config, &report)
                }
                Err(error) => {
                    eprintln!("openwave: blob orphan audit failed: {error}");
                    self.config.failure_delay
                }
            };
            tokio::time::sleep(delay).await;
        }
    }

    pub(crate) async fn audit_once(&self) -> Result<BlobOrphanAuditReport> {
        let _audit = self.audit.lock().await;
        let cutoff = SystemTime::now()
            .checked_sub(self.config.grace_period)
            .ok_or_else(|| AgentError::msg("blob orphan grace period exceeds system time"))?;
        let mut inventory = self.inventory.lock().await;
        let scan = if inventory.is_empty() {
            let root = Arc::clone(&self.blob_root);
            let scan = tokio::task::spawn_blocking(move || scan_candidates(&root, cutoff))
                .await
                .map_err(|error| {
                    AgentError::Store(format!("blob orphan scan task failed: {error}"))
                })??;
            inventory.extend(scan.candidates.iter().copied());
            Some(scan)
        } else {
            None
        };
        let candidates = (0..self.config.batch_size)
            .filter_map(|_| inventory.pop_front())
            .collect::<Vec<_>>();
        drop(inventory);
        let mut report = BlobOrphanAuditReport {
            scanned: scan.as_ref().map_or(0, |scan| scan.scanned),
            eligible: scan.as_ref().map_or(0, |scan| scan.eligible),
            failures: scan.map_or_else(Vec::new, |scan| scan.failures),
            ..BlobOrphanAuditReport::default()
        };
        for blob_id in candidates {
            match self.audit_blob(blob_id, cutoff).await {
                Ok(true) => report.enqueued += 1,
                Ok(false) => report.skipped += 1,
                Err(error) => {
                    report.failures.push(BlobOrphanAuditFailure {
                        blob_id: Some(blob_id),
                        error: error.to_string(),
                    });
                }
            }
        }
        report.remaining = self.inventory.lock().await.len();
        if report.enqueued > 0 {
            self.wake.notify_one();
        }
        Ok(report)
    }

    async fn audit_blob(&self, blob_id: Uuid, cutoff: SystemTime) -> Result<bool> {
        let _permit = self.blob_writes.acquire(blob_id).await?;
        let path = self.blob_root.join(format!("{blob_id}.blob"));
        let still_eligible = tokio::task::spawn_blocking(move || eligible_file(&path, cutoff))
            .await
            .map_err(|error| {
                AgentError::Store(format!("blob orphan metadata task failed: {error}"))
            })??;
        if !still_eligible {
            return Ok(false);
        }
        self.store.ensure_orphan_blob_retirement(blob_id).await
    }
}

fn next_delay(config: BlobOrphanAuditorConfig, report: &BlobOrphanAuditReport) -> Duration {
    if report.remaining == 0 {
        config.interval
    } else if report.failures.is_empty() {
        config.drain_delay
    } else {
        config.failure_delay
    }
}

fn scan_candidates(root: &Path, cutoff: SystemTime) -> Result<ScanResult> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScanResult {
                scanned: 0,
                eligible: 0,
                candidates: VecDeque::new(),
                failures: Vec::new(),
            });
        }
        Err(error) => return Err(scan_error("read blob directory", error)),
    };
    let mut scanned = 0;
    let mut eligible = Vec::new();
    let mut failures = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                failures.push(BlobOrphanAuditFailure {
                    blob_id: None,
                    error: scan_error("read blob directory entry", error).to_string(),
                });
                continue;
            }
        };
        let Some(blob_id) = blob_id_from_name(&entry.file_name()) else {
            continue;
        };
        scanned += 1;
        match entry.file_type().and_then(|kind| {
            if kind.is_file() {
                entry.metadata().map(Some)
            } else {
                Ok(None)
            }
        }) {
            Ok(Some(metadata)) => match metadata.modified() {
                Ok(modified) if modified <= cutoff => eligible.push(blob_id),
                Ok(_) => {}
                Err(error) => failures.push(BlobOrphanAuditFailure {
                    blob_id: Some(blob_id),
                    error: scan_error("read blob modification time", error).to_string(),
                }),
            },
            Ok(None) => {}
            Err(error) => failures.push(BlobOrphanAuditFailure {
                blob_id: Some(blob_id),
                error: scan_error("read blob metadata", error).to_string(),
            }),
        }
    }
    eligible.sort_unstable();
    let eligible_count = eligible.len();
    let candidates = eligible.into();
    Ok(ScanResult {
        scanned,
        eligible: eligible_count,
        candidates,
        failures,
    })
}

fn eligible_file(path: &Path, cutoff: SystemTime) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata
            .modified()
            .map_err(|error| scan_error("read blob modification time", error))?
            <= cutoff),
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(scan_error("read blob metadata", error)),
    }
}

fn blob_id_from_name(name: &OsStr) -> Option<Uuid> {
    let name = name.to_str()?;
    let id = name.strip_suffix(".blob")?.parse::<Uuid>().ok()?;
    (name == format!("{id}.blob")).then_some(id)
}

fn scan_error(action: &str, error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to {action}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs::{File, FileTimes};

    use openwave_core::{
        BlobRetirementStatus, BlobStore, DbStore, DocumentId, DocumentSourceBlob,
        DocumentSourceUpsert, FsBlobStore,
    };

    use super::*;

    fn config(batch_size: usize) -> BlobOrphanAuditorConfig {
        BlobOrphanAuditorConfig {
            grace_period: Duration::from_secs(60 * 60),
            interval: Duration::from_secs(6 * 60 * 60),
            drain_delay: Duration::from_millis(1),
            failure_delay: Duration::from_secs(1),
            batch_size,
        }
    }

    async fn store(dir: &tempfile::TempDir) -> Arc<DbStore> {
        Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("auditor.db").display()
            ))
            .await
            .unwrap(),
        )
    }

    fn source(blob: DocumentSourceBlob) -> DocumentSourceUpsert {
        let id = DocumentId::new();
        DocumentSourceUpsert {
            id,
            chat_id: None,
            project_id: None,
            source_uri: Some(format!("file:///{id}.bin")),
            media_type: "application/octet-stream".into(),
            title: None,
            source_blob: blob,
            updated_at: chrono::Utc::now(),
        }
    }

    fn age_blob(root: &Path, blob_id: Uuid, age: Duration) {
        let modified = SystemTime::now().checked_sub(age).unwrap();
        File::options()
            .write(true)
            .open(root.join(format!("{blob_id}.blob")))
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn auditor(
        store: Arc<DbStore>,
        root: &Path,
        wake: Arc<Notify>,
        batch_size: usize,
    ) -> BlobOrphanAuditor {
        BlobOrphanAuditor::new(
            store,
            root,
            Arc::new(BlobWriteGuard::new(root.join("locks"))),
            wake,
            config(batch_size),
        )
    }

    #[tokio::test]
    async fn audit_only_enqueues_old_unreferenced_canonical_blob_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        let store = store(&dir).await;
        let blobs = FsBlobStore::new(&root);
        let old_orphan = DocumentSourceBlob::from_bytes(b"old orphan");
        let young_orphan = DocumentSourceBlob::from_bytes(b"young orphan");
        let referenced = DocumentSourceBlob::from_bytes(b"old referenced");
        blobs
            .put(old_orphan.id, b"old orphan".to_vec())
            .await
            .unwrap();
        blobs
            .put(young_orphan.id, b"young orphan".to_vec())
            .await
            .unwrap();
        blobs
            .put(referenced.id, b"old referenced".to_vec())
            .await
            .unwrap();
        age_blob(&root, old_orphan.id, Duration::from_secs(2 * 60 * 60));
        age_blob(&root, referenced.id, Duration::from_secs(2 * 60 * 60));
        let live_source = source(referenced.clone());
        store
            .accept_document_source_and_enqueue_parse(&live_source, "parser-v1", 3)
            .await
            .unwrap();
        std::fs::write(root.join("not-a-blob.txt"), b"ignored").unwrap();
        std::fs::write(root.join("not-a-uuid.blob"), b"ignored").unwrap();
        let wake = Arc::new(Notify::new());
        let auditor = auditor(store.clone(), &root, wake.clone(), 16);

        let report = auditor.audit_once().await.unwrap();
        assert_eq!(report.scanned, 3);
        assert_eq!(report.eligible, 2);
        assert_eq!(report.enqueued, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.remaining, 0);
        assert!(report.failures.is_empty());
        assert_eq!(
            store
                .get_blob_retirement(old_orphan.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            BlobRetirementStatus::Queued
        );
        assert_eq!(
            store.get_blob_retirement(young_orphan.id).await.unwrap(),
            None
        );
        assert_eq!(
            store.get_blob_retirement(referenced.id).await.unwrap(),
            None
        );
        tokio::time::timeout(Duration::from_millis(100), wake.notified())
            .await
            .expect("retirement worker was not notified");
    }

    #[tokio::test]
    async fn bounded_audits_rotate_across_all_old_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        let store = store(&dir).await;
        let blobs = FsBlobStore::new(&root);
        let mut blob_ids = Vec::new();
        for index in 0..3_u8 {
            let bytes = vec![index; 8];
            let blob = DocumentSourceBlob::from_bytes(&bytes);
            blobs.put(blob.id, bytes).await.unwrap();
            age_blob(&root, blob.id, Duration::from_secs(2 * 60 * 60));
            blob_ids.push(blob.id);
        }
        let auditor = auditor(store.clone(), &root, Arc::new(Notify::new()), 2);
        let first = auditor.audit_once().await.unwrap();
        assert_eq!(first.scanned, 3);
        assert_eq!(first.eligible, 3);
        assert_eq!(first.enqueued, 2);
        assert_eq!(first.remaining, 1);
        let second = auditor.audit_once().await.unwrap();
        assert_eq!(second.scanned, 0, "the retained inventory avoids a rescan");
        assert_eq!(second.eligible, 0);
        assert_eq!(second.enqueued, 1);
        assert_eq!(second.remaining, 0);
        for blob_id in blob_ids {
            assert_eq!(
                store
                    .get_blob_retirement(blob_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                BlobRetirementStatus::Queued
            );
        }
    }

    #[tokio::test]
    async fn audit_rechecks_age_after_acquiring_the_blob_guard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        let store = store(&dir).await;
        let blobs = FsBlobStore::new(&root);
        let blob = DocumentSourceBlob::from_bytes(b"recently republished");
        blobs
            .put(blob.id, b"recently republished".to_vec())
            .await
            .unwrap();
        age_blob(&root, blob.id, Duration::from_secs(2 * 60 * 60));
        let auditor = auditor(store.clone(), &root, Arc::new(Notify::new()), 16);
        let cutoff = SystemTime::now() - Duration::from_secs(60 * 60);
        File::options()
            .write(true)
            .open(root.join(format!("{}.blob", blob.id)))
            .unwrap()
            .set_times(FileTimes::new().set_modified(SystemTime::now()))
            .unwrap();

        assert!(!auditor.audit_blob(blob.id, cutoff).await.unwrap());
        assert_eq!(store.get_blob_retirement(blob.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn poison_blob_does_not_prevent_a_later_filesystem_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("blobs");
        let store = store(&dir).await;
        std::fs::create_dir_all(&root).unwrap();
        let poison = Uuid::from_u128(1);
        std::fs::write(root.join(format!("{poison}.blob")), b"poison").unwrap();
        age_blob(&root, poison, Duration::from_secs(2 * 60 * 60));
        let lock_root = root.join("locks");
        std::fs::create_dir_all(lock_root.join(format!("{poison}.lock"))).unwrap();
        let auditor = BlobOrphanAuditor::new(
            store.clone(),
            &root,
            Arc::new(BlobWriteGuard::new(lock_root)),
            Arc::new(Notify::new()),
            config(2),
        );

        let first = auditor.audit_once().await.unwrap();
        assert_eq!(first.failures.len(), 1);
        assert_eq!(first.remaining, 0);
        assert_eq!(
            next_delay(auditor.config, &first),
            auditor.config.interval,
            "a completed poisoned sweep must not rescan on the short failure delay"
        );

        let later = Uuid::from_u128(2);
        std::fs::write(root.join(format!("{later}.blob")), b"later").unwrap();
        age_blob(&root, later, Duration::from_secs(2 * 60 * 60));
        let second = auditor.audit_once().await.unwrap();
        assert_eq!(second.scanned, 2, "an empty inventory begins a new sweep");
        assert_eq!(second.failures.len(), 1);
        assert_eq!(second.enqueued, 1);
        assert_eq!(second.remaining, 0);
        assert_eq!(
            store
                .get_blob_retirement(later)
                .await
                .unwrap()
                .unwrap()
                .status,
            BlobRetirementStatus::Queued
        );
    }
}

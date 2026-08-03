//! Verified shared package cache for local sandboxes.
//!
//! Per-chat HOME already persists `pip install --user` state within one
//! conversation. This module adds the cross-conversation substrate: a
//! host-owned wheel cache that a second conversation can install pinned
//! packages from with no network at all.
//!
//! The trust story is deliberately one-directional:
//!
//! - **Population is host-side only.** The host runs its own `pip download`
//!   (wheels only, so no package code executes) against the real registry over
//!   host TLS; pip authenticates every artifact against the index's published
//!   hashes. Nothing a sandbox wrote is ever promoted: the CONNECT broker
//!   keeps TLS end to end, so sandbox-side downloads cannot be validated by
//!   the host and are never trusted.
//! - **Promotion re-hashes every artifact** into a host-owned manifest. A
//!   file whose bytes later disagree with the manifest — or a file the
//!   manifest never recorded — is deleted, not served. Wheel filenames are
//!   immutable upstream, so a staged file colliding with a recorded entry
//!   under different bytes is refused outright.
//! - **Sandboxes see the cache read-only.** The Seatbelt profile allows reads
//!   of the wheels directory and never lists it as writable, so one
//!   conversation cannot craft an entry another conversation would consume.
//!
//! Wheel compatibility is part of the cache key: artifacts live under a
//! runtime key derived from the same interpreter the sandbox runs
//! (`cp<maj><min>-<platform>-<machine>`), so an OS or interpreter upgrade
//! starts a fresh keyspace instead of serving stale incompatible wheels.
//! Lifecycle is bounded: a total-size cap evicts the oldest promotions first.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::CodeExecutionError;

/// Dotted sibling of the per-chat workspaces at the scratch root, like the
/// receipt and env-home directories: file tools and provider sync never see it.
pub const PACKAGE_CACHE_DIR: &str = ".code-execution-package-cache";

/// Environment variable the local sandbox exports at the read-only wheels
/// directory when the cache is mounted.
pub const PACKAGE_CACHE_ENV: &str = "OPENWAVE_PACKAGE_CACHE";

const MANIFEST_FILE: &str = "manifest.json";
const WHEELS_DIR: &str = "wheels";
const MAX_ARTIFACT_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_TOTAL_BYTES: u64 = 1_024 * 1_024 * 1_024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_RUNTIME_KEY_BYTES: usize = 64;
const RUNTIME_KEY_TIMEOUT: Duration = Duration::from_secs(10);
const PIP_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_REPORTED_STDERR_BYTES: usize = 2_048;

/// One runtime keyspace of the host-owned shared package cache.
pub struct SharedPackageCache {
    runtime_dir: PathBuf,
}

/// What one verification-and-promotion pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PromotionReport {
    /// Staged artifacts verified and promoted into the cache.
    pub promoted: usize,
    /// Staged artifacts refused (name/type/size, or hash disagreement with an
    /// already-recorded entry).
    pub refused: usize,
    /// Cached artifacts removed because their bytes no longer match the
    /// manifest, or because no manifest entry records them.
    pub invalidated: usize,
    /// Cached artifacts evicted to keep the cache under its size bound.
    pub evicted: usize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    sha256: String,
    bytes: u64,
    promoted_at: u64,
}

fn cache_error(message: impl Into<String>) -> CodeExecutionError {
    CodeExecutionError::Sandbox(message.into())
}

/// Whether `key` is a well-formed runtime key: bounded, lowercase
/// alphanumerics plus `._-`, and not dotted at the front (so it can never
/// collide with the cache's own control files or hide from listings).
fn is_valid_runtime_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_RUNTIME_KEY_BYTES
        && !key.starts_with('.')
        && key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// Whether `name` is a plausible wheel filename safe to record and serve:
/// bounded ASCII, no separators or leading dot, ending in `.whl`.
fn is_valid_artifact_name(name: &str) -> bool {
    name.len() <= 255
        && name.ends_with(".whl")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

fn secure_dir(path: &Path) -> Result<(), CodeExecutionError> {
    fs::create_dir_all(path)
        .map_err(|_| cache_error("could not create the shared package cache"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| cache_error("could not inspect the shared package cache"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(cache_error(
            "shared package cache is not a regular directory",
        ));
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| cache_error("could not secure the shared package cache"))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Option<(String, u64)> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        total += read as u64;
        hasher.update(&buffer[..read]);
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Some((hex, total))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

impl SharedPackageCache {
    /// Open (creating if needed) the cache keyspace for one runtime key under
    /// `cache_root`. The root and keyspace are host-owned `0o700` directories.
    pub fn open(cache_root: &Path, runtime_key: &str) -> Result<Self, CodeExecutionError> {
        if !is_valid_runtime_key(runtime_key) {
            return Err(cache_error("shared package cache runtime key is invalid"));
        }
        secure_dir(cache_root)?;
        let runtime_dir = cache_root.join(runtime_key);
        secure_dir(&runtime_dir)?;
        secure_dir(&runtime_dir.join(WHEELS_DIR))?;
        Ok(Self { runtime_dir })
    }

    /// The directory of verified wheels, the only path a sandbox is shown.
    #[must_use]
    pub fn wheels_dir(&self) -> PathBuf {
        self.runtime_dir.join(WHEELS_DIR)
    }

    /// Whether the cache holds at least one verified artifact worth mounting.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.load_manifest()
            .is_some_and(|manifest| !manifest.entries.is_empty())
    }

    /// An unreadable or unparseable manifest is `None`: integrity is unknown,
    /// so nothing is served until verification rebuilds from an empty record.
    fn load_manifest(&self) -> Option<Manifest> {
        let path = self.runtime_dir.join(MANIFEST_FILE);
        let file = fs::File::open(&path).ok()?;
        if file.metadata().ok()?.len() > MAX_MANIFEST_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn store_manifest(&self, manifest: &Manifest) -> Result<(), CodeExecutionError> {
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|_| cache_error("could not encode the package cache manifest"))?;
        let temporary = self.runtime_dir.join(format!(".{MANIFEST_FILE}.tmp"));
        fs::write(&temporary, bytes)
            .and_then(|()| fs::rename(&temporary, self.runtime_dir.join(MANIFEST_FILE)))
            .map_err(|_| cache_error("could not persist the package cache manifest"))
    }

    /// Verify the cached artifacts against the manifest and promote staged
    /// ones, then evict down to the size bound. See [`promote_with_limits`].
    pub fn verify_and_promote(
        &self,
        staging: &Path,
    ) -> Result<PromotionReport, CodeExecutionError> {
        self.promote_with_limits(Some(staging), MAX_ARTIFACT_BYTES, MAX_TOTAL_BYTES)
    }

    /// Re-verify every cached artifact without promoting anything.
    pub fn verify(&self) -> Result<PromotionReport, CodeExecutionError> {
        self.promote_with_limits(None, MAX_ARTIFACT_BYTES, MAX_TOTAL_BYTES)
    }

    fn promote_with_limits(
        &self,
        staging: Option<&Path>,
        max_artifact_bytes: u64,
        max_total_bytes: u64,
    ) -> Result<PromotionReport, CodeExecutionError> {
        let wheels = self.wheels_dir();
        let mut report = PromotionReport::default();
        // Integrity-unknown manifests rebuild from empty: every unrecorded
        // file below is then invalidated rather than adopted.
        let mut manifest = self.load_manifest().unwrap_or_default();

        // Verification pass: the manifest is the record of what the trusted
        // acquisition produced. Bytes that disagree with it — and files it
        // never recorded — are removed, never served or re-adopted.
        let mut verified = BTreeMap::new();
        let entries = fs::read_dir(&wheels)
            .map_err(|_| cache_error("shared package cache wheels are unavailable"))?;
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                let _ = fs::remove_file(entry.path());
                report.invalidated += 1;
                continue;
            };
            let regular = entry
                .path()
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
            let recorded = manifest.entries.get(&name);
            let intact = regular
                && recorded.is_some_and(|entry_record| {
                    sha256_file(&entry.path()).is_some_and(|(sha256, bytes)| {
                        sha256 == entry_record.sha256 && bytes == entry_record.bytes
                    })
                });
            if intact {
                verified.insert(name.clone(), recorded.expect("checked above").clone());
            } else {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(&path);
                } else {
                    let _ = fs::remove_file(&path);
                }
                report.invalidated += 1;
            }
        }
        manifest.entries = verified;

        // Promotion pass: staged files come from the host-side acquisition,
        // but are still re-hashed into the manifest here so later tampering is
        // detectable, and a name collision with different bytes is refused —
        // wheel filenames are immutable upstream, so disagreement is never
        // legitimate.
        if let Some(staging) = staging {
            let staged = fs::read_dir(staging)
                .map_err(|_| cache_error("package cache staging directory is unavailable"))?;
            for entry in staged.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    report.refused += 1;
                    continue;
                };
                let regular = entry
                    .path()
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
                if !regular || !is_valid_artifact_name(&name) {
                    report.refused += 1;
                    continue;
                }
                let Some((sha256, bytes)) = sha256_file(&entry.path()) else {
                    report.refused += 1;
                    continue;
                };
                if bytes > max_artifact_bytes {
                    report.refused += 1;
                    continue;
                }
                if let Some(existing) = manifest.entries.get(&name) {
                    if existing.sha256 != sha256 {
                        report.refused += 1;
                    }
                    // Identical bytes are already cached; nothing to do.
                    continue;
                }
                if fs::rename(entry.path(), wheels.join(&name)).is_err() {
                    report.refused += 1;
                    continue;
                }
                manifest.entries.insert(
                    name,
                    ManifestEntry {
                        sha256,
                        bytes,
                        promoted_at: unix_now(),
                    },
                );
                report.promoted += 1;
            }
        }

        // Bounded lifecycle: oldest promotions leave first.
        let mut total: u64 = manifest.entries.values().map(|entry| entry.bytes).sum();
        while total > max_total_bytes {
            let Some(oldest) = manifest
                .entries
                .iter()
                .min_by_key(|(name, entry)| (entry.promoted_at, (*name).clone()))
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            let removed = manifest.entries.remove(&oldest).expect("key came from map");
            let _ = fs::remove_file(wheels.join(&oldest));
            total = total.saturating_sub(removed.bytes);
            report.evicted += 1;
        }

        self.store_manifest(&manifest)?;
        Ok(report)
    }

    /// Derive the wheel-compatibility cache key from the interpreter the local
    /// sandbox actually runs. `None` disables the cache on hosts where the
    /// interpreter is unusable.
    pub async fn runtime_key(python: &Path) -> Option<String> {
        let probe = tokio::process::Command::new(python)
            .args([
                "-c",
                "import platform, sys; \
                 print(f'cp{sys.version_info[0]}{sys.version_info[1]}-{sys.platform}-{platform.machine().lower()}')",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(RUNTIME_KEY_TIMEOUT, probe)
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let key = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        is_valid_runtime_key(&key).then_some(key)
    }

    /// Host-side acquisition: download the exactly pinned requirements (and
    /// their transitive closure) as wheels with the host's own pip, then
    /// verify and promote them.
    ///
    /// `--only-binary=:all:` keeps acquisition metadata-only — no sdist build
    /// step ever executes package code on the host — and pip authenticates
    /// each downloaded file against the index's published hashes over host
    /// TLS. The sandbox contributes nothing to this path: the pins come from
    /// repo-authored skill manifests, already validated as exact
    /// `package==version` requirements.
    pub async fn populate_with_pip(
        &self,
        python: &Path,
        pins: &[String],
    ) -> Result<PromotionReport, CodeExecutionError> {
        if pins.is_empty() {
            return Ok(PromotionReport::default());
        }
        if !pins
            .iter()
            .all(|pin| crate::skills::is_pinned_python_dep(pin))
        {
            return Err(cache_error(
                "package cache pins must be exact package==version requirements",
            ));
        }
        let staging = self
            .runtime_dir
            .join(format!(".staging-{}", uuid::Uuid::new_v4()));
        secure_dir(&staging)?;
        let result = self.download_into(python, pins, &staging).await;
        let promoted = match result {
            Ok(()) => self.verify_and_promote(&staging),
            Err(error) => Err(error),
        };
        let _ = fs::remove_dir_all(&staging);
        promoted
    }

    async fn download_into(
        &self,
        python: &Path,
        pins: &[String],
        staging: &Path,
    ) -> Result<(), CodeExecutionError> {
        let download = tokio::process::Command::new(python)
            .args([
                "-m",
                "pip",
                "download",
                "--only-binary=:all:",
                "--disable-pip-version-check",
                "--no-input",
                "--quiet",
                "--dest",
            ])
            .arg(staging)
            .args(pins)
            .current_dir(&self.runtime_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(PIP_DOWNLOAD_TIMEOUT, download)
            .await
            .map_err(|_| cache_error("package cache acquisition timed out"))?
            .map_err(|_| cache_error("could not run the host package acquisition"))?;
        if !output.status.success() {
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            stderr.truncate(
                (0..=MAX_REPORTED_STDERR_BYTES.min(stderr.len()))
                    .rev()
                    .find(|&end| stderr.is_char_boundary(end))
                    .unwrap_or(0),
            );
            return Err(cache_error(format!(
                "package cache acquisition failed: {}",
                stderr.trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(dir: &Path, name: &str, bytes: &[u8]) {
        fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn promotion_records_hashes_and_verification_refuses_tampering() {
        let root = tempfile::tempdir().unwrap();
        let cache = SharedPackageCache::open(root.path(), "cp39-darwin-arm64").unwrap();
        assert!(!cache.is_ready());
        assert!(SharedPackageCache::open(root.path(), "../escape").is_err());
        assert!(SharedPackageCache::open(root.path(), ".hidden").is_err());

        let staging = root.path().join("staging");
        fs::create_dir(&staging).unwrap();
        stage(&staging, "fpdf2-2.8.3-py3-none-any.whl", b"fpdf-bytes");
        stage(&staging, "pypdf-5.1.0-py3-none-any.whl", b"pypdf-bytes");
        stage(&staging, "notes.txt", b"not a wheel");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.path().join("outside"),
            staging.join("linked-1.0-py3-none-any.whl"),
        )
        .unwrap();

        let report = cache.verify_and_promote(&staging).unwrap();
        assert_eq!(report.promoted, 2);
        assert!(report.refused >= 1, "non-wheel entries must be refused");
        assert!(cache.is_ready());
        let wheels = cache.wheels_dir();
        assert!(wheels.join("fpdf2-2.8.3-py3-none-any.whl").is_file());
        assert!(!wheels.join("notes.txt").exists());
        assert!(!wheels.join("linked-1.0-py3-none-any.whl").exists());

        // A re-staged filename with different bytes is refused: the recorded
        // artifact stays authoritative.
        let restage = root.path().join("restage");
        fs::create_dir(&restage).unwrap();
        stage(&restage, "fpdf2-2.8.3-py3-none-any.whl", b"poisoned");
        let conflicted = cache.verify_and_promote(&restage).unwrap();
        assert_eq!(conflicted.promoted, 0);
        assert_eq!(conflicted.refused, 1);
        assert_eq!(
            fs::read(wheels.join("fpdf2-2.8.3-py3-none-any.whl")).unwrap(),
            b"fpdf-bytes"
        );

        // Bytes tampered after promotion — and files the manifest never
        // recorded — are removed on the next verification, never served.
        fs::write(wheels.join("fpdf2-2.8.3-py3-none-any.whl"), b"tampered").unwrap();
        fs::write(wheels.join("planted-1.0-py3-none-any.whl"), b"planted").unwrap();
        let verified = cache.verify().unwrap();
        assert_eq!(verified.invalidated, 2);
        assert!(!wheels.join("fpdf2-2.8.3-py3-none-any.whl").exists());
        assert!(!wheels.join("planted-1.0-py3-none-any.whl").exists());
        assert!(wheels.join("pypdf-5.1.0-py3-none-any.whl").is_file());
        assert!(cache.is_ready());

        // A corrupt manifest means integrity is unknown: everything on disk is
        // invalidated instead of adopted.
        fs::write(
            root.path().join("cp39-darwin-arm64/manifest.json"),
            b"{ not json",
        )
        .unwrap();
        assert!(!cache.is_ready());
        let rebuilt = cache.verify().unwrap();
        assert_eq!(rebuilt.invalidated, 1);
        assert!(!wheels.join("pypdf-5.1.0-py3-none-any.whl").exists());
    }

    #[test]
    fn eviction_bounds_the_cache_and_drops_the_oldest_promotions_first() {
        let root = tempfile::tempdir().unwrap();
        let cache = SharedPackageCache::open(root.path(), "cp39-darwin-arm64").unwrap();
        let staging = root.path().join("staging");
        fs::create_dir(&staging).unwrap();
        stage(&staging, "first-1.0-py3-none-any.whl", &[b'a'; 40]);
        cache
            .promote_with_limits(Some(&staging), 1_024, 100)
            .unwrap();
        // A later promotion pushes the total over the bound; the older entry
        // leaves and the newer survives.
        stage(&staging, "second-1.0-py3-none-any.whl", &[b'b'; 80]);
        let report = cache
            .promote_with_limits(Some(&staging), 1_024, 100)
            .unwrap();
        assert_eq!(report.promoted, 1);
        assert_eq!(report.evicted, 1);
        let wheels = cache.wheels_dir();
        assert!(!wheels.join("first-1.0-py3-none-any.whl").exists());
        assert!(wheels.join("second-1.0-py3-none-any.whl").is_file());

        // An artifact over the per-file bound is refused outright.
        stage(&staging, "huge-1.0-py3-none-any.whl", &[b'c'; 200]);
        let oversized = cache
            .promote_with_limits(Some(&staging), 150, 1_000)
            .unwrap();
        assert_eq!(oversized.promoted, 0);
        assert_eq!(oversized.refused, 1);
        assert!(!wheels.join("huge-1.0-py3-none-any.whl").exists());
    }
}

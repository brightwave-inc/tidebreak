//! Outbound trust for the sandbox's intercepting proxy.
//!
//! The supervising environment routes every outbound TLS connection through
//! an intercepting sidecar and mounts the sidecar's CA certificate into the
//! pod. A child process that does not trust that CA fails certificate
//! verification on its first request, so before anything leaves the pod the
//! agent waits for the certificate, merges it with the image's system roots,
//! and hands every spawned child the resulting bundle through the standard
//! trust variables.
//!
//! The agent never mutates its own process environment. The crate forbids
//! `unsafe`, and a global mutation would race every thread; instead
//! [`Trust::environment`] returns the pairs and each spawned child carries
//! them explicitly.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Path of the sidecar's CA certificate, when the environment overrides it.
pub const CA_CERTIFICATE_VARIABLE: &str = "SANDBOX_CA_CERTIFICATE";
/// Seconds to wait for the certificate, when the environment overrides it.
pub const CA_TIMEOUT_VARIABLE: &str = "SANDBOX_CA_TIMEOUT";

const DEFAULT_CA_CERTIFICATE: &str = "/var/run/model-gateway/sandbox-ca/ca.crt";
const DEFAULT_CA_TIMEOUT: Duration = Duration::from_secs(60);
/// Where a Linux image keeps its trusted roots.
const SYSTEM_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// Where the trust inputs live for one run.
#[derive(Clone, Debug)]
pub struct TrustOptions {
    /// The sidecar CA certificate the agent waits for.
    pub certificate: PathBuf,
    /// How long to wait for the certificate to appear.
    pub timeout: Duration,
    /// The image's own root bundle, when one exists to merge.
    pub baseline: Option<PathBuf>,
    /// Where the merged bundle is written.
    pub bundle: PathBuf,
}

impl TrustOptions {
    /// Resolves the paths the environment declares, with defaults for the
    /// rest.
    #[must_use]
    pub fn from_env() -> Self {
        let certificate = std::env::var(CA_CERTIFICATE_VARIABLE)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_CA_CERTIFICATE), PathBuf::from);
        let timeout = std::env::var(CA_TIMEOUT_VARIABLE)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map_or(DEFAULT_CA_TIMEOUT, Duration::from_secs);
        Self {
            certificate,
            timeout,
            baseline: system_roots(),
            bundle: std::env::temp_dir().join("sandbox-ca-bundle.pem"),
        }
    }
}

/// Finds the image's root bundle: the standard path, or the same path
/// relative to the agent's own install prefix for a relocated toolchain.
fn system_roots() -> Option<PathBuf> {
    let system = PathBuf::from(SYSTEM_CA_BUNDLE);
    if system.is_file() {
        return Some(system);
    }
    let executable = std::env::current_exe().ok()?;
    let relative = executable
        .parent()?
        .parent()?
        .join("etc/ssl/certs/ca-certificates.crt");
    relative.is_file().then_some(relative)
}

/// The prepared trust material every spawned child carries.
#[derive(Clone, Debug)]
pub struct Trust {
    /// The merged bundle: system roots plus the sidecar CA.
    pub bundle: PathBuf,
    /// The sidecar CA alone, for the variables that are additive.
    pub certificate: PathBuf,
    /// Whether system roots were found and merged in.
    pub merged_system_roots: bool,
}

impl Trust {
    /// The trust variables each spawned child carries.
    ///
    /// The first four replace their tool's default root store, so they name
    /// the merged bundle; `NODE_EXTRA_CA_CERTS` is additive on top of Node's
    /// own roots, so it names the sidecar CA alone.
    #[must_use]
    pub fn environment(&self) -> [(&'static str, &Path); 5] {
        [
            ("SSL_CERT_FILE", self.bundle.as_path()),
            ("REQUESTS_CA_BUNDLE", self.bundle.as_path()),
            ("CURL_CA_BUNDLE", self.bundle.as_path()),
            ("GIT_SSL_CAINFO", self.bundle.as_path()),
            ("NODE_EXTRA_CA_CERTS", self.certificate.as_path()),
        ]
    }
}

/// Trust could not be prepared; the pod cannot reach anything without it.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TrustError {
    /// What went wrong, naming the path involved.
    pub message: String,
}

impl TrustError {
    fn new(message: String) -> Self {
        Self { message }
    }
}

/// Waits for the sidecar CA and writes the merged bundle.
///
/// Blocking: call it before the async loop starts. The wait polls once a
/// second until the certificate file exists and is non-empty, because the
/// sidecar and the workload container start concurrently and the certificate
/// is written when the sidecar is ready.
pub fn prepare(options: &TrustOptions) -> Result<Trust, TrustError> {
    wait_for_certificate(&options.certificate, options.timeout)?;
    let certificate = std::fs::read(&options.certificate).map_err(|error| {
        TrustError::new(format!(
            "{} could not be read: {error}",
            options.certificate.display()
        ))
    })?;
    let mut bundle = Vec::new();
    let mut merged_system_roots = false;
    if let Some(baseline) = &options.baseline {
        let mut roots = std::fs::read(baseline).map_err(|error| {
            TrustError::new(format!(
                "the system root bundle {} could not be read: {error}",
                baseline.display()
            ))
        })?;
        if !roots.ends_with(b"\n") {
            roots.push(b'\n');
        }
        bundle = roots;
        merged_system_roots = true;
    }
    bundle.extend_from_slice(&certificate);
    std::fs::write(&options.bundle, &bundle).map_err(|error| {
        TrustError::new(format!(
            "the trust bundle {} could not be written: {error}",
            options.bundle.display()
        ))
    })?;
    Ok(Trust {
        bundle: options.bundle.clone(),
        certificate: options.certificate.clone(),
        merged_system_roots,
    })
}

fn wait_for_certificate(path: &Path, timeout: Duration) -> Result<(), TrustError> {
    let deadline = Instant::now() + timeout;
    loop {
        if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(TrustError::new(format!(
                "{} never appeared; the trust sidecar did not become ready within {} seconds",
                path.display(),
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(root: &Path) -> TrustOptions {
        TrustOptions {
            certificate: root.join("ca.crt"),
            timeout: Duration::ZERO,
            baseline: None,
            bundle: root.join("bundle.pem"),
        }
    }

    #[test]
    fn the_bundle_merges_roots_ahead_of_the_sidecar_ca() {
        let root = tempfile::tempdir().unwrap();
        let mut options = options(root.path());
        std::fs::write(&options.certificate, "SIDECAR\n").unwrap();
        let baseline = root.path().join("roots.crt");
        // No trailing newline: the merge must add one so the PEM blocks
        // stay separated.
        std::fs::write(&baseline, "ROOTS").unwrap();
        options.baseline = Some(baseline);

        let trust = prepare(&options).unwrap();
        assert!(trust.merged_system_roots);
        assert_eq!(
            std::fs::read_to_string(&trust.bundle).unwrap(),
            "ROOTS\nSIDECAR\n"
        );
        // The additive variable names the sidecar CA alone; the replacing
        // ones name the merged bundle.
        let environment = trust.environment();
        assert!(environment
            .iter()
            .take(4)
            .all(|(_, path)| *path == trust.bundle));
        assert_eq!(environment[4].0, "NODE_EXTRA_CA_CERTS");
        assert_eq!(environment[4].1, trust.certificate);
    }

    #[test]
    fn no_baseline_means_the_bundle_is_the_sidecar_ca() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        std::fs::write(&options.certificate, "SIDECAR\n").unwrap();
        let trust = prepare(&options).unwrap();
        assert!(!trust.merged_system_roots);
        assert_eq!(std::fs::read_to_string(&trust.bundle).unwrap(), "SIDECAR\n");
    }

    #[test]
    fn a_certificate_that_never_appears_fails_naming_the_path() {
        let root = tempfile::tempdir().unwrap();
        let error = prepare(&options(root.path())).unwrap_err();
        assert!(error.message.contains("ca.crt"));
        assert!(error.message.contains("never appeared"));
    }

    /// The sidecar creates the file before writing it; an empty file must
    /// read as "not ready yet", not as an empty trust bundle.
    #[test]
    fn an_empty_certificate_is_not_ready() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        std::fs::write(&options.certificate, "").unwrap();
        let error = prepare(&options).unwrap_err();
        assert!(error.message.contains("never appeared"));
    }
}

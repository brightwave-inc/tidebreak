//! Managed Node.js runtime for skills whose npm packages need an interpreter.
//!
//! Skills that declare npm dependencies need a Node to run them, and the local
//! sandbox pins `PATH` to system directories where most machines have nothing.
//! Rather than making the user install Node, the app fetches its own copy the
//! first time a turn stages such a skill: an exact pinned version from
//! nodejs.org's official distribution, digest-verified before a single byte of
//! it is unpacked, installed under the app's data directory
//! (`tools/node/<version>/`) with no admin rights and no writes outside
//! app-owned paths. The resolved directory is what the execution backend is
//! handed as a read-only allowance; nothing else about it is exposed.
//!
//! The version is pinned to match the container image's `node:20-bookworm-slim`
//! so a skill behaves the same locally and in the cloud. The npm bundled in the
//! official tarball is the npm the sandbox uses — there is no separate npm
//! install to keep in step.
//!
//! Supply-chain posture, deliberately rigid, and the same as the managed
//! LibreOffice install: the URL names an exact version — never "latest" — and
//! the artifact's SHA-256 is pinned in this file next to it, taken from the
//! Node project's published checksum file:
//!
//! - <https://nodejs.org/dist/v20.20.2/SHASUMS256.txt>
//!
//! macOS and Linux are supported on both shipped architectures. The local
//! sandbox remains macOS-only, but code mode uses the same managed runtime to
//! install and launch its pinned harness packages on Linux.
//!
//! Gatekeeper: a download performed by this process carries no
//! `com.apple.quarantine` attribute — quarantine is applied by applications
//! that opt in via `LSFileQuarantineEnabled` (browsers), not by the kernel — so
//! the unpacked `node` runs without any Gatekeeper prompt. Verified end to end
//! on a real download of this pinned artifact: `bin/node` and the bundled
//! `bin/npm` both run straight out of the unpacked tree. The quarantine
//! attribute is still stripped after the unpack, deliberately: it costs one
//! `xattr` invocation and keeps the install working if a future code path ever
//! hands us a quarantined file.
//!
//! Integrity at rest: the install directory carries an `installed.json` marker
//! recording the version and the verified tarball digest. Resolution trusts the
//! managed install only when the marker matches the constants pinned here — a
//! marker for a different digest (a tampered or half-written install, or a
//! stale layout after the pin moves) makes the managed copy invisible and a
//! fresh install replaces it. The tree is not re-hashed per turn; the marker
//! plus the `node` and `npm` entrypoints' presence is the check.
//!
//! Failure discipline: one failed install is remembered for the rest of the app
//! run and reported as the unavailable reason. Nothing re-downloads on its own;
//! an explicit retry clears the memory. Installs serialize, so several skills
//! needing Node at once produce one download.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

/// The exact Node version this build installs and trusts, matching the
/// container image's `node:20-bookworm-slim`.
pub(crate) const NODE_VERSION: &str = "20.20.2";

/// The unpacked runtime is about 154 MB and the tarball about 40 MB; refuse to
/// start on a disk with less headroom than that plus slack.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const REQUIRED_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// Official Node archives for the supported targets are below 55 MB. Bound
/// the streamed response independently of Content-Length so a bad endpoint
/// cannot fill the app-data volume before the digest check gets a vote.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(any(target_os = "macos", target_os = "linux", test))]
const DOWNLOAD_TOO_LARGE: &str = "The Node download was larger than expected and was discarded";

struct PinnedArtifact {
    // Read only by the macOS/Linux install path; unsupported platforms carry
    // the pinned shape (`PINNED = None`) without an installer to consume it.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    url: &'static str,
    sha256: &'static str,
    /// The single directory the official tarball unpacks into, which the
    /// install moves into place as the version directory.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    archive_root: &'static str,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-darwin-arm64.tar.gz",
    sha256: "466e05f3477c20dfb723054dfebffe55bc74660ee77f612166fca121dacb65b6",
    archive_root: "node-v20.20.2-darwin-arm64",
});

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-darwin-x64.tar.gz",
    sha256: "8be6f5e4bb128c82774f8a0b8d7a1cc1365a7977d9657cece0ca647b3fe04e61",
    archive_root: "node-v20.20.2-darwin-x64",
});

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-arm64.tar.gz",
    sha256: "47ef73d543ecf6eb19435f6c03a0ac4809b3bf0dd6b26c7c571efc2a6572a74d",
    archive_root: "node-v20.20.2-linux-arm64",
});

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.gz",
    sha256: "19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b",
    archive_root: "node-v20.20.2-linux-x64",
});

#[cfg(not(any(
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
    all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
)))]
static PINNED: Option<PinnedArtifact> = None;

/// Whether this platform can install its own Node runtime.
fn supported() -> bool {
    PINNED.is_some()
}

/// The install failure most recently hit this app run, if any. While set,
/// staging reports Node unavailable with this reason instead of downloading
/// again; an explicit retry clears it.
fn last_failure() -> Option<String> {
    state()
        .lock()
        .expect("install state lock")
        .last_failure
        .clone()
}

fn managed_version_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("node").join(NODE_VERSION)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("node").join("staging")
}

fn marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join("installed.json")
}

fn managed_binary(version_dir: &Path) -> PathBuf {
    version_dir.join("bin").join("node")
}

fn managed_npm(version_dir: &Path) -> PathBuf {
    version_dir.join("bin").join("npm")
}

/// What `installed.json` records: which artifact the directory was verified
/// against. Written only after the digest check and the unpack both succeeded.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    version: String,
    tarball_sha256: String,
}

/// The managed Node runtime's root directory, if a verified install of the
/// pinned artifact is present. This is the cheap at-rest check: marker matches
/// the pin, and both `bin/node` and `bin/npm` exist.
pub(crate) fn managed_node_root(data_dir: &Path) -> Option<PathBuf> {
    let pinned = PINNED.as_ref()?;
    managed_node_root_expecting(data_dir, pinned.sha256)
}

/// Seam for [`managed_node_root`]: the same resolution against an explicit
/// expected digest, so the marker gate is testable off-macOS.
fn managed_node_root_expecting(data_dir: &Path, expected_sha256: &str) -> Option<PathBuf> {
    let version_dir = managed_version_dir(data_dir);
    let marker = std::fs::read(marker_path(&version_dir)).ok()?;
    let marker: InstallMarker = serde_json::from_slice(&marker).ok()?;
    if marker.version != NODE_VERSION || marker.tarball_sha256 != expected_sha256 {
        return None;
    }
    (managed_binary(&version_dir).is_file() && managed_npm(&version_dir).is_file())
        .then_some(version_dir)
}

struct InstallState {
    last_failure: Option<String>,
    installing: bool,
}

fn state() -> &'static Mutex<InstallState> {
    static STATE: LazyLock<Mutex<InstallState>> = LazyLock::new(|| {
        Mutex::new(InstallState {
            last_failure: None,
            installing: false,
        })
    });
    &STATE
}

/// Serializes installs; several staged skills hitting a cold machine at once
/// produce one download.
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    &LOCK
}

/// The one install path both entry points share: serialize on the install
/// lock, short-circuit if a concurrent caller already finished, and remember
/// the outcome.
async fn ensure_installed(app: &AppHandle) -> Result<(), String> {
    let data_dir = crate::data_dir(app)?;
    let _serialized = install_lock().lock().await;
    if managed_node_root(&data_dir).is_some() {
        // Another caller finished the install while this one waited.
        state().lock().expect("install state lock").last_failure = None;
        return Ok(());
    }

    {
        let mut state = state().lock().expect("install state lock");
        state.last_failure = None;
        state.installing = true;
    }
    let outcome = run_install(&data_dir).await;
    {
        let mut state = state().lock().expect("install state lock");
        state.installing = false;
        state.last_failure = outcome.as_ref().err().cloned();
    }
    outcome
}

/// Start the managed install in the background, at most once per app run.
///
/// This is what the host-tool broker's `ensure` calls: it returns immediately,
/// never blocks a turn, and defers to every existing rule — a verified managed
/// install and a remembered failure each make it a no-op. Turn staging reads
/// the resulting truth through `status`, so a turn staged mid-install says
/// "installing" rather than pretending the runtime is there.
fn warm_node_runtime(app: AppHandle) {
    if !supported() {
        return;
    }
    static WARM_REQUESTED: AtomicBool = AtomicBool::new(false);
    if WARM_REQUESTED.swap(true, Ordering::Relaxed) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Ok(data_dir) = crate::data_dir(&app) else {
            return;
        };
        if managed_node_root(&data_dir).is_some() || last_failure().is_some() {
            return;
        }
        // Failures are remembered, not retried; the explicit retry command is
        // the only thing that clears them.
        let _ = ensure_installed(&app).await;
    });
}

/// Install the managed Node runtime, clearing a remembered failure first.
///
/// The explicit retry the failure memory is waiting for: the background warm-up
/// gives up for the rest of the app run after one failure, and this is how that
/// decision is taken back.
#[tauri::command]
pub(crate) async fn install_node_runtime(app: AppHandle) -> Result<(), String> {
    state().lock().expect("install state lock").last_failure = None;
    ensure_installed(&app).await
}

/// The host-tool broker's `ensure` for Node.
pub(crate) fn ensure(app: AppHandle) {
    warm_node_runtime(app);
}

/// Explicit retry for a surface whose user asked to try provisioning again.
/// Unlike background `ensure`, this clears the run-scoped failure memory on
/// the calling thread before the install is spawned, so a status poll cannot
/// still return the previous failure.
pub(crate) fn retry(app: AppHandle) {
    state().lock().expect("install state lock").last_failure = None;
    tauri::async_runtime::spawn(async move {
        let _ = ensure_installed(&app).await;
    });
}

/// The current truth about the managed Node runtime on this machine.
///
/// Only the managed install counts. A system `node` on the user's `PATH` is
/// deliberately not consulted: the sandbox is handed a directory to expose
/// read-only, and an arbitrary host install is neither version-pinned nor
/// somewhere the sandbox profile may reach.
pub(crate) async fn status(app: &AppHandle) -> tidebreak_code_execution::HostToolStatus {
    use tidebreak_code_execution::HostToolStatus;
    let Ok(data_dir) = crate::data_dir(app) else {
        return HostToolStatus::Unavailable("the app data directory is unavailable".into());
    };
    if managed_node_root(&data_dir).is_some() {
        return HostToolStatus::Available;
    }
    if state().lock().expect("install state lock").installing {
        return HostToolStatus::Installing;
    }
    if let Some(reason) = last_failure() {
        return HostToolStatus::Unavailable(reason);
    }
    if supported() {
        HostToolStatus::Unavailable(format!("Node {NODE_VERSION} is not installed yet"))
    } else {
        HostToolStatus::Unavailable(
            "automatic Node install is not supported on this platform; install Node 20 yourself \
             to use skills that need it"
                .into(),
        )
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run_install(data_dir: &Path) -> Result<(), String> {
    let pinned = PINNED
        .as_ref()
        .ok_or_else(|| "Automatic install is not supported on this platform".to_owned())?;

    let staging = staging_dir(data_dir);
    // A previous run's partial download or half-unpacked tree; start clean.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|error| format!("Could not prepare the install directory: {error}"))?;

    let result = install_into(data_dir, &staging, pinned).await;

    // The staging directory never survives: success moved the tree out,
    // failure leaves only partial artifacts worth reclaiming.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn run_install(_data_dir: &Path) -> Result<(), String> {
    Err("Automatic install is not supported on this platform".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn install_into(
    data_dir: &Path,
    staging: &Path,
    pinned: &PinnedArtifact,
) -> Result<(), String> {
    crate::office_install::ensure_free_disk(
        staging,
        REQUIRED_FREE_BYTES,
        "Not enough free disk space to install Node (about 500 MB is needed)",
    )
    .await?;

    let tarball = staging.join("node.tar.gz");
    download(pinned.url, &tarball).await?;

    // Verify against the pinned digest before touching the archive in any way.
    let digest = {
        let tarball = tarball.clone();
        tokio::task::spawn_blocking(move || crate::office_install::sha256_hex_of_file(&tarball))
            .await
            .map_err(|error| format!("Could not verify the download: {error}"))?
            .map_err(|error| format!("Could not verify the download: {error}"))?
    };
    if digest != pinned.sha256 {
        let _ = tokio::fs::remove_file(&tarball).await;
        return Err(
            "The downloaded Node did not match its published checksum, so it was discarded. \
             This can mean a corrupted download or a tampered one — try again later."
                .to_owned(),
        );
    }

    let unpacked = staging.join("unpacked");
    tokio::fs::create_dir(&unpacked)
        .await
        .map_err(|error| format!("Could not prepare the install directory: {error}"))?;
    unpack(&tarball, &unpacked).await?;
    let extracted = unpacked.join(pinned.archive_root);
    if !managed_binary(&extracted).is_file() {
        return Err("The downloaded Node archive did not contain a runtime".to_owned());
    }

    // Deliberate quarantine strip on macOS; see the module docs for why this
    // is a no-op today and kept anyway. Linux has no corresponding attribute.
    #[cfg(target_os = "macos")]
    {
        let _ = crate::office_install::run_tool(
            "/usr/bin/xattr",
            &[
                "-dr".as_ref(),
                "com.apple.quarantine".as_ref(),
                extracted.as_os_str(),
            ],
            Duration::from_secs(120),
        )
        .await;
    }

    let version_dir = managed_version_dir(data_dir);
    tokio::fs::create_dir_all(
        version_dir
            .parent()
            .ok_or_else(|| "Could not prepare the install directory".to_owned())?,
    )
    .await
    .map_err(|error| format!("Could not prepare the install directory: {error}"))?;
    // A leftover unverified tree (no marker names it) makes the rename fail;
    // replace it.
    let _ = tokio::fs::remove_dir_all(&version_dir).await;
    tokio::fs::rename(&extracted, &version_dir)
        .await
        .map_err(|error| format!("Could not move Node into place: {error}"))?;

    // The marker lands last, so a crash anywhere above leaves an install that
    // resolution ignores and the next attempt replaces.
    let marker = serde_json::to_vec_pretty(&InstallMarker {
        version: NODE_VERSION.to_owned(),
        tarball_sha256: pinned.sha256.to_owned(),
    })
    .map_err(|error| format!("Could not record the install: {error}"))?;
    let marker_file = marker_path(&version_dir);
    let partial = marker_file.with_extension("json.partial");
    tokio::fs::write(&partial, marker)
        .await
        .map_err(|error| format!("Could not record the install: {error}"))?;
    tokio::fs::rename(&partial, &marker_file)
        .await
        .map_err(|error| format!("Could not record the install: {error}"))?;

    if managed_node_root(data_dir).is_none() {
        return Err("The installed Node did not verify".to_owned());
    }
    Ok(())
}

/// Unpack the official tarball into `destination`.
///
/// The in-process tar reader preserves the archive's `bin/npm` and `bin/npx`
/// symlinks into `lib/node_modules/npm`, so the installed runtime does not
/// depend on a system `tar` binary being present on the reader's machine.
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn unpack(tarball: &Path, destination: &Path) -> Result<(), String> {
    let tarball = tarball.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::time::timeout(
        Duration::from_secs(300),
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&tarball)
                .map_err(|error| format!("Could not open the Node archive: {error}"))?;
            let gzip = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(gzip);
            archive
                .unpack(&destination)
                .map_err(|error| format!("Could not unpack Node: {error}"))
        }),
    )
    .await
    .map_err(|_| "Unpacking Node timed out".to_owned())?
    .map_err(|error| format!("Could not join the Node unpack task: {error}"))?
}

/// Stream the artifact to disk. Verification happens after, against the whole
/// file.
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn download(url: &str, destination: &Path) -> Result<(), String> {
    use futures::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let client = reqwest::Client::builder()
        .build()
        .map_err(|error| format!("Could not start the download: {error}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Could not download Node: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not download Node: the server answered {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(DOWNLOAD_TOO_LARGE.to_owned());
    }

    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("Could not write the download: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut downloaded = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("The download was interrupted: {error}"))?;
        downloaded = next_downloaded_size(downloaded, chunk.len())?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Could not write the download: {error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("Could not write the download: {error}"))?;
    Ok(())
}

/// Account for one response chunk before it reaches disk. Content-Length is
/// only a hint; this is the bound for chunked or dishonest responses.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn next_downloaded_size(downloaded: u64, chunk_bytes: usize) -> Result<u64, String> {
    let chunk_bytes = u64::try_from(chunk_bytes).map_err(|_| DOWNLOAD_TOO_LARGE.to_owned())?;
    let next = downloaded
        .checked_add(chunk_bytes)
        .ok_or_else(|| DOWNLOAD_TOO_LARGE.to_owned())?;
    if next > MAX_ARCHIVE_BYTES {
        return Err(DOWNLOAD_TOO_LARGE.to_owned());
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_pin_matches_the_supported_desktop_hosts() {
        assert_eq!(
            supported(),
            cfg!(any(target_os = "macos", target_os = "linux"))
                && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        );
    }

    #[test]
    fn managed_download_refuses_bytes_past_its_ceiling() {
        assert_eq!(
            next_downloaded_size(MAX_ARCHIVE_BYTES - 1, 1),
            Ok(MAX_ARCHIVE_BYTES)
        );
        assert_eq!(
            next_downloaded_size(MAX_ARCHIVE_BYTES, 1).unwrap_err(),
            DOWNLOAD_TOO_LARGE
        );
        assert_eq!(
            next_downloaded_size(u64::MAX, 1).unwrap_err(),
            DOWNLOAD_TOO_LARGE
        );
    }

    /// At-rest integrity: the managed runtime only resolves behind a marker
    /// naming the pinned version and digest — no marker, or a marker for a
    /// different artifact, and the tree is invisible to resolution, so nothing
    /// is ever exposed to the sandbox on the strength of a directory name.
    #[test]
    fn managed_runtime_resolves_only_with_a_matching_marker() {
        let expected = "466e05f3477c20dfb723054dfebffe55bc74660ee77f612166fca121dacb65b6";
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = managed_version_dir(data_dir.path());
        let binary = managed_binary(&version_dir);
        std::fs::create_dir_all(binary.parent().expect("parent")).expect("dirs");
        std::fs::write(&binary, b"#!/bin/sh\n").expect("binary");
        std::fs::write(managed_npm(&version_dir), b"#!/bin/sh\n").expect("npm");

        // Tree present, no marker: an interrupted install, not trusted.
        assert_eq!(managed_node_root_expecting(data_dir.path(), expected), None);

        let write_marker = |digest: &str| {
            std::fs::write(
                marker_path(&version_dir),
                serde_json::to_vec(&InstallMarker {
                    version: NODE_VERSION.to_owned(),
                    tarball_sha256: digest.to_owned(),
                })
                .expect("marker json"),
            )
            .expect("write marker");
        };

        write_marker(&"0".repeat(64));
        assert_eq!(managed_node_root_expecting(data_dir.path(), expected), None);

        write_marker(expected);
        assert_eq!(
            managed_node_root_expecting(data_dir.path(), expected),
            Some(version_dir)
        );
    }

    /// npm ships as a symlink into `lib/node_modules/npm`, so an unpack that
    /// materialized links as copies — or dropped them — would leave a runtime
    /// that cannot run a single npm command. This drives the real unpack path.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn unpack_preserves_the_bundled_npm_symlink_and_node_mode() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("node-v0.0.0-test");
        std::fs::create_dir_all(source.join("bin")).expect("bin");
        std::fs::create_dir_all(source.join("lib/node_modules/npm/bin")).expect("npm");
        std::fs::write(source.join("lib/node_modules/npm/bin/npm-cli.js"), b"//\n")
            .expect("npm-cli");
        let node = source.join("bin/node");
        std::fs::write(&node, b"#!/bin/sh\n").expect("node");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).expect("node mode");
        std::os::unix::fs::symlink(
            "../lib/node_modules/npm/bin/npm-cli.js",
            source.join("bin/npm"),
        )
        .expect("symlink");

        let tarball = dir.path().join("node.tar.gz");
        let file = std::fs::File::create(&tarball).expect("tarball");
        let gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut archive = tar::Builder::new(gzip);
        archive.follow_symlinks(false);
        archive
            .append_dir_all("node-v0.0.0-test", &source)
            .expect("append source");
        let mut gzip = archive.into_inner().expect("finish tar");
        gzip.flush().expect("flush gzip");
        gzip.finish().expect("finish gzip");

        let destination = dir.path().join("unpacked");
        std::fs::create_dir(&destination).expect("destination");
        unpack(&tarball, &destination).await.expect("unpack");

        let npm = destination.join("node-v0.0.0-test/bin/npm");
        assert!(std::fs::symlink_metadata(&npm)
            .expect("npm metadata")
            .file_type()
            .is_symlink());
        assert!(npm.exists(), "the npm symlink resolves to its target");
        assert_ne!(
            std::fs::metadata(destination.join("node-v0.0.0-test/bin/node"))
                .expect("node metadata")
                .permissions()
                .mode()
                & 0o111,
            0,
            "the runtime remains executable"
        );
    }
}

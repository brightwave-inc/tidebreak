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
//! official archive is the npm the sandbox uses — there is no separate npm
//! install to keep in step.
//!
//! Supply-chain posture, deliberately rigid, and the same as the managed
//! LibreOffice install: the URL names an exact version — never "latest" — and
//! the artifact's SHA-256 is pinned in this file next to it, taken from the
//! Node project's published checksum file:
//!
//! - <https://nodejs.org/dist/v20.20.2/SHASUMS256.txt>
//!
//! macOS, Linux, and Windows are supported on both shipped architectures. The
//! local sandbox remains macOS-only, but code mode uses the same managed
//! runtime to install and launch its pinned harness packages on Linux and
//! Windows.
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
//! recording the version and the verified artifact digest. Resolution trusts
//! the managed install only when the marker matches the constants pinned here
//! — a marker for a different digest (a tampered or half-written install, or
//! a stale layout after the pin moves) makes the managed copy invisible and a
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
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::time::Duration;

use tauri::AppHandle;
use tidebreak_managed_node::{
    current_managed_node_pin, managed_node_executable, managed_node_install_marker,
    managed_node_marker_path, managed_node_root as verified_managed_node_root,
    managed_node_version_dir, managed_npm_executable, MANAGED_NODE_VERSION,
};

/// The exact Node version this build installs and trusts, matching the
/// container image's `node:20-bookworm-slim`.
pub(crate) const NODE_VERSION: &str = MANAGED_NODE_VERSION;

/// The unpacked runtime is about 154 MB and the archive about 40 MB; refuse to
/// start on a disk with less headroom than that plus slack.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
const REQUIRED_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// Official Node archives for the supported targets are below 55 MB. Bound
/// the streamed response independently of Content-Length so a bad endpoint
/// cannot fill the app-data volume before the digest check gets a vote.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

/// Bound decompressed output independently of the ZIP directory's declared
/// sizes so a malformed archive cannot consume the app-data volume.
#[cfg(any(target_os = "windows", test))]
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(any(target_os = "windows", test))]
const MAX_ARCHIVE_ENTRIES: usize = 100_000;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
const DOWNLOAD_TOO_LARGE: &str = "The Node download was larger than expected and was discarded";

#[derive(Clone, Copy)]
enum ArchiveFormat {
    #[cfg(any(target_os = "macos", target_os = "linux", test))]
    TarGz,
    #[cfg(any(target_os = "windows", test))]
    Zip,
}

struct PinnedArtifact {
    // Read only by the supported install path; unsupported platforms carry
    // the pinned shape (`PINNED = None`) without an installer to consume it.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
    url: &'static str,
    /// The single directory the official archive unpacks into, which the
    /// install moves into place as the version directory.
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
    archive_root: &'static str,
    #[cfg_attr(
        not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
        allow(dead_code)
    )]
    archive_format: ArchiveFormat,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-darwin-arm64.tar.gz",
    archive_root: "node-v20.20.2-darwin-arm64",
    archive_format: ArchiveFormat::TarGz,
});

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-darwin-x64.tar.gz",
    archive_root: "node-v20.20.2-darwin-x64",
    archive_format: ArchiveFormat::TarGz,
});

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-arm64.tar.gz",
    archive_root: "node-v20.20.2-linux-arm64",
    archive_format: ArchiveFormat::TarGz,
});

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-linux-x64.tar.gz",
    archive_root: "node-v20.20.2-linux-x64",
    archive_format: ArchiveFormat::TarGz,
});

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-win-arm64.zip",
    archive_root: "node-v20.20.2-win-arm64",
    archive_format: ArchiveFormat::Zip,
});

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://nodejs.org/dist/v20.20.2/node-v20.20.2-win-x64.zip",
    archive_root: "node-v20.20.2-win-x64",
    archive_format: ArchiveFormat::Zip,
});

#[cfg(not(any(
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
    all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ),
    all(
        target_os = "windows",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
)))]
static PINNED: Option<PinnedArtifact> = None;

/// Whether this platform can install its own Node runtime.
fn supported() -> bool {
    PINNED.is_some() && current_managed_node_pin().is_some()
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
    managed_node_version_dir(data_dir)
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("node").join("staging")
}

fn marker_path(version_dir: &Path) -> PathBuf {
    managed_node_marker_path(version_dir)
}

fn managed_binary(version_dir: &Path) -> PathBuf {
    managed_node_executable(version_dir)
}

/// The managed Node runtime's root directory, if a verified install of the
/// pinned artifact is present. This is the cheap at-rest check: the marker
/// matches the pin and both platform entrypoints exist.
pub(crate) fn managed_node_root(data_dir: &Path) -> Option<PathBuf> {
    verified_managed_node_root(data_dir)
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

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
async fn run_install(_data_dir: &Path) -> Result<(), String> {
    Err("Automatic install is not supported on this platform".to_owned())
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
async fn install_into(
    data_dir: &Path,
    staging: &Path,
    pinned: &PinnedArtifact,
) -> Result<(), String> {
    let managed_pin = current_managed_node_pin()
        .ok_or_else(|| "Automatic install is not supported on this platform".to_owned())?;
    crate::office_install::ensure_free_disk(
        staging,
        REQUIRED_FREE_BYTES,
        "Not enough free disk space to install Node (about 500 MB is needed)",
    )
    .await?;

    let archive_path = match pinned.archive_format {
        #[cfg(any(target_os = "macos", target_os = "linux", test))]
        ArchiveFormat::TarGz => staging.join("node.tar.gz"),
        #[cfg(any(target_os = "windows", test))]
        ArchiveFormat::Zip => staging.join("node.zip"),
    };
    download(pinned.url, &archive_path).await?;

    // Verify against the pinned digest before touching the archive in any way.
    let digest = {
        let archive_path = archive_path.clone();
        tokio::task::spawn_blocking(move || {
            crate::office_install::sha256_hex_of_file(&archive_path)
        })
        .await
        .map_err(|error| format!("Could not verify the download: {error}"))?
        .map_err(|error| format!("Could not verify the download: {error}"))?
    };
    if digest != managed_pin.artifact_sha256 {
        let _ = tokio::fs::remove_file(&archive_path).await;
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
    unpack(&archive_path, &unpacked, pinned.archive_format).await?;
    let extracted = unpacked.join(pinned.archive_root);
    if !managed_binary(&extracted).is_file() || !managed_npm_executable(&extracted).is_file() {
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
    let marker = managed_node_install_marker(managed_pin)
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

/// Unpack the official archive into `destination`.
///
/// Unix tarballs preserve the archive's `bin/npm` and `bin/npx` symlinks.
/// Windows ZIPs are extracted through a bounded path validator that accepts
/// only regular files and directories below `destination`.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
async fn unpack(
    archive_path: &Path,
    destination: &Path,
    format: ArchiveFormat,
) -> Result<(), String> {
    let archive_path = archive_path.to_path_buf();
    let destination = destination.to_path_buf();
    tokio::time::timeout(
        Duration::from_secs(300),
        tokio::task::spawn_blocking(move || match format {
            #[cfg(any(target_os = "macos", target_os = "linux", test))]
            ArchiveFormat::TarGz => unpack_tar_gz(&archive_path, &destination),
            #[cfg(any(target_os = "windows", test))]
            ArchiveFormat::Zip => unpack_zip(&archive_path, &destination),
        }),
    )
    .await
    .map_err(|_| "Unpacking Node timed out".to_owned())?
    .map_err(|error| format!("Could not join the Node unpack task: {error}"))?
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn unpack_tar_gz(tarball: &Path, destination: &Path) -> Result<(), String> {
    let file = std::fs::File::open(tarball)
        .map_err(|error| format!("Could not open the Node archive: {error}"))?;
    let gzip = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gzip);
    archive
        .unpack(destination)
        .map_err(|error| format!("Could not unpack Node: {error}"))
}

#[cfg(any(target_os = "windows", test))]
fn unpack_zip(archive_path: &Path, destination: &Path) -> Result<(), String> {
    use std::io::{Read as _, Write as _};

    let destination_metadata = std::fs::symlink_metadata(destination)
        .map_err(|error| format!("Could not inspect the Node install directory: {error}"))?;
    if !destination_metadata.is_dir() || destination_metadata.file_type().is_symlink() {
        return Err("The Node install destination was not a real directory".to_owned());
    }

    let file = std::fs::File::open(archive_path)
        .map_err(|error| format!("Could not open the Node archive: {error}"))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("Could not read the Node ZIP archive: {error}"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("The Node ZIP archive contained too many entries".to_owned());
    }

    let mut unpacked_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("Could not read the Node ZIP archive: {error}"))?;
        if entry.encrypted() {
            return Err("The Node ZIP archive contained an encrypted entry".to_owned());
        }
        if entry.is_symlink() || zip_entry_is_link_like(entry.unix_mode()) {
            return Err("The Node ZIP archive contained a link-like entry".to_owned());
        }

        let relative = safe_zip_entry_path(entry.name())?;
        let output_path = destination.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&output_path)
                .map_err(|error| format!("Could not unpack Node: {error}"))?;
            continue;
        }
        if !entry.is_file() {
            return Err("The Node ZIP archive contained an unsupported entry type".to_owned());
        }

        let remaining = MAX_UNPACKED_BYTES
            .checked_sub(unpacked_bytes)
            .ok_or_else(|| "The Node ZIP archive expanded beyond its size limit".to_owned())?;
        if entry.size() > remaining {
            return Err("The Node ZIP archive expanded beyond its size limit".to_owned());
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not unpack Node: {error}"))?;
        }
        let mut output = std::fs::File::create(&output_path)
            .map_err(|error| format!("Could not unpack Node: {error}"))?;
        let copied = std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut output)
            .map_err(|error| format!("Could not unpack Node: {error}"))?;
        if copied > remaining {
            return Err("The Node ZIP archive expanded beyond its size limit".to_owned());
        }
        output
            .flush()
            .map_err(|error| format!("Could not unpack Node: {error}"))?;
        unpacked_bytes = unpacked_bytes
            .checked_add(copied)
            .ok_or_else(|| "The Node ZIP archive expanded beyond its size limit".to_owned())?;
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn safe_zip_entry_path(name: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    let path_name = name.strip_suffix('/').unwrap_or(name);
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || path_name.is_empty()
        || path_name.split('/').any(windows_zip_segment_is_unsafe)
    {
        return Err("The Node ZIP archive contained an unsafe path".to_owned());
    }

    let path = Path::new(name);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("The Node ZIP archive contained an unsafe path".to_owned());
    }
    Ok(path.to_path_buf())
}

#[cfg(any(target_os = "windows", test))]
fn windows_zip_segment_is_unsafe(segment: &str) -> bool {
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.ends_with(['.', ' '])
        || segment
            .chars()
            .any(|character| character.is_control() || r#"<>:"|?*"#.contains(character))
    {
        return true;
    }

    let basename = segment.split('.').next().unwrap_or(segment);
    matches!(
        basename.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CLOCK$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(any(target_os = "windows", test))]
fn zip_entry_is_link_like(mode: Option<u32>) -> bool {
    let Some(mode) = mode else {
        return false;
    };
    let file_type = mode & 0o170_000;
    file_type != 0 && file_type != 0o040_000 && file_type != 0o100_000
}

/// Stream the artifact to disk. Verification happens after, against the whole
/// file.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
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
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
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
        unpack(&tarball, &destination, ArchiveFormat::TarGz)
            .await
            .expect("unpack");

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

    #[tokio::test]
    async fn unpack_zip_extracts_the_windows_runtime_layout() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let dir = tempfile::tempdir().expect("tempdir");
        let archive_path = dir.path().join("node.zip");
        let file = std::fs::File::create(&archive_path).expect("zip");
        let mut archive = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        archive
            .add_directory("node-v0.0.0-win-x64/", options)
            .expect("root");
        archive
            .start_file("node-v0.0.0-win-x64/node.exe", options)
            .expect("node entry");
        archive.write_all(b"node").expect("node");
        archive
            .start_file("node-v0.0.0-win-x64/npm.cmd", options)
            .expect("npm entry");
        archive.write_all(b"@echo off\r\n").expect("npm");
        archive
            .start_file(
                "node-v0.0.0-win-x64/node_modules/npm/bin/npm-cli.js",
                options,
            )
            .expect("nested entry");
        archive.write_all(b"// npm\n").expect("nested");
        archive.finish().expect("finish zip");

        let destination = dir.path().join("unpacked");
        std::fs::create_dir(&destination).expect("destination");
        unpack(&archive_path, &destination, ArchiveFormat::Zip)
            .await
            .expect("unpack");

        let root = destination.join("node-v0.0.0-win-x64");
        assert_eq!(std::fs::read(root.join("node.exe")).expect("node"), b"node");
        assert_eq!(
            std::fs::read(root.join("npm.cmd")).expect("npm"),
            b"@echo off\r\n"
        );
        assert_eq!(
            std::fs::read(root.join("node_modules/npm/bin/npm-cli.js")).expect("nested"),
            b"// npm\n"
        );
    }

    #[tokio::test]
    async fn unpack_zip_rejects_traversal_without_writing_outside_destination() {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;

        let dir = tempfile::tempdir().expect("tempdir");
        let archive_path = dir.path().join("node.zip");
        let file = std::fs::File::create(&archive_path).expect("zip");
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file("../escaped.txt", SimpleFileOptions::default())
            .expect("traversal entry");
        archive.write_all(b"escaped").expect("entry");
        archive.finish().expect("finish zip");

        let destination = dir.path().join("unpacked");
        std::fs::create_dir(&destination).expect("destination");
        let error = unpack(&archive_path, &destination, ArchiveFormat::Zip)
            .await
            .expect_err("traversal must fail");
        assert!(error.contains("unsafe path"), "unexpected error: {error}");
        assert!(!dir.path().join("escaped.txt").exists());
    }

    #[test]
    fn zip_path_and_entry_validation_rejects_windows_escape_forms_and_links() {
        for name in [
            "../escape",
            "/absolute",
            "C:/absolute",
            r"C:\absolute",
            r"root\..\escape",
            "root//file",
            "root/./file",
            "root/CON",
            "root/NUL.txt",
            "root/trailing. ",
            "root/stream:name",
        ] {
            assert!(
                safe_zip_entry_path(name).is_err(),
                "unsafe path was accepted: {name}"
            );
        }
        assert!(safe_zip_entry_path("node-v0.0.0-win-x64/node.exe").is_ok());
        assert!(safe_zip_entry_path("node-v0.0.0-win-x64/").is_ok());

        assert!(zip_entry_is_link_like(Some(0o120_777)));
        assert!(zip_entry_is_link_like(Some(0o060_644)));
        assert!(!zip_entry_is_link_like(Some(0o100_644)));
        assert!(!zip_entry_is_link_like(Some(0o040_755)));
        assert!(!zip_entry_is_link_like(None));
    }
}

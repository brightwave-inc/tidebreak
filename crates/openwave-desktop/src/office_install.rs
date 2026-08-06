//! Managed LibreOffice install for the presentation preview.
//!
//! The preview converts decks with LibreOffice, and most machines do not have
//! one. Rather than sending the user off to install it, the app fetches its
//! own copy the first time a preview needs it: an exact pinned version from
//! The Document Foundation's official download service, digest-verified
//! before a single byte of it is unpacked, installed under the app's data
//! directory (`tools/libreoffice/<version>/LibreOffice.app`) with no admin
//! rights and no writes outside app-owned paths.
//!
//! Supply-chain posture, deliberately rigid: the URL names an exact version —
//! never "latest" — and the artifact's SHA-256 is pinned in this file next to
//! it. `download.documentfoundation.org` redirects to third-party mirrors
//! (MirrorBrain); those mirrors are untrusted and do not need to be trusted,
//! because the download is rejected unless it hashes to the pinned digest.
//! The digests below were taken from TDF's published checksum files:
//!
//! - <https://download.documentfoundation.org/libreoffice/stable/25.8.7/mac/aarch64/LibreOffice_25.8.7_MacOS_aarch64.dmg.sha256>
//! - <https://download.documentfoundation.org/libreoffice/stable/25.8.7/mac/x86_64/LibreOffice_25.8.7_MacOS_x86-64.dmg.sha256>
//!
//! macOS is the supported platform (TDF ships `.dmg` only, so install mounts
//! the image read-only with `hdiutil`, copies the bundle out with `ditto`,
//! and detaches). Other platforms keep the install-it-yourself hint.
//!
//! Gatekeeper: a download performed by this process carries no
//! `com.apple.quarantine` attribute — quarantine is applied by applications
//! that opt in via `LSFileQuarantineEnabled` (browsers), not by the kernel —
//! and copying out of an unquarantined disk image propagates none either, so
//! the CLI-invoked `soffice` runs without any Gatekeeper prompt. Verified
//! end-to-end on a real download of this pinned artifact. The quarantine
//! attribute is still stripped after the copy, deliberately: it costs one
//! `xattr` invocation and keeps the install working if a future code path
//! ever hands us a quarantined file.
//!
//! Integrity at rest: the install directory carries an `installed.json`
//! marker recording the version and the verified dmg digest. Resolution
//! trusts the managed install only when the marker matches the constants
//! pinned here — a marker for a different digest (a tampered or half-written
//! install, or a stale layout after the pin moves) makes the managed copy
//! invisible and a fresh install replaces it. The 800 MB bundle is not
//! re-hashed per preview; the marker plus the binary's presence is the check.
//!
//! Failure discipline: one failed or cancelled install is remembered for the
//! rest of the app run and reported alongside the install hint. Nothing
//! re-downloads on its own; the user's explicit retry clears the memory.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
#[cfg(target_os = "macos")]
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Emitter as _;

/// The exact LibreOffice version this build installs and trusts.
pub(crate) const LIBREOFFICE_VERSION: &str = "25.8.7";

/// Shown in UI copy and used for the disk-space check; the aarch64 dmg is
/// 299,584,229 bytes and the copied bundle about 816 MB.
#[cfg(target_os = "macos")]
const REQUIRED_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Progress events the preview panel renders as a determinate bar.
#[cfg(target_os = "macos")]
pub(crate) const INSTALL_PROGRESS_EVENT: &str = "presentation-converter-install-progress";

struct PinnedArtifact {
    // Read only by the macOS install path; other platforms carry the pinned
    // shape (`PINNED = None`) without an installer to consume the URL.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    url: &'static str,
    sha256: &'static str,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://download.documentfoundation.org/libreoffice/stable/25.8.7/mac/aarch64/LibreOffice_25.8.7_MacOS_aarch64.dmg",
    sha256: "e7556aa61e282f89578ebaf35afdb09c94dcf9d6ee7c137004377bee81a6e900",
});

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
static PINNED: Option<PinnedArtifact> = Some(PinnedArtifact {
    url: "https://download.documentfoundation.org/libreoffice/stable/25.8.7/mac/x86_64/LibreOffice_25.8.7_MacOS_x86-64.dmg",
    sha256: "110439f207b5e420d7c8e441180c8374431761d549a1c14b584045ac56cd71c5",
});

#[cfg(not(target_os = "macos"))]
static PINNED: Option<PinnedArtifact> = None;

/// Whether this platform can install its own converter.
pub(crate) fn supported() -> bool {
    PINNED.is_some()
}

/// The install failure (or cancellation) most recently hit this app run, if
/// any. While set, the preview shows the hint instead of re-downloading;
/// an explicit retry clears it.
pub(crate) fn last_failure() -> Option<String> {
    state()
        .lock()
        .expect("install state lock")
        .last_failure
        .clone()
}

fn managed_version_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("tools")
        .join("libreoffice")
        .join(LIBREOFFICE_VERSION)
}

#[cfg(target_os = "macos")]
fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("libreoffice").join("staging")
}

fn marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join("installed.json")
}

fn managed_binary(version_dir: &Path) -> PathBuf {
    version_dir.join("LibreOffice.app/Contents/MacOS/soffice")
}

/// What `installed.json` records: which artifact the directory was verified
/// against. Written only after the digest check and the copy both succeeded.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    version: String,
    dmg_sha256: String,
}

/// The managed `soffice`, if a verified install of the pinned artifact is
/// present. This is the cheap at-rest check: marker matches the pin, binary
/// exists.
pub(crate) fn managed_soffice(data_dir: &Path) -> Option<PathBuf> {
    let pinned = PINNED.as_ref()?;
    managed_soffice_expecting(data_dir, pinned.sha256)
}

/// Seam for [`managed_soffice`]: the same resolution against an explicit
/// expected digest, so the marker gate is testable off-macOS.
fn managed_soffice_expecting(data_dir: &Path, expected_sha256: &str) -> Option<PathBuf> {
    let version_dir = managed_version_dir(data_dir);
    let marker = std::fs::read(marker_path(&version_dir)).ok()?;
    let marker: InstallMarker = serde_json::from_slice(&marker).ok()?;
    if marker.version != LIBREOFFICE_VERSION || marker.dmg_sha256 != expected_sha256 {
        return None;
    }
    let binary = managed_binary(&version_dir);
    binary.is_file().then_some(binary)
}

struct InstallState {
    last_failure: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
}

fn state() -> &'static Mutex<InstallState> {
    static STATE: LazyLock<Mutex<InstallState>> = LazyLock::new(|| {
        Mutex::new(InstallState {
            last_failure: None,
            cancel: None,
        })
    });
    &STATE
}

/// Serializes installs; two presentation panels hitting a cold machine at
/// once produce one download.
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    &LOCK
}

/// Install the managed LibreOffice. Emits [`INSTALL_PROGRESS_EVENT`] while
/// running; on failure the reason is both returned and remembered so the
/// preview stops auto-retrying until the user asks again.
#[tauri::command]
pub(crate) async fn install_presentation_converter(app: AppHandle) -> Result<(), String> {
    ensure_installed(&app).await
}

/// The one install path both entry points share: serialize on the install
/// lock, short-circuit if a concurrent caller already finished, run the
/// install with a registered cancel flag, and remember the outcome.
async fn ensure_installed(app: &AppHandle) -> Result<(), String> {
    let data_dir = crate::data_dir(app)?;
    let _serialized = install_lock().lock().await;
    if managed_soffice(&data_dir).is_some() {
        // Another caller finished the install while this one waited.
        state().lock().expect("install state lock").last_failure = None;
        return Ok(());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut state = state().lock().expect("install state lock");
        state.last_failure = None;
        state.cancel = Some(cancel.clone());
    }
    let outcome = run_install(app, &data_dir, &cancel).await;
    {
        let mut state = state().lock().expect("install state lock");
        state.cancel = None;
        state.last_failure = outcome.as_ref().err().cloned();
    }
    outcome
}

/// Start the managed install in the background, ahead of the first preview.
///
/// The renderer fires this when a turn produces its first presentation
/// output: by the time the user clicks the deck, the ~300 MB download is
/// already under way or done, instead of starting at the moment a preview
/// panel needs it. Deliberately quiet and non-binding — it never blocks
/// anything, runs at most once per app run, and defers to every existing
/// rule: a verified managed install, a *working* converter the user already
/// has, or a remembered failure each make it a no-op. A broken launcher
/// script on `PATH` (a package manager's leftover) does not count as a
/// working converter here, exactly as it does not at conversion time.
///
/// Progress still goes out on [`INSTALL_PROGRESS_EVENT`], so a preview panel
/// opened mid-warm-up joins the install (over the same lock) and shows the
/// real download bar rather than starting a second download.
#[tauri::command]
pub(crate) fn warm_presentation_converter(app: AppHandle) {
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
        if managed_soffice(&data_dir).is_some() || last_failure().is_some() {
            return;
        }
        if crate::office_pdf::workable_system_soffice().await {
            return;
        }
        // Failures are remembered like any install failure; the preview
        // surfaces the reason with a Try again instead of re-downloading.
        let _ = ensure_installed(&app).await;
    });
}

/// Flag the in-flight install to stop. The install command itself returns
/// the cancellation as its error.
#[tauri::command]
pub(crate) fn cancel_presentation_converter_install() {
    if let Some(cancel) = &state().lock().expect("install state lock").cancel {
        cancel.store(true, Ordering::Relaxed);
    }
}

/// The desktop's host-tool broker: skill-declared host dependencies resolve
/// through the same managed installer, warm-up guard, and failure memory the
/// preview panel uses, so there is exactly one install path however the need
/// arrives — turn staging, the preview, or an explicit retry.
pub(crate) struct DesktopHostToolBroker {
    app: AppHandle,
}

impl DesktopHostToolBroker {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl openwave_code_execution::HostToolBroker for DesktopHostToolBroker {
    fn ensure(&self, tool: openwave_code_execution::HostDep) {
        match tool {
            // The warm-up already embodies the discipline `ensure` promises:
            // returns immediately, at most one download per app run, no-op
            // when a working converter exists or a failure is remembered.
            openwave_code_execution::HostDep::LibreOffice => {
                warm_presentation_converter(self.app.clone());
            }
            openwave_code_execution::HostDep::Node => {
                crate::node_install::ensure(self.app.clone());
            }
        }
    }

    async fn status(
        &self,
        tool: openwave_code_execution::HostDep,
    ) -> openwave_code_execution::HostToolStatus {
        match tool {
            openwave_code_execution::HostDep::LibreOffice => libreoffice_status(&self.app).await,
            openwave_code_execution::HostDep::Node => crate::node_install::status(&self.app).await,
        }
    }

    async fn managed_root(
        &self,
        tool: openwave_code_execution::HostDep,
    ) -> Option<std::path::PathBuf> {
        match tool {
            // Conversion runs on the host, so LibreOffice has no root anything
            // downstream needs.
            openwave_code_execution::HostDep::LibreOffice => None,
            // Only a verified managed install resolves, so a backend handed
            // this path is never exposing an unpinned or half-written tree.
            openwave_code_execution::HostDep::Node => {
                let data_dir = crate::data_dir(&self.app).ok()?;
                crate::node_install::managed_node_root(&data_dir)
            }
        }
    }
}

/// The current truth about LibreOffice on this machine, in the same
/// resolution order conversion uses: the verified managed install, then a
/// working system one. "Working" is probed, not assumed, exactly as the
/// warm-up does — a leftover launcher script must not report Available.
async fn libreoffice_status(app: &AppHandle) -> openwave_code_execution::HostToolStatus {
    use openwave_code_execution::HostToolStatus;
    let Ok(data_dir) = crate::data_dir(app) else {
        return HostToolStatus::Unavailable("the app data directory is unavailable".into());
    };
    if managed_soffice(&data_dir).is_some() {
        return HostToolStatus::Available;
    }
    if crate::office_pdf::workable_system_soffice().await {
        return HostToolStatus::Available;
    }
    if state().lock().expect("install state lock").cancel.is_some() {
        return HostToolStatus::Installing;
    }
    if let Some(reason) = last_failure() {
        return HostToolStatus::Unavailable(reason);
    }
    if supported() {
        HostToolStatus::Unavailable("LibreOffice is not installed yet".into())
    } else {
        HostToolStatus::Unavailable(
            "automatic LibreOffice install is not supported on this platform".into(),
        )
    }
}

#[cfg(target_os = "macos")]
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    /// `downloading` is determinate; `installing` covers verify + mount +
    /// copy and is not.
    phase: &'static str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

#[cfg(target_os = "macos")]
fn emit_progress(app: &AppHandle, progress: InstallProgress) {
    let _ = app.emit(INSTALL_PROGRESS_EVENT, progress);
}

#[cfg(target_os = "macos")]
const CANCELLED: &str = "Download cancelled";

#[cfg(target_os = "macos")]
async fn run_install(app: &AppHandle, data_dir: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let pinned = PINNED
        .as_ref()
        .ok_or_else(|| "Automatic install is not supported on this platform".to_owned())?;

    let staging = staging_dir(data_dir);
    // A previous run's partial download or dead mountpoint; start clean.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|error| format!("Could not prepare the install directory: {error}"))?;

    let result = install_into(app, data_dir, &staging, pinned, cancel).await;

    // The staging directory never survives: success moved the bundle out,
    // failure leaves only partial artifacts worth reclaiming.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

#[cfg(not(target_os = "macos"))]
async fn run_install(
    _app: &AppHandle,
    _data_dir: &Path,
    _cancel: &AtomicBool,
) -> Result<(), String> {
    Err("Automatic install is not supported on this platform".to_owned())
}

#[cfg(target_os = "macos")]
async fn install_into(
    app: &AppHandle,
    data_dir: &Path,
    staging: &Path,
    pinned: &PinnedArtifact,
    cancel: &AtomicBool,
) -> Result<(), String> {
    ensure_free_disk(
        staging,
        REQUIRED_FREE_BYTES,
        "Not enough free disk space to install LibreOffice (about 2 GB is needed)",
    )
    .await?;

    let dmg = staging.join("LibreOffice.dmg");
    download(app, pinned.url, &dmg, cancel).await?;

    emit_progress(
        app,
        InstallProgress {
            phase: "installing",
            downloaded_bytes: 0,
            total_bytes: None,
        },
    );

    // Verify against the pinned digest before touching the image in any way.
    // Hashing streams from disk; the incremental work was already paid during
    // the download's page-cache-warm write, so this is quick even at 300 MB.
    let digest = {
        let dmg = dmg.clone();
        tokio::task::spawn_blocking(move || sha256_hex_of_file(&dmg))
            .await
            .map_err(|error| format!("Could not verify the download: {error}"))?
            .map_err(|error| format!("Could not verify the download: {error}"))?
    };
    if digest != pinned.sha256 {
        let _ = tokio::fs::remove_file(&dmg).await;
        return Err(
            "The downloaded LibreOffice did not match its published checksum, so it was discarded. \
             This can mean a corrupted mirror or a tampered download — try again later."
                .to_owned(),
        );
    }

    // TDF ships macOS builds as .dmg only, so: mount read-only, copy the
    // bundle out with ditto (preserves the code-signed structure), detach.
    let mount = staging.join("mnt");
    tokio::fs::create_dir(&mount)
        .await
        .map_err(|error| format!("Could not prepare the install directory: {error}"))?;
    run_tool(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
        Duration::from_secs(120),
    )
    .await?;

    let copied = staging.join("LibreOffice.app");
    let copy_result = run_tool(
        "/usr/bin/ditto",
        &[
            mount.join("LibreOffice.app").as_os_str(),
            copied.as_os_str(),
        ],
        Duration::from_secs(600),
    )
    .await;
    // Detach regardless: a mounted image outliving the install would pin the
    // staging directory and confuse the next attempt.
    let _ = run_tool(
        "/usr/bin/hdiutil",
        &["detach".as_ref(), mount.as_os_str()],
        Duration::from_secs(120),
    )
    .await;
    copy_result?;

    // Deliberate quarantine strip; see the module docs for why this is a
    // no-op today and kept anyway.
    let _ = run_tool(
        "/usr/bin/xattr",
        &[
            "-dr".as_ref(),
            "com.apple.quarantine".as_ref(),
            copied.as_os_str(),
        ],
        Duration::from_secs(120),
    )
    .await;

    let version_dir = managed_version_dir(data_dir);
    tokio::fs::create_dir_all(&version_dir)
        .await
        .map_err(|error| format!("Could not prepare the install directory: {error}"))?;
    let installed_app = version_dir.join("LibreOffice.app");
    // A leftover unverified bundle (no marker names it) makes the rename
    // fail; replace it.
    let _ = tokio::fs::remove_dir_all(&installed_app).await;
    tokio::fs::rename(&copied, &installed_app)
        .await
        .map_err(|error| format!("Could not move LibreOffice into place: {error}"))?;

    // The marker lands last, so a crash anywhere above leaves an install that
    // resolution ignores and the next attempt replaces.
    let marker = serde_json::to_vec_pretty(&InstallMarker {
        version: LIBREOFFICE_VERSION.to_owned(),
        dmg_sha256: pinned.sha256.to_owned(),
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

    if managed_soffice(data_dir).is_none() {
        return Err("The installed LibreOffice did not verify".to_owned());
    }
    Ok(())
}

/// Stream the artifact to disk with determinate progress and cooperative
/// cancellation. Verification happens after, against the whole file.
#[cfg(target_os = "macos")]
async fn download(
    app: &AppHandle,
    url: &str,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    use futures::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    let client = reqwest::Client::builder()
        .build()
        .map_err(|error| format!("Could not start the download: {error}"))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("Could not download LibreOffice: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not download LibreOffice: the server answered {}",
            response.status()
        ));
    }
    let total_bytes = response.content_length();
    emit_progress(
        app,
        InstallProgress {
            phase: "downloading",
            downloaded_bytes: 0,
            total_bytes,
        },
    );

    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("Could not write the download: {error}"))?;
    let mut stream = response.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_reported: u64 = 0;
    const REPORT_EVERY_BYTES: u64 = 4 * 1024 * 1024;
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.to_owned());
        }
        let chunk = chunk.map_err(|error| format!("The download was interrupted: {error}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("Could not write the download: {error}"))?;
        downloaded += chunk.len() as u64;
        if downloaded - last_reported >= REPORT_EVERY_BYTES {
            last_reported = downloaded;
            emit_progress(
                app,
                InstallProgress {
                    phase: "downloading",
                    downloaded_bytes: downloaded,
                    total_bytes,
                },
            );
        }
    }
    file.flush()
        .await
        .map_err(|error| format!("Could not write the download: {error}"))?;
    Ok(())
}

/// SHA-256 of a file's contents, streamed, as lowercase hex.
// Compiled on every platform — its test is platform-neutral — but only the
// macOS install paths call it from the lib target.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn sha256_hex_of_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Run one system tool to completion under a hard timeout, failing on a
/// non-zero exit with its stderr in the reason.
#[cfg(target_os = "macos")]
pub(crate) async fn run_tool(
    program: &str,
    args: &[&std::ffi::OsStr],
    timeout: Duration,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("Could not run {program}: {error}"))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{program} timed out"))?
        .map_err(|error| format!("Could not run {program}: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed: {}", detail.trim()));
    }
    Ok(())
}

/// Refuse to start a large install on a nearly full disk. `df` because the
/// standard library has no free-space query and this path is macOS-only.
/// `shortfall` is the message the caller wants when the check fails, so each
/// managed install names itself and its own size.
#[cfg(target_os = "macos")]
pub(crate) async fn ensure_free_disk(
    directory: &Path,
    required: u64,
    shortfall: &str,
) -> Result<(), String> {
    let output = tokio::process::Command::new("/bin/df")
        .arg("-Pk")
        .arg(directory)
        .output()
        .await
        .map_err(|error| format!("Could not check free disk space: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let available_kib: u64 = stdout
        .lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| "Could not check free disk space".to_owned())?;
    if available_kib.saturating_mul(1024) < required {
        return Err(shortfall.to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verification gate: a wrong digest is rejected and a right one
    /// accepted, against real bytes on disk. This is the check standing
    /// between a compromised mirror and an unpacked application bundle.
    #[test]
    fn download_digest_verification_accepts_only_the_pinned_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("artifact.dmg");
        std::fs::write(&artifact, b"hello world").expect("write");

        let actual = sha256_hex_of_file(&artifact).expect("hash");
        assert_eq!(
            actual,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_ne!(actual, "0".repeat(64));
    }

    /// At-rest integrity: the managed install only resolves behind a marker
    /// naming the pinned version and digest — no marker, or a marker for a
    /// different artifact, and the bundle is invisible to resolution.
    #[test]
    fn managed_install_resolves_only_with_a_matching_marker() {
        let expected = "e7556aa61e282f89578ebaf35afdb09c94dcf9d6ee7c137004377bee81a6e900";
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = managed_version_dir(data_dir.path());
        let binary = managed_binary(&version_dir);
        std::fs::create_dir_all(binary.parent().expect("parent")).expect("dirs");
        std::fs::write(&binary, b"#!/bin/sh\n").expect("binary");

        // Bundle present, no marker: an interrupted install, not trusted.
        assert_eq!(managed_soffice_expecting(data_dir.path(), expected), None);

        let write_marker = |digest: &str| {
            std::fs::write(
                marker_path(&version_dir),
                serde_json::to_vec(&InstallMarker {
                    version: LIBREOFFICE_VERSION.to_owned(),
                    dmg_sha256: digest.to_owned(),
                })
                .expect("marker json"),
            )
            .expect("write marker");
        };

        write_marker(&"0".repeat(64));
        assert_eq!(managed_soffice_expecting(data_dir.path(), expected), None);

        write_marker(expected);
        assert_eq!(
            managed_soffice_expecting(data_dir.path(), expected),
            Some(binary)
        );
    }
}

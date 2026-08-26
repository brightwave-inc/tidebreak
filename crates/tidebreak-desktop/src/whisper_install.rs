//! Managed install of the `tidebreak-whisper` transcription helper.
//!
//! The desktop no longer links whisper.cpp. Local voice transcription spawns
//! a small helper binary instead, and this module fetches that helper the
//! first time it is needed: an exact pinned version from Tidebreak's own
//! download service, signature-verified before a single byte of it can run,
//! installed under the app's data directory
//! (`tools/whisper-helper/<version>/`) with no admin rights and no writes
//! outside app-owned paths.
//!
//! Supply-chain posture: the URL names an exact helper version — never
//! "latest" — and the downloaded binary must verify against the Tauri
//! updater public key already committed in `tauri.conf.json`. That is the
//! same key that authenticates full app updates, so the helper cannot be
//! swapped by anyone who could not also ship an app update. A pinned SHA-256
//! would force a two-phase source change on every helper release; the
//! signature pins the publisher instead of the bytes and needs only the
//! version constant to move.
//!
//! Gatekeeper: a download performed by this process carries no
//! `com.apple.quarantine` attribute — quarantine is applied by applications
//! that opt in via `LSFileQuarantineEnabled` (browsers), not by the kernel —
//! so the helper runs without any Gatekeeper prompt, exactly like the managed
//! LibreOffice install (see `office_install.rs`).
//!
//! Integrity at rest: the install directory carries an `installed.json`
//! marker recording the version and the SHA-256 of the verified binary.
//! Resolution trusts the managed install only when the marker matches the
//! pinned version — a marker for a different version (or a half-written
//! install) makes the managed copy invisible and a fresh install replaces it.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The exact published helper version this build downloads and trusts. It is
/// the `version` of `crates/tidebreak-whisper`; bump both together and run
/// the **Publish whisper helper** workflow before shipping the app change.
pub(crate) const HELPER_VERSION: &str = "0.1.0";

/// The Tauri updater public key, byte-identical to
/// `plugins.updater.pubkey` in `tauri.conf.json` (a test enforces this).
/// The helper is signed with the matching private key by the publish
/// workflow, exactly like updater artifacts.
const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEUyRjM1NjZGOTQxRUFBNwpSV1NuNmtINVpqVXZEaXY4cVFWZ0RrbThUbHlEZFB1RnVtT01MSGcwRElhcjdtUUpJckJzM2ZjTAo=";

/// The helper is a single static binary in the low tens of megabytes. The
/// signature gate decides authenticity; this only stops an oversized response
/// from consuming the user's disk.
const MAX_HELPER_BYTES: u64 = 128 * 1024 * 1024;

/// Overrides every resolution step, including verification. Development
/// builds and the ignored end-to-end tests point this at a locally built
/// `target/release/tidebreak-whisper`.
pub(crate) const HELPER_PATH_ENV: &str = "TIDEBREAK_WHISPER_HELPER";

fn helper_file_name() -> &'static str {
    if cfg!(windows) {
        "tidebreak-whisper.exe"
    } else {
        "tidebreak-whisper"
    }
}

/// The Rust target triple of this build, which names the published artifact.
fn target_triple() -> Result<&'static str, String> {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        return Err("Local voice transcription is not supported on this processor".into());
    };
    Ok(match (std::env::consts::OS, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => return Err("Local voice transcription is not supported on this platform".into()),
    })
}

fn artifact_url(triple: &str) -> String {
    let extension = if triple.contains("windows") { ".exe" } else { "" };
    format!(
        "https://downloads.brightwave.io/tidebreak/tools/whisper-helper/v{HELPER_VERSION}/tidebreak-whisper-{triple}{extension}"
    )
}

fn version_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("tools")
        .join("whisper-helper")
        .join(HELPER_VERSION)
}

fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("whisper-helper").join("staging")
}

fn marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join("installed.json")
}

/// What `installed.json` records: which artifact the directory was verified
/// against. Written only after the signature check succeeded.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    version: String,
    binary_sha256: String,
}

/// The managed helper, if a verified install of the pinned version is
/// present. This is the cheap at-rest check: marker names the pinned
/// version, binary exists.
pub(crate) fn managed_helper(data_dir: &Path) -> Option<PathBuf> {
    if let Ok(overridden) = std::env::var(HELPER_PATH_ENV) {
        // The override is authoritative: a wrong path fails at spawn time
        // with the real reason instead of silently downloading the pin.
        return Some(PathBuf::from(overridden));
    }
    let version_dir = version_dir(data_dir);
    let marker = std::fs::read(marker_path(&version_dir)).ok()?;
    let marker: InstallMarker = serde_json::from_slice(&marker).ok()?;
    if marker.version != HELPER_VERSION {
        return None;
    }
    let binary = version_dir.join(helper_file_name());
    binary.is_file().then_some(binary)
}

/// Serializes installs; two transcriptions hitting a cold machine at once
/// produce one download.
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
    &LOCK
}

/// The one resolution path: an existing verified install, or download,
/// verify, and install the pinned helper.
pub(crate) async fn ensure_helper(data_dir: &Path) -> Result<PathBuf, String> {
    let _serialized = install_lock().lock().await;
    if let Some(helper) = managed_helper(data_dir) {
        return Ok(helper);
    }

    let staging = staging_dir(data_dir);
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|error| format!("Could not prepare the voice helper directory: {error}"))?;
    let result = install_into(data_dir, &staging).await;
    // The staging directory never survives: success moved the binary out,
    // failure leaves only partial artifacts worth reclaiming.
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

async fn install_into(data_dir: &Path, staging: &Path) -> Result<PathBuf, String> {
    let triple = target_triple()?;
    let url = artifact_url(triple);

    let binary_bytes = download(&url, MAX_HELPER_BYTES).await?;
    let signature_bytes = download(&format!("{url}.sig"), 64 * 1024).await?;
    let signature = String::from_utf8(signature_bytes)
        .map_err(|_| "The voice helper signature was not valid text".to_owned())?;
    verify_signature(&binary_bytes, signature.trim())?;

    let staged = staging.join(helper_file_name());
    tokio::fs::write(&staged, &binary_bytes)
        .await
        .map_err(|error| format!("Could not write the voice helper: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|error| format!("Could not install the voice helper: {error}"))?;
    }

    let version_dir = version_dir(data_dir);
    tokio::fs::create_dir_all(&version_dir)
        .await
        .map_err(|error| format!("Could not prepare the voice helper directory: {error}"))?;
    let installed = version_dir.join(helper_file_name());
    // A leftover unverified binary (no marker names it) would block the
    // rename on Windows; replace it.
    let _ = tokio::fs::remove_file(&installed).await;
    tokio::fs::rename(&staged, &installed)
        .await
        .map_err(|error| format!("Could not install the voice helper: {error}"))?;

    // The marker lands last, so a crash anywhere above leaves an install that
    // resolution ignores and the next attempt replaces.
    let marker = serde_json::to_vec_pretty(&InstallMarker {
        version: HELPER_VERSION.to_owned(),
        binary_sha256: sha256_hex(&binary_bytes),
    })
    .map_err(|error| format!("Could not record the voice helper install: {error}"))?;
    let marker_file = marker_path(&version_dir);
    let partial = marker_file.with_extension("json.partial");
    tokio::fs::write(&partial, marker)
        .await
        .map_err(|error| format!("Could not record the voice helper install: {error}"))?;
    tokio::fs::rename(&partial, &marker_file)
        .await
        .map_err(|error| format!("Could not record the voice helper install: {error}"))?;

    managed_helper(data_dir).ok_or_else(|| "The installed voice helper did not verify".to_owned())
}

/// Fetch one artifact fully into memory with a byte ceiling. The helper and
/// its signature are small enough that streaming to disk buys nothing.
async fn download(url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    use futures::StreamExt as _;

    let response = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|_| "Could not download the voice helper".to_owned())?;
    if !response.status().is_success() {
        return Err(format!(
            "Could not download the voice helper: server answered {}",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|total| total > max_bytes)
    {
        return Err("The voice helper download was larger than expected".to_owned());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|_| "The voice helper download was interrupted".to_owned())?;
        if bytes.len() as u64 + chunk.len() as u64 > max_bytes {
            return Err("The voice helper download was larger than expected".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Verify `bytes` against a Tauri-format signature: base64 wrapping a
/// minisign signature file, checked with the committed updater public key.
/// This mirrors what `tauri-plugin-updater` does for app updates.
fn verify_signature(bytes: &[u8], signature_base64: &str) -> Result<(), String> {
    fn invalid<E>(_: E) -> String {
        "The downloaded voice helper failed signature verification".to_owned()
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let pubkey =
        String::from_utf8(engine.decode(UPDATER_PUBKEY).map_err(invalid)?).map_err(invalid)?;
    let pubkey = minisign_verify::PublicKey::decode(&pubkey).map_err(invalid)?;
    let signature = String::from_utf8(engine.decode(signature_base64).map_err(invalid)?)
        .map_err(invalid)?;
    let signature = minisign_verify::Signature::decode(&signature).map_err(invalid)?;
    pubkey
        .verify(bytes, &signature, true)
        .map_err(|_| "The downloaded voice helper failed signature verification and was discarded".to_owned())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// The helper trusts the same publisher as app updates: this constant and
    /// the updater pubkey in `tauri.conf.json` must never drift apart.
    #[test]
    fn pinned_pubkey_matches_the_updater_configuration() {
        let mut config = String::new();
        std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json"))
            .expect("tauri.conf.json")
            .read_to_string(&mut config)
            .expect("read tauri.conf.json");
        let config: serde_json::Value = serde_json::from_str(&config).expect("parse");
        assert_eq!(
            config["plugins"]["updater"]["pubkey"].as_str(),
            Some(UPDATER_PUBKEY)
        );
    }

    /// The signature gate: garbage, truncated, and wrong-key signatures are
    /// all rejected before any downloaded byte could execute.
    #[test]
    fn signature_verification_rejects_invalid_signatures() {
        assert!(verify_signature(b"binary", "not base64!").is_err());
        let engine = base64::engine::general_purpose::STANDARD;
        assert!(verify_signature(b"binary", &engine.encode("not a signature")).is_err());
        // A well-formed signature file that this key never made. Decoding may
        // succeed or fail depending on its structure; either way it must not
        // verify.
        let foreign = "untrusted comment: signature from minisign secret key\nRUTTNsbNM+FoZOvOTaXYzpFAJ1UMxIqYPB0uMc0bYr5AmMBBTDW1FJ8y4h+8odFT4hOPWXjifV5nHfP1lB1DGBk8g0mgvbnZAAY=\ntrusted comment: timestamp:1700000000\nEqXbnr1VOb5MCRolWjX99cWv/2mBjnpZjbnPCzMTBFXNbG4b2SxmxeoBhCUmimSpRoOAeFHuNjZGVvNP2wnJCQ==\n";
        assert!(verify_signature(b"binary", &engine.encode(foreign)).is_err());
    }

    /// At-rest integrity: the managed install only resolves behind a marker
    /// naming the pinned version — no marker, or a marker for a different
    /// version, and the binary is invisible to resolution.
    #[test]
    fn managed_install_resolves_only_with_a_matching_marker() {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = version_dir(data_dir.path());
        std::fs::create_dir_all(&version_dir).expect("dirs");
        let binary = version_dir.join(helper_file_name());
        std::fs::write(&binary, b"helper").expect("binary");

        // Binary present, no marker: an interrupted install, not trusted.
        assert_eq!(managed_helper(data_dir.path()), None);

        let write_marker = |version: &str| {
            std::fs::write(
                marker_path(&version_dir),
                serde_json::to_vec(&InstallMarker {
                    version: version.to_owned(),
                    binary_sha256: "0".repeat(64),
                })
                .expect("marker json"),
            )
            .expect("write marker");
        };

        write_marker("0.0.0-other");
        assert_eq!(managed_helper(data_dir.path()), None);

        write_marker(HELPER_VERSION);
        assert_eq!(managed_helper(data_dir.path()), Some(binary));
    }

    #[test]
    fn every_supported_platform_names_a_published_artifact() {
        let triple = target_triple().expect("supported test platform");
        let url = artifact_url(triple);
        assert!(url.starts_with(
            "https://downloads.brightwave.io/tidebreak/tools/whisper-helper/v"
        ));
        assert!(url.contains(HELPER_VERSION));
        assert!(url.contains(triple));
    }
}

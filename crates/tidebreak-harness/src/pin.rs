//! Pinned harness installs under the Tidebreak data directory.
//!
//! Each engine is an exact npm package version. Probe and launch use that
//! copy. The user's PATH is not the engine (decision 0041).
//!
//! The pin is the floor, not the only version. A reader on the `latest`
//! update channel installs whatever the registry publishes, into a sibling
//! directory keyed by that exact version; the marker inside says which one it
//! is, so nothing on disk is trusted by being newest. See
//! [`latest_published_version`] and [`ensure_installed_version`].

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidebreak_core::HarnessKind;
use tidebreak_managed_node::{
    managed_node_executable, managed_node_path_dir, managed_npm_executable,
};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{is_absolute_executable, spawn_process_tree};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

/// A registry lookup is one small HTTP request, but npm still resolves the
/// registry, reads its config, and may wait on a slow network.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(60);

/// One exact harness pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPin {
    /// Engine this pin belongs to.
    pub kind: HarnessKind,
    /// Exact package version.
    pub version: &'static str,
    /// npm package name.
    pub package: &'static str,
    /// Binary name inside `node_modules/.bin`.
    pub bin: &'static str,
}

/// Current pins. Bump deliberately; these are not floating "latest" tags.
pub const PINS: &[HarnessPin] = &[
    HarnessPin {
        kind: HarnessKind::ClaudeCode,
        version: "2.1.234",
        package: "@anthropic-ai/claude-code",
        bin: "claude",
    },
    HarnessPin {
        kind: HarnessKind::Codex,
        version: "0.147.0",
        package: "@openai/codex",
        bin: "codex",
    },
    HarnessPin {
        kind: HarnessKind::Opencode,
        version: "1.18.18",
        package: "opencode-ai",
        bin: "opencode",
    },
    HarnessPin {
        kind: HarnessKind::Grok,
        version: "1.0.4",
        package: "@xai-official/grok",
        bin: "grok",
    },
];

/// The pin for a kind, if we ship one.
#[must_use]
pub fn pin_for(kind: HarnessKind) -> Option<&'static HarnessPin> {
    PINS.iter().find(|pin| pin.kind == kind)
}

/// `{data_dir}/tools/harnesses/{kind}/{version}` for the pin itself.
#[must_use]
pub fn install_dir(data_dir: &Path, pin: &HarnessPin) -> PathBuf {
    install_dir_for(data_dir, pin, pin.version)
}

/// `{data_dir}/tools/harnesses/{kind}/{version}` for any exact version of
/// the pin's package.
#[must_use]
pub fn install_dir_for(data_dir: &Path, pin: &HarnessPin, version: &str) -> PathBuf {
    versions_dir(data_dir, pin.kind).join(version)
}

/// `{data_dir}/tools/harnesses/{kind}`: one subdirectory per installed
/// version.
fn versions_dir(data_dir: &Path, kind: HarnessKind) -> PathBuf {
    data_dir.join("tools").join("harnesses").join(kind.as_str())
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join("installed.json")
}

fn binary_path(dir: &Path, pin: &HarnessPin) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{}.cmd", pin.bin)
    } else {
        pin.bin.to_owned()
    };
    dir.join("node_modules").join(".bin").join(name)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct InstallMarker {
    package: String,
    version: String,
}

/// The managed binary for this pin, if the marker matches and the file exists.
#[must_use]
pub fn managed_binary(data_dir: &Path, kind: HarnessKind) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    managed_binary_version(data_dir, kind, pin.version)
}

/// The managed binary for one exact version of `kind`, if its marker names
/// that version of the pinned package and the file exists.
#[must_use]
pub fn managed_binary_version(
    data_dir: &Path,
    kind: HarnessKind,
    version: &str,
) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    let dir = install_dir_for(data_dir, pin, version);
    let marker: InstallMarker =
        serde_json::from_str(&std::fs::read_to_string(marker_path(&dir)).ok()?).ok()?;
    if marker.package != pin.package || marker.version != version {
        return None;
    }
    let binary = binary_path(&dir, pin);
    is_absolute_executable(&binary).then_some(binary)
}

/// Every version of `kind` installed under the data directory, newest first.
///
/// A directory counts only when its marker names the pinned package at the
/// version the directory is named for and the binary inside is executable —
/// the same bar [`managed_binary_version`] holds one directory to. A torn or
/// foreign tree is not a version.
#[must_use]
pub fn installed_versions(data_dir: &Path, kind: HarnessKind) -> Vec<String> {
    let Some(pin) = pin_for(kind) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(versions_dir(data_dir, pin.kind)) else {
        return Vec::new();
    };
    let mut versions: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|version| managed_binary_version(data_dir, kind, version).is_some())
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions
}

/// Order two package versions.
///
/// Dotted numeric components compare as numbers, so `2.1.251` is newer than
/// `2.1.99`. A release without a pre-release suffix is newer than the same
/// release with one; two suffixes compare as text. Good enough for the
/// registries the pins come from, without a semver dependency.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_core, a_rest) = split_version(a);
    let (b_core, b_rest) = split_version(b);
    let len = a_core.len().max(b_core.len());
    for index in 0..len {
        let left = a_core.get(index).copied().unwrap_or(0);
        let right = b_core.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    match (a_rest, b_rest) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => left.cmp(right),
    }
}

/// The numeric core of a version and its pre-release suffix, if any.
fn split_version(version: &str) -> (Vec<u64>, Option<&str>) {
    let version = version.trim().trim_start_matches('v');
    let end = version
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(version.len());
    let core = version[..end]
        .split('.')
        .filter_map(|part| part.parse::<u64>().ok())
        .collect();
    let rest = version[end..].trim_start_matches(['-', '+']);
    (core, (!rest.is_empty()).then_some(rest))
}

/// Whether `candidate` is a package version the registry could have
/// published: something npm would accept in `pkg@version`, and nothing a
/// shell or a path would read differently.
fn is_plausible_version(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    chars.next().is_some_and(|first| first.is_ascii_digit())
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
        && candidate.len() <= 64
}

/// Ask the registry which version `kind`'s package publishes as `latest`.
///
/// Runs `npm view <package> dist-tags.latest` under the host-verified managed
/// Node runtime, the same npm the install uses, so the answer names a version
/// that install can fetch. Never runs on a listing path: a lookup is a
/// network round trip, and the doctor's Check for updates and a deliberate
/// install are the only callers.
pub async fn latest_published_version(
    kind: HarnessKind,
    managed_node_root: Option<&Path>,
) -> Result<String, String> {
    let node_root = verified_managed_node_root(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before checking for updates".to_owned())?;
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let mut command = Command::new(managed_npm_executable(node_root));
    command
        .args([
            "view",
            "--no-fund",
            "--no-audit",
            "--no-update-notifier",
            pin.package,
            "dist-tags.latest",
        ])
        .env("PATH", prepend_path(&managed_node_path_dir(node_root)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = spawn_process_tree(&mut command)
        .map_err(|err| format!("npm view {}: {err}", pin.package))?;
    let output = timeout(LOOKUP_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("npm view {} timed out", pin.package))?
        .map_err(|err| format!("npm view {}: {err}", pin.package))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "npm view {} failed: {}",
            pin.package,
            stderr.lines().last().unwrap_or("unknown error")
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !is_plausible_version(&version) {
        return Err(format!(
            "npm view {} answered with something other than a version",
            pin.package
        ));
    }
    Ok(version)
}

/// One lock per install directory, so two callers cannot npm-install into it
/// at once.
///
/// A pin now has two installers — the warm install the New Workspace dialog
/// starts, and the create path's own fallback — and npm has no guard of its
/// own for two processes writing one `node_modules`. The marker is written
/// only after a successful install, so a torn tree left by an interleaved run
/// would be blessed by the next marker write rather than reinstalled. Keying
/// on the directory rather than the kind keeps two data directories (tests,
/// profiles) from serializing against each other.
fn install_lock(dir: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("harness install locks")
        .entry(dir.to_path_buf())
        .or_default()
        .clone()
}

/// Install the pin with the host-verified managed Node runtime's npm if it is
/// not present.
///
/// The caller owns verification of `managed_node_root` (for the desktop this
/// is the digest-gated host-tool broker). This crate deliberately does not
/// scan `{data_dir}/tools/node`: a sibling directory that merely looks like a
/// Node install must never become executable code by being newest on disk.
///
/// Concurrent calls for one pin are serialized: the first installs and the
/// rest return the binary it produced.
pub async fn ensure_installed(
    data_dir: &Path,
    kind: HarnessKind,
    managed_node_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    ensure_installed_version(data_dir, kind, pin.version, managed_node_root).await
}

/// Install one exact version of `kind`'s package, if it is not present.
///
/// The `latest` update channel lands here with the version the registry
/// named; the pinned channel lands here through [`ensure_installed`]. Same
/// lock, same marker, same verified npm: a newer version is held to exactly
/// the bar the pin is.
pub async fn ensure_installed_version(
    data_dir: &Path,
    kind: HarnessKind,
    version: &str,
    managed_node_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let node_root = verified_managed_node_root(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before pinning harnesses".to_owned())?;
    if !is_plausible_version(version) {
        return Err(format!("{version:?} is not a package version"));
    }
    if let Some(existing) = managed_binary_version(data_dir, kind, version) {
        return Ok(existing);
    }
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let dir = install_dir_for(data_dir, pin, version);
    let lock = install_lock(&dir);
    let _guard = lock.lock().await;
    // Another caller may have installed this version while this one waited.
    if let Some(existing) = managed_binary_version(data_dir, kind, version) {
        return Ok(existing);
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("could not create harness install dir: {err}"))?;
    let spec = format!("{}@{}", pin.package, version);
    let mut command = Command::new(managed_npm_executable(node_root));
    command
        .args([
            "install",
            "--omit=dev",
            "--no-fund",
            "--no-audit",
            "--no-progress",
            &spec,
        ])
        .current_dir(&dir)
        .env("PATH", prepend_path(&managed_node_path_dir(node_root)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child =
        spawn_process_tree(&mut command).map_err(|err| format!("npm install {spec}: {err}"))?;
    let output = timeout(INSTALL_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("npm install {spec} timed out"))?
        .map_err(|err| format!("npm install {spec}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "npm install {spec} failed: {}",
            stderr.lines().last().unwrap_or("unknown error")
        ));
    }
    let marker = InstallMarker {
        package: pin.package.to_owned(),
        version: version.to_owned(),
    };
    tokio::fs::write(
        marker_path(&dir),
        serde_json::to_vec_pretty(&marker).map_err(|err| err.to_string())?,
    )
    .await
    .map_err(|err| format!("could not write harness install marker: {err}"))?;
    managed_binary_version(data_dir, kind, version).ok_or_else(|| {
        format!(
            "npm install {spec} finished but {} was not executable",
            pin.bin
        )
    })
}

/// The verified root when both platform-native Node and npm entrypoints exist.
///
/// Unix npm is a script that resolves `node` from PATH. Windows npm is a
/// `.cmd` shim that first resolves the sibling `node.exe`. Both are validated
/// here and the managed runtime directory is still placed first on PATH for
/// the harness shims installed under `node_modules/.bin`.
fn verified_managed_node_root(managed_node_root: Option<&Path>) -> Option<&Path> {
    let root = managed_node_root?;
    (is_absolute_executable(&managed_node_executable(root))
        && is_absolute_executable(&managed_npm_executable(root)))
    .then_some(root)
}

fn prepend_path(bin: &Path) -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths).unwrap_or_else(|_| bin.as_os_str().to_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_pin() {
        for kind in [
            HarnessKind::ClaudeCode,
            HarnessKind::Codex,
            HarnessKind::Opencode,
            HarnessKind::Grok,
        ] {
            assert!(pin_for(kind).is_some(), "{kind}");
        }
    }

    #[test]
    fn marker_mismatch_hides_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let dir = install_dir(tmp.path(), pin);
        let binary = binary_path(&dir, pin);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            marker_path(&dir),
            r#"{"package":"@anthropic-ai/claude-code","version":"0.0.1"}"#,
        )
        .unwrap();
        assert_eq!(managed_binary(tmp.path(), HarnessKind::ClaudeCode), None);
        std::fs::write(
            marker_path(&dir),
            serde_json::to_vec(&InstallMarker {
                package: pin.package.to_owned(),
                version: pin.version.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            managed_binary(tmp.path(), HarnessKind::ClaudeCode),
            Some(binary)
        );
    }

    fn write_install(data_dir: &Path, kind: HarnessKind, version: &str) -> PathBuf {
        let pin = pin_for(kind).unwrap();
        let dir = install_dir_for(data_dir, pin, version);
        let binary = binary_path(&dir, pin);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            marker_path(&dir),
            serde_json::to_vec(&InstallMarker {
                package: pin.package.to_owned(),
                version: version.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        binary
    }

    /// A newer version sits beside the pin in its own directory. Neither
    /// hides the other, and the listing puts the newer one first — by
    /// number, not by text, so `2.1.251` beats `2.1.99`.
    #[test]
    fn installed_versions_list_newest_first_beside_the_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::ClaudeCode;
        let pin = pin_for(kind).unwrap();
        let pinned = write_install(tmp.path(), kind, pin.version);
        let newer = write_install(tmp.path(), kind, "2.1.251");
        write_install(tmp.path(), kind, "2.1.99");
        // A directory whose marker disagrees with its name is not a version.
        let torn = install_dir_for(tmp.path(), pin, "9.9.9");
        std::fs::create_dir_all(&torn).unwrap();
        std::fs::write(
            marker_path(&torn),
            r#"{"package":"@anthropic-ai/claude-code","version":"1.0.0"}"#,
        )
        .unwrap();

        assert_eq!(
            installed_versions(tmp.path(), kind),
            vec![
                "2.1.251".to_owned(),
                pin.version.to_owned(),
                "2.1.99".to_owned()
            ]
        );
        assert_eq!(managed_binary(tmp.path(), kind), Some(pinned));
        assert_eq!(
            managed_binary_version(tmp.path(), kind, "2.1.251"),
            Some(newer)
        );
        assert_eq!(managed_binary_version(tmp.path(), kind, "9.9.9"), None);
    }

    #[test]
    fn versions_compare_by_number_then_prerelease() {
        assert_eq!(compare_versions("2.1.251", "2.1.99"), Ordering::Greater);
        assert_eq!(compare_versions("2.1.234", "2.1.234"), Ordering::Equal);
        assert_eq!(compare_versions("2.2.0", "2.1.300"), Ordering::Greater);
        assert_eq!(compare_versions("2.1.0-beta.1", "2.1.0"), Ordering::Less);
        assert_eq!(compare_versions("v1.0.4", "1.0.5"), Ordering::Less);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn a_registry_answer_must_look_like_a_version() {
        assert!(is_plausible_version("2.1.251"));
        assert!(is_plausible_version("1.0.0-rc.2"));
        assert!(!is_plausible_version(""));
        assert!(!is_plausible_version("latest"));
        assert!(!is_plausible_version("../2.1.251"));
        assert!(!is_plausible_version("2.1.251; rm -rf /"));
    }

    #[test]
    fn harness_binary_uses_the_platform_npm_shim_name() {
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let path = binary_path(Path::new("install"), pin);
        #[cfg(windows)]
        assert_eq!(path.file_name().unwrap(), "claude.cmd");
        #[cfg(not(windows))]
        assert_eq!(path.file_name().unwrap(), "claude");
    }

    #[test]
    fn missing_managed_node_is_an_install_error() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory that resembles an install is not authority. Only the
        // verified root supplied by the host may be used.
        let decoy_bin = tmp.path().join("tools/node/decoy/bin");
        std::fs::create_dir_all(&decoy_bin).unwrap();
        for name in ["node", "npm"] {
            let path = decoy_bin.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(ensure_installed(tmp.path(), HarnessKind::ClaudeCode, None))
            .unwrap_err();
        assert!(err.contains("managed Node"), "{err}");
    }

    /// The warm install and the create path's fallback can ask for one pin at
    /// the same time. Only one of them may be inside the install directory.
    #[tokio::test]
    async fn one_pin_directory_admits_one_installer_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let dir = install_dir(tmp.path(), pin);
        let inside = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let installers = (0..4).map(|_| {
            let (dir, inside, peak) = (dir.clone(), Arc::clone(&inside), Arc::clone(&peak));
            tokio::spawn(async move {
                let lock = install_lock(&dir);
                let _guard = lock.lock().await;
                let now = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                tokio::task::yield_now().await;
                inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        for installer in installers.collect::<Vec<_>>() {
            installer.await.unwrap();
        }

        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A second pin is not held up by the first.
        let other = install_dir(tmp.path(), pin_for(HarnessKind::Codex).unwrap());
        assert!(!Arc::ptr_eq(&install_lock(&dir), &install_lock(&other)));
    }
}

//! Pinned harness installs under the Tidebreak data directory.
//!
//! Each engine is an exact npm package version. Probe and launch use that
//! copy. The user's PATH is not the engine (decision 0041).

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

/// `{data_dir}/tools/harnesses/{kind}/{version}`
#[must_use]
pub fn install_dir(data_dir: &Path, pin: &HarnessPin) -> PathBuf {
    data_dir
        .join("tools")
        .join("harnesses")
        .join(pin.kind.as_str())
        .join(pin.version)
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
    let dir = install_dir(data_dir, pin);
    let marker: InstallMarker =
        serde_json::from_str(&std::fs::read_to_string(marker_path(&dir)).ok()?).ok()?;
    if marker.package != pin.package || marker.version != pin.version {
        return None;
    }
    let binary = binary_path(&dir, pin);
    is_absolute_executable(&binary).then_some(binary)
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
    let node_root = verified_managed_node_root(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before pinning harnesses".to_owned())?;
    if let Some(existing) = managed_binary(data_dir, kind) {
        return Ok(existing);
    }
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let dir = install_dir(data_dir, pin);
    let lock = install_lock(&dir);
    let _guard = lock.lock().await;
    // Another caller may have installed this pin while this one waited.
    if let Some(existing) = managed_binary(data_dir, kind) {
        return Ok(existing);
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("could not create harness install dir: {err}"))?;
    let spec = format!("{}@{}", pin.package, pin.version);
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
        version: pin.version.to_owned(),
    };
    tokio::fs::write(
        marker_path(&dir),
        serde_json::to_vec_pretty(&marker).map_err(|err| err.to_string())?,
    )
    .await
    .map_err(|err| format!("could not write harness install marker: {err}"))?;
    managed_binary(data_dir, kind).ok_or_else(|| {
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

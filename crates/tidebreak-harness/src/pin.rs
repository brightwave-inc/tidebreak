//! Pinned harness installs under the Tidebreak data directory.
//!
//! Each engine is an exact npm package version. Probe and launch use that
//! copy. The user's PATH is not the engine (decision 0041).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidebreak_core::HarnessKind;
use tokio::process::Command;
use tokio::time::timeout;

use crate::is_absolute_executable;

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
    dir.join("node_modules").join(".bin").join(pin.bin)
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

/// Install the pin with the host-verified managed Node runtime's npm if it is
/// not present.
///
/// The caller owns verification of `managed_node_root` (for the desktop this
/// is the digest-gated host-tool broker). This crate deliberately does not
/// scan `{data_dir}/tools/node`: a sibling directory that merely looks like a
/// Node install must never become executable code by being newest on disk.
pub async fn ensure_installed(
    data_dir: &Path,
    kind: HarnessKind,
    managed_node_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let node_bin = managed_node_bin(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before pinning harnesses".to_owned())?;
    if let Some(existing) = managed_binary(data_dir, kind) {
        return Ok(existing);
    }
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let dir = install_dir(data_dir, pin);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| format!("could not create harness install dir: {err}"))?;
    let spec = format!("{}@{}", pin.package, pin.version);
    let mut command = Command::new(node_bin.join("npm"));
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
        .env("PATH", prepend_path(&node_bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(INSTALL_TIMEOUT, command.output())
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

/// `<verified-root>/bin` when both `node` and `npm` exist.
///
/// Official Node's `bin/npm` is `#!/usr/bin/env node`, so the sibling `node`
/// must be first on the child PATH. GUI PATH is never consulted.
fn managed_node_bin(managed_node_root: Option<&Path>) -> Option<PathBuf> {
    let bin = managed_node_root?.join("bin");
    (is_absolute_executable(&bin.join("node")) && is_absolute_executable(&bin.join("npm")))
        .then_some(bin)
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
}

//! Shared verification contract for Tidebreak's managed Node runtime.
//!
//! The desktop owns downloading and unpacking Node. Consumers only trust the
//! resulting directory when its marker names the exact artifact pinned for the
//! current platform and both required entrypoints are present. Keeping this
//! check below the desktop lets a headless server reuse an install in the same
//! Tidebreak data directory without scanning arbitrary version directories or
//! falling back to ambient `PATH`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The exact Node version Tidebreak installs and trusts.
pub const MANAGED_NODE_VERSION: &str = "20.20.2";

/// The current platform's trusted managed-Node artifact identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedNodePin {
    pub version: &'static str,
    pub tarball_sha256: &'static str,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    tarball_sha256: "466e05f3477c20dfb723054dfebffe55bc74660ee77f612166fca121dacb65b6",
});

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    tarball_sha256: "8be6f5e4bb128c82774f8a0b8d7a1cc1365a7977d9657cece0ca647b3fe04e61",
});

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    tarball_sha256: "47ef73d543ecf6eb19435f6c03a0ac4809b3bf0dd6b26c7c571efc2a6572a74d",
});

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    tarball_sha256: "19e56f0825510207dd904f087fe52faa0a4eb6b2aab5f0ea7a33830d04888b8b",
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
static CURRENT_PIN: Option<ManagedNodePin> = None;

/// The exact managed-Node artifact trusted on this platform, if supported.
#[must_use]
pub fn current_managed_node_pin() -> Option<&'static ManagedNodePin> {
    CURRENT_PIN.as_ref()
}

/// `{data_dir}/tools/node/<version>` for the current managed-Node pin.
#[must_use]
pub fn managed_node_version_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("tools")
        .join("node")
        .join(MANAGED_NODE_VERSION)
}

/// Marker written only after the desktop verifies and installs the archive.
#[must_use]
pub fn managed_node_marker_path(version_dir: &Path) -> PathBuf {
    version_dir.join("installed.json")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    version: String,
    tarball_sha256: String,
}

/// Serialize the marker for an artifact that has already passed digest
/// verification and unpacking.
pub fn managed_node_install_marker(pin: &ManagedNodePin) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&InstallMarker {
        version: pin.version.to_owned(),
        tarball_sha256: pin.tarball_sha256.to_owned(),
    })
}

/// Resolve the current platform's verified managed-Node root from one exact
/// data-directory location. No version-directory scan or PATH fallback occurs.
#[must_use]
pub fn managed_node_root(data_dir: &Path) -> Option<PathBuf> {
    managed_node_root_expecting(data_dir, current_managed_node_pin()?)
}

fn managed_node_root_expecting(data_dir: &Path, pin: &ManagedNodePin) -> Option<PathBuf> {
    let version_dir = managed_node_version_dir(data_dir);
    let marker = std::fs::read(managed_node_marker_path(&version_dir)).ok()?;
    let marker: InstallMarker = serde_json::from_slice(&marker).ok()?;
    if marker.version != pin.version || marker.tarball_sha256 != pin.tarball_sha256 {
        return None;
    }
    (version_dir.join("bin/node").is_file() && version_dir.join("bin/npm").is_file())
        .then_some(version_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_pin_matches_shipped_desktop_hosts() {
        assert_eq!(
            current_managed_node_pin().is_some(),
            cfg!(any(target_os = "macos", target_os = "linux"))
                && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        );
    }

    #[test]
    fn managed_runtime_resolves_only_with_a_matching_marker() {
        let expected = ManagedNodePin {
            version: MANAGED_NODE_VERSION,
            tarball_sha256: "466e05f3477c20dfb723054dfebffe55bc74660ee77f612166fca121dacb65b6",
        };
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = managed_node_version_dir(data_dir.path());
        let bin = version_dir.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join("node"), b"#!/bin/sh\n").expect("node");
        std::fs::write(bin.join("npm"), b"#!/bin/sh\n").expect("npm");

        assert_eq!(
            managed_node_root_expecting(data_dir.path(), &expected),
            None
        );

        let wrong = ManagedNodePin {
            tarball_sha256: "0",
            ..expected
        };
        std::fs::write(
            managed_node_marker_path(&version_dir),
            managed_node_install_marker(&wrong).expect("marker"),
        )
        .expect("write marker");
        assert_eq!(
            managed_node_root_expecting(data_dir.path(), &expected),
            None
        );

        std::fs::write(
            managed_node_marker_path(&version_dir),
            managed_node_install_marker(&expected).expect("marker"),
        )
        .expect("write marker");
        assert_eq!(
            managed_node_root_expecting(data_dir.path(), &expected),
            Some(version_dir)
        );
    }
}

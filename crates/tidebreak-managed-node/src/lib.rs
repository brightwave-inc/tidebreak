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
pub const MANAGED_NODE_VERSION: &str = "24.21.0";

// From the exact ZIP rows in Node's v24.21.0 SHASUMS256.txt. Keep the
// filenames beside the tests below: the neighboring .7z, MSI, and standalone
// node.exe rows are different artifacts with different digests.
#[allow(dead_code)] // Each target uses only its own architecture; tests bind both names.
const WINDOWS_ARM64_ZIP_SHA256: &str =
    "8779b1bde1d39f8d420e3b57aa657b39891af434d3de44a919044cec06785921";
#[allow(dead_code)] // Each target uses only its own architecture; tests bind both names.
const WINDOWS_X64_ZIP_SHA256: &str =
    "158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541";

/// The current platform's trusted managed-Node artifact identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedNodePin {
    pub version: &'static str,
    pub artifact_sha256: &'static str,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
});

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097",
});

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: "724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5",
});

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff",
});

#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: WINDOWS_ARM64_ZIP_SHA256,
});

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
static CURRENT_PIN: Option<ManagedNodePin> = Some(ManagedNodePin {
    version: MANAGED_NODE_VERSION,
    artifact_sha256: WINDOWS_X64_ZIP_SHA256,
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

#[derive(Debug, Clone, Copy)]
enum ManagedNodeLayout {
    #[cfg_attr(windows, allow(dead_code))]
    Unix,
    #[cfg_attr(not(windows), allow(dead_code))]
    Windows,
}

#[cfg(windows)]
const CURRENT_LAYOUT: ManagedNodeLayout = ManagedNodeLayout::Windows;
#[cfg(not(windows))]
const CURRENT_LAYOUT: ManagedNodeLayout = ManagedNodeLayout::Unix;

/// The managed Node interpreter for this platform's official archive layout.
#[must_use]
pub fn managed_node_executable(version_dir: &Path) -> PathBuf {
    managed_node_executable_for_layout(version_dir, CURRENT_LAYOUT)
}

/// The managed npm executable or command shim for this platform's official
/// archive layout.
#[must_use]
pub fn managed_npm_executable(version_dir: &Path) -> PathBuf {
    managed_npm_executable_for_layout(version_dir, CURRENT_LAYOUT)
}

/// The directory to prepend to `PATH` for this platform's managed runtime.
#[must_use]
pub fn managed_node_path_dir(version_dir: &Path) -> PathBuf {
    managed_node_path_dir_for_layout(version_dir, CURRENT_LAYOUT)
}

fn managed_node_executable_for_layout(version_dir: &Path, layout: ManagedNodeLayout) -> PathBuf {
    match layout {
        ManagedNodeLayout::Unix => version_dir.join("bin").join("node"),
        ManagedNodeLayout::Windows => version_dir.join("node.exe"),
    }
}

fn managed_npm_executable_for_layout(version_dir: &Path, layout: ManagedNodeLayout) -> PathBuf {
    match layout {
        ManagedNodeLayout::Unix => version_dir.join("bin").join("npm"),
        ManagedNodeLayout::Windows => version_dir.join("npm.cmd"),
    }
}

fn managed_node_path_dir_for_layout(version_dir: &Path, layout: ManagedNodeLayout) -> PathBuf {
    match layout {
        ManagedNodeLayout::Unix => version_dir.join("bin"),
        ManagedNodeLayout::Windows => version_dir.to_path_buf(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    version: String,
    #[serde(alias = "tarballSha256")]
    artifact_sha256: String,
}

/// Serialize the marker for an artifact that has already passed digest
/// verification and unpacking.
pub fn managed_node_install_marker(pin: &ManagedNodePin) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&InstallMarker {
        version: pin.version.to_owned(),
        artifact_sha256: pin.artifact_sha256.to_owned(),
    })
}

/// Resolve the current platform's verified managed-Node root from one exact
/// data-directory location. No version-directory scan or PATH fallback occurs.
#[must_use]
pub fn managed_node_root(data_dir: &Path) -> Option<PathBuf> {
    managed_node_root_expecting(data_dir, current_managed_node_pin()?)
}

fn managed_node_root_expecting(data_dir: &Path, pin: &ManagedNodePin) -> Option<PathBuf> {
    managed_node_root_expecting_layout(data_dir, pin, CURRENT_LAYOUT)
}

fn managed_node_root_expecting_layout(
    data_dir: &Path,
    pin: &ManagedNodePin,
    layout: ManagedNodeLayout,
) -> Option<PathBuf> {
    let version_dir = managed_node_version_dir(data_dir);
    let marker = std::fs::read(managed_node_marker_path(&version_dir)).ok()?;
    let marker: InstallMarker = serde_json::from_slice(&marker).ok()?;
    if marker.version != pin.version || marker.artifact_sha256 != pin.artifact_sha256 {
        return None;
    }
    (managed_node_executable_for_layout(&version_dir, layout).is_file()
        && managed_npm_executable_for_layout(&version_dir, layout).is_file())
    .then_some(version_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_pin_matches_shipped_desktop_hosts() {
        assert_eq!(
            current_managed_node_pin().is_some(),
            cfg!(any(
                target_os = "macos",
                target_os = "linux",
                target_os = "windows"
            )) && cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
        );
    }

    #[test]
    fn windows_zip_pins_match_the_named_official_artifacts() {
        assert_eq!(
            ("node-v24.21.0-win-arm64.zip", WINDOWS_ARM64_ZIP_SHA256),
            (
                "node-v24.21.0-win-arm64.zip",
                "8779b1bde1d39f8d420e3b57aa657b39891af434d3de44a919044cec06785921"
            )
        );
        assert_eq!(
            ("node-v24.21.0-win-x64.zip", WINDOWS_X64_ZIP_SHA256),
            (
                "node-v24.21.0-win-x64.zip",
                "158f7685b44de51f6c0df1d153526cbcd3e1bc739a8dfc607721cef75de9e541"
            )
        );
    }

    #[test]
    fn official_archive_layouts_resolve_runtime_entrypoints() {
        let root = Path::new("runtime");

        assert_eq!(
            managed_node_executable_for_layout(root, ManagedNodeLayout::Unix),
            root.join("bin/node")
        );
        assert_eq!(
            managed_npm_executable_for_layout(root, ManagedNodeLayout::Unix),
            root.join("bin/npm")
        );
        assert_eq!(
            managed_node_path_dir_for_layout(root, ManagedNodeLayout::Unix),
            root.join("bin")
        );

        assert_eq!(
            managed_node_executable_for_layout(root, ManagedNodeLayout::Windows),
            root.join("node.exe")
        );
        assert_eq!(
            managed_npm_executable_for_layout(root, ManagedNodeLayout::Windows),
            root.join("npm.cmd")
        );
        assert_eq!(
            managed_node_path_dir_for_layout(root, ManagedNodeLayout::Windows),
            root
        );
    }

    #[test]
    fn marker_writes_platform_neutral_digest_and_reads_legacy_name() {
        let expected = ManagedNodePin {
            version: MANAGED_NODE_VERSION,
            artifact_sha256: "digest",
        };
        let marker = managed_node_install_marker(&expected).expect("marker");
        let marker_json: serde_json::Value = serde_json::from_slice(&marker).expect("json");
        assert_eq!(marker_json["artifactSha256"], "digest");
        assert!(marker_json.get("tarballSha256").is_none());

        let legacy: InstallMarker =
            serde_json::from_slice(br#"{"version":"24.21.0","tarballSha256":"digest"}"#)
                .expect("legacy marker");
        assert_eq!(legacy.artifact_sha256, "digest");
    }

    #[test]
    fn managed_runtime_resolves_only_with_a_matching_marker() {
        let expected = ManagedNodePin {
            version: MANAGED_NODE_VERSION,
            artifact_sha256: "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
        };
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = managed_node_version_dir(data_dir.path());
        let node = managed_node_executable(&version_dir);
        let npm = managed_npm_executable(&version_dir);
        std::fs::create_dir_all(node.parent().expect("node parent")).expect("node parent");
        std::fs::create_dir_all(npm.parent().expect("npm parent")).expect("npm parent");
        std::fs::write(node, b"node").expect("node");
        std::fs::write(npm, b"npm").expect("npm");

        assert_eq!(
            managed_node_root_expecting(data_dir.path(), &expected),
            None
        );

        let wrong = ManagedNodePin {
            artifact_sha256: "0",
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

    #[test]
    fn verifier_requires_the_selected_platform_layout() {
        let expected = ManagedNodePin {
            version: MANAGED_NODE_VERSION,
            artifact_sha256: "digest",
        };
        let data_dir = tempfile::tempdir().expect("tempdir");
        let version_dir = managed_node_version_dir(data_dir.path());
        std::fs::create_dir_all(version_dir.join("bin")).expect("bin");
        std::fs::write(version_dir.join("bin/node"), b"node").expect("unix node");
        std::fs::write(version_dir.join("bin/npm"), b"npm").expect("unix npm");
        std::fs::write(
            managed_node_marker_path(&version_dir),
            managed_node_install_marker(&expected).expect("marker"),
        )
        .expect("write marker");

        assert_eq!(
            managed_node_root_expecting_layout(data_dir.path(), &expected, ManagedNodeLayout::Unix),
            Some(version_dir.clone())
        );
        assert_eq!(
            managed_node_root_expecting_layout(
                data_dir.path(),
                &expected,
                ManagedNodeLayout::Windows
            ),
            None
        );

        std::fs::write(version_dir.join("node.exe"), b"node").expect("windows node");
        std::fs::write(version_dir.join("npm.cmd"), b"npm").expect("windows npm");
        assert_eq!(
            managed_node_root_expecting_layout(
                data_dir.path(),
                &expected,
                ManagedNodeLayout::Windows
            ),
            Some(version_dir)
        );
    }
}

//! The optional host-folder seam: how server-side app surfaces see the host
//! broker's approved connected folders.
//!
//! Nothing in this crate can reach the broker — it is a sidecar only the
//! desktop process spawns — so the surface is a trait object installed after
//! assembly (the `code_execution` pattern). An embedding without one
//! (headless `tidebreak serve`, tests, generic hosts) reads an empty folder
//! surface: folder bindings refuse to grant and read stale, honestly,
//! instead of parking. See `docs/folder-bindings.md`.

use async_trait::async_trait;

use tidebreak_core::id::{AppId, HostRootId};
use tidebreak_core::local_app::FolderAccess;

/// One host-approved connected folder, projected renderer-safe: the stable
/// broker root id and its display name, never a path.
#[derive(Debug, Clone)]
pub struct ApprovedFolder {
    pub root_id: HostRootId,
    pub display_name: String,
}

/// One directory entry under an approved folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// Entry name — a single path segment, never a path.
    pub name: String,
    /// Whether the entry is itself a directory.
    pub directory: bool,
}

/// The outcome of one bounded app-folder write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderWriteReceipt {
    /// Bytes written.
    pub bytes: usize,
    /// Whether an existing file was replaced.
    pub replaced: bool,
}

/// The closed failure vocabulary of app-folder operations.
///
/// Renderer-facing: each variant's display text is what an app frame may see,
/// so no variant ever carries a path, an OS error, or broker internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderOpError {
    /// The folder is no longer a live registration.
    NotConnected,
    /// The path does not name an existing entry.
    NotFound,
    /// The path grammar refused (absolute, traversal, or malformed).
    InvalidPath,
    /// The file or listing exceeds the host's transfer bound.
    TooLarge,
    /// A create found an existing file, or a replace found none.
    WrongMode,
    /// Anything else — the host refused or failed, with no detail an app
    /// frame is entitled to.
    Failed,
}

impl std::fmt::Display for FolderOpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotConnected => "the folder is no longer connected",
            Self::NotFound => "no such file or folder",
            Self::InvalidPath => "the path is not a valid folder-relative path",
            Self::TooLarge => "the content exceeds the folder transfer bound",
            Self::WrongMode => "the file already exists, or there is nothing to replace",
            Self::Failed => "the folder operation failed",
        })
    }
}

/// The host-folder surface for local-app folder bindings.
///
/// Implementations sit over the host broker's app-folder control surface,
/// which owns the host-level half — live-registration checks, pinned
/// descriptors, byte bounds. The caller owns the consent half: nothing here
/// is dispatched until the app grant admitted the invoke.
#[async_trait]
pub trait HostFolders: Send + Sync {
    /// Every host-approved connected folder, read live per request — never
    /// cached across one — so grant enforcement always judges the
    /// registration a root id resolves to *now*.
    async fn approved_roots(&self) -> tidebreak_core::Result<Vec<ApprovedFolder>>;

    /// List one directory under an approved folder, within the host's
    /// listing bounds. `app` names the invoking app in the host's audit
    /// trail; the caller has already enforced its grant.
    async fn list_folder(
        &self,
        app: AppId,
        root: HostRootId,
        path: &str,
    ) -> Result<Vec<FolderEntry>, FolderOpError>;

    /// Read one file under an approved folder as bounded opaque bytes.
    async fn read_file(
        &self,
        app: AppId,
        root: HostRootId,
        path: &str,
    ) -> Result<Vec<u8>, FolderOpError>;

    /// Write one file under an approved folder, atomically, within the
    /// host's write bound. `replace` selects the create-vs-replace mode; the
    /// caller has already enforced the grant's access level.
    async fn write_file(
        &self,
        app: AppId,
        root: HostRootId,
        path: &str,
        content: &[u8],
        replace: bool,
    ) -> Result<FolderWriteReceipt, FolderOpError>;
}

/// SHA-256 fingerprint of one folder binding's canonical form, the value an
/// app grant pins.
///
/// The digest is taken over the UTF-8 bytes of a compact JSON object with
/// **exactly these keys, in exactly this order**:
///
/// ```json
/// {"v":2,"kind":"folder","root_id":"<uuid>","access":"read"|"read_write"}
/// ```
///
/// `root_id` is the broker's persisted registration identity: disconnecting
/// or forgetting the folder removes it from the current lookup, so every
/// grant naming it fails closed to re-consent, and reconnecting the same
/// directory mints a fresh root id — a broken approval chain never re-arms
/// an old grant. Both inputs are the binding's own consent-bearing fields,
/// so this fingerprint's job is existence and form versioning rather than
/// configuration drift. Display names and paths never enter the form (a
/// fingerprint must not be a path oracle), and physical identity
/// (device/inode) stays the broker's enforcement at use, not consent-time
/// state. `kind` roots the form beside `mcp_server` and `rest_api` so no
/// two kinds can collide on a canonical serialization; `v:2` aligns with
/// their current version.
///
/// **This canonical form is a compatibility surface.** Persisted grants
/// store the digest; changing the form (or the meaning of any field in it)
/// invalidates every folder grant and must bump `v`.
pub(crate) fn folder_fingerprint(folder: HostRootId, access: FolderAccess) -> [u8; 32] {
    use serde::Serialize;
    use sha2::Digest as _;

    #[derive(Serialize)]
    struct CanonicalForm<'a> {
        v: u32,
        kind: &'static str,
        root_id: &'a HostRootId,
        access: &'static str,
    }

    let canonical = CanonicalForm {
        v: 2,
        kind: "folder",
        root_id: &folder,
        access: access.as_str(),
    };
    let bytes =
        serde_json::to_vec(&canonical).expect("a canonical form serializes infallibly to JSON");
    sha2::Sha256::digest(&bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariants the canonical form exists for: the fingerprint derives
    /// from exactly root id + access — the two consent-bearing fields — and
    /// nothing else can move it, while either field moving always moves it.
    #[test]
    fn folder_fingerprints_derive_from_root_and_access_only() {
        let folder = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
        let other = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();

        let baseline = folder_fingerprint(folder, FolderAccess::Read);
        assert_eq!(folder_fingerprint(folder, FolderAccess::Read), baseline);
        assert_ne!(
            folder_fingerprint(folder, FolderAccess::ReadWrite),
            baseline,
            "access is consent-bearing"
        );
        assert_ne!(
            folder_fingerprint(other, FolderAccess::Read),
            baseline,
            "a reconnected folder is a fresh consent"
        );
    }
}

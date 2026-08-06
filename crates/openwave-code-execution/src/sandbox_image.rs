//! The published OpenWave documents sandbox image, as one pin.
//!
//! Two backends in this crate run the same image: the Daytona adapter
//! registers it as a snapshot in the caller's organization, and the Docker
//! adapter runs it directly on the host's container runtime. Environment
//! parity across backends is the whole point of the image — LibreOffice, the
//! document skills' pinned Python dependencies, Node with the deck library,
//! and the bundled exec helper scripts — so the two must never drift onto
//! different digests.
//!
//! The pin is rewritten by the job in
//! `.github/workflows/publish-sandbox-image.yml` after every image publish,
//! alongside `PUBLISHED_IMAGE_DIGEST` in `openwave-server`'s `sandbox_docker`
//! and the E2B template definition. That job matches on the constant names
//! below, so renaming one means editing the workflow in the same change.

/// The official documents image, pinned by manifest-list digest. A
/// `repository@sha256:…` ref is content-addressed: the runtime resolves
/// exactly those bytes or fails, so repointing the tag on the registry
/// changes nothing here.
pub(crate) const DOCUMENTS_IMAGE: &str = "ghcr.io/brightwave-inc/openwave-sandbox-agent-documents@sha256:dd22da7a3c5b1f315e888da902e7a46ae034585e2ab5c09c0ae4588a69f158a2";

/// Resources the image needs beyond a container runtime's stock defaults.
/// LibreOffice, the Java filters it converts through, and the preinstalled
/// Python closure are what set the floor; the numbers are the same shape the
/// Daytona snapshot declares, so a document run behaves the same on either
/// backend.
pub(crate) const DOCUMENTS_CPU: u32 = 2;
pub(crate) const DOCUMENTS_MEMORY_GB: u32 = 4;
pub(crate) const DOCUMENTS_DISK_GB: u32 = 10;

/// Whether an image ref is pinned by a well-formed content digest
/// (`…@sha256:<64 hex>`).
///
/// Only a digest-pinned ref is verified at resolution: a tag ref is whatever
/// the registry points it at today, so an operator override that names one
/// gives up the integrity check. Backends that accept an override report
/// which case they are in rather than assuming the pinned one.
#[must_use]
pub(crate) fn image_digest_pinned(image: &str) -> bool {
    image
        .rsplit_once("@sha256:")
        .is_some_and(|(repository, hex)| {
            !repository.is_empty()
                && hex.len() == 64
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin is what makes every backend run the same bytes; a ref that
    /// lost its digest would silently downgrade each of them to whatever the
    /// registry serves that day.
    #[test]
    fn the_shared_documents_pin_is_digest_addressed() {
        assert!(image_digest_pinned(DOCUMENTS_IMAGE));
        assert!(!image_digest_pinned("ghcr.io/example/image:v1"));
        assert!(!image_digest_pinned("ghcr.io/example/image@sha256:short"));
    }
}

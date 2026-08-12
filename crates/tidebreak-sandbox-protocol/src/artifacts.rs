//! Artifact collection scoped to the run.
//!
//! The protocol's artifact surface is defined fresh here, scoped to the run,
//! rather than inherited from the workspace capability. A run exposes a bounded
//! [`ArtifactManifest`]; the host collects the manifest, then fetches each
//! artifact's bounded bytes. Workspace artifacts are proposals until the host
//! accepts them into the conversation's output record — acceptance is a
//! host-side operation, out of this protocol's scope.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};

use crate::protocol::{ErrorCode, ErrorResponse, MAX_ARTIFACTS, MAX_ARTIFACT_BYTES};

/// Safe metadata for one collectable artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEntry {
    /// The run-relative name the host fetches by.
    pub name: String,
    /// The decoded byte length, so the host can bound its work before fetching.
    pub bytes: usize,
    /// SHA-256 of the content, for integrity.
    pub sha256: [u8; 32],
}

/// The bounded set of artifacts a run exposes for collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub entries: Vec<ArtifactEntry>,
}

impl ArtifactManifest {
    /// Whether the manifest is within its declared bound.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.entries.len() <= MAX_ARTIFACTS
    }
}

/// The bounded bytes of one fetched artifact, base64 for the JSON transport.
///
/// Content is deliberately not logged or `Debug`-printed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactContent {
    pub content_base64: String,
    pub bytes: usize,
    pub sha256: [u8; 32],
}

impl ArtifactContent {
    /// Encode `bytes` into a bounded artifact content, computing its digest.
    ///
    /// # Errors
    /// [`ErrorCode::TooLarge`] if the content exceeds [`MAX_ARTIFACT_BYTES`].
    pub fn encode(bytes: &[u8]) -> Result<Self, ErrorResponse> {
        use sha2::{Digest, Sha256};
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ErrorResponse::new(
                ErrorCode::TooLarge,
                "artifact exceeds its size bound",
                false,
            ));
        }
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        Ok(Self {
            content_base64: BASE64.encode(bytes),
            bytes: bytes.len(),
            sha256: digest,
        })
    }

    /// The digest as fetched.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.sha256
    }
}

impl std::fmt::Debug for ArtifactContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArtifactContent")
            .field("content_base64", &"[redacted]")
            .field("bytes", &self.bytes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_computes_digest_and_bounds() {
        let content = ArtifactContent::encode(b"report").unwrap();
        assert_eq!(content.bytes, 6);
        use sha2::{Digest, Sha256};
        let expected: [u8; 32] = Sha256::digest(b"report").into();
        assert_eq!(content.digest(), expected);
    }

    #[test]
    fn oversize_artifact_is_rejected() {
        let big = vec![0u8; MAX_ARTIFACT_BYTES + 1];
        let error = ArtifactContent::encode(&big).unwrap_err();
        assert_eq!(error.code, ErrorCode::TooLarge);
    }

    #[test]
    fn content_bytes_do_not_leak_in_debug() {
        let content = ArtifactContent::encode(b"secret-bytes").unwrap();
        assert!(!format!("{content:?}").contains("secret-bytes"));
    }
}

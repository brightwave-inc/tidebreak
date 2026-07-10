//! Strongly-typed identifiers for retrieval entities.
//!
//! Same pattern as `openwave-core`'s ids: a UUID newtype per entity so the
//! compiler stops us mixing a [`DocumentId`] with a [`ChunkId`]. Ids serialize
//! transparently as the bare UUID string.

pub use openwave_core::DocumentId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Declares a UUID-backed identifier newtype with the common impls.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh, random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

id_type!(
    /// Identifies one chunk of a document.
    ///
    /// Chunk ids are *derived*, not random: [`ChunkId::derive`] hashes the parent
    /// document id together with the chunk's byte span, so re-chunking the same
    /// document produces the same ids. That makes upserts idempotent and keeps
    /// citations stable across re-ingestion.
    ChunkId
);

impl ChunkId {
    /// Namespace UUID for chunk-id derivation. A fixed, arbitrary v4 value that
    /// scopes the name-based hash so it never collides with other v5 uses.
    const NAMESPACE: Uuid = Uuid::from_u128(0x9f8d_2c31_5b47_4e6a_a1c2_d3e4_f506_1728);

    /// Derive a stable id from the parent document and a byte span.
    ///
    /// Two chunks with the same `(document_id, start, end)` always get the same
    /// id; different spans (even overlapping ones) get distinct ids.
    #[must_use]
    pub fn derive(document_id: DocumentId, start: usize, end: usize) -> Self {
        // Namespace by the document, then hash the span. Two levels keeps the
        // input space clean: same document + same span => same id, always.
        let per_document = Uuid::new_v5(&Self::NAMESPACE, document_id.as_uuid().as_bytes());
        let name = format!("{start}:{end}");
        Self(Uuid::new_v5(&per_document, name.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_different_types_are_distinct() {
        let doc = DocumentId::new();
        let chunk = ChunkId::new();
        assert_ne!(doc.0, chunk.0);
    }

    #[test]
    fn roundtrips_through_string_and_json() {
        let id = DocumentId::new();
        assert_eq!(id.to_string().parse::<DocumentId>().unwrap(), id);

        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<DocumentId>(&json).unwrap(), id);
    }

    #[test]
    fn derived_document_ids_are_stable_per_uri() {
        let core_id: openwave_core::DocumentId = DocumentId::derive("file:///a.txt");
        assert_eq!(core_id, DocumentId::derive("file:///a.txt"));
        assert_eq!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///b.txt")
        );
    }

    #[test]
    fn derived_chunk_ids_are_deterministic_and_span_sensitive() {
        let doc = DocumentId::new();
        // Same document + span => same id.
        assert_eq!(ChunkId::derive(doc, 0, 100), ChunkId::derive(doc, 0, 100));
        // Different span => different id.
        assert_ne!(ChunkId::derive(doc, 0, 100), ChunkId::derive(doc, 0, 101));
        // Different document => different id for the same span.
        let other = DocumentId::new();
        assert_ne!(ChunkId::derive(doc, 0, 100), ChunkId::derive(other, 0, 100));
    }
}

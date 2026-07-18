//! Strongly-typed identifiers for retrieval entities.
//!
//! Same pattern as `openwave-core`'s ids: a UUID newtype per entity so the
//! compiler stops us mixing a [`DocumentId`] with a [`ChunkId`]. Ids serialize
//! transparently as the bare UUID string.

pub use openwave_core::{ChunkId, DocumentId};

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

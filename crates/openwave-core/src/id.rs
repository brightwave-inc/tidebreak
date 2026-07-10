//! Strongly-typed identifiers.
//!
//! Every entity gets its own newtype over a UUID so the compiler stops us from,
//! say, passing a [`TurnId`] where a [`ChatId`] is expected. All ids
//! serialize transparently (as the bare UUID string), so on the wire and in the
//! `Store` they are indistinguishable from a plain UUID.

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
    /// Identifies a project: an optional grouping a chat may belong to.
    ProjectId
);
id_type!(
    /// Identifies an authoritative source document.
    ///
    /// Usually minted fresh with [`DocumentId::new`], but [`DocumentId::derive`]
    /// preserves the existing stable URI identity used by retrieval ingestion.
    DocumentId
);
id_type!(
    /// Identifies one durable document-processing job.
    DocumentJobId
);

impl DocumentId {
    /// Namespace UUID for URI-derived document ids. This value is part of the
    /// persisted identity contract and must remain stable.
    const NAMESPACE: Uuid = Uuid::from_u128(0x1d0c_7a44_9e21_4b83_bc55_6677_8899_aabb);

    /// Derive a stable id from a source URI in the unscoped corpus.
    ///
    /// Project-owned documents must use [`DocumentId::derive_for_project`] so
    /// the same URI can belong to more than one corpus without aliasing.
    #[must_use]
    pub fn derive(uri: &str) -> Self {
        Self(Uuid::new_v5(&Self::NAMESPACE, uri.as_bytes()))
    }

    /// Derive a stable id from a project and source URI.
    #[must_use]
    pub fn derive_for_project(project_id: ProjectId, uri: &str) -> Self {
        let project_namespace = Uuid::new_v5(&Self::NAMESPACE, project_id.as_uuid().as_bytes());
        Self(Uuid::new_v5(&project_namespace, uri.as_bytes()))
    }
}
id_type!(
    /// Identifies a persistent conversation (owns a workspace directory).
    ChatId
);
id_type!(
    /// Identifies a persisted message within a chat.
    MessageId
);
id_type!(
    /// Identifies one turn: a single user input through to the final answer.
    TurnId
);
id_type!(
    /// Identifies one step within a turn: a single LLM call and its tools.
    StepId
);
id_type!(
    /// Identifies one tool call, stable across its request/approval/result.
    CallId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_different_types_are_distinct_uuids() {
        let chat = ChatId::new();
        let turn = TurnId::new();
        assert_ne!(chat.0, turn.0);
    }

    #[test]
    fn roundtrips_through_string_and_json() {
        let id = ChatId::new();
        assert_eq!(id.to_string().parse::<ChatId>().unwrap(), id);

        let json = serde_json::to_string(&id).unwrap();
        // Transparent: serializes as the bare quoted UUID, no wrapper.
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<ChatId>(&json).unwrap(), id);
    }

    #[test]
    fn document_uri_derivation_is_stable() {
        assert_eq!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive("file:///a.txt"),
            DocumentId::derive("file:///b.txt")
        );

        let project_a = ProjectId::new();
        let project_b = ProjectId::new();
        assert_eq!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive_for_project(project_a, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive_for_project(project_b, "file:///a.txt")
        );
        assert_ne!(
            DocumentId::derive_for_project(project_a, "file:///a.txt"),
            DocumentId::derive("file:///a.txt")
        );
    }
}

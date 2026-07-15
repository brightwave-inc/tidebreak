//! Strongly-typed identifiers.
//!
//! Every entity gets its own newtype over a UUID so the compiler stops us from,
//! say, passing a [`TurnId`] where a [`ChatId`] is expected. All ids
//! serialize transparently (as the bare UUID string), so on the wire and in the
//! `Store` they are indistinguishable from a plain UUID.

use serde::{Deserialize, Serialize};
use thiserror::Error;
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

/// Opaque identifier for a folder registered with a host broker.
///
/// This is product projection data, not authority: possession of an id never
/// grants access to the corresponding host path. The broker independently
/// validates live attachments, consent, capabilities, and revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct HostRootId(Uuid);

impl HostRootId {
    /// Build a root id from its non-nil wire UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, HostRootIdError> {
        if uuid.is_nil() {
            Err(HostRootIdError::Nil)
        } else {
            Ok(Self(uuid))
        }
    }

    /// Borrow the underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl<'de> Deserialize<'de> for HostRootId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = Uuid::deserialize(deserializer)?;
        Self::from_uuid(uuid).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for HostRootId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for HostRootId {
    type Err = HostRootIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_uuid(Uuid::parse_str(value)?)
    }
}

/// Invalid opaque host-root identity.
#[derive(Debug, Error)]
pub enum HostRootIdError {
    /// The value is not a UUID.
    #[error("invalid host root id: {0}")]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is reserved and never identifies a broker root.
    #[error("host root id must not be nil")]
    Nil,
}
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
    /// Identifies a persistent conversation.
    ChatId
);
id_type!(
    /// Identifies one durable foreground or sandboxed background agent run.
    AgentRunId
);

impl AgentRunId {
    /// Namespace for the one foreground coordinator derived for each chat.
    const FOREGROUND_NAMESPACE: Uuid = Uuid::from_u128(0x38fd_6a64_b02f_4e91_a60c_7173_9af0_5b21);

    /// Derive the stable foreground coordinator identity for a chat.
    #[must_use]
    pub fn foreground_for_chat(chat_id: ChatId) -> Self {
        Self(Uuid::new_v5(
            &Self::FOREGROUND_NAMESPACE,
            chat_id.as_uuid().as_bytes(),
        ))
    }
}

/// Stable product and broker idempotency identity for one root-attachment change.
///
/// The same UUID is used when reconciling the change with the host broker. Nil
/// is reserved so a missing operation identity cannot become durable work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RootAttachmentChangeId(Uuid);

impl RootAttachmentChangeId {
    /// Generate a fresh change identity.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Build a change identity from a non-nil UUID.
    pub fn from_uuid(uuid: Uuid) -> Result<Self, RootAttachmentChangeIdError> {
        if uuid.is_nil() {
            Err(RootAttachmentChangeIdError::Nil)
        } else {
            Ok(Self(uuid))
        }
    }

    /// Borrow the UUID also used as the broker operation identity.
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for RootAttachmentChangeId {
    fn default() -> Self {
        Self::new()
    }
}

impl<'de> Deserialize<'de> for RootAttachmentChangeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let uuid = Uuid::deserialize(deserializer)?;
        Self::from_uuid(uuid).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for RootAttachmentChangeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for RootAttachmentChangeId {
    type Err = RootAttachmentChangeIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_uuid(Uuid::parse_str(value)?)
    }
}

/// Invalid durable root-attachment change identity.
#[derive(Debug, Error)]
pub enum RootAttachmentChangeIdError {
    /// The value is not a UUID.
    #[error("invalid root attachment change id: {0}")]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is reserved and never identifies durable attachment work.
    #[error("root attachment change id must not be nil")]
    Nil,
}
id_type!(
    /// Identifies a persisted message within a chat.
    MessageId
);
id_type!(
    /// Identifies one turn: a single user input through to the final answer.
    TurnId
);
id_type!(
    /// Identifies one idempotent steering instruction for a live turn.
    TurnSteerId
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
    fn foreground_agent_run_ids_are_stable_and_chat_scoped() {
        let first_chat = ChatId::new();
        let second_chat = ChatId::new();
        assert_eq!(
            AgentRunId::foreground_for_chat(first_chat),
            AgentRunId::foreground_for_chat(first_chat)
        );
        assert_ne!(
            AgentRunId::foreground_for_chat(first_chat),
            AgentRunId::foreground_for_chat(second_chat)
        );
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
    fn host_root_ids_reject_nil_and_roundtrip() {
        let uuid = Uuid::new_v4();
        let id = HostRootId::from_uuid(uuid).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<HostRootId>(&json).unwrap(), id);
        assert!(HostRootId::from_uuid(Uuid::nil()).is_err());
        assert!(
            serde_json::from_str::<HostRootId>("\"00000000-0000-0000-0000-000000000000\"").is_err()
        );
    }

    #[test]
    fn root_attachment_change_ids_reject_nil_and_roundtrip() {
        let uuid = Uuid::new_v4();
        let id = RootAttachmentChangeId::from_uuid(uuid).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            serde_json::from_str::<RootAttachmentChangeId>(&json).unwrap(),
            id
        );
        assert!(RootAttachmentChangeId::from_uuid(Uuid::nil()).is_err());
        assert!(serde_json::from_str::<RootAttachmentChangeId>(
            "\"00000000-0000-0000-0000-000000000000\""
        )
        .is_err());
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

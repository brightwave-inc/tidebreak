//! Opaque identities and execution authority used at the host boundary.

use std::{fmt, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// An identity or authority context violated a broker invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdError {
    /// Nil is a sentinel, not a valid persisted broker identity.
    #[error("broker identities must not be nil")]
    Nil,
}

/// Text parsing failure for an opaque broker identity.
#[derive(Debug, Error)]
pub enum ParseIdError {
    /// The value was not a UUID.
    #[error(transparent)]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is syntactically a UUID but not a broker identity.
    #[error(transparent)]
    InvalidIdentity(#[from] IdError),
}

macro_rules! uuid_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Allocate a fresh random identity.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Validate an identity received from trusted product state.
            pub fn from_uuid(id: Uuid) -> Result<Self, IdError> {
                if id.is_nil() {
                    Err(IdError::Nil)
                } else {
                    Ok(Self(id))
                }
            }

            /// Return the underlying UUID.
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::from_uuid(Uuid::parse_str(value)?).map_err(Into::into)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let id = Uuid::deserialize(deserializer)?;
                Self::from_uuid(id).map_err(D::Error::custom)
            }
        }
    };
}

uuid_id!(RootId, "Host-local identity for one registered root.");
uuid_id!(
    AppId,
    "Trusted product identity for one local app acting on its folder grant."
);
uuid_id!(
    GrantId,
    "Stable identity for one consented capability grant."
);
uuid_id!(
    OperationId,
    "Stable identity for an idempotent broker operation."
);
uuid_id!(
    RequestId,
    "Correlation identity for one broker request and response."
);

/// Product object to which standing host consent belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    /// Consent shared at project scope.
    Project,
    /// Consent belonging to a standalone or conversation-specific context.
    Conversation,
}

/// Exact subject named by a standing grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct GrantSubject {
    kind: SubjectKind,
    id: Uuid,
}

impl GrantSubject {
    /// Build a project grant subject.
    pub fn project(id: Uuid) -> Result<Self, IdError> {
        Self::new(SubjectKind::Project, id)
    }

    /// Build a conversation grant subject.
    pub fn conversation(id: Uuid) -> Result<Self, IdError> {
        Self::new(SubjectKind::Conversation, id)
    }

    fn new(kind: SubjectKind, id: Uuid) -> Result<Self, IdError> {
        if id.is_nil() {
            Err(IdError::Nil)
        } else {
            Ok(Self { kind, id })
        }
    }

    /// Semantic kind of this product subject.
    pub const fn kind(self) -> SubjectKind {
        self.kind
    }

    /// Product UUID without its semantic kind.
    pub const fn id(self) -> Uuid {
        self.id
    }
}

impl<'de> Deserialize<'de> for GrantSubject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireSubject {
            kind: SubjectKind,
            id: Uuid,
        }

        let wire = WireSubject::deserialize(deserializer)?;
        Self::new(wire.kind, wire.id).map_err(D::Error::custom)
    }
}

/// Trusted conversation identity supplied to one broker operation.
///
/// Project consent and conversation attachment are separate facts. A project
/// chat therefore carries both IDs; a standalone chat carries only its
/// conversation ID. The agent never chooses either value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct ExecutionContext {
    conversation_id: Uuid,
    project_id: Option<Uuid>,
}

impl ExecutionContext {
    /// Context for a conversation inside a project.
    pub fn project_chat(conversation_id: Uuid, project_id: Uuid) -> Result<Self, IdError> {
        if conversation_id.is_nil() || project_id.is_nil() {
            return Err(IdError::Nil);
        }
        Ok(Self {
            conversation_id,
            project_id: Some(project_id),
        })
    }

    /// Context for a standalone conversation.
    pub fn standalone(conversation_id: Uuid) -> Result<Self, IdError> {
        if conversation_id.is_nil() {
            return Err(IdError::Nil);
        }
        Ok(Self {
            conversation_id,
            project_id: None,
        })
    }

    /// Exact conversation whose attached-root set must cover the operation.
    pub const fn conversation_id(self) -> Uuid {
        self.conversation_id
    }

    /// Optional project whose standing grants may authorize the conversation.
    pub const fn project_id(self) -> Option<Uuid> {
        self.project_id
    }

    pub(crate) fn grant_subject_matches(self, subject: GrantSubject) -> bool {
        match subject.kind() {
            SubjectKind::Project => self.project_id == Some(subject.id()),
            SubjectKind::Conversation => self.conversation_id == subject.id(),
        }
    }
}

impl<'de> Deserialize<'de> for ExecutionContext {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireContext {
            conversation_id: Uuid,
            project_id: Option<Uuid>,
        }

        let wire = WireContext::deserialize(deserializer)?;
        match wire.project_id {
            Some(project_id) => Self::project_chat(wire.conversation_id, project_id),
            None => Self::standalone(wire.conversation_id),
        }
        .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_kind_is_part_of_its_identity() {
        let id = Uuid::new_v4();
        assert_ne!(
            GrantSubject::project(id).unwrap(),
            GrantSubject::conversation(id).unwrap()
        );
    }

    #[test]
    fn opaque_ids_roundtrip_and_reject_nil() {
        let root = RootId::new();
        let encoded = serde_json::to_string(&root).unwrap();
        assert_eq!(serde_json::from_str::<RootId>(&encoded).unwrap(), root);
        assert_eq!(root.to_string().parse::<RootId>().unwrap(), root);
        assert!(serde_json::from_str::<RootId>(&format!("\"{}\"", Uuid::nil())).is_err());
    }

    #[test]
    fn execution_context_rejects_nil_ids_during_construction_and_serde() {
        assert_eq!(ExecutionContext::standalone(Uuid::nil()), Err(IdError::Nil));
        let encoded = format!(
            r#"{{"conversation_id":"{}","project_id":null}}"#,
            Uuid::nil()
        );
        assert!(serde_json::from_str::<ExecutionContext>(&encoded).is_err());
    }
}

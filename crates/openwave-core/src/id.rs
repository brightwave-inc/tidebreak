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
}

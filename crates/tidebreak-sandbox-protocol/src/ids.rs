//! Opaque identities carried across the sandbox-agent boundary.
//!
//! The UUID newtypes mirror `tidebreak-host-broker`'s `id` module deliberately:
//! nil is a sentinel and never a valid persisted identity, and every identity
//! round-trips through serde as a bare string. The event stream's sequence and
//! cursor are ordinal `u64`s rather than opaque UUIDs, because their ordering
//! *is* their contract.

use std::{fmt, str::FromStr};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// An identity received over the wire violated a protocol invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdError {
    /// Nil is a sentinel, not a valid sandbox-protocol identity.
    #[error("sandbox-protocol identities must not be nil")]
    Nil,
}

/// Text parsing failure for an opaque identity.
#[derive(Debug, Error)]
pub enum ParseIdError {
    /// The value was not a UUID.
    #[error(transparent)]
    InvalidUuid(#[from] uuid::Error),
    /// Nil is syntactically a UUID but not a valid identity.
    #[error(transparent)]
    InvalidIdentity(#[from] IdError),
}

macro_rules! uuid_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Allocate a fresh random identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Validate an identity received from trusted state.
            pub fn from_uuid(id: Uuid) -> Result<Self, IdError> {
                if id.is_nil() {
                    Err(IdError::Nil)
                } else {
                    Ok(Self(id))
                }
            }

            /// Return the underlying UUID.
            #[must_use]
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

uuid_id!(
    RunId,
    "Durable identity of one sandbox-resident agent run.\n\nThe reverse-RPC operation log, event cursor, and grant provenance are all\nscoped to this run and outlive any single connection to the sandbox."
);
uuid_id!(
    OperationId,
    "Durable per-run identity for one idempotent reverse operation.\n\nStable across reconnects: a request re-issued after a disconnect carries the\nsame `OperationId` and is answered from the recorded outcome, never executed\ntwice."
);
uuid_id!(
    RequestId,
    "Per-attempt correlation identity for one request/response pair.\n\nA re-issue of the same operation over a fresh connection uses a new\n`RequestId` but the same `OperationId`."
);
uuid_id!(
    SandboxTag,
    "Host-minted correlation tag stamped into a provisioned sandbox's metadata.\n\nThe host commits this tag on the run before asking the backend for a sandbox\nand uses it to reclaim an orphan by listing the backend's sandboxes by tag."
);

/// Monotonic sequence number the sandbox stamps onto each event it emits.
///
/// Sequence numbers start at one, increase by one per event, and never repeat
/// within a run. Their ordering is the event stream's contract: the host
/// commits a batch, advances its cursor to the highest sequence it committed,
/// and discards any re-delivered sequence at or below that cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sequence(u64);

impl Sequence {
    /// The first sequence number an event may carry.
    pub const FIRST: Sequence = Sequence(1);

    /// Wrap a raw sequence value. Zero is reserved for [`EventCursor::START`].
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw ordinal.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next sequence number after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// The host's committed position in a run's event stream.
///
/// A cursor of [`EventCursor::START`] (zero) requests the stream from its
/// beginning. On reattachment the host resumes from its last committed cursor,
/// so the sandbox replays only events strictly newer than it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventCursor(u64);

impl EventCursor {
    /// Resume from the very start of the stream.
    pub const START: EventCursor = EventCursor(0);

    /// A cursor that has committed up to and including `sequence`.
    #[must_use]
    pub const fn committed(sequence: Sequence) -> Self {
        Self(sequence.0)
    }

    /// The raw committed ordinal.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Whether `sequence` is newer than this cursor and so must be delivered.
    #[must_use]
    pub const fn precedes(self, sequence: Sequence) -> bool {
        self.0 < sequence.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_roundtrip_and_reject_nil() {
        let run = RunId::new();
        let encoded = serde_json::to_string(&run).unwrap();
        assert_eq!(serde_json::from_str::<RunId>(&encoded).unwrap(), run);
        assert_eq!(run.to_string().parse::<RunId>().unwrap(), run);
        assert!(serde_json::from_str::<RunId>(&format!("\"{}\"", Uuid::nil())).is_err());
        assert!(serde_json::from_str::<OperationId>(&format!("\"{}\"", Uuid::nil())).is_err());
    }

    #[test]
    fn cursor_delivers_only_newer_sequences() {
        let cursor = EventCursor::committed(Sequence::new(4));
        assert!(!cursor.precedes(Sequence::new(4)));
        assert!(cursor.precedes(Sequence::new(5)));
        assert_eq!(Sequence::FIRST.next(), Sequence::new(2));
    }
}

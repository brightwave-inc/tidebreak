//! The chat event socket's wire contract, as a Rust client reads it.
//!
//! The desktop renderer decodes the server's JSON through TypeScript generated
//! from these same definitions (see `docs/wire-types.md`). The CLI used to keep
//! a third, hand-written mirror of the frame shapes, and nothing tied that
//! mirror to the server: a field renamed here compiled, shipped, and failed at
//! runtime. This module is the one place a Rust client imports the socket's
//! types from, so a rename is a compile error in the CLI the way it is a type
//! error in the renderer.
//!
//! # Strictness
//!
//! A client that decodes through these types gets the renderer's contract, not
//! a looser one:
//!
//! - Every vocabulary is closed. Tool names, approval kinds, tool statuses,
//!   failure categories, and grant rungs are the server's own enums, so a
//!   value outside them fails to decode rather than folding to a string.
//! - Every frame rejects unknown keys (`deny_unknown_fields`), matching the
//!   renderer's `onlyKeys` guards. A field the server does not declare is not
//!   part of the contract, and a client that silently dropped it could not
//!   tell a newer server from a malformed frame.
//! - An event type the client does not know fails the frame. The client drops
//!   that frame and does not advance its cursor past it, so a reconnect replays
//!   it and drops it again. This is the one place a version-skewed client (a
//!   CLI attached to a newer desktop) loses information, and it is deliberate:
//!   the renderer ships with its server and never sees a newer event, so a
//!   tolerant client would be the only surface with a different contract.
//!
//! # Limits
//!
//! [`limits`] holds the guard sizes the renderer applies to opaque strings on
//! this surface. They are generated into `wire.ts` from here, so the two
//! clients cannot disagree about how long an id, a timestamp, or a cursor may
//! be.

pub use crate::event_projection::{
    RendererAgentEvent, RendererChatFrame, RendererChatMetadata, RendererModelIdentity,
    RendererRefusal, RendererSequencedEvent, RendererToolFailure, RendererToolFailureCode,
    RendererToolFailureReason, RendererToolStatus, RendererTurnUsage, TurnFailureCategory,
};
pub use crate::providers::ProviderKind;
pub use crate::routes::{
    AgentActivityHistoryItem, AgentActivityKind, AgentActivityOutcome, ApprovalGrantRung,
};

/// Guard sizes for the opaque strings a client draws from this surface.
///
/// A renderer validates what it is about to draw rather than trusting that the
/// sender already clamped it, and a CLI bounds the same fields before it prints
/// them. Both read these numbers: the renderer through the constants generated
/// into `wire.ts`, the CLI directly.
pub mod limits {
    /// Longest opaque identifier a client accepts (call, turn, chat, run, and
    /// workspace ids). Every id the server sends is a UUID, so the ceiling is
    /// well above what a valid payload ever needs.
    pub const MAX_WIRE_ID_CHARS: usize = 128;

    /// Longest timestamp string a client accepts. RFC 3339 needs about 35.
    pub const MAX_WIRE_TIMESTAMP_CHARS: usize = 64;

    /// Longest opaque pagination cursor a client accepts.
    pub const MAX_WIRE_CURSOR_CHARS: usize = 256;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_name<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_value(value)
            .expect("a wire enum serializes")
            .as_str()
            .expect("a unit variant serializes as a string")
            .to_owned()
    }

    /// `as_str` exists so a client can print a category without a serde round
    /// trip; this is what keeps it from drifting from the wire spelling.
    #[test]
    fn turn_failure_category_names_match_the_wire() {
        for category in [
            TurnFailureCategory::RateLimited,
            TurnFailureCategory::Auth,
            TurnFailureCategory::ProviderAccess,
            TurnFailureCategory::Transient,
            TurnFailureCategory::Unknown,
        ] {
            assert_eq!(category.as_str(), wire_name(&category));
        }
    }

    #[test]
    fn activity_names_match_the_wire() {
        for kind in [
            AgentActivityKind::Exec,
            AgentActivityKind::WebSearch,
            AgentActivityKind::UpdateTaskPlan,
            AgentActivityKind::ReadDelegatedFile,
            AgentActivityKind::ListConnectedFolders,
            AgentActivityKind::ListFolder,
            AgentActivityKind::ReadConnectedFile,
            AgentActivityKind::ImportConnectedFile,
        ] {
            assert_eq!(kind.as_str(), wire_name(&kind));
        }
        for outcome in [
            AgentActivityOutcome::Waiting,
            AgentActivityOutcome::Running,
            AgentActivityOutcome::Completed,
            AgentActivityOutcome::Failed,
            AgentActivityOutcome::Cancelled,
        ] {
            assert_eq!(outcome.as_str(), wire_name(&outcome));
        }
    }

    /// The limits bound what the server actually sends. A limit below a real
    /// value would make every client reject valid payloads.
    #[test]
    fn limits_admit_what_the_server_sends() {
        let id = serde_json::to_value(tidebreak_core::TurnId(uuid::Uuid::from_u128(1)))
            .expect("an id serializes");
        assert!(id.as_str().expect("ids are strings").chars().count() <= limits::MAX_WIRE_ID_CHARS);

        let timestamp = serde_json::to_value(chrono::DateTime::<chrono::Utc>::from_timestamp(
            1_756_700_000,
            123_456_789,
        ))
        .expect("a timestamp serializes");
        assert!(
            timestamp
                .as_str()
                .expect("timestamps are strings")
                .chars()
                .count()
                <= limits::MAX_WIRE_TIMESTAMP_CHARS
        );
    }

    /// Unknown keys fail the frame, at every level a client decodes.
    #[test]
    fn frames_reject_unknown_keys() {
        let event = r#"{"seq":1,"event":{"type":"text_delta","text":"hi"}}"#;
        assert!(serde_json::from_str::<RendererChatFrame>(event).is_ok());
        for malformed in [
            r#"{"seq":1,"event":{"type":"text_delta","text":"hi"},"extra":1}"#,
            r#"{"seq":1,"event":{"type":"text_delta","text":"hi","extra":1}}"#,
            r#"{"metadata":"titled","title":"A chat","extra":1}"#,
            r#"{"seq":1,"event":{"type":"turn_completed","usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"extra":1}}}"#,
        ] {
            assert!(
                serde_json::from_str::<RendererChatFrame>(malformed).is_err(),
                "should reject: {malformed}"
            );
        }
    }

    /// An event type this build does not know fails the frame rather than
    /// folding to something a client would misread.
    #[test]
    fn unknown_event_types_fail_the_frame() {
        let unknown = r#"{"seq":9,"event":{"type":"some_future_event"}}"#;
        assert!(serde_json::from_str::<RendererChatFrame>(unknown).is_err());
        let unknown_status = r#"{"seq":9,"event":{"type":"tool_call_completed","call_id":"00000000-0000-0000-0000-000000000003","status":"cancelled"}}"#;
        assert!(serde_json::from_str::<RendererChatFrame>(unknown_status).is_err());
    }
}

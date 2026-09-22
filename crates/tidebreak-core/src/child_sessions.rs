//! The `code_wait` contract: what a parent conversation waits on, and how a
//! wait that outlives its bound becomes a durable park.
//!
//! A parent session starts child sessions and reads their results with
//! `code_wait`. The tool answers inline while the children settle quickly.
//! When they do not, the same call becomes a durable park: the turn stops,
//! the engine releases its lease, and the call resumes once every named child
//! has settled. A server restart changes nothing, because the park names the
//! children and each child's own state is durable.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SessionId;

/// Stable native tool name for the parent's child-session wait.
pub const CODE_WAIT_TOOL: &str = "code_wait";

/// How long `code_wait` answers inline before it parks.
pub const CODE_WAIT_INLINE_BOUND_SECONDS: u64 = 20;

/// Most children one call may name.
pub const MAX_CODE_WAIT_CHILDREN: usize = 8;

/// The model-facing arguments of one `code_wait` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWaitArgs {
    /// Child sessions to read, in the order the parent asked for them.
    pub session_ids: Vec<SessionId>,
}

/// Parse and bound one `code_wait` argument value.
///
/// Returns `None` for anything the tool would refuse anyway: a missing or
/// mistyped list, an empty one, one past [`MAX_CODE_WAIT_CHILDREN`], or one
/// that names the same child twice. A park is durable, so its identity is
/// validated before it is written rather than after it is read.
#[must_use]
pub fn code_wait_session_ids(arguments: &Value) -> Option<Vec<SessionId>> {
    let parsed = serde_json::from_value::<CodeWaitArgs>(arguments.clone()).ok()?;
    if parsed.session_ids.is_empty() || parsed.session_ids.len() > MAX_CODE_WAIT_CHILDREN {
        return None;
    }
    let mut seen = std::collections::HashSet::with_capacity(parsed.session_ids.len());
    for id in &parsed.session_ids {
        if id.0.is_nil() || !seen.insert(*id) {
            return None;
        }
    }
    Some(parsed.session_ids)
}

/// Whether one `code_wait` result still has children running.
///
/// The tool answers with `{"waiting": …, "sessions": […]}`. `waiting` is true
/// only while a named child is unsettled, which is exactly when the call has
/// to park instead of returning. Anything unparseable counts as settled: a
/// malformed result is a failure the model should read, not a reason to park
/// a turn on children nobody can name.
#[must_use]
pub fn code_wait_result_is_waiting(content: &str) -> bool {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|value| value.get("waiting").and_then(Value::as_bool))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_arguments_are_bounded_and_distinct() {
        let first = SessionId::new();
        let second = SessionId::new();
        assert_eq!(
            code_wait_session_ids(&serde_json::json!({"session_ids": [first, second]})),
            Some(vec![first, second])
        );
        assert_eq!(
            code_wait_session_ids(&serde_json::json!({"session_ids": [first, first]})),
            None
        );
        assert_eq!(
            code_wait_session_ids(&serde_json::json!({"session_ids": []})),
            None
        );
        assert_eq!(code_wait_session_ids(&serde_json::json!({})), None);
        let overlong = (0..=MAX_CODE_WAIT_CHILDREN)
            .map(|_| SessionId::new())
            .collect::<Vec<_>>();
        assert_eq!(
            code_wait_session_ids(&serde_json::json!({"session_ids": overlong})),
            None
        );
    }

    #[test]
    fn only_a_true_waiting_flag_parks() {
        assert!(code_wait_result_is_waiting(
            r#"{"waiting":true,"sessions":[]}"#
        ));
        assert!(!code_wait_result_is_waiting(
            r#"{"waiting":false,"sessions":[]}"#
        ));
        assert!(!code_wait_result_is_waiting("not json"));
        assert!(!code_wait_result_is_waiting(r#"{"sessions":[]}"#));
    }
}

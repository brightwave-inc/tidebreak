//! Native permission requests share the durable managed human-decision transport.

#[cfg(unix)]
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use tidebreak_harness::ApprovalDecision;

/// Only an explicit, recorded approval releases the native tool once.
#[cfg(unix)]
pub async fn request(socket: &Path, raw: Value) -> ApprovalDecision {
    let request = tidebreak_core::code::SupervisorToolRequest {
        request_id: format!("approval-{}", uuid::Uuid::new_v4()),
        tool: "request_tool_approval".into(),
        arguments: json!({"raw":raw}),
        turn: None,
        cancelled: false,
    };
    match crate::tool_bridge::call(socket, &request).await {
        Ok(result) => decision_from_output(&result["output"]),
        Err(error) => ApprovalDecision::Deny {
            feedback: Some(error),
        },
    }
}

#[cfg(not(unix))]
pub async fn request(_socket: &Path, _raw: Value) -> ApprovalDecision {
    ApprovalDecision::Deny {
        feedback: Some("managed approvals require a Unix socket".into()),
    }
}

pub fn decision_from_output(output: &Value) -> ApprovalDecision {
    if output["is_error"] == false && output["data"]["decision"] == "approved" {
        ApprovalDecision::Approve
    } else {
        ApprovalDecision::Deny {
            feedback: Some(
                output["data"]["feedback"]
                    .as_str()
                    .or_else(|| output["content"].as_str())
                    .unwrap_or("the native tool was not approved")
                    .to_owned(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_malformed_or_failed_results_never_approve() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"data":{"decision":"approved"}}),
            serde_json::json!({"is_error":true,"data":{"decision":"approved"}}),
            serde_json::json!({"is_error":false,"data":{"decision":"accepted"}}),
            serde_json::json!({"is_error":false,"data":{"decision":"rejected"}}),
        ] {
            assert!(matches!(
                decision_from_output(&value),
                ApprovalDecision::Deny { .. }
            ));
        }
        assert_eq!(
            decision_from_output(
                &serde_json::json!({"is_error":false,"data":{"decision":"approved"}})
            ),
            ApprovalDecision::Approve
        );
    }
}

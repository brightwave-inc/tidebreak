use super::*;

const APPROVE: &str =
    include_str!("../../../../fixtures/codex/0.153.0/mcp-approval-approve.ndjson");
const DENY: &str = include_str!("../../../../fixtures/codex/0.153.0/mcp-approval-deny.ndjson");

fn frames(capture: &str) -> Vec<Value> {
    capture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
        .collect()
}

fn start_and_request() -> (CodexStreamParser, Value) {
    let mut parser = CodexStreamParser::new();
    let capture = frames(APPROVE);
    for frame in &capture[..2] {
        parser.push_line(&frame.to_string());
    }
    (parser, capture[2].clone())
}

#[test]
fn captured_codex_mcp_approval_roundtrips() {
    for (capture, expected) in [
        (APPROVE, ApprovalDecision::Approve),
        (DENY, ApprovalDecision::Deny { feedback: None }),
    ] {
        let outcome = CodexStreamParser::parse_ndjson(capture);
        assert_eq!(outcome.unrecognized, 0);
        assert_eq!(
            outcome
                .events
                .iter()
                .filter(|event| matches!(event, HarnessEvent::ApprovalRequested { .. }))
                .count(),
            1
        );
        assert!(outcome.events.iter().any(|event| matches!(event, HarnessEvent::ApprovalResolved { harness_ref, decision } if harness_ref.call_id == "call_fixture" && decision == &expected)));
        assert!(matches!(
            outcome.events.last(),
            Some(HarnessEvent::TurnCompleted { .. })
        ));
    }
}

#[test]
fn captured_mcp_replies_match_pinned_wire_responses() {
    for (capture, decision) in [
        (APPROVE, ApprovalDecision::Approve),
        (DENY, ApprovalDecision::Deny { feedback: None }),
    ] {
        let mut parser = CodexStreamParser::new();
        let capture = frames(capture);
        for frame in &capture[..3] {
            parser.push_line(&frame.to_string());
        }
        let pending = parser
            .pending_approval("call_fixture")
            .expect("recognized MCP approval");
        assert_eq!(pending.response(&decision).unwrap(), capture[3]);
    }
    let command = PendingApproval::command(json!(42));
    assert_eq!(
        command.response(&ApprovalDecision::Approve).unwrap(),
        json!({"id":42,"result":{"decision":"accept"}})
    );
    assert_eq!(
        command
            .response(&ApprovalDecision::Deny { feedback: None })
            .unwrap(),
        json!({"id":42,"result":{"decision":"decline"}})
    );
}

#[test]
fn arbitrary_or_mismatched_elicitation_never_becomes_tool_consent() {
    let changes = [
        ("/params/mode", json!("url")),
        ("/params/mode", json!("openai/form")),
        ("/params/mode", json!("openaiForm")),
        ("/params/_meta", Value::Null),
        ("/params/_meta/codex_approval_kind", json!("generic_form")),
        (
            "/params/requestedSchema",
            json!({"type":"object","properties":{"secret":{"type":"string"}}}),
        ),
        ("/params/serverName", json!("other-server")),
        ("/params/threadId", json!("other-thread")),
        ("/params/turnId", json!("other-turn")),
        ("/params/turnId", Value::Null),
        (
            "/params/_meta/tool_params",
            json!({"different":"arguments"}),
        ),
        (
            "/params/message",
            json!("Allow the tb-browser MCP server to run tool \"browser_click\"?"),
        ),
        ("/id", Value::Null),
    ];
    for (pointer, value) in changes {
        let (mut parser, mut request) = start_and_request();
        *request.pointer_mut(pointer).unwrap() = value;
        let events = parser.push_line(&request.to_string());
        assert!(
            matches!(
                events.as_slice(),
                [HarnessEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    ..
                }]
            ),
            "{pointer}: {events:?}"
        );
        assert!(parser.pending_approvals.is_empty(), "{pointer}");
        assert_eq!(
            parser.take_rejected_elicitations(),
            vec![
                json!({"id":request["id"],"result":{"action":"decline","content":null,"_meta":null}})
            ]
        );
    }
}

#[test]
fn missing_completed_ambiguous_and_stale_items_cannot_authorize_elicitation() {
    for mode in [
        "missing",
        "completed",
        "ambiguous",
        "turn-completed",
        "new-turn",
    ] {
        let (mut parser, request) = start_and_request();
        let capture = frames(APPROVE);
        match mode {
            "missing" => {
                parser.active_mcp_calls.clear();
            }
            "completed" => {
                parser.push_line(&capture[5].to_string());
            }
            "ambiguous" => {
                let mut duplicate = capture[1].clone();
                duplicate["params"]["item"]["id"] = json!("other-call");
                parser.push_line(&duplicate.to_string());
            }
            "turn-completed" => {
                parser.push_line(&capture[6].to_string());
            }
            "new-turn" => {
                let mut next = capture[0].clone();
                next["params"]["turn"]["id"] = json!("new-turn");
                parser.push_line(&next.to_string());
            }
            _ => unreachable!(),
        }
        let events = parser.push_line(&request.to_string());
        assert!(
            matches!(events.as_slice(), [HarnessEvent::HarnessNotice { .. }]),
            "{mode}: {events:?}"
        );
        assert!(parser.pending_approvals.is_empty());
        assert_eq!(parser.take_rejected_elicitations().len(), 1);
    }
}

#[test]
fn mcp_arguments_named_command_keep_mcp_classification() {
    let mut parser = CodexStreamParser::new();
    let mut capture = frames(APPROVE);
    capture[1]["params"]["item"]["arguments"] = json!({"command":"private command argument"});
    capture[2]["params"]["_meta"]["tool_params"] =
        capture[1]["params"]["item"]["arguments"].clone();
    parser.push_line(&capture[0].to_string());
    parser.push_line(&capture[1].to_string());
    let events = parser.push_line(&capture[2].to_string());
    assert!(
        matches!(events.as_slice(), [HarnessEvent::ApprovalRequested { raw, kind: Some(tidebreak_core::ApprovalKind::Other { summary }), .. }]
        if summary == "mcp__tb-browser__browser_screenshot" && raw["input"]["command"] == "private command argument" && raw["_meta"] == capture[2]["params"]["_meta"])
    );
}

#[test]
fn replay_correlates_rpc_id_instead_of_first_parked_approval() {
    let (mut parser, mut request) = start_and_request();
    request["id"] = json!("mcp-request");
    parser.push_line(&request.to_string());
    parser.push_line(&json!({"id":9,"method":"item/commandExecution/requestApproval","params":{"itemId":"command-call"}}).to_string());
    let events = parser
        .push_line(&json!({"dir":"out","msg":{"id":9,"result":{"decision":"accept"}}}).to_string());
    assert!(
        matches!(events.as_slice(), [HarnessEvent::ApprovalResolved { harness_ref, .. }] if harness_ref.call_id == "command-call")
    );
    assert!(parser.pending_approval("call_fixture").is_some());
    let events = parser.push_line(&json!({"dir":"out","msg":{"id":"mcp-request","result":{"action":"decline","content":null,"_meta":null}}}).to_string());
    assert!(
        matches!(events.as_slice(), [HarnessEvent::ApprovalResolved { harness_ref, decision: ApprovalDecision::Deny { .. } }] if harness_ref.call_id == "call_fixture")
    );
    assert!(parser.pending_approvals.is_empty());
}

#[test]
fn resolved_or_finished_requests_cannot_be_decided_again() {
    for index in [4, 5, 6] {
        let (mut parser, request) = start_and_request();
        parser.push_line(&request.to_string());
        parser.push_line(&frames(APPROVE)[index].to_string());
        assert!(
            parser.pending_approval("call_fixture").is_none(),
            "frame {index}"
        );
    }
}

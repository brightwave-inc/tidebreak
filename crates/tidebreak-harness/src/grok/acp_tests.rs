use super::*;
use std::sync::Arc;

fn frames(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/grok/1.0.13")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn captured_permission() -> (Control, Value) {
    let mut state = Control {
        session_id: Some("fixture-session".into()),
        active: true,
        ..Control::default()
    };
    for frame in frames("acp-approve.ndjson") {
        let value = &frame["value"];
        match value.get("method").and_then(Value::as_str) {
            Some("session/request_permission") => return (state, value.clone()),
            Some("session/update") => {
                let update = &value["params"]["update"];
                if let Some(id) = update.get("toolCallId").and_then(Value::as_str) {
                    if update.get("rawInput").is_some() {
                        state.tools.insert(id.into(), update.clone());
                    }
                }
            }
            _ => {}
        }
    }
    panic!("captured permission missing");
}

#[test]
fn acp_uses_captured_one_use_choices_and_exact_tool_binding() {
    let (mut state, value) = captured_permission();
    let permission = parse_permission(&value, &state).unwrap();
    assert_eq!(permission.allow_once, "allow-once");
    assert_eq!(permission.reject_once, "reject-once");
    assert_eq!(
        permission_reply(&permission.rpc_id, Some(&permission.allow_once))["result"]["outcome"],
        json!({"outcome":"selected","optionId":"allow-once"})
    );
    let mut wrong = value.clone();
    wrong["params"]["sessionId"] = json!("other-session");
    assert!(parse_permission(&wrong, &state).is_none());
    wrong = value.clone();
    wrong["params"]["toolCall"]["_meta"]["x.ai/tool"]["input"]["command"] =
        json!("different command");
    assert!(parse_permission(&wrong, &state).is_none());
    wrong = value.clone();
    wrong["params"]["options"][1]["kind"] = json!("allow_always");
    assert!(parse_permission(&wrong, &state).is_none());
    state.stopped = true;
    assert!(parse_permission(&value, &state).is_none());
}

#[test]
fn captured_acp_updates_keep_command_results_and_terminal_outcomes() {
    for (fixture, interrupted) in [
        ("acp-approve.ndjson", false),
        ("acp-deny.ndjson", true),
        ("acp-cancel.ndjson", true),
    ] {
        let mut parser = GrokStreamParser::new();
        let mut events = Vec::new();
        for frame in frames(fixture) {
            if frame["direction"] != "server" {
                continue;
            }
            let value = &frame["value"];
            if value["method"] == "session/update" {
                if let Some(mapped) = map_update(&value["params"]["update"]) {
                    events.extend(parser.push_line(&mapped.to_string()));
                }
            } else if value["id"] == 3 && value.get("result").is_some() {
                events.extend(parser.push_line(&json!({"type":"end","sessionId":"fixture-session","stopReason":value["result"]["stopReason"]}).to_string()));
            }
        }
        assert!(events.iter().any(
            |e| matches!(e,HarnessEvent::ToolStarted {name,..} if name == "run_terminal_command")
        ));
        assert_eq!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::TurnInterrupted)),
            interrupted
        );
        assert_eq!(
            events
                .iter()
                .any(|e| matches!(e, HarnessEvent::TurnCompleted { .. })),
            !interrupted
        );
    }
}

#[test]
fn approval_capabilities_are_only_enabled_for_the_captured_pin() {
    for version in ["1.0.13", "grok 1.0.13 (5e9a58528b76)"] {
        assert!(supports_version(version));
        assert!(refuse_versioned_mode(PermissionMode::Ask, version).is_ok());
        assert!(refuse_versioned_mode(PermissionMode::Plan, version).is_err());
    }
    for version in ["1.0.4", "1.0.14", "unknown", "1.0.130"] {
        assert!(!supports_version(version));
        assert!(refuse_versioned_mode(PermissionMode::Ask, version).is_err());
    }
}

#[test]
fn acp_usage_keeps_last_prompt_separate_from_cumulative_spend() {
    let usage = json!({"inputTokens":900,"outputTokens":44,"cachedReadTokens":200,"cacheCreationTokens":100});
    let mut parser = GrokStreamParser::new();
    parser.set_last_call_context_tokens(450);
    let events = parser.push_line(&json!({"type":"end","stopReason":"end_turn","sessionId":"fixture-session","usage":usage_fields(&usage)}).to_string());
    let report = events
        .iter()
        .find_map(|event| match event {
            HarnessEvent::TurnCompleted { usage } => Some(usage),
            _ => None,
        })
        .unwrap();
    assert_eq!(report.input_tokens, 600);
    assert_eq!(report.cache_read_input_tokens, 200);
    assert_eq!(report.cache_creation_input_tokens, 100);
    assert_eq!(report.context_tokens, 450);
    assert_eq!(report.first_call_context_tokens, None);
}

#[derive(Default)]
struct Sink {
    events: Mutex<Vec<HarnessEvent>>,
}
#[async_trait]
impl crate::HarnessEventSink for Sink {
    async fn emit(&self, event: HarnessEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[cfg(unix)]
const SERVER: &str = r#"#!/usr/bin/python3
import json,os,signal,sys
frames=[json.loads(line)['value'] for line in open(os.environ['ACP_FIXTURE'])]
permission=next(v for v in frames if v.get('method')=='session/request_permission')
updates=[v for v in frames if v.get('method')=='session/update']
pre=[v for v in updates if v['params']['update'].get('status') not in ('in_progress','completed') and v['params']['update']['sessionUpdate']!='agent_message_chunk']
post=[v for v in updates if v not in pre]
def emit(v):print(json.dumps(v),flush=True)
def record(v):
 with open(os.environ['ACP_RECORD'],'a') as f:f.write(json.dumps(v)+'\n')
for line in sys.stdin:
 v=json.loads(line);record(v);method=v.get('method')
 stall=os.environ['ACP_RECORD']+'.stall'
 if os.path.exists(stall) and open(stall).read()==method:continue
 if method=='initialize' and os.path.exists(os.environ['ACP_RECORD']+'.blockwrite'):
  emit({'jsonrpc':'2.0','id':'x'*131072,'method':'client/stall'})
  open(os.environ['ACP_RECORD']+'.waiting','w').close()
  while True:signal.pause()
 if method=='initialize':emit({'jsonrpc':'2.0','id':v['id'],'result':{'protocolVersion':1}})
 elif method in ('session/new','session/load'):
  if method=='session/load':emit({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'fixture-session','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'OLD_REPLAY'}}}})
  emit({'jsonrpc':'2.0','id':v['id'],'result':{'sessionId':'fixture-session'}})
 elif method=='session/set_model':emit({'jsonrpc':'2.0','id':v['id'],'result':{'_meta':{'model':{'Ok':v['params']['modelId']}}}})
 elif method=='session/set_mode':emit({'jsonrpc':'2.0','id':v['id'],'result':{}})
 elif method=='session/prompt':
  for frame in pre:emit(frame)
  emit(permission)
  if os.environ.get('ACP_DUPLICATE')=='1':emit(permission)
 elif method=='session/cancel':emit({'jsonrpc':'2.0','id':3,'result':{'stopReason':'cancelled'}})
 elif v.get('id')==0 and 'result' in v:
  outcome=v['result']['outcome'];accepted=outcome.get('optionId')=='allow-once'
  if accepted:
   open(os.environ['ACP_EXECUTED'],'w').write('approved')
   for frame in post:emit(frame)
  emit({'jsonrpc':'2.0','id':3,'result':{'stopReason':'end_turn' if accepted else 'cancelled'}})
"#;

#[cfg(unix)]
fn session(dir: &Path, mode: PermissionMode, duplicate: bool) -> (Arc<GrokSession>, Arc<Sink>) {
    use std::os::unix::fs::PermissionsExt;
    let binary = dir.join("grok");
    std::fs::write(&binary, SERVER).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sink = Arc::new(Sink::default());
    let mut extra_env = vec![
        (
            "ACP_FIXTURE".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/grok/1.0.13/acp-approve.ndjson")
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "ACP_RECORD".into(),
            dir.join("record").to_string_lossy().into_owned(),
        ),
        (
            "ACP_EXECUTED".into(),
            dir.join("executed").to_string_lossy().into_owned(),
        ),
    ];
    if duplicate {
        extra_env.push(("ACP_DUPLICATE".into(), "1".into()));
    }
    let spec = SessionSpec {
        owner: tidebreak_core::OwnerId::local(),
        session_id: tidebreak_core::SessionId::new(),
        worktree: dir.into(),
        allowed_read_roots: Vec::new(),
        permission_mode: mode,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        resume_ref: None,
        extra_argv: Vec::new(),
        extra_env,
        relay_key_env: None,
        env: Vec::new(),
        approval: None,
        binary: Some(binary),
        sink: sink.clone(),
        browser: None,
        native: None,
    };
    (Arc::new(GrokSession::new(spec, "1.0.13".into())), sink)
}

fn turn() -> TurnInput {
    TurnInput {
        turn_id: None,
        text: "Capture fixture".into(),
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        images: Vec::new(),
    }
}

async fn wait_approval(sink: &Sink) -> HarnessApprovalRef {
    timeout(Duration::from_secs(3), async {
        loop {
            let approval = sink
                .events
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find_map(|e| match e {
                    HarnessEvent::ApprovalRequested { harness_ref, .. } => {
                        Some(harness_ref.clone())
                    }
                    _ => None,
                });
            if let Some(approval) = approval {
                return approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ACP request never reached Tidebreak approvals")
}

#[cfg(unix)]
#[tokio::test]
async fn normal_acp_turns_approve_deny_and_resume_without_replaying_history() {
    for decision in [
        ApprovalDecision::Approve,
        ApprovalDecision::Deny { feedback: None },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (session, sink) = session(dir.path(), PermissionMode::Auto, false);
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(turn()).await }
        });
        let approval = wait_approval(&sink).await;
        assert!(!dir.path().join("executed").exists());
        session
            .decide(approval.clone(), decision.clone())
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            TurnOutcome::Clean
        );
        assert_eq!(
            dir.path().join("executed").exists(),
            matches!(decision, ApprovalDecision::Approve)
        );
        assert!(matches!(
            session
                .decide(approval.clone(), ApprovalDecision::Approve)
                .await,
            Err(HarnessError::ApprovalWaiterMissing(_))
        ));
        assert!(sink
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, HarnessEvent::ApprovalResolved { .. })));
        sink.events.lock().unwrap().clear();
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(turn()).await }
        });
        let next = wait_approval(&sink).await;
        assert_ne!(approval.call_id, next.call_id);
        assert!(matches!(
            session.decide(approval, ApprovalDecision::Approve).await,
            Err(HarnessError::ApprovalWaiterMissing(_))
        ));
        session
            .decide(next, ApprovalDecision::Deny { feedback: None })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            TurnOutcome::Clean
        );
        assert!(!sink.events.lock().unwrap().iter().any(
            |e| matches!(e,HarnessEvent::AssistantDelta{text} if text.contains("OLD_REPLAY"))
        ));
        assert!(std::fs::read_to_string(dir.path().join("record"))
            .unwrap()
            .contains("session/load"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn acp_uses_current_allow_mode_without_native_grants_or_bypass_flags() {
    let dir = tempfile::tempdir().unwrap();
    let (session, sink) = session(dir.path(), PermissionMode::Ask, false);
    session
        .set_permission_mode(PermissionMode::Allow)
        .await
        .unwrap();
    let plan = session.compose_acp_plan(&turn()).unwrap();
    assert!(!plan
        .argv
        .iter()
        .any(|arg| arg == "--always-approve" || arg == "--yolo"));
    assert_eq!(session.run_turn(turn()).await.unwrap(), TurnOutcome::Clean);
    assert!(dir.path().join("executed").exists());
    assert!(!sink
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, HarnessEvent::ApprovalRequested { .. })));
    let record = std::fs::read_to_string(dir.path().join("record")).unwrap();
    assert!(record.contains("allow-once"));
    assert!(!record.contains("always-allow"));
}

#[cfg(unix)]
#[tokio::test]
async fn acp_cancel_and_duplicate_requests_leave_no_live_approval() {
    for duplicate in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (session, sink) = session(dir.path(), PermissionMode::Auto, duplicate);
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(turn()).await }
        });
        let approval = wait_approval(&sink).await;
        if !duplicate {
            session.interrupt().await.unwrap();
        }
        timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!dir.path().join("executed").exists());
        assert!(matches!(
            session.decide(approval, ApprovalDecision::Approve).await,
            Err(HarnessError::ApprovalWaiterMissing(_))
        ));
        assert!(sink.events.lock().unwrap().iter().any(|e| matches!(
            e,
            HarnessEvent::ApprovalResolved {
                decision: ApprovalDecision::Deny { .. },
                ..
            }
        )));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn acp_applies_turn_model_and_effort_before_prompting() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = session(dir.path(), PermissionMode::Allow, false);
    let mut input = turn();
    input.model = Some("grok-4.6".into());
    input.reasoning_effort = Some(ReasoningEffort::High);
    assert_eq!(session.run_turn(input).await.unwrap(), TurnOutcome::Clean);
    let calls: Vec<Value> = std::fs::read_to_string(dir.path().join("record"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let index = |method| {
        calls
            .iter()
            .position(|call| call["method"] == method)
            .unwrap()
    };
    assert!(index("session/set_model") < index("session/prompt"));
    assert!(index("session/set_mode") < index("session/prompt"));
    assert_eq!(calls[index("session/set_mode")]["params"]["modeId"], "high");
}

#[cfg(unix)]
#[tokio::test]
async fn acp_revalidates_the_tool_after_an_update_while_approval_waits() {
    for changed in ["raw_input", "metadata_input", "name"] {
        let dir = tempfile::tempdir().unwrap();
        let (session, sink) = session(dir.path(), PermissionMode::Auto, false);
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(turn()).await }
        });
        let approval = wait_approval(&sink).await;
        let (_, original) = captured_permission();
        let tool_id = original["params"]["toolCall"]["toolCallId"].clone();
        let mut update = json!({"sessionUpdate":"tool_call_update", "toolCallId":tool_id});
        match changed {
            "raw_input" => update["rawInput"] = json!({"command":"changed command"}),
            "metadata_input" => {
                update["_meta"] = json!({"x.ai/tool":{"input":{"command":"changed command"}}})
            }
            "name" => update["_meta"] = json!({"x.ai/tool":{"name":"different_tool"}}),
            _ => unreachable!(),
        }
        session.handle_acp_frame(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture-session","update":update}}), &mut GrokStreamParser::new(), false).await.unwrap();
        assert!(
            matches!(
                session
                    .decide(approval.clone(), ApprovalDecision::Approve)
                    .await,
                Err(HarnessError::ApprovalBindingMismatch(_))
            ),
            "changed {changed} was approved"
        );
        assert_eq!(
            timeout(Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            TurnOutcome::Clean
        );
        assert!(
            !dir.path().join("executed").exists(),
            "changed {changed} executed"
        );
        assert!(sink.events.lock().unwrap().iter().any(|event| matches!(event,
            HarnessEvent::ApprovalResolved {harness_ref,decision:ApprovalDecision::Deny{..}} if harness_ref.call_id == approval.call_id)));
        let calls: Vec<Value> = std::fs::read_to_string(dir.path().join("record"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let reply = calls
            .iter()
            .find(|call| call["id"] == 0 && call.get("result").is_some())
            .unwrap();
        assert_eq!(reply["result"]["outcome"], json!({"outcome":"cancelled"}));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn acp_does_not_echo_malformed_rpc_ids_or_create_approval_waiters() {
    let dir = tempfile::tempdir().unwrap();
    let (session, sink) = session(dir.path(), PermissionMode::Auto, false);
    let running = tokio::spawn({
        let session = session.clone();
        async move { session.run_turn(turn()).await }
    });
    let approval = wait_approval(&sink).await;
    let (_, original) = captured_permission();
    for malformed in [
        None,
        Some(Value::Null),
        Some(json!(true)),
        Some(json!([0])),
        Some(json!({"id":0})),
        Some(json!(1.5)),
    ] {
        let mut request = original.clone();
        if let Some(id) = malformed {
            request["id"] = id;
        } else {
            request.as_object_mut().unwrap().remove("id");
        }
        session.request_acp_permission(request).await.unwrap();
    }
    {
        let state = session.acp.lock().await;
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.seen.len(), 1);
    }
    session
        .decide(approval, ApprovalDecision::Deny { feedback: None })
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        TurnOutcome::Clean
    );
    let calls: Vec<Value> = std::fs::read_to_string(dir.path().join("record"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let replies: Vec<_> = calls
        .iter()
        .filter(|call| call.get("result").is_some())
        .collect();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["id"], 0);
    assert_eq!(
        sink.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(
                event,
                HarnessEvent::HarnessNotice {
                    level: HarnessNoticeLevel::Warning,
                    ..
                }
            ))
            .count(),
        6
    );
}

#[cfg(unix)]
#[tokio::test]
async fn acp_stop_cancels_every_startup_stage_and_allows_the_next_turn() {
    for stage in [
        "initialize",
        "session/new",
        "session/load",
        "session/set_model",
        "session/set_mode",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (session, sink) = session(dir.path(), PermissionMode::Auto, false);
        std::fs::write(dir.path().join("record.stall"), stage).unwrap();
        if stage == "session/load" {
            *session.resume_ref.lock().unwrap() = Some("fixture-session".into());
        }
        let mut input = turn();
        input.model = Some("grok-4.6".into());
        input.reasoning_effort = Some(ReasoningEffort::High);
        let running = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(input).await }
        });
        timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(dir.path().join("record"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|call| call["method"] == stage)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Grok never reached {stage}"));
        let stopped_at = tokio::time::Instant::now();
        timeout(Duration::from_secs(6), session.interrupt())
            .await
            .unwrap_or_else(|_| panic!("Stop hung during {stage}"))
            .unwrap();
        let outcome = timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !matches!(outcome, Ok(TurnOutcome::Clean)),
            "{stage}: {outcome:?}"
        );
        assert!(
            session.child.lock().await.is_none(),
            "{stage}: child remains"
        );
        assert!(session.child_pid().is_none(), "{stage}: PID remains");
        assert!(session.acp.lock().await.pending.is_empty());
        assert!(!sink.events.lock().unwrap().iter().any(|event| matches!(
            event,
            HarnessEvent::TurnStarted | HarnessEvent::TurnCompleted { .. }
        )));
        assert!(!std::fs::read_to_string(dir.path().join("record"))
            .unwrap()
            .contains("session/prompt"));
        assert!(
            stopped_at.elapsed() < Duration::from_secs(1),
            "Stop during {stage} took {:?}",
            stopped_at.elapsed()
        );
        std::fs::remove_file(dir.path().join("record.stall")).unwrap();
        let next = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(turn()).await }
        });
        let approval = wait_approval(&sink).await;
        session
            .decide(approval, ApprovalDecision::Deny { feedback: None })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(3), next)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            TurnOutcome::Clean
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn acp_stop_cancels_a_setup_write_before_taking_its_lock() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = session(dir.path(), PermissionMode::Auto, false);
    std::fs::write(dir.path().join("record.blockwrite"), "").unwrap();
    let running = tokio::spawn({
        let session = session.clone();
        async move { session.run_turn(turn()).await }
    });
    timeout(Duration::from_secs(3), async {
        loop {
            if dir.path().join("record.waiting").exists() && session.acp.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Grok did not stop reading during the setup reply");
    let stopped_at = tokio::time::Instant::now();
    timeout(Duration::from_secs(12), session.interrupt())
        .await
        .expect("Stop hung behind the setup write lock")
        .unwrap();
    let outcome = timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap();
    assert!(!matches!(outcome, Ok(TurnOutcome::Clean)));
    assert!(session.child.lock().await.is_none());
    assert!(session.child_pid().is_none());
    assert!(
        stopped_at.elapsed() < Duration::from_secs(1),
        "Stop waited for the setup write: {:?}",
        stopped_at.elapsed()
    );
}

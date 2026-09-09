//! Grok 1.0.13 ACP: one supervised process per turn, with native session resume.

use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::{json, Value};
use tidebreak_core::{ApprovalKind, HarnessNoticeLevel, MAX_NOTICE_CHARS};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::time::timeout;

use super::*;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RPC_LINE: usize = 16 * 1_024 * 1_024;

/// Enable only the version whose bidirectional protocol has captured fixtures.
pub(crate) fn supports_version(version: &str) -> bool {
    version
        .trim()
        .strip_prefix("grok ")
        .unwrap_or(version.trim())
        .split_whitespace()
        .next()
        == Some("1.0.13")
}

#[derive(Default)]
pub(super) struct Control {
    stdin: Option<ChildStdin>,
    session_id: Option<String>,
    generation: String,
    stopped: bool,
    active: bool,
    pending: HashMap<String, Permission>,
    seen: HashSet<String>,
    tools: HashMap<String, Value>,
}

struct Permission {
    rpc_id: Value,
    tool_id: String,
    tool_binding: ToolBinding,
    allow_once: String,
    reject_once: String,
}

#[derive(PartialEq, Eq)]
struct ToolBinding {
    name: String,
    input: Value,
    raw_input: Value,
}

impl ToolBinding {
    fn from_tool(tool: &Value) -> Option<Self> {
        Some(Self {
            name: tool.pointer("/_meta/x.ai~1tool/name")?.as_str()?.to_owned(),
            input: tool
                .pointer("/_meta/x.ai~1tool/input")
                .or_else(|| tool.get("rawInput"))?
                .clone(),
            raw_input: tool.get("rawInput")?.clone(),
        })
    }
}

fn valid_rpc_id(value: &Value) -> bool {
    value.is_string() || value.is_i64() || value.is_u64()
}

struct Reader {
    stdout: ChildStdout,
    lines: StreamLineBuffer,
    ready: VecDeque<String>,
}

impl Reader {
    async fn next(&mut self) -> Result<Value, HarnessError> {
        let budget = StreamBudget {
            max_partial_line: MAX_RPC_LINE,
            ..StreamBudget::default()
        };
        loop {
            if let Some(line) = self.ready.pop_front() {
                if line.trim().is_empty() {
                    continue;
                }
                return serde_json::from_str(&line)
                    .map_err(|_| HarnessError::Other("Grok ACP returned invalid JSON".into()));
            }
            let mut chunk = vec![0; budget.chunk_size];
            let n = self.stdout.read(&mut chunk).await?;
            if n == 0 {
                return Err(HarnessError::Other(
                    "Grok ACP closed before the turn finished".into(),
                ));
            }
            let tick = self.lines.push(&chunk[..n], budget);
            if tick.overflow_chunks > 0 {
                return Err(HarnessError::Other(
                    "Grok ACP exceeded the message size limit".into(),
                ));
            }
            self.ready.extend(tick.lines);
        }
    }
}

impl Control {
    async fn write(&mut self, value: &Value) -> Result<(), HarnessError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| HarnessError::Other("Grok ACP is not running".into()))?;
        let mut bytes = serde_json::to_vec(value).expect("JSON value serializes");
        bytes.push(b'\n');
        timeout(WRITE_TIMEOUT, async {
            stdin.write_all(&bytes).await?;
            stdin.flush().await
        })
        .await
        .map_err(|_| HarnessError::Other("Grok ACP write timed out".into()))??;
        Ok(())
    }
}

fn permission_reply(id: &Value, option: Option<&str>) -> Value {
    let outcome = match option {
        Some(option) => json!({"outcome":"selected", "optionId":option}),
        None => json!({"outcome":"cancelled"}),
    };
    json!({"jsonrpc":"2.0", "id":id, "result":{"outcome":outcome}})
}

/// Match the exact active tool and the one-use choices before showing consent.
fn parse_permission(value: &Value, state: &Control) -> Option<Permission> {
    let id = value.get("id")?;
    if !valid_rpc_id(id) {
        return None;
    }
    let params = value.get("params")?;
    if params.to_string().len() > 64 * 1_024 {
        return None;
    }
    if !state.active
        || state.stopped
        || params.get("sessionId")?.as_str()? != state.session_id.as_deref()?
    {
        return None;
    }
    let tool = params.get("toolCall")?;
    let tool_id = tool.get("toolCallId")?.as_str()?;
    let observed = state.tools.get(tool_id)?;
    let tool_binding = ToolBinding::from_tool(tool)?;
    if tool_binding != ToolBinding::from_tool(observed)? {
        return None;
    }
    let choices = params.get("options")?.as_array()?;
    let mut ids = HashSet::new();
    for choice in choices {
        if !ids.insert(choice.get("optionId")?.as_str()?) {
            return None;
        }
    }
    let one = |kind: &str| {
        let mut matches = choices
            .iter()
            .filter(|v| v.get("kind").and_then(Value::as_str) == Some(kind));
        let id = matches.next()?.get("optionId")?.as_str()?;
        (matches.next().is_none() && !id.is_empty()).then(|| id.to_owned())
    };
    Some(Permission {
        rpc_id: id.clone(),
        tool_id: tool_id.to_owned(),
        tool_binding,
        allow_once: one("allow_once")?,
        reject_once: one("reject_once")?,
    })
}

fn permission_kind(value: &Value, cwd: &Path) -> ApprovalKind {
    let tool = &value["params"]["toolCall"];
    let name = tool
        .pointer("/_meta/x.ai~1tool/name")
        .and_then(Value::as_str)
        .unwrap_or("Grok tool");
    if name == "run_terminal_command" {
        if let Some(command) = tool.pointer("/rawInput/command").and_then(Value::as_str) {
            return ApprovalKind::Command {
                cmd: command.to_owned(),
                cwd: Some(cwd.to_string_lossy().into_owned()),
            };
        }
    }
    ApprovalKind::Other {
        summary: name.to_owned(),
    }
}

impl GrokSession {
    fn compose_acp_plan(&self, input: &TurnInput) -> Result<LaunchPlan, HarnessError> {
        // Reuse the relay credential and launch environment contract. ACP sends
        // prompt/session data over stdin, so no prompt or resume token enters argv.
        let mut plan = self.compose_plan(
            Path::new("unused-acp-prompt"),
            input.model.as_deref(),
            input.reasoning_effort,
        )?;
        let mut argv = vec![plan.argv[0].clone(), "agent".into(), "--no-leader".into()];
        if let Some(model) = input.model.as_deref().or(self.spec.model.as_deref()) {
            argv.extend(["--model".into(), model.into()]);
        }
        if let Some(effort) = input
            .reasoning_effort
            .or(self.spec.reasoning_effort)
            .and_then(|level| {
                level.clamp_to(crate::grok::effort_ladder_for_version(Some(&self.version)))
            })
        {
            argv.extend(["--reasoning-effort".into(), effort.as_str().into()]);
        }
        // Keep the native default permission gate in every mode. Explicit Allow
        // answers each native request with allow_once; it creates no saved grant.
        argv.extend(self.spec.extra_argv.iter().cloned());
        argv.push("stdio".into());
        plan.argv = argv;
        plan.env
            .retain(|(key, _)| key != "GROK_DISABLE_AUTOUPDATER");
        plan.env
            .push(("GROK_DISABLE_AUTOUPDATER".into(), "1".into()));
        validate_launch_plan_with(&plan, BypassPolicy::Forbidden)?;
        Ok(plan)
    }

    pub(super) async fn run_acp_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        refuse_versioned_mode(self.permission_mode(), &self.version)?;
        if !input.images.is_empty() {
            return Err(HarnessError::Other(
                "Grok 1.0.13 does not accept prompt images; inspect screenshots with read_file"
                    .into(),
            ));
        }
        let plan = self.compose_acp_plan(&input)?;
        {
            let mut state = self.acp.lock().await;
            *state = Control {
                generation: Uuid::new_v4().to_string(),
                ..Control::default()
            };
            self.acp_stop.send_replace(false);
        }
        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_child_env_tokio(
            &mut command,
            HarnessKind::Grok,
            self.spec.env.iter().cloned(),
            &plan.env,
            self.spec.browser.as_ref(),
            self.spec.native.as_ref(),
        );
        let mut child = spawn_process_tree(&mut command)?;
        let stdin = child
            .take_stdin()
            .ok_or_else(|| HarnessError::Other("Grok ACP has no stdin".into()))?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| HarnessError::Other("Grok ACP has no stdout".into()))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| HarnessError::Other("Grok ACP has no stderr".into()))?;
        self.pid.set(child.id());
        *self.child.lock().await = Some(child);
        self.acp.lock().await.stdin = Some(stdin);
        let stderr_task = tokio::spawn(drain_capped(stderr, MAX_STDERR_BYTES));
        let mut reader = Reader {
            stdout,
            lines: StreamLineBuffer::new(),
            ready: VecDeque::new(),
        };
        let mut parser = GrokStreamParser::new();
        parser.set_version(&self.version);
        let result = self.drive_acp(&input, &mut reader, &mut parser).await;
        self.cancel_acp_waiters().await;
        self.acp.lock().await.stdin.take();
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.terminate().await;
        }
        self.pid.clear();
        self.acp_done.notify_waiters();
        self.unrecognized
            .fetch_add(parser.unrecognized(), Ordering::SeqCst);
        let _ = stderr_task.await;
        result
    }

    async fn drive_acp(
        &self,
        input: &TurnInput,
        reader: &mut Reader,
        parser: &mut GrokStreamParser,
    ) -> Result<TurnOutcome, HarnessError> {
        let initialized = self.acp_rpc(1, "initialize", json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"Tidebreak","version":env!("CARGO_PKG_VERSION")}}), reader, parser, true).await?;
        if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err(HarnessError::Other(
                "Grok ACP returned an unsupported protocol version".into(),
            ));
        }
        let resume = self.resume_ref.lock().expect("grok resume").clone();
        let mut params = json!({"cwd":self.spec.worktree,"mcpServers":[],"_meta":{"yoloMode":false,"autoMode":false}});
        let method = if let Some(id) = &resume {
            params["sessionId"] = json!(id);
            "session/load"
        } else {
            "session/new"
        };
        let response = self
            .acp_rpc(2, method, params, reader, parser, true)
            .await
            .map_err(|error| {
                if resume.is_some()
                    && (error.to_string().contains("not found")
                        || error.to_string().contains("Failed to restore session"))
                {
                    HarnessError::ResumeLost(format!(
                        "Grok ACP could not load the stored session: {error}"
                    ))
                } else {
                    error
                }
            })?;
        let session_id = match resume {
            Some(id) => id,
            None => response
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| HarnessError::Other("Grok ACP returned no session ID".into()))?
                .to_owned(),
        };
        *self.resume_ref.lock().expect("grok resume") = Some(session_id.clone());
        if let Some(model) = input.model.as_deref().or(self.spec.model.as_deref()) {
            if response
                .pointer("/models/currentModelId")
                .and_then(Value::as_str)
                != Some(model)
            {
                let selected = self
                    .acp_rpc(
                        4,
                        "session/set_model",
                        json!({"sessionId":session_id,"modelId":model}),
                        reader,
                        parser,
                        true,
                    )
                    .await?;
                if selected.pointer("/_meta/model/Ok").and_then(Value::as_str) != Some(model) {
                    return Err(HarnessError::Other(
                        "Grok ACP did not confirm the requested model".into(),
                    ));
                }
            }
        }
        if let Some(effort) = input
            .reasoning_effort
            .or(self.spec.reasoning_effort)
            .and_then(|level| {
                level.clamp_to(crate::grok::effort_ladder_for_version(Some(&self.version)))
            })
        {
            self.acp_rpc(
                5,
                "session/set_mode",
                json!({"sessionId":session_id,"modeId":effort.as_str()}),
                reader,
                parser,
                true,
            )
            .await?;
        }
        {
            let mut state = self.acp.lock().await;
            state.stopped |= *self.acp_stop.borrow();
            if state.stopped {
                return Ok(TurnOutcome::Incomplete {
                    detail: "Grok ACP was stopped during startup".into(),
                });
            }
            state.session_id = Some(session_id.clone());
            state.active = true;
        }
        self.spec
            .sink
            .emit(HarnessEvent::SessionStarted {
                harness_kind: HarnessKind::Grok,
                harness_version: self.version.clone(),
                resume_ref: Some(session_id.clone()),
            })
            .await;
        self.spec.sink.emit(HarnessEvent::TurnStarted).await;
        let text = computer_use_prompt(
            &input.text,
            self.spec.browser.as_ref(),
            self.spec.native.as_ref(),
        )?;
        let result = self
            .acp_rpc(
                3,
                "session/prompt",
                json!({"sessionId":session_id,"prompt":[{"type":"text","text":text}]}),
                reader,
                parser,
                false,
            )
            .await?;
        if let Some(tokens) = result.pointer("/_meta/inputTokens").and_then(Value::as_u64) {
            parser.set_last_call_context_tokens(tokens);
        }
        let mut end = json!({"type":"end","sessionId":session_id,"stopReason":result.get("stopReason").cloned().unwrap_or(Value::Null)});
        if let Some(usage) = result.pointer("/_meta/usage") {
            end["usage"] = usage_fields(usage);
        }
        for event in parser.push_line(&end.to_string()) {
            if !matches!(event, HarnessEvent::SessionStarted { .. }) {
                self.spec.sink.emit(event).await;
            }
        }
        Ok(TurnOutcome::Clean)
    }

    async fn acp_rpc(
        &self,
        id: i64,
        method: &str,
        params: Value,
        reader: &mut Reader,
        parser: &mut GrokStreamParser,
        setup: bool,
    ) -> Result<Value, HarnessError> {
        let mut stop = self.acp_stop.subscribe();
        let request = async {
            {
                let mut state = self.acp.lock().await;
                state.stopped |= *self.acp_stop.borrow();
                if state.stopped {
                    return Err(HarnessError::Other("Grok ACP was stopped".into()));
                }
                state
                    .write(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                    .await?;
            }
            loop {
                let value = reader.next().await?;
                if value.get("id").and_then(Value::as_i64) == Some(id)
                    && value.get("method").is_none()
                {
                    if let Some(error) = value.get("error") {
                        let message = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("request failed");
                        return Err(HarnessError::Other(format!(
                            "Grok ACP {method}: {}",
                            message.chars().take(MAX_NOTICE_CHARS).collect::<String>()
                        )));
                    }
                    return value.get("result").cloned().ok_or_else(|| {
                        HarnessError::Other("Grok ACP response has no result".into())
                    });
                }
                self.handle_acp_frame(value, parser, setup).await?;
            }
        };
        if setup {
            tokio::select! {
                biased;
                _ = stop.wait_for(|stopped| *stopped) => {
                    Err(HarnessError::Other("Grok ACP was stopped during startup".into()))
                }
                result = timeout(HANDSHAKE_TIMEOUT, request) => {
                    result.map_err(|_| HarnessError::Other(format!("Grok ACP {method} timed out")))?
                }
            }
        } else {
            request.await
        }
    }

    async fn handle_acp_frame(
        &self,
        value: Value,
        parser: &mut GrokStreamParser,
        setup: bool,
    ) -> Result<(), HarnessError> {
        match value.get("method").and_then(Value::as_str) {
            Some("session/request_permission") => self.request_acp_permission(value).await,
            Some("session/update") => {
                let params = &value["params"];
                let mut state = self.acp.lock().await;
                if setup || !state.active || params.get("sessionId").and_then(Value::as_str) != state.session_id.as_deref() { return Ok(()); }
                let update = &params["update"];
                let kind = update.get("sessionUpdate").and_then(Value::as_str).unwrap_or("");
                if matches!(kind, "tool_call" | "tool_call_update") {
                    if let Some(id) = update.get("toolCallId").and_then(Value::as_str) {
                        if matches!(update.get("status").and_then(Value::as_str), Some("completed" | "failed")) {
                            state.tools.remove(id);
                        } else if kind == "tool_call" {
                            state.tools.insert(id.to_owned(), update.clone());
                        } else if let Some(observed) = state.tools.get_mut(id) {
                            if let Some(input) = update.get("rawInput") {
                                observed["rawInput"] = input.clone();
                            }
                            if let Some(metadata) = update.pointer("/_meta/x.ai~1tool").and_then(Value::as_object) {
                                let mut merged = observed.pointer("/_meta/x.ai~1tool")
                                    .and_then(Value::as_object).cloned().unwrap_or_default();
                                merged.extend(metadata.iter().map(|(key, value)| (key.clone(), value.clone())));
                                observed["_meta"] = json!({"x.ai/tool":merged});
                            }
                        }
                    }
                }
                drop(state);
                let mapped = map_update(update);
                if let Some(mapped) = mapped {
                    for event in parser.push_line(&mapped.to_string()) { self.spec.sink.emit(event).await; }
                }
                Ok(())
            }
            Some(_) if value.get("id").is_some_and(valid_rpc_id) => {
                self.acp.lock().await.write(&json!({"jsonrpc":"2.0","id":value["id"],"error":{"code":-32601,"message":"Tidebreak does not support this Grok client request"}})).await
            }
            _ => Ok(()),
        }
    }

    async fn request_acp_permission(&self, value: Value) -> Result<(), HarnessError> {
        let Some(id) = value.get("id").filter(|id| valid_rpc_id(id)).cloned() else {
            self.spec.sink.emit(HarnessEvent::HarnessNotice {
                level: HarnessNoticeLevel::Warning,
                message: "Grok sent an approval request without a valid string or integer ID; Tidebreak ignored it".into(),
            }).await;
            return Ok(());
        };
        let mut state = self.acp.lock().await;
        state.stopped |= *self.acp_stop.borrow();
        let unique = state.seen.insert(id.to_string());
        let permission = unique.then(|| parse_permission(&value, &state)).flatten();
        let Some(permission) = permission else {
            // A duplicate request cannot leave a stale approval card behind.
            let stale: Vec<_> = state
                .pending
                .iter()
                .filter(|(_, p)| p.rpc_id == id)
                .map(|(key, _)| key.clone())
                .collect();
            for key in &stale {
                state.pending.remove(key);
            }
            state.write(&permission_reply(&id, None)).await?;
            drop(state);
            for key in stale {
                self.spec
                    .sink
                    .emit(HarnessEvent::ApprovalResolved {
                        harness_ref: HarnessApprovalRef::engine(key),
                        decision: ApprovalDecision::Deny { feedback: None },
                    })
                    .await;
            }
            self.spec.sink.emit(HarnessEvent::HarnessNotice { level:HarnessNoticeLevel::Warning, message:"Grok requested an unsupported, stale, or mismatched approval; Tidebreak cancelled it".into() }).await;
            return Ok(());
        };
        let key = format!("grok-acp:{}:{}", state.generation, id);
        if self.permission_mode() == PermissionMode::Allow {
            return state
                .write(&permission_reply(&id, Some(&permission.allow_once)))
                .await;
        }
        state.pending.insert(key.clone(), permission);
        self.spec
            .sink
            .emit(HarnessEvent::ApprovalRequested {
                harness_ref: HarnessApprovalRef::engine(key),
                raw: value["params"].clone(),
                kind: Some(permission_kind(&value, &self.spec.worktree)),
            })
            .await;
        drop(state);
        Ok(())
    }

    pub(super) async fn decide_acp(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        if !matches!(
            decision,
            ApprovalDecision::Approve | ApprovalDecision::Deny { .. }
        ) {
            return Err(HarnessError::DecisionUnsupported(
                "Grok ACP supports one-use approval or denial".into(),
            ));
        }
        let mut state = self.acp.lock().await;
        state.stopped |= *self.acp_stop.borrow();
        let permission = state
            .pending
            .remove(&approval.call_id)
            .ok_or_else(|| HarnessError::ApprovalWaiterMissing(approval.call_id.clone()))?;
        let binding_matches = state
            .tools
            .get(&permission.tool_id)
            .and_then(ToolBinding::from_tool)
            .is_some_and(|binding| binding == permission.tool_binding);
        if state.stopped || !state.active || !binding_matches {
            let _ = state
                .write(&permission_reply(&permission.rpc_id, None))
                .await;
            drop(state);
            self.spec
                .sink
                .emit(HarnessEvent::ApprovalResolved {
                    harness_ref: approval.clone(),
                    decision: ApprovalDecision::Deny { feedback: None },
                })
                .await;
            return Err(HarnessError::ApprovalBindingMismatch(
                "the Grok tool changed or finished while its approval was waiting".into(),
            ));
        }
        let option = if matches!(decision, ApprovalDecision::Approve) {
            &permission.allow_once
        } else {
            &permission.reject_once
        };
        state
            .write(&permission_reply(&permission.rpc_id, Some(option)))
            .await
            .map_err(|error| HarnessError::ApprovalAcknowledgementLost(error.to_string()))?;
        drop(state);
        self.spec
            .sink
            .emit(HarnessEvent::ApprovalResolved {
                harness_ref: approval,
                decision,
            })
            .await;
        Ok(())
    }

    async fn cancel_acp_waiters(&self) {
        let mut state = self.acp.lock().await;
        state.active = false;
        state.stopped |= *self.acp_stop.borrow();
        let mut writable = true;
        while let Some((key, rpc_id)) = state
            .pending
            .iter()
            .next()
            .map(|(key, permission)| (key.clone(), permission.rpc_id.clone()))
        {
            if writable && state.write(&permission_reply(&rpc_id, None)).await.is_err() {
                writable = false;
            }
            self.spec
                .sink
                .emit(HarnessEvent::ApprovalResolved {
                    harness_ref: HarnessApprovalRef::engine(key.clone()),
                    decision: ApprovalDecision::Deny { feedback: None },
                })
                .await;
            // If Stop times out during a write, turn cleanup still owns this denial.
            state.pending.remove(&key);
        }
    }

    /// Give the ACP cancellation a bounded chance to stop its tools before killing the tree.
    pub(super) async fn interrupt_acp(&self) -> Result<bool, HarnessError> {
        let done = self.acp_done.notified();
        tokio::pin!(done);
        done.as_mut().enable();
        // Wake setup before taking the write lock: the child may have stopped
        // reading stdin, so an in-flight request can hold that lock.
        self.acp_stop.send_replace(true);
        let cancel = async {
            {
                let mut state = self.acp.lock().await;
                state.stopped = true;
                if let Some(session_id) = state.session_id.clone() {
                    let _ = state.write(&json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session_id}})).await;
                }
            }
            self.cancel_acp_waiters().await;
            if self.child.lock().await.is_some() {
                done.await;
            }
        };
        // The grace period covers lock acquisition, writes, and the reply.
        Ok(timeout(INTERRUPT_GRACE, cancel).await.is_ok())
    }
}

fn usage_fields(usage: &Value) -> Value {
    let number = |key| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    // ACP inputTokens includes cached reads; the print parser counts them separately.
    json!({"input_tokens":number("inputTokens").saturating_sub(number("cachedReadTokens")).saturating_sub(number("cacheCreationTokens")),"output_tokens":number("outputTokens"),"cache_read_input_tokens":number("cachedReadTokens"),"cache_creation_input_tokens":number("cacheCreationTokens"),"reasoning_tokens":number("reasoningTokens")})
}

fn map_update(update: &Value) -> Option<Value> {
    let kind = update.get("sessionUpdate")?.as_str()?;
    match kind {
        "agent_message_chunk" | "agent_thought_chunk" => {
            let text = update.pointer("/content/text")?.as_str()?;
            Some(
                json!({"type":if kind == "agent_message_chunk" {"text"} else {"thought"}, "data":text}),
            )
        }
        "tool_call" | "tool_call_update" => {
            let mut mapped = update.clone();
            mapped["type"] = json!(kind);
            if let Some(name) = update.pointer("/_meta/x.ai~1tool/name") {
                mapped["toolName"] = name.clone();
            }
            Some(mapped)
        }
        // Startup inventory and resumed user history are not new assistant output.
        "available_commands_update"
        | "session_info_update"
        | "user_message_chunk"
        | "current_mode_update"
        | "config_option_update" => None,
        _ => Some(json!({"type":format!("acp/{kind}")})),
    }
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;

//! Long-lived `opencode serve` child driven over HTTP + SSE.

use std::net::TcpListener;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use crate::browser_channel::apply_child_env_tokio;
use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::opencode::parse::OpencodeStreamParser;
use crate::{
    spawn_process_tree, ApprovalDecision, BrowserChannelSpec, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessSession, ProcessTreeChild, SessionSpec, StreamBudget, StreamLineBuffer,
    TurnInput, TurnOutcome,
};
use tidebreak_core::CodePermissionMode;

const READY_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_STDERR_BYTES: usize = 64 * 1_024;
const AUTO_FLAG: &str = "--auto";
/// Environment key for the OpenCode config JSON override.
const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
/// MCP server name used in the OpenCode config for the browser tool bridge.
const BROWSER_MCP_SERVER: &str = "tb-browser";

/// Live opencode session: one `serve` child for the session lifetime.
pub struct OpencodeSession {
    spec: SessionSpec,
    resume_ref: Mutex<Option<String>>,
    child: AsyncMutex<Option<ProcessTreeChild>>,
    child_pid: AtomicU32,
    base_url: String,
    client: reqwest::Client,
    parser: Mutex<OpencodeStreamParser>,
    events: AsyncMutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl OpencodeSession {
    pub(super) fn new(spec: SessionSpec) -> Self {
        let resume_ref = spec.resume_ref.clone();
        Self {
            spec,
            resume_ref: Mutex::new(resume_ref),
            child: AsyncMutex::new(None),
            child_pid: AtomicU32::new(0),
            base_url: String::new(),
            client: reqwest::Client::new(),
            parser: Mutex::new(OpencodeStreamParser::new()),
            events: AsyncMutex::new(None),
        }
    }
}

/// Build the OpenCode MCP server entry for the browser bridge.
///
/// `bridge_command` is the absolute path from [`BrowserChannelSpec::bridge_command`].
/// The capfile is NOT in the JSON — it's inherited through the process
/// environment via `TIDEBREAK_BROWSER_CAPFILE`.
#[must_use]
fn browser_mcp_config_json(bridge_command: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "type": "local",
        "command": [bridge_command.to_string_lossy(), "browser-mcp"],
    })
}

/// Merge the browser MCP server entry into an optional existing
/// `OPENCODE_CONFIG_CONTENT` JSON string.
///
/// If `config_content` is `None` or empty, a fresh config object is created.
/// If it parses as a JSON object, the browser entry is merged into its `mcp`
/// key, preserving all unrelated entries. A conflicting `tb-browser` entry
/// (different content) is rejected. An identical entry is idempotent.
/// Malformed or non-object JSON is rejected with a clear error.
fn merge_browser_mcp(
    config_content: Option<&str>,
    bridge_command: &std::path::Path,
) -> Result<String, HarnessError> {
    let browser_entry = browser_mcp_config_json(bridge_command);
    match config_content.map(str::trim).filter(|s| !s.is_empty()) {
        None => {
            let config = serde_json::json!({
                "mcp": { BROWSER_MCP_SERVER: browser_entry }
            });
            Ok(config.to_string())
        }
        Some(raw) => {
            let mut config: serde_json::Value = serde_json::from_str(raw).map_err(|err| {
                HarnessError::Other(format!("OPENCODE_CONFIG_CONTENT is not valid JSON: {err}"))
            })?;
            let obj = config.as_object_mut().ok_or_else(|| {
                HarnessError::Other("OPENCODE_CONFIG_CONTENT must be a JSON object".into())
            })?;
            let mcp = obj
                .entry("mcp".to_owned())
                .or_insert_with(|| serde_json::json!({}));
            let mcp_obj = mcp.as_object_mut().ok_or_else(|| {
                HarnessError::Other(
                    "OPENCODE_CONFIG_CONTENT `mcp` key must be a JSON object".into(),
                )
            })?;
            if let Some(existing) = mcp_obj.get(BROWSER_MCP_SERVER) {
                if existing != &browser_entry {
                    return Err(HarnessError::Other(format!(
                        "OPENCODE_CONFIG_CONTENT already has a conflicting `{BROWSER_MCP_SERVER}` MCP entry"
                    )));
                }
                // Identical entry — idempotent, no change needed.
            } else {
                mcp_obj.insert(BROWSER_MCP_SERVER.to_owned(), browser_entry);
            }
            Ok(config.to_string())
        }
    }
}

/// Argv for the long-lived serve child. Prompt never appears here.
pub(crate) fn compose_serve_plan(
    binary: &std::path::Path,
    extra_argv: &[String],
    cwd: &std::path::Path,
    extra_env: &[(String, String)],
    port: u16,
    browser: Option<&BrowserChannelSpec>,
) -> Result<LaunchPlan, HarnessError> {
    let mut argv = vec![
        binary.to_string_lossy().into_owned(),
        "serve".into(),
        "--hostname".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ];
    argv.extend(extra_argv.iter().cloned());
    if argv
        .iter()
        .any(|arg| arg == AUTO_FLAG || arg.starts_with("--auto="))
    {
        return Err(HarnessError::LaunchRejected(crate::BypassFlagError(
            AUTO_FLAG.into(),
        )));
    }
    let mut env = extra_env.to_vec();
    // Strip any existing OPENCODE_CONFIG_CONTENT from extra_env; the browser
    // merge produces the authoritative value when a browser channel is present.
    let existing_config = env
        .iter()
        .find(|(key, _)| key == OPENCODE_CONFIG_CONTENT)
        .map(|(_, value)| value.clone());
    env.retain(|(key, _)| {
        !BrowserChannelSpec::is_reserved_env_key(key)
            && key != "PWD"
            && key != OPENCODE_CONFIG_CONTENT
    });
    if let Some(spec) = browser {
        let merged = merge_browser_mcp(existing_config.as_deref(), spec.bridge_command())?;
        env.push((OPENCODE_CONFIG_CONTENT.to_owned(), merged));
    } else if let Some(existing) = existing_config {
        // No browser channel — preserve the original config without merging.
        env.push((OPENCODE_CONFIG_CONTENT.to_owned(), existing));
    }
    let plan = LaunchPlan {
        argv,
        cwd: cwd.to_path_buf(),
        env,
    };
    validate_launch_plan(&plan)?;
    Ok(plan)
}

/// `POST /session` agent + permission ruleset for a permission mode.
///
/// Never composed as `--auto`. Plan selects the native `plan` agent
/// (disallows edit tools). Ask parks bash/edit. Auto allows workspace
/// edits and still asks for bash.
#[must_use]
pub(crate) fn session_create_body(mode: CodePermissionMode, model: Option<&str>) -> Value {
    let mut body = match mode {
        CodePermissionMode::Plan => json!({ "agent": "plan" }),
        CodePermissionMode::Ask => json!({
            "agent": "build",
            "permission": [
                {"permission": "bash", "pattern": "*", "action": "ask"},
                {"permission": "edit", "pattern": "*", "action": "ask"},
                {"permission": "read", "pattern": "*", "action": "allow"}
            ]
        }),
        CodePermissionMode::Auto => json!({
            "agent": "build",
            "permission": [
                {"permission": "edit", "pattern": "*", "action": "allow"},
                {"permission": "read", "pattern": "*", "action": "allow"},
                {"permission": "bash", "pattern": "*", "action": "ask"}
            ]
        }),
        CodePermissionMode::Allow => json!({
            "agent": "build",
            "permission": [
                {"permission": "bash", "pattern": "*", "action": "allow"},
                {"permission": "edit", "pattern": "*", "action": "allow"},
                {"permission": "read", "pattern": "*", "action": "allow"},
                {"permission": "grep", "pattern": "*", "action": "allow"},
                {"permission": "glob", "pattern": "*", "action": "allow"},
                {"permission": "list", "pattern": "*", "action": "allow"},
                {"permission": "external_directory", "pattern": "*", "action": "allow"},
                {"permission": "websearch", "pattern": "*", "action": "allow"},
                {"permission": "webfetch", "pattern": "*", "action": "allow"},
                {"permission": "task", "pattern": "*", "action": "allow"}
            ]
        }),
    };
    if let Some(model) = model.and_then(session_model_field) {
        body["model"] = model;
    }
    body
}

/// `POST /session` model object. The captured 1.18.18 schema wants
/// `{ providerID, id }`; `{ modelID }` and `{ providerID, modelID }` are 400.
fn session_model_field(model: &str) -> Option<Value> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((provider, id)) = split_provider_model(trimmed) {
        return Some(json!({ "providerID": provider, "id": id }));
    }
    let id = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let provider = infer_opencode_provider(id)?;
    Some(json!({ "providerID": provider, "id": id }))
}

fn split_provider_model(model: &str) -> Option<(&str, &str)> {
    let (provider, id) = model.split_once('/')?;
    if provider.is_empty() || id.is_empty() {
        return None;
    }
    // A gateway routing handle is not an OpenCode provider.
    if provider.eq_ignore_ascii_case("model-gateway")
        || provider.eq_ignore_ascii_case("model_gateway")
    {
        return None;
    }
    Some((provider, id))
}

fn infer_opencode_provider(id: &str) -> Option<&'static str> {
    let leaf = id.to_ascii_lowercase();
    if leaf.contains("claude")
        || leaf.contains("sonnet")
        || leaf.contains("opus")
        || leaf.contains("haiku")
    {
        return Some("anthropic");
    }
    if leaf.contains("gpt") || leaf.contains("codex") || openai_o_series(&leaf) {
        return Some("openai");
    }
    if leaf.contains("grok") {
        return Some("xai");
    }
    if leaf.contains("gemini") {
        return Some("google");
    }
    if leaf.contains("pickle") {
        return Some("opencode");
    }
    None
}

fn openai_o_series(id: &str) -> bool {
    id.strip_prefix('o')
        .is_some_and(|rest| rest.starts_with(|ch: char| ch.is_ascii_digit()))
}

fn pick_loopback_port() -> Result<u16, HarnessError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Spawn the serve child, wait for `/global/health`, create or resume a session.
pub(super) async fn attach(spec: SessionSpec) -> Result<OpencodeSession, HarnessError> {
    let mut session = OpencodeSession::new(spec);
    let port = pick_loopback_port()?;
    let plan = compose_serve_plan(
        &session.spec.binary,
        &session.spec.extra_argv,
        &session.spec.worktree,
        &session.spec.extra_env,
        port,
        session.spec.browser.as_ref(),
    )?;
    let mut command = Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_child_env_tokio(
        &mut command,
        session.spec.env.iter().cloned(),
        &plan.env,
        session.spec.browser.as_ref(),
    );
    let mut child = spawn_process_tree(&mut command)?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| HarnessError::Other("engine child has no stdout".into()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| HarnessError::Other("engine child has no stderr".into()))?;
    if let Some(pid) = child.id() {
        session.child_pid.store(pid, Ordering::SeqCst);
    }
    *session.child.lock().await = Some(child);
    tokio::spawn(async move {
        let _ = drain_capped(stdout, MAX_STDERR_BYTES).await;
    });
    tokio::spawn(async move {
        let _ = drain_capped(stderr, MAX_STDERR_BYTES).await;
    });

    session.base_url = format!("http://127.0.0.1:{port}");
    session.wait_until_healthy().await?;
    session.subscribe_events().await?;
    session.open_or_resume_session().await?;
    Ok(session)
}

impl OpencodeSession {
    fn directory_query(&self) -> [(String, String); 1] {
        [(
            "directory".into(),
            self.spec.worktree.to_string_lossy().into_owned(),
        )]
    }

    async fn wait_until_healthy(&self) -> Result<(), HarnessError> {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        let url = format!("{}/global/health", self.base_url);
        loop {
            if tokio::time::Instant::now() > deadline {
                return Err(HarnessError::Other(
                    "timed out waiting for opencode serve /global/health".into(),
                ));
            }
            match timeout(Duration::from_secs(1), self.client.get(&url).send()).await {
                Ok(Ok(resp)) if resp.status().is_success() => return Ok(()),
                _ => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn subscribe_events(&self) -> Result<(), HarnessError> {
        let url = format!("{}/event", self.base_url);
        let response = self
            .client
            .get(&url)
            .query(&self.directory_query())
            .send()
            .await
            .map_err(|err| HarnessError::Other(format!("event stream: {err}")))?;
        if !response.status().is_success() {
            return Err(HarnessError::Other(format!(
                "event stream status {}",
                response.status()
            )));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut response = response;
            let mut lines = StreamLineBuffer::new();
            let budget = StreamBudget::default();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        let tick = lines.push(&chunk, budget);
                        for line in tick.lines {
                            if let Some(event) = sse_data_line(&line) {
                                if tx.send(event).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Ok(None) | Err(_) => return,
                }
            }
        });
        *self.events.lock().await = Some(rx);
        Ok(())
    }

    async fn open_or_resume_session(&self) -> Result<(), HarnessError> {
        let resume = self.resume_ref.lock().expect("opencode resume").clone();
        if let Some(resume) = resume {
            let path = format!("/session/{resume}");
            let url = format!("{}{path}", self.base_url);
            let query = self.directory_query();
            let (status, body) = self
                .http("GET", &path, &url, None, Some(query.as_slice()))
                .await?;
            if status == 404 {
                // The server does not know this session any more: the stored
                // ref is dead, not a transient failure.
                return Err(HarnessError::ResumeLost(format!(
                    "opencode has no session {resume}"
                )));
            }
            if !(200..300).contains(&status) {
                return Err(HarnessError::Other(format!(
                    "resume GET {path} status {status}"
                )));
            }
            self.emit_http_in(&path, status, body).await;
            return Ok(());
        }
        let path = "/session";
        let url = format!("{}{path}", self.base_url);
        let body = session_create_body(self.spec.permission_mode, self.spec.model.as_deref());
        let query = self.directory_query();
        let (status, parsed) = self
            .http("POST", path, &url, Some(body), Some(query.as_slice()))
            .await?;
        self.emit_http_in(path, status, parsed.clone()).await;
        if !(200..300).contains(&status) {
            return Err(http_status_error("POST", path, status, &parsed));
        }
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            *self.resume_ref.lock().expect("opencode resume") = Some(id.to_owned());
        }
        Ok(())
    }

    async fn http(
        &self,
        method: &str,
        path: &str,
        url: &str,
        body: Option<Value>,
        query: Option<&[(String, String)]>,
    ) -> Result<(u16, Value), HarnessError> {
        self.emit_http_out(method, path, body.clone()).await;
        let mut request = match method {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            other => {
                return Err(HarnessError::Other(format!(
                    "unsupported http method {other}"
                )));
            }
        };
        if let Some(query) = query {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|err| HarnessError::Other(format!("http {method} {path}: {err}")))?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .map_err(|err| HarnessError::Other(format!("http body {path}: {err}")))?;
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Ok((status, parsed))
    }

    async fn emit_http_out(&self, method: &str, path: &str, body: Option<Value>) {
        let line = serde_json::to_string(&json!({
            "dir": "out",
            "msg": { "kind": "http", "method": method, "path": path, "body": body }
        }))
        .unwrap_or_default();
        self.emit_parsed(&line).await;
    }

    async fn emit_http_in(&self, path: &str, status: u16, body: Value) {
        let line = serde_json::to_string(&json!({
            "dir": "in",
            "msg": { "kind": "http", "status": status, "path": path, "body": body }
        }))
        .unwrap_or_default();
        self.emit_parsed(&line).await;
    }

    async fn emit_parsed(&self, line: &str) -> Vec<HarnessEvent> {
        let events = self.parser.lock().expect("opencode parser").push_line(line);
        for event in &events {
            if let HarnessEvent::SessionStarted {
                resume_ref: Some(resume),
                ..
            } = event
            {
                *self.resume_ref.lock().expect("opencode resume") = Some(resume.clone());
            }
            if let HarnessEvent::ApprovalRequested { harness_ref, .. } = event {
                if self.spec.permission_mode == CodePermissionMode::Allow {
                    // Allow already grants every known rule. A request that
                    // still arrives must not park a card.
                    let _ = self
                        .decide(harness_ref.clone(), ApprovalDecision::Approve)
                        .await;
                    continue;
                }
            }
            self.spec.sink.emit(event.clone()).await;
        }
        events
    }

    async fn emit_sse_events(&self, event: &str) -> Vec<HarnessEvent> {
        let Ok(parsed) = serde_json::from_str::<Value>(event) else {
            return Vec::new();
        };
        let line = serde_json::to_string(&json!({
            "dir": "in",
            "msg": { "kind": "sse", "event": parsed }
        }))
        .unwrap_or_default();
        self.emit_parsed(&line).await
    }

    async fn read_until_terminal_turn(&self) -> Result<(), HarnessError> {
        loop {
            let event = {
                let mut slot = self.events.lock().await;
                let Some(rx) = slot.as_mut() else {
                    return Err(HarnessError::Other("event stream is not connected".into()));
                };
                rx.recv().await
            };
            let Some(event) = event else {
                return Err(HarnessError::Other(
                    "engine event stream closed before the turn finished".into(),
                ));
            };
            let mut terminal = false;
            for parsed in self.emit_sse_events(&event).await {
                if matches!(
                    parsed,
                    HarnessEvent::TurnCompleted { .. }
                        | HarnessEvent::TurnFailed { .. }
                        | HarnessEvent::TurnInterrupted
                ) {
                    terminal = true;
                }
            }
            if terminal {
                return Ok(());
            }
        }
    }
}

#[async_trait]
impl HarnessSession for OpencodeSession {
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, HarnessError> {
        let session_id = self.resume_ref.lock().expect("opencode resume").clone();
        let Some(session_id) = session_id else {
            return Err(HarnessError::Other("session has no resume ref".into()));
        };
        let path = format!("/session/{session_id}/prompt_async");
        let url = format!("{}{path}", self.base_url);
        let body = json!({
            "parts": [{ "type": "text", "text": input.text }]
        });
        let query = self.directory_query();
        let (status, parsed) = self
            .http("POST", &path, &url, Some(body), Some(query.as_slice()))
            .await?;
        self.emit_http_in(&path, status, parsed).await;
        if status != 204 && !(200..300).contains(&status) {
            return Err(HarnessError::Other(format!("POST {path} status {status}")));
        }
        // Long-lived server child: its exit is a session-level failure, not a
        // turn outcome.
        self.read_until_terminal_turn().await?;
        Ok(TurnOutcome::Clean)
    }

    async fn decide(
        &self,
        approval: HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let path = format!("/permission/{}/reply", approval.call_id);
        let url = format!("{}{path}", self.base_url);
        let body = match &decision {
            ApprovalDecision::Approve => json!({ "reply": "once" }),
            ApprovalDecision::Deny { feedback } => {
                let mut value = json!({ "reply": "reject" });
                if let Some(message) = feedback {
                    value["message"] = json!(message);
                }
                value
            }
        };
        let query = self.directory_query();
        let (status, parsed) = self
            .http("POST", &path, &url, Some(body), Some(query.as_slice()))
            .await?;
        self.emit_http_in(&path, status, parsed).await;
        if !(200..300).contains(&status) {
            return Err(HarnessError::Other(format!("POST {path} status {status}")));
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<(), HarnessError> {
        let session_id = self.resume_ref.lock().expect("opencode resume").clone();
        if let Some(session_id) = session_id {
            let path = format!("/session/{session_id}/abort");
            let url = format!("{}{path}", self.base_url);
            let query = self.directory_query();
            let result = self
                .http("POST", &path, &url, None, Some(query.as_slice()))
                .await;
            if let Ok((status, parsed)) = result {
                self.emit_http_in(&path, status, parsed).await;
                if (200..300).contains(&status) {
                    return Ok(());
                }
            }
        }
        let mut slot = self.child.lock().await;
        let Some(child) = slot.as_mut() else {
            return Ok(());
        };
        child.interrupt(Duration::from_secs(2)).await?;
        *slot = None;
        self.child_pid.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn resume_ref(&self) -> Option<String> {
        self.resume_ref.lock().expect("opencode resume").clone()
    }

    fn child_pid(&self) -> Option<i64> {
        let pid = self.child_pid.load(Ordering::SeqCst);
        (pid != 0).then_some(i64::from(pid))
    }

    fn unrecognized_events(&self) -> u64 {
        // One long-lived parser per session, so its own count is already
        // cumulative.
        self.parser.lock().expect("opencode parser").unrecognized()
    }

    async fn shutdown(self: Box<Self>) -> Result<(), HarnessError> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.terminate().await;
        }
        Ok(())
    }
}

fn http_status_error(method: &str, path: &str, status: u16, body: &Value) -> HarnessError {
    let detail = http_error_detail(body);
    if detail.is_empty() {
        HarnessError::Other(format!("{method} {path} status {status}"))
    } else {
        HarnessError::Other(format!("{method} {path} status {status}: {detail}"))
    }
}

fn http_error_detail(body: &Value) -> String {
    if let Some(message) = body
        .pointer("/message")
        .or_else(|| body.pointer("/error"))
        .and_then(Value::as_str)
    {
        return message.to_owned();
    }
    if body.is_null() {
        return String::new();
    }
    let rendered = body.to_string();
    const CAP: usize = 200;
    if rendered.len() > CAP {
        format!("{}…", &rendered[..CAP])
    } else {
        rendered
    }
}

fn sse_data_line(line: &str) -> Option<String> {
    let line = line.trim_end();
    let payload = line.strip_prefix("data:")?;
    let payload = payload.strip_prefix(' ').unwrap_or(payload);
    if payload.is_empty() {
        return None;
    }
    Some(payload.to_owned())
}

async fn drain_capped<R>(mut reader: R, cap: usize) -> String
where
    R: AsyncReadExt + Unpin,
{
    let mut out = Vec::new();
    let mut buf = [0_u8; 4_096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let room = cap - out.len();
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_plan_is_clean() {
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            None,
        )
        .unwrap();
        assert_eq!(
            plan.argv,
            [
                "/usr/bin/opencode",
                "serve",
                "--hostname",
                "127.0.0.1",
                "--port",
                "4096"
            ]
        );
        validate_launch_plan(&plan).unwrap();
        assert!(!plan.argv.iter().any(|arg| arg == "--auto"));
    }

    #[test]
    fn extra_auto_flag_is_rejected() {
        let err = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &["--auto".into()],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::LaunchRejected(_)));
    }

    #[test]
    fn extra_bypass_flag_is_rejected() {
        let err = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &["--dangerously-skip-permissions".into()],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::LaunchRejected(_)));
    }

    #[test]
    fn permission_mode_mapping_matches_0033() {
        assert_eq!(
            session_create_body(CodePermissionMode::Plan, None)["agent"],
            "plan"
        );
        assert_eq!(
            session_create_body(CodePermissionMode::Ask, None)["agent"],
            "build"
        );
        assert_eq!(
            session_create_body(CodePermissionMode::Auto, None)["agent"],
            "build"
        );
        let ask = session_create_body(CodePermissionMode::Ask, None);
        let rules = ask["permission"].as_array().unwrap();
        assert!(rules
            .iter()
            .any(|rule| { rule["permission"] == "bash" && rule["action"] == "ask" }));
        let auto = session_create_body(CodePermissionMode::Auto, None);
        let rules = auto["permission"].as_array().unwrap();
        assert!(rules
            .iter()
            .any(|rule| { rule["permission"] == "edit" && rule["action"] == "allow" }));
        assert!(rules
            .iter()
            .any(|rule| { rule["permission"] == "bash" && rule["action"] == "ask" }));
        let allow = session_create_body(CodePermissionMode::Allow, None);
        assert_eq!(allow["agent"], "build");
        let rules = allow["permission"].as_array().unwrap();
        assert!(rules.iter().all(|rule| rule["action"] == "allow"));
        assert!(rules
            .iter()
            .any(|rule| { rule["permission"] == "bash" && rule["action"] == "allow" }));
        assert!(rules.iter().any(|rule| {
            rule["permission"] == "external_directory" && rule["action"] == "allow"
        }));
    }

    #[test]
    fn session_model_uses_provider_and_id() {
        let slash = session_create_body(CodePermissionMode::Plan, Some("anthropic/claude-opus-5"));
        assert_eq!(slash["model"]["providerID"], "anthropic");
        assert_eq!(slash["model"]["id"], "claude-opus-5");
        assert!(slash["model"].get("modelID").is_none());

        let bare = session_create_body(CodePermissionMode::Allow, Some("gpt-5.6-sol"));
        assert_eq!(bare["model"]["providerID"], "openai");
        assert_eq!(bare["model"]["id"], "gpt-5.6-sol");

        let grok = session_create_body(CodePermissionMode::Ask, Some("grok-4.5"));
        assert_eq!(grok["model"]["providerID"], "xai");
        assert_eq!(grok["model"]["id"], "grok-4.5");

        let gemini = session_create_body(CodePermissionMode::Plan, Some("gemini-3-pro"));
        assert_eq!(gemini["model"]["providerID"], "google");

        let pickle = session_create_body(CodePermissionMode::Plan, Some("big-pickle"));
        assert_eq!(pickle["model"]["providerID"], "opencode");
        assert_eq!(pickle["model"]["id"], "big-pickle");

        let gateway = session_create_body(
            CodePermissionMode::Plan,
            Some("model-gateway/claude-opus-5"),
        );
        assert_eq!(gateway["model"]["providerID"], "anthropic");
        assert_eq!(gateway["model"]["id"], "claude-opus-5");

        let unknown = session_create_body(CodePermissionMode::Plan, Some("mystery-weights"));
        assert!(unknown.get("model").is_none());
    }

    #[test]
    fn browser_present_adds_opencode_config_with_tb_browser() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            Some(&spec),
        )
        .unwrap();
        let config_str = plan
            .env
            .iter()
            .find(|(key, _)| key == OPENCODE_CONFIG_CONTENT)
            .map(|(_, value)| value.as_str())
            .expect("OPENCODE_CONFIG_CONTENT must be set");
        let config: serde_json::Value = serde_json::from_str(config_str).unwrap();
        assert_eq!(config["mcp"]["tb-browser"]["type"], "local");
        let cmd = config["mcp"]["tb-browser"]["command"].as_array().unwrap();
        assert_eq!(cmd[0], "/usr/local/bin/tidebreak");
        assert_eq!(cmd[1], "browser-mcp");
    }

    #[test]
    fn browser_absent_does_not_add_opencode_config() {
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            None,
        )
        .unwrap();
        assert!(
            !plan
                .env
                .iter()
                .any(|(key, _)| key == OPENCODE_CONFIG_CONTENT),
            "no OPENCODE_CONFIG_CONTENT when browser is absent"
        );
    }

    #[test]
    fn merge_preserves_existing_mcp_entries() {
        let existing = serde_json::json!({
            "mcp": { "other-server": { "type": "local", "command": ["foo"] } }
        })
        .to_string();
        let merged = merge_browser_mcp(
            Some(&existing),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert!(config["mcp"].get("other-server").is_some());
        assert!(config["mcp"].get("tb-browser").is_some());
    }

    #[test]
    fn merge_rejects_malformed_config() {
        let result = merge_browser_mcp(
            Some("not valid json"),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn merge_rejects_non_object_config() {
        let result = merge_browser_mcp(
            Some("[1, 2, 3]"),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn merge_rejects_conflicting_tb_browser() {
        let existing = serde_json::json!({
            "mcp": { "tb-browser": { "type": "local", "command": ["different"] } }
        })
        .to_string();
        let result = merge_browser_mcp(
            Some(&existing),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn merge_idempotent_for_identical_tb_browser() {
        let bridge = std::path::Path::new("/usr/local/bin/tidebreak");
        let first = merge_browser_mcp(None, bridge).unwrap();
        let second = merge_browser_mcp(Some(&first), bridge).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn bridge_command_with_spaces_remains_one_array_element() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("/tmp/browser-cap.json"),
            std::path::PathBuf::from("/Applications/Tidebreak.app/Contents/bin/tidebreak"),
        );
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            Some(&spec),
        )
        .unwrap();
        let config_str = plan
            .env
            .iter()
            .find(|(key, _)| key == OPENCODE_CONFIG_CONTENT)
            .map(|(_, value)| value.as_str())
            .unwrap();
        let config: serde_json::Value = serde_json::from_str(config_str).unwrap();
        let cmd = config["mcp"]["tb-browser"]["command"].as_array().unwrap();
        assert_eq!(cmd.len(), 2);
        assert_eq!(cmd[0], "/Applications/Tidebreak.app/Contents/bin/tidebreak");
        assert_eq!(cmd[1], "browser-mcp");
    }

    #[test]
    fn capfile_path_is_not_in_config_json() {
        let capfile = std::path::PathBuf::from("/tmp/secret-cap-abc123.json");
        let spec = BrowserChannelSpec::new(
            capfile.clone(),
            std::path::PathBuf::from("/usr/local/bin/tidebreak"),
        );
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[],
            4096,
            Some(&spec),
        )
        .unwrap();
        let config_str = plan
            .env
            .iter()
            .find(|(key, _)| key == OPENCODE_CONFIG_CONTENT)
            .map(|(_, value)| value.as_str())
            .unwrap();
        let capfile_str = capfile.to_string_lossy();
        assert!(
            !config_str.contains(capfile_str.as_ref()),
            "capfile path must not appear in config JSON"
        );
    }

    #[test]
    fn merge_rejects_non_object_mcp_value() {
        let existing = serde_json::json!({
            "mcp": "not an object"
        })
        .to_string();
        let result = merge_browser_mcp(
            Some(&existing),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn merge_preserves_unrelated_top_level_keys() {
        let existing = serde_json::json!({
            "theme": "dark",
            "model": "claude-sonnet-5",
            "mcp": {
                "existing-server": {
                    "type": "local",
                    "command": ["existing"]
                }
            }
        })
        .to_string();
        let merged = merge_browser_mcp(
            Some(&existing),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["model"], "claude-sonnet-5");
        assert!(config["mcp"].get("existing-server").is_some());
        assert!(config["mcp"].get("tb-browser").is_some());
    }

    #[test]
    fn merge_creates_mcp_key_when_absent() {
        let existing = serde_json::json!({
            "theme": "dark"
        })
        .to_string();
        let merged = merge_browser_mcp(
            Some(&existing),
            std::path::Path::new("/usr/local/bin/tidebreak"),
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(config["theme"], "dark");
        assert!(config["mcp"].get("tb-browser").is_some());
    }

    #[test]
    fn existing_config_passthrough_when_browser_none() {
        let existing = serde_json::json!({
            "mcp": { "other-server": { "type": "local", "command": ["foo"] } }
        })
        .to_string();
        let plan = compose_serve_plan(
            std::path::Path::new("/usr/bin/opencode"),
            &[],
            std::path::Path::new("/workspace"),
            &[("OPENCODE_CONFIG_CONTENT".to_owned(), existing.clone())],
            4096,
            None,
        )
        .unwrap();
        let config_str = plan
            .env
            .iter()
            .find(|(key, _)| key == OPENCODE_CONFIG_CONTENT)
            .map(|(_, value)| value.as_str())
            .expect("existing config must survive when browser is None");
        let config: serde_json::Value = serde_json::from_str(config_str).unwrap();
        assert!(config["mcp"].get("other-server").is_some());
        assert!(config["mcp"].get("tb-browser").is_none());
    }

    #[test]
    fn bridge_command_with_windows_backslash_path() {
        let spec = BrowserChannelSpec::new(
            std::path::PathBuf::from("C:\\Temp\\browser-cap.json"),
            std::path::PathBuf::from("C:\\Program Files\\Tidebreak\\tidebreak.exe"),
        );
        let entry = browser_mcp_config_json(spec.bridge_command());
        let cmd = entry["command"].as_array().unwrap();
        assert_eq!(cmd.len(), 2);
        assert_eq!(cmd[0], "C:\\Program Files\\Tidebreak\\tidebreak.exe");
        assert_eq!(cmd[1], "browser-mcp");
        assert_eq!(entry["type"], "local");
    }
}

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

use crate::launch::{validate_launch_plan, LaunchPlan};
use crate::opencode::parse::OpencodeStreamParser;
use crate::{
    filter_child_env, spawn_process_tree, ApprovalDecision, HarnessApprovalRef, HarnessError,
    HarnessEvent, HarnessSession, ProcessTreeChild, SessionSpec, StreamBudget, StreamLineBuffer,
    TurnInput, TurnOutcome,
};
use tidebreak_core::CodePermissionMode;

const READY_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_STDERR_BYTES: usize = 64 * 1_024;
const AUTO_FLAG: &str = "--auto";

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

/// Argv for the long-lived serve child. Prompt never appears here.
pub(crate) fn compose_serve_plan(
    binary: &std::path::Path,
    extra_argv: &[String],
    cwd: &std::path::Path,
    extra_env: &[(String, String)],
    port: u16,
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
    env.retain(|(key, _)| !key.to_ascii_uppercase().starts_with("TIDEBREAK_") && key != "PWD");
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
                {"permission": "read", "pattern": "*", "action": "allow"}
            ]
        }),
    };
    if let Some(model) = model {
        body["model"] = match model.split_once('/') {
            Some((provider, model_id)) => json!({
                "providerID": provider,
                "modelID": model_id,
            }),
            None => json!({ "modelID": model }),
        };
    }
    body
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
    )?;
    let mut command = Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    for (key, value) in filter_child_env(session.spec.env.iter().cloned()) {
        command.env(key, value);
    }
    for (key, value) in &plan.env {
        command.env(key, value);
    }
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
        if !(200..300).contains(&status) {
            return Err(HarnessError::Other(format!(
                "POST /session status {status}"
            )));
        }
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            *self.resume_ref.lock().expect("opencode resume") = Some(id.to_owned());
        }
        self.emit_http_in(path, status, parsed).await;
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
    }
}

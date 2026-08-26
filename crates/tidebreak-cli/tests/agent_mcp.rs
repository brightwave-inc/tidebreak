//! End-to-end tests of `tidebreak agent-mcp` over a real `tidebreak serve`.
//!
//! The child speaks MCP on stdio (initialize → initialized → tools/list →
//! tools/call). The server is the production engine with the feature-gated
//! scripted provider, so the attach contract is the real one.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TURN_TIMEOUT: Duration = Duration::from_secs(60);

struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

struct Mcp {
    _reaper: Reaper,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Mcp {
    fn connect(url: &str, token: &str, data_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
            .args(["agent-mcp", "--server", url])
            .env("TIDEBREAK_SERVER_TOKEN", token)
            .env("TIDEBREAK_DATA_DIR", data_dir)
            .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
            .env_remove("TIDEBREAK_SERVER_URL")
            .env_remove("TIDEBREAK_MCP_CONFIG")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env("NO_PROXY", "*")
            .env("no_proxy", "*")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tidebreak agent-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let stderr = child.stderr.take().expect("stderr");
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = BufReader::new(stderr).read_to_end(&mut sink);
        });
        let mut mcp = Self {
            _reaper: Reaper(child),
            stdin,
            stdout,
            next_id: 1,
        };
        mcp.handshake();
        mcp
    }

    fn handshake(&mut self) {
        let listed = self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "agent-mcp-test", "version": "1"},
            }),
        );
        assert!(
            listed["result"]["capabilities"]["tools"].is_object(),
            "initialize: {listed}"
        );
        self.notify("notifications/initialized", json!({}));
        let tools = self.rpc("tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name"))
            .collect();
        for expected in [
            "chat_create",
            "chat_list",
            "chat_status",
            "chat_run_turn",
            "chat_wait",
            "chat_decide",
            "chat_events",
            "chat_steer",
            "chat_cancel",
            "profile_snapshot",
            "model_role_set",
            "web_search_select",
            "exec_select",
            "chat_set_model",
            "chat_set_permission_mode",
            "chat_attach_file",
            "chat_outputs",
            "chat_output_read",
            "agent_runs",
            "agent_run_cancel",
        ] {
            assert!(
                names.contains(&expected),
                "missing {expected} in {names:?}\n{tools}"
            );
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        let line = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{line}").expect("write notify");
        self.stdin.flush().expect("flush notify");
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{line}").expect("write rpc");
        self.stdin.flush().expect("flush rpc");
        let started = Instant::now();
        let mut response = String::new();
        loop {
            response.clear();
            let n = self.stdout.read_line(&mut response).expect("read rpc");
            assert!(n > 0, "agent-mcp closed stdout after {method}");
            assert!(
                started.elapsed() < TURN_TIMEOUT,
                "timed out waiting for {method}: {response}"
            );
            let parsed: Value =
                serde_json::from_str(&response).unwrap_or_else(|_| panic!("rpc line: {response}"));
            if parsed.get("id") == Some(&json!(id)) {
                return parsed;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.rpc(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        );
        assert_eq!(
            response["result"]["isError"], false,
            "{name} failed: {response}"
        );
        response["result"]["structuredContent"].clone()
    }
}

fn spawn_serve(dir: &Path, extra_env: &[(&str, &str)]) -> (Reaper, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
    command
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir)
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "*")
        .env("no_proxy", "*");
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve");
    let stdout = child.stdout.take().unwrap();
    let reaper = Reaper(child);
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    let url = format!(
        "http://{}",
        addr_line.rsplit("http://").next().unwrap().trim()
    );
    let token = token_line
        .strip_prefix("tidebreak: token ")
        .expect("token line")
        .trim()
        .to_owned();
    (reaper, url, token)
}

/// (a) chat_create then chat_run_turn to completion with assistant text.
/// (c) chat_events returns frames after a cursor.
#[test]
fn create_run_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!([{"text": "hello from the script"}]).to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let created = mcp.call("chat_create", json!({}));
    let chat_id = created["chat_id"].as_str().expect("chat_id").to_owned();

    let turn = mcp.call(
        "chat_run_turn",
        json!({
            "chat_id": chat_id,
            "prompt": "say hello",
        }),
    );
    assert_eq!(turn["status"], "completed", "{turn}");
    assert_eq!(turn["assistant_text"], "hello from the script", "{turn}");
    let cursor = turn["events_cursor"].as_i64().expect("cursor");
    assert!(cursor > 0, "{turn}");

    let events = mcp.call(
        "chat_events",
        json!({
            "chat_id": chat_id,
            "after_seq": 0,
        }),
    );
    let frames = events["events"].as_array().expect("events");
    assert!(
        frames.iter().any(|frame| {
            frame.pointer("/event/type").and_then(Value::as_str) == Some("turn_completed")
        }),
        "{events}"
    );
    let mid = frames[0]["seq"].as_i64().expect("first seq");
    let later = mcp.call(
        "chat_events",
        json!({
            "chat_id": chat_id,
            "after_seq": mid,
            "max_events": 50,
        }),
    );
    let later_frames = later["events"].as_array().expect("later events");
    assert!(
        later_frames
            .iter()
            .all(|frame| frame["seq"].as_i64().is_some_and(|seq| seq > mid)),
        "{later}"
    );
    assert!(
        later_frames.len() < frames.len(),
        "after_seq must drop earlier frames: first={} later={}",
        frames.len(),
        later_frames.len()
    );
}

/// (b) a scripted exec approval: chat_run_turn returns needs_approval with the
/// call_id, chat_decide approve settles the turn.
#[test]
fn approval_then_decide() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!([
        {
            "tool": "exec",
            "input": {"summary": "Run a command", "command": "true"}
        },
        {"text": "ran it"}
    ])
    .to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let created = mcp.call("chat_create", json!({ "permission_mode": "ask" }));
    let chat_id = created["chat_id"].as_str().expect("chat_id").to_owned();

    let parked = mcp.call(
        "chat_run_turn",
        json!({
            "chat_id": chat_id,
            "prompt": "run the command",
        }),
    );
    assert_eq!(parked["status"], "needs_approval", "{parked}");
    let call_id = parked["pending"]["call_id"]
        .as_str()
        .expect("pending call_id")
        .to_owned();

    let settled = mcp.call(
        "chat_decide",
        json!({
            "chat_id": chat_id,
            "decision": {
                "type": "approval",
                "call_id": call_id,
                "decision": "approve",
            },
        }),
    );
    assert_eq!(settled["status"], "completed", "{settled}");
    assert_eq!(settled["assistant_text"], "ran it", "{settled}");
}

/// (d) a short timeout returns status running and a later chat_wait completes
/// the same turn.
#[test]
fn short_timeout_then_wait() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!([{
        "text": "later",
        "delay_ms": 2500
    }])
    .to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let created = mcp.call("chat_create", json!({}));
    let chat_id = created["chat_id"].as_str().expect("chat_id").to_owned();

    let running = mcp.call(
        "chat_run_turn",
        json!({
            "chat_id": chat_id,
            "prompt": "take your time",
            "timeout_seconds": 1,
        }),
    );
    assert_eq!(running["status"], "running", "{running}");

    let finished = mcp.call(
        "chat_wait",
        json!({
            "chat_id": chat_id,
            "timeout_seconds": 15,
        }),
    );
    assert_eq!(finished["status"], "completed", "{finished}");
    assert_eq!(finished["assistant_text"], "later", "{finished}");
}

/// (a) profile_snapshot returns the profile axes and contains no key material.
#[test]
fn profile_snapshot_returns_axes_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!([{"text": "unused"}]).to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let snapshot = mcp.call("profile_snapshot", json!({}));
    assert!(snapshot.get("settings").is_some(), "{snapshot}");
    assert!(snapshot["providers"].is_array(), "{snapshot}");
    assert!(snapshot["models"].is_array(), "{snapshot}");
    assert!(snapshot["roles"].is_array(), "{snapshot}");
    assert!(snapshot.get("web_search").is_some(), "{snapshot}");
    assert!(snapshot.get("exec").is_some(), "{snapshot}");

    let roles = snapshot["roles"].as_array().expect("roles");
    assert!(
        roles.iter().any(|role| role["role"] == "chat"),
        "{snapshot}"
    );
    for provider in snapshot["providers"].as_array().expect("providers") {
        assert!(provider.get("id").is_some(), "{provider}");
        assert!(provider.get("kind").is_some(), "{provider}");
        assert!(provider.get("has_credential").is_some(), "{provider}");
        assert!(provider.get("credential").is_none(), "{provider}");
        assert!(provider.get("api_key").is_none(), "{provider}");
        assert!(provider.get("base_url").is_none(), "{provider}");
    }

    let dumped = snapshot.to_string();
    for needle in [
        "\"api_key\"",
        "\"credential\"",
        "\"token\"",
        "\"secret\"",
        "\"password\"",
        "sk-",
    ] {
        assert!(
            !dumped.contains(needle),
            "snapshot leaked {needle}: {snapshot}"
        );
    }
}

/// (b) chat_set_permission_mode then a scripted approval-requiring turn parks.
#[test]
fn set_permission_mode_then_approval_parks() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!([
        {
            "tool": "exec",
            "input": {"summary": "Run a command", "command": "true"}
        },
        {"text": "ran it"}
    ])
    .to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let created = mcp.call("chat_create", json!({ "permission_mode": "allow" }));
    let chat_id = created["chat_id"].as_str().expect("chat_id").to_owned();
    let set = mcp.call(
        "chat_set_permission_mode",
        json!({
            "chat_id": chat_id,
            "permission_mode": "ask",
        }),
    );
    assert_eq!(set["permission_mode"], "ask", "{set}");

    let parked = mcp.call(
        "chat_run_turn",
        json!({
            "chat_id": chat_id,
            "prompt": "run the command",
        }),
    );
    assert_eq!(parked["status"], "needs_approval", "{parked}");
    assert!(parked["pending"]["call_id"].as_str().is_some(), "{parked}");
}

/// (c) chat_attach_file then a turn that reads the attached document.
#[test]
fn attach_file_then_turn_reads_document() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.md");
    std::fs::write(&source, "alpha-marker-42 in the attached notes\n").unwrap();
    let script = json!([
        {"tool": "list_documents", "input": {}},
        {"text": "I read the attached notes"}
    ])
    .to_string();
    let (_server, url, token) =
        spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let created = mcp.call("chat_create", json!({}));
    let chat_id = created["chat_id"].as_str().expect("chat_id").to_owned();
    let attached = mcp.call(
        "chat_attach_file",
        json!({
            "chat_id": chat_id,
            "path": source.to_string_lossy(),
        }),
    );
    let document_id = attached["id"].as_str().expect("attach id").to_owned();
    assert_eq!(attached["kind"], "document", "{attached}");

    let turn = mcp.call(
        "chat_run_turn",
        json!({
            "chat_id": chat_id,
            "prompt": "read the attached notes",
        }),
    );
    assert_eq!(turn["status"], "completed", "{turn}");
    assert_eq!(
        turn["assistant_text"], "I read the attached notes",
        "{turn}"
    );

    let events = mcp.call(
        "chat_events",
        json!({
            "chat_id": chat_id,
            "after_seq": 0,
        }),
    );
    let frames = events["events"].as_array().expect("events");
    let listed = frames.iter().any(|frame| {
        let event = frame.get("event").unwrap_or(frame);
        event.get("type").and_then(Value::as_str) == Some("tool_call_completed")
            && event
                .pointer("/result")
                .map(|result| result.to_string())
                .is_some_and(|dump| dump.contains("notes.md") || dump.contains(&document_id))
    });
    assert!(
        listed,
        "list_documents did not surface the attached file {document_id}: {events}"
    );
}

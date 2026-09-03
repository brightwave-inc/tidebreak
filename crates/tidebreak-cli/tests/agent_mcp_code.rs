//! End-to-end tests of `tidebreak agent-mcp` code-mode tools over a real
//! `tidebreak serve` with the debug-only scripted harness.

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
                "clientInfo": {"name": "agent-mcp-code-test", "version": "1"},
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
            "code_harnesses",
            "code_repo_add",
            "code_run_turn",
            "code_wait",
            "code_decide",
            "code_diff",
            "code_git_status",
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

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    std::fs::write(path.join("README.md"), "demo\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args([
            "-c",
            "user.name=Tidebreak",
            "-c",
            "user.email=tidebreak@example.test",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
}

fn plain_events() -> Value {
    json!([
        {
            "type": "session_started",
            "harness_kind": "claude_code",
            "harness_version": "scripted",
            "resume_ref": "scripted-session"
        },
        {"type": "turn_started"},
        {"type": "assistant_delta", "text": "hello from the scripted engine"},
        {"type": "turn_completed", "usage": {}}
    ])
}

fn open_session(mcp: &mut Mcp, repo_dir: &Path, permission_mode: &str) -> (String, String) {
    let repo = mcp.call(
        "code_repo_add",
        json!({
            "source": repo_dir.to_str().unwrap(),
            "name": "sample",
        }),
    );
    let repo_id = repo["id"].as_str().expect("repo id").to_owned();
    let workspace = mcp.call(
        "code_workspace_create",
        json!({
            "repo_id": repo_id,
            "name": "ws",
        }),
    );
    let workspace_id = workspace["id"].as_str().expect("workspace id").to_owned();
    let session = mcp.call(
        "code_session_create",
        json!({
            "workspace_id": workspace_id,
            "harness": "claude_code",
            "permission_mode": permission_mode,
        }),
    );
    (
        workspace_id,
        session["id"].as_str().expect("session id").to_owned(),
    )
}

/// (a) repo → workspace → session → code_run_turn to completion.
#[test]
fn repo_workspace_session_run_to_completion() {
    let dir = tempfile::tempdir().unwrap();
    let script = plain_events().to_string();
    let (_server, url, token) = spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_HARNESS", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let repo_dir = dir.path().join("sample-repo");
    init_git_repo(&repo_dir);
    let (_workspace_id, session_id) = open_session(&mut mcp, &repo_dir, "plan");

    let turn = mcp.call(
        "code_run_turn",
        json!({
            "session_id": session_id,
            "prompt": "say hello",
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(turn["status"], "completed", "{turn}");
    assert_eq!(
        turn["assistant_text"], "hello from the scripted engine",
        "{turn}"
    );
}

/// (b) a scripted approval: code_run_turn returns needs_approval, code_decide
/// approve settles.
#[test]
fn approval_then_decide() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!({
        "approvals": true,
        "events": [
            {
                "type": "session_started",
                "harness_kind": "claude_code",
                "harness_version": "scripted",
                "resume_ref": "scripted-session"
            },
            {"type": "turn_started"},
            {
                "type": "approval_requested",
                "harness_ref": {"call_id": "toolu_scripted"},
                "raw": {
                    "tool_name": "Write",
                    "input": {
                        "file_path": "/workspace/probe.txt",
                        "content": "hello"
                    },
                    "tool_use_id": "toolu_scripted"
                }
            },
            {"type": "assistant_delta", "text": "after the decision"},
            {"type": "turn_completed", "usage": {}}
        ]
    })
    .to_string();
    let (_server, url, token) = spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_HARNESS", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let repo_dir = dir.path().join("sample-repo");
    init_git_repo(&repo_dir);
    let (_workspace_id, session_id) = open_session(&mut mcp, &repo_dir, "ask");

    let parked = mcp.call(
        "code_run_turn",
        json!({
            "session_id": session_id,
            "prompt": "write the file",
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(parked["status"], "needs_approval", "{parked}");
    let approval_id = parked["pending"]["approval_id"]
        .as_str()
        .expect("pending approval_id")
        .to_owned();

    let settled = mcp.call(
        "code_decide",
        json!({
            "session_id": session_id,
            "decision": {
                "approval_id": approval_id,
                "decision": "approve",
            },
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(settled["status"], "completed", "{settled}");
    assert_eq!(settled["assistant_text"], "after the decision", "{settled}");
}

/// (c) submit while a turn runs → status queued, then code_wait drains both.
#[test]
fn queued_then_wait_drains_both() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!({
        "turn_delay_ms": 2500,
        "events": [
            {
                "type": "session_started",
                "harness_kind": "claude_code",
                "harness_version": "scripted",
                "resume_ref": "scripted-session"
            },
            {"type": "turn_started"},
            {"type": "assistant_delta", "text": "later"},
            {"type": "turn_completed", "usage": {}}
        ]
    })
    .to_string();
    let (_server, url, token) = spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_HARNESS", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let repo_dir = dir.path().join("sample-repo");
    init_git_repo(&repo_dir);
    let (_workspace_id, session_id) = open_session(&mut mcp, &repo_dir, "plan");

    let running = mcp.call(
        "code_run_turn",
        json!({
            "session_id": session_id,
            "prompt": "first",
            "timeout_seconds": 1,
        }),
    );
    assert_eq!(running["status"], "running", "{running}");

    let queued = mcp.call(
        "code_run_turn",
        json!({
            "session_id": session_id,
            "prompt": "second",
            "timeout_seconds": 5,
        }),
    );
    assert_eq!(queued["status"], "queued", "{queued}");
    assert!(queued["turn_id"].is_string(), "{queued}");
    assert!(queued["queue_position"].is_number(), "{queued}");

    let finished = mcp.call(
        "code_wait",
        json!({
            "session_id": session_id,
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(finished["status"], "completed", "{finished}");
    assert_eq!(finished["assistant_text"], "later", "{finished}");

    let turns = mcp.call("code_turns", json!({ "session_id": session_id }));
    let history = turns["turns"].as_array().expect("turns");
    assert_eq!(history.len(), 2, "{turns}");
    assert!(
        history.iter().all(|turn| turn["status"] == "completed"),
        "{turns}"
    );
}

/// (d) code_diff / code_git_status return sane shapes after a scripted edit.
#[test]
fn diff_and_git_status_after_scripted_edit() {
    let dir = tempfile::tempdir().unwrap();
    let script = json!({
        "writes": [{"path": "edited.txt", "contents": "scripted edit\n"}],
        "events": [
            {
                "type": "session_started",
                "harness_kind": "claude_code",
                "harness_version": "scripted",
                "resume_ref": "scripted-session"
            },
            {"type": "turn_started"},
            {"type": "assistant_delta", "text": "edited"},
            {"type": "turn_completed", "usage": {}}
        ]
    })
    .to_string();
    let (_server, url, token) = spawn_serve(dir.path(), &[("TIDEBREAK_SCRIPTED_HARNESS", &script)]);
    let mut mcp = Mcp::connect(&url, &token, dir.path());

    let repo_dir = dir.path().join("sample-repo");
    init_git_repo(&repo_dir);
    let (workspace_id, session_id) = open_session(&mut mcp, &repo_dir, "plan");

    let turn = mcp.call(
        "code_run_turn",
        json!({
            "session_id": session_id,
            "prompt": "edit the file",
            "timeout_seconds": 30,
        }),
    );
    assert_eq!(turn["status"], "completed", "{turn}");

    let diff = mcp.call("code_diff", json!({ "workspace_id": workspace_id }));
    let diff_text = diff["diff"].as_str().unwrap_or("");
    assert!(
        diff_text.contains("edited.txt") || diff_text.contains("scripted edit"),
        "{diff}"
    );
    assert!(diff["stat"].is_object(), "{diff}");

    let status = mcp.call("code_git_status", json!({ "workspace_id": workspace_id }));
    assert_eq!(status["dirty"], true, "{status}");
    assert!(status["suggested_commit_message"].is_string(), "{status}");
}

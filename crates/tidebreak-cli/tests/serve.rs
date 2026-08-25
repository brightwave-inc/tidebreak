//! End-to-end tests of the `tidebreak` process surfaces.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(15);
/// A turn boots the engine and runs the tool loop, so it is given more room
/// than a process that only has to refuse and exit.
const TURN_EXIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Kills the daemon on drop — including on an assertion panic, since
/// `std::process::Child` does not reap on its own.
struct Reaper(Child);

impl Reaper {
    fn wait_status(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_with_output(&mut self, timeout: Duration) -> Output {
        let status = self.wait_status(timeout);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        if let Some(mut pipe) = self.0.stdout.take() {
            pipe.read_to_end(&mut stdout).unwrap();
        }
        if let Some(mut pipe) = self.0.stderr.take() {
            pipe.read_to_end(&mut stderr).unwrap();
        }
        Output {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[test]
fn serve_announces_its_address_and_answers_health() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve");

    let stdout = child.stdout.take().unwrap();
    let _reaper = Reaper(child);

    // The address is printed only after the listener is bound, so reading these
    // two lines also synchronizes the test with a server ready to accept.
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();

    assert!(
        addr_line.contains("listening on http://127.0.0.1:"),
        "unexpected addr line: {addr_line:?}"
    );
    assert!(
        token_line.starts_with("tidebreak: token "),
        "unexpected token line: {token_line:?}"
    );

    let addr = addr_line.rsplit("http://").next().unwrap().trim();
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(response.contains("200 OK"), "response: {response}");
    assert!(response.trim_end().ends_with("ok"), "response: {response}");
}

#[test]
fn serve_mounts_external_mcp_servers_from_boot_config() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("mcp-workspace");
    std::fs::create_dir(&workspace).unwrap();
    let config_path = dir.path().join("mcp.json");
    let config = serde_json::json!({
        "servers": [{
            "name": "fixture",
            "command": env!("CARGO_BIN_EXE_tidebreak"),
            "args": ["mcp", workspace],
            "request_timeout_ms": 5_000
        }]
    });
    std::fs::write(&config_path, config.to_string()).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir.path().join("data"))
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env("TIDEBREAK_MCP_CONFIG", &config_path)
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn configured tidebreak serve");

    let stdout = child.stdout.take().unwrap();
    let _reaper = Reaper(child);
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    assert!(addr_line.contains("listening on http://127.0.0.1:"));
    assert!(token_line.starts_with("tidebreak: token "));
}

#[test]
fn serve_fails_closed_when_a_configured_mcp_server_cannot_start() {
    let dir = tempfile::tempdir().unwrap();
    let missing_command = dir.path().join("missing-mcp-server");
    let config_path = dir.path().join("mcp.json");
    let config = serde_json::json!({
        "servers": [{
            "name": "broken",
            "command": missing_command
        }]
    });
    std::fs::write(&config_path, config.to_string()).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir.path().join("data"))
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env("TIDEBREAK_MCP_CONFIG", &config_path)
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve with broken MCP config");
    let mut child = Reaper(child);
    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("external MCP server broken failed to start"),
        "stderr: {stderr}"
    );
}

#[test]
fn serve_fails_closed_when_a_selected_mcp_environment_variable_is_missing() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_ENV_FROM_MUST_NOT_EXIST_46F54489";
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("mcp.json");
    let config = serde_json::json!({
        "servers": [{
            "name": "protected",
            "command": env!("CARGO_BIN_EXE_tidebreak"),
            "args": ["mcp", dir.path()],
            "env_from": [MISSING]
        }]
    });
    std::fs::write(&config_path, config.to_string()).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir.path().join("data"))
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env("TIDEBREAK_MCP_CONFIG", &config_path)
        .env_remove(MISSING)
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve with missing MCP environment");
    let mut child = Reaper(child);
    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("external MCP server protected failed to start"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains(MISSING), "stderr: {stderr}");
    assert!(stderr.contains("is not set"), "stderr: {stderr}");
}

#[test]
fn mcp_serves_read_only_tools_with_protocol_pure_stdout() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("note.txt"), "workspace note").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("mcp")
        .arg(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak mcp");

    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"note.txt"}}}"#,
        "\n",
    );
    let mut child = Reaper(child);
    let mut stdin = child.0.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).unwrap();
    drop(stdin);

    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 3, "stdout: {stdout}");
    let tool_names: Vec<&str> = responses[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(tool_names, ["list_dir", "read_file"]);
    assert_eq!(
        responses[2]["result"]["content"][0]["text"],
        "workspace note"
    );
}

// Linux filesystems accept arbitrary non-NUL byte paths; macOS commonly rejects
// ill-formed UTF-8 names before the CLI can observe them.
#[cfg(target_os = "linux")]
#[test]
fn mcp_accepts_a_non_utf8_workspace_path() {
    use std::os::unix::ffi::OsStringExt;

    let parent = tempfile::tempdir().unwrap();
    let workspace = parent
        .path()
        .join(std::ffi::OsString::from_vec(b"workspace-\xff".to_vec()));
    std::fs::create_dir(&workspace).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("mcp")
        .arg(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak mcp with non-UTF-8 workspace");
    let mut child = Reaper(child);

    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

/// Print mode's contract at the process boundary: stdout is the output and
/// nothing else, and a turn that cannot run exits non-zero instead of hanging
/// on a prompt no one is there to answer. With no credential configured the
/// engine boots and refuses the message, which is the shortest path through
/// that boundary.
#[test]
fn print_mode_fails_with_clean_stdout_when_no_model_provider_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("-p")
        .arg("hello")
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak -p");
    let mut child = Reaper(child);

    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("provider") && stderr.contains("credential"),
        "stderr: {stderr}"
    );
}

/// The headless run record 7 exists for: one unattended `-p` turn, in `allow`
/// mode, on a data directory that has never been used, that calls a tool for
/// real and finishes. The model is scripted (`TIDEBREAK_SCRIPTED_PROVIDER`, see
/// `tidebreak-server`'s `scripted_provider` module) so the turn is deterministic
/// without egressing; everything under it — the embedded engine, the turn
/// worker, the tool registry, the journal, and the event stream the CLI reads
/// back — is the production path. Under `--output-format json` stdout *is* the
/// journal of this turn, so what the run printed is what was recorded.
#[test]
fn a_scripted_tool_using_turn_completes_in_allow_mode() {
    let dir = tempfile::tempdir().unwrap();
    let script = serde_json::json!([
        {
            "tool": "update_task_plan",
            "input": {"steps": [{"content": "Record the plan", "status": "in_progress"}]}
        },
        {"text": "plan recorded"}
    ]);
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "-p",
            "make a plan",
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
        ])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env("TIDEBREAK_SCRIPTED_PROVIDER", script.to_string())
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a scripted tidebreak -p");
    let mut child = Reaper(child);

    let output = child.wait_with_output(TURN_EXIT_TIMEOUT);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    // Each line is one journal frame: `seq` plus the event itself.
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .expect("every printed line is one JSON frame")["event"]
                .clone()
        })
        .collect();
    assert!(
        events.iter().any(
            |event| event["type"] == "tool_call_started" && event["name"] == "update_task_plan"
        ),
        "stdout: {stdout}"
    );
    let completed = events
        .iter()
        .find(|event| event["type"] == "tool_call_completed")
        .unwrap_or_else(|| panic!("the scripted tool call never completed\nstdout: {stdout}"));
    assert_eq!(completed["status"], "completed", "stdout: {stdout}");
    assert!(
        events.iter().any(|event| event["type"] == "turn_completed"),
        "stdout: {stdout}"
    );
}

/// Start `tidebreak serve` on `dir` and read back where it is and how to
/// authenticate. Both lines are printed only after the listener is bound, so
/// this also synchronizes with a server ready to accept.
fn spawn_serve(dir: &Path) -> (Reaper, String, String) {
    spawn_serve_with_env(dir, &[])
}

fn spawn_serve_with_env(dir: &Path, extra_env: &[(&str, &str)]) -> (Reaper, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
    command
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", dir)
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("ANTHROPIC_API_KEY");
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

/// Attach mode's whole point: a second process reaches a data directory the
/// first one owns by being its client, and the call is the real thing — a
/// route answering over HTTP against the running server's state. The attaching
/// process is pointed at the *same* data directory on purpose: a client that
/// quietly embedded a server of its own would hit the ownership guard and fail
/// here rather than pass by accident.
#[test]
fn a_setup_command_attaches_to_a_running_server() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, url, token) = spawn_serve(dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["provider", "list", "--output-format", "json", "--server"])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run an attached provider list");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "stderr: {stderr}");
    let listed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the route's answer on one line");
    assert!(
        listed["providers"]
            .as_array()
            .is_some_and(|providers| providers
                .iter()
                .any(|provider| provider["kind"] == "anthropic")),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// `--attach` reads the listen.json the server published into the data
/// directory — no token on argv, same HTTP client as `--server`.
#[test]
fn a_setup_command_attaches_via_listen_json() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, _url, _token) = spawn_serve(dir.path());

    let listen = dir.path().join("listen.json");
    assert!(
        listen.is_file(),
        "serve must publish listen.json for --attach"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["provider", "list", "--output-format", "json", "--attach"])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_SERVER_TOKEN")
        .stdin(Stdio::null())
        .output()
        .expect("run --attach provider list");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "stderr: {stderr}");
    let listed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the route's answer on one line");
    assert!(
        listed["providers"]
            .as_array()
            .is_some_and(|providers| providers
                .iter()
                .any(|provider| provider["kind"] == "anthropic")),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// The same rule for the families that read and write a conversation's files.
/// Their failure mode when `--server` is ignored is not a broken flag but a
/// silently wrong target: the command reads the local data directory while the
/// caller believes it reached the deployment. The client here is pointed at a
/// *different, empty* data directory from the one the server owns, so an
/// embedding command would answer about a database that has never heard of this
/// chat — only a command that actually attached can find it.
#[test]
fn the_output_and_attach_families_reach_the_attached_server() {
    let served = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (_server, url, token) = spawn_serve(served.path());
    let chat = create_chat(&url, &token);

    let source = elsewhere.path().join("contract.txt");
    std::fs::write(&source, b"the terms are as follows\n").unwrap();
    let attached = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["attach", &chat])
        .arg(&source)
        .arg("--server")
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run an attached attach");
    let stderr = String::from_utf8_lossy(&attached.stderr).into_owned();
    assert!(attached.status.success(), "stderr: {stderr}");
    let ingested = String::from_utf8_lossy(&attached.stdout).trim().to_owned();
    assert!(
        tidebreak_core::DocumentId::from_str(&ingested).is_ok(),
        "the new document's id belongs on stdout, got {ingested:?}"
    );

    let listed = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["output", "list", &chat, "--server"])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run an attached output list");
    let stderr = String::from_utf8_lossy(&listed.stderr).into_owned();
    assert!(listed.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("no outputs"),
        "the attached chat exists and has produced nothing yet: {stderr}"
    );
}

/// `output list --output-format json` writes one object on stdout — the same
/// single-value shape setup uses — so a driver never has to scrape TSV or the
/// empty-catalog banner on stderr.
#[test]
fn output_list_json_is_one_object_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, url, token) = spawn_serve(dir.path());
    let chat = create_chat(&url, &token);

    let listed = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "output",
            "list",
            &chat,
            "--output-format",
            "json",
            "--server",
        ])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run output list --output-format json");

    let stderr = String::from_utf8_lossy(&listed.stderr).into_owned();
    assert!(listed.status.success(), "stderr: {stderr}");
    let body: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("one JSON object on stdout");
    assert_eq!(
        body,
        serde_json::json!({ "deliverables": [], "truncated": false }),
        "stdout: {}",
        String::from_utf8_lossy(&listed.stdout)
    );
    assert!(
        stderr.is_empty(),
        "json mode keeps the empty-catalog note off stderr: {stderr}"
    );
}

/// Create a chat on the running server and return its id, so a command under
/// test has something on the *server's* side to name.
fn create_chat(url: &str, token: &str) -> String {
    let body = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            reqwest::Client::new()
                .post(format!("{url}/chats"))
                .bearer_auth(token)
                .json(&serde_json::json!({}))
                .send()
                .await
                .expect("create a chat")
                .error_for_status()
                .expect("the create route answers 2xx")
                .json::<serde_json::Value>()
                .await
                .expect("the created chat")
        });
    body["id"].as_str().expect("the chat's id").to_owned()
}

/// The other half of the same rule: a second process that tries to *embed* a
/// server over an owned data directory is refused before it touches the
/// database, and told what to do instead.
#[test]
fn a_second_process_embedding_the_same_data_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, _url, _token) = spawn_serve(dir.path());

    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["provider", "list"])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run a second embedding command");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("already running") && stderr.contains("--server"),
        "the refusal must name attach mode: {stderr}"
    );
}

/// The claim must be honest across a crash. A killed server runs no shutdown
/// path at all, so if ownership were recorded as something the next process
/// reads back, the directory would stay locked forever; the OS lock it actually
/// uses dies with the process that held it.
#[test]
fn a_killed_server_leaves_its_data_directory_usable() {
    let dir = tempfile::tempdir().unwrap();
    let (mut server, _url, _token) = spawn_serve(dir.path());
    // SIGKILL: no unwinding, no destructors, no clean shutdown.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    assert!(
        dir.path().join("tidebreak.lock").exists(),
        "the marker file outlives the process that made it"
    );

    let (_reclaimed, url, _token) = spawn_serve(dir.path());
    assert!(url.starts_with("http://127.0.0.1:"), "url: {url}");
}

/// A fresh `-p` must name the chat it created on stderr so the next invocation
/// can pass `--chat` — stdout stays the answer / journal alone.
#[test]
fn print_mode_prints_a_new_chat_id_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let script = serde_json::json!([{"text": "ok"}]);
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["-p", "hi", "--permission-mode", "allow"])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env("TIDEBREAK_SCRIPTED_PROVIDER", script.to_string())
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_SERVER_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak -p");
    let mut child = Reaper(child);

    let output = child.wait_with_output(TURN_EXIT_TIMEOUT);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let chat = stderr
        .lines()
        .find_map(|line| line.strip_prefix("tidebreak: chat "))
        .unwrap_or_else(|| panic!("new chat id missing from stderr: {stderr}"));
    assert!(
        tidebreak_core::ChatId::from_str(chat).is_ok(),
        "chat id on stderr: {chat:?}"
    );
    assert!(
        !stdout.contains(chat),
        "chat id must not leak onto stdout: {stdout}"
    );
}

/// `--model` is applied before the turn; a selection the server rejects must
/// fail the process without writing assistant text to stdout.
#[test]
fn print_mode_rejects_an_unknown_model_before_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "-p",
            "hello",
            "--model",
            "openai::definitely-not-a-real-model",
            "--permission-mode",
            "allow",
        ])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_SERVER_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak -p --model");
    let mut child = Reaper(child);

    let output = child.wait_with_output(PROCESS_EXIT_TIMEOUT);
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.to_lowercase().contains("model") || stderr.to_lowercase().contains("provider"),
        "stderr: {stderr}"
    );
}

/// A well-formed 1×1 PNG. Dimensions come from the header, so the pixels do
/// not need to be a real decode — they just have to sniff as `image/png`.
fn one_pixel_png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    bytes.extend_from_slice(&png_chunk(b"IDAT", &[]));
    bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
    bytes
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut payload = kind.to_vec();
    payload.extend_from_slice(data);
    let mut crc = 0xffff_ffffu32;
    for byte in &payload {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    crc = !crc;
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&payload);
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

fn get_json(url: &str, token: &str, path: &str) -> serde_json::Value {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            reqwest::Client::new()
                .get(format!("{url}{path}"))
                .bearer_auth(token)
                .send()
                .await
                .expect("GET")
                .error_for_status()
                .expect("the route answers 2xx")
                .json::<serde_json::Value>()
                .await
                .expect("JSON body")
        })
}

fn json_lines(bytes: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is JSON"))
        .collect()
}

fn wait_for_terminal_status(url: &str, token: &str, chat: &str, status: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let transcript = get_json(url, token, &format!("/chats/{chat}/messages"));
        if transcript["terminal_turns"]
            .as_array()
            .is_some_and(|turns| turns.iter().any(|turn| turn["status"] == status))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "turn did not reach {status}: {transcript}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// #2115: `tidebreak attach` of an image must land on the next `-p` user
/// message. Publishing the blob is not enough — `GET /chats/{id}/messages`
/// has to show it.
#[test]
fn attaching_an_image_lands_on_the_next_print_turn() {
    let served = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let script = serde_json::json!([{"text": "saw the image"}]).to_string();
    let (_server, url, token) =
        spawn_serve_with_env(served.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let chat = create_chat(&url, &token);

    let source = elsewhere.path().join("dashboard.png");
    std::fs::write(&source, one_pixel_png()).unwrap();
    let attached = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["attach", &chat])
        .arg(&source)
        .arg("--server")
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run attach");
    let stderr = String::from_utf8_lossy(&attached.stderr).into_owned();
    assert!(attached.status.success(), "stderr: {stderr}");
    let attachment_id = String::from_utf8_lossy(&attached.stdout).trim().to_owned();
    assert!(
        uuid::Uuid::parse_str(&attachment_id).is_ok(),
        "the published attachment id belongs on stdout, got {attachment_id:?}"
    );
    assert!(
        stderr.contains("attached") && stderr.contains("image/png"),
        "stderr: {stderr}"
    );

    let printed = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "-p",
            "what is in this image",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--server",
        ])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run -p after attach");
    let stdout = String::from_utf8_lossy(&printed.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&printed.stderr).into_owned();
    assert!(
        printed.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    let transcript = get_json(&url, &token, &format!("/chats/{chat}/messages"));
    let images = &transcript["messages"][0]["image_attachments"];
    assert_eq!(
        images,
        &serde_json::json!([{
            "attachment_id": attachment_id,
            "media_type": "image/png",
            "width": 1,
            "height": 1,
        }]),
        "transcript: {transcript}"
    );
}

/// A document published through `tidebreak attach` is pending for exactly the
/// next successfully submitted message. The message binds the document to the
/// transcript. On macOS, where the local executor is available, exec also sees
/// the same bytes under `documents/`.
#[test]
fn attaching_a_document_lands_on_the_next_turn_and_local_exec_can_read_it() {
    let served = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let script = if cfg!(target_os = "macos") {
        serde_json::json!([
            {
                "tool": "exec",
                "input": {
                    "summary": "Read the attached CSV",
                    "command": "sh",
                    "args": ["-c", "cat documents/*"],
                    "files": ["documents"]
                }
            },
            {"text": "read the CSV"}
        ])
    } else {
        serde_json::json!([{"text": "read the CSV"}])
    }
    .to_string();
    let (_server, url, token) =
        spawn_serve_with_env(served.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let chat = create_chat(&url, &token);

    let source = elsewhere.path().join("sales.csv");
    std::fs::write(&source, "region,total\nwest,42\n").unwrap();
    let attached = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["attach", &chat])
        .arg(&source)
        .arg("--server")
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("attach the CSV");
    let stderr = String::from_utf8_lossy(&attached.stderr).into_owned();
    assert!(attached.status.success(), "stderr: {stderr}");
    let document_id = String::from_utf8_lossy(&attached.stdout).trim().to_owned();
    assert!(
        tidebreak_core::DocumentId::from_str(&document_id).is_ok(),
        "document id: {document_id:?}"
    );

    let pending = elsewhere
        .path()
        .join("pending-document-attachments")
        .join(&chat);
    let pending_ids: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
    assert_eq!(pending_ids, serde_json::json!([document_id]));

    let printed = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "-p",
            "read the attached sales CSV",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
            "--server",
        ])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", elsewhere.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run -p after attaching the CSV");
    let stdout = String::from_utf8_lossy(&printed.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&printed.stderr).into_owned();
    assert!(
        printed.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !pending.exists(),
        "a successful submission clears the pending id"
    );

    #[cfg(target_os = "macos")]
    {
        let events = json_lines(&printed.stdout);
        let completed = events
            .iter()
            .filter_map(|line| line.get("event"))
            .find(|event| event["type"] == "tool_call_completed")
            .unwrap_or_else(|| panic!("exec did not complete: {stdout}"));
        assert_eq!(completed["status"], "completed", "stdout: {stdout}");
        assert_eq!(
            completed["result"]["stdout"], "region,total\nwest,42\n",
            "stdout: {stdout}"
        );
    }

    let transcript = get_json(&url, &token, &format!("/chats/{chat}/messages"));
    assert_eq!(
        transcript["messages"][0]["file_attachments"],
        serde_json::json!([{
            "document_id": document_id,
            "name": "sales.csv",
            "media_type": "text/csv",
        }]),
        "transcript: {transcript}"
    );
}

/// An approval with no stdin driver is an unavailable interaction, not a user
/// rejection. Print mode reports one halt, cancels the parked turn, and exits
/// with the interaction status instead of letting the model retry.
#[test]
fn an_undriven_approval_halts_once_and_cancels_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let script = serde_json::json!([{
        "tool": "exec",
        "input": {"summary": "Run a command", "command": "true"}
    }])
    .to_string();
    let (_server, url, token) =
        spawn_serve_with_env(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let chat = create_chat(&url, &token);

    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args([
            "-p",
            "run the command",
            "--chat",
            &chat,
            "--permission-mode",
            "ask",
            "--output-format",
            "json",
            "--server",
        ])
        .arg(&url)
        .env("TIDEBREAK_SERVER_TOKEN", &token)
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .expect("run an undriven approval turn");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    let lines = json_lines(&output.stdout);
    assert_eq!(
        lines
            .iter()
            .filter(|line| {
                line.pointer("/event/type")
                    .and_then(serde_json::Value::as_str)
                    == Some("approval_required")
            })
            .count(),
        1,
        "stdout: {stdout}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["type"] == "approval_request")
            .count(),
        1,
        "stdout: {stdout}"
    );
    let halts = lines
        .iter()
        .filter(|line| line["type"] == "halted")
        .collect::<Vec<_>>();
    assert_eq!(halts.len(), 1, "stdout: {stdout}");
    assert_eq!(halts[0]["reason"], "approval_driver_unavailable");
    assert!(!stdout.contains("approval_decided"), "stdout: {stdout}");

    wait_for_terminal_status(&url, &token, &chat, "cancelled");
}

/// `--attach` follows the profile's new `listen.json` after a server restart.
/// The turn starts on the first endpoint and finishes through the second one.
#[test]
fn an_attached_print_turn_refreshes_listen_json_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let script = serde_json::json!([
        {
            "tool": "exec",
            "input": {
                "summary": "Pause during restart",
                "command": "sh",
                "args": ["-c", "sleep 3"]
            }
        },
        {"text": "recovered after restart"}
    ])
    .to_string();
    let (mut first_server, url, first_token) =
        spawn_serve_with_env(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    let chat = create_chat(&url, &first_token);

    let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
    command
        .args([
            "-p",
            "survive the restart",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
            "--attach",
        ])
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_SERVER_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Reaper(command.spawn().expect("spawn an attached print turn"));
    let stdout = child.0.stdout.take().unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut captured = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .is_some_and(|value| {
                    value
                        .pointer("/event/type")
                        .and_then(serde_json::Value::as_str)
                        == Some("tool_call_started")
                })
            {
                let _ = started_tx.send(());
            }
            captured.push_str(&line);
            captured.push('\n');
        }
        captured
    });

    started_rx
        .recv_timeout(TURN_EXIT_TIMEOUT)
        .expect("the turn reaches exec before the restart");
    first_server.0.kill().unwrap();
    first_server.0.wait().unwrap();

    let (_second_server, _second_url, second_token) =
        spawn_serve_with_env(dir.path(), &[("TIDEBREAK_SCRIPTED_PROVIDER", &script)]);
    assert_ne!(
        first_token, second_token,
        "a restart rotates attach credentials"
    );

    let status = child.wait_status(Duration::from_secs(90));
    let stdout = reader.join().unwrap();
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("\"type\":\"turn_completed\""),
        "stdout: {stdout}"
    );
}

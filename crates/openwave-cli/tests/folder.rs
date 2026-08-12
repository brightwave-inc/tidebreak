//! Operator-provisioned folder consent, end to end at the process boundary.
//!
//! Two things are exercised here, both of which only a whole process can show.
//! First, that a grant the CLI records is the same record the desktop reads and
//! revokes rather than a parallel one — the desktop side represented by the
//! broker query its consent surface runs (`ListGrantStatements`, behind
//! `list_capability_consents`) against the same data directory. Second, that an
//! unattended turn can then *use* that folder: the folder tools are executed by
//! whichever process owns the broker state, and a headless install has to
//! answer them or the turn parks forever.
//!
//! The connected folder lives under the real home directory because that is
//! what the host root policy allows: on macOS a temporary directory
//! canonicalizes under `/private`, which the policy refuses by design.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use openwave_host_broker::{
    Broker, Capability, ConsentMethod, ControlEnvelope, ControlRequest, ControlResult, GrantId,
    GrantStatementSummary, GrantSubject, RequestId, Response, RevokeGrantRequest, RootPolicy,
    Scope, PROTOCOL_VERSION,
};

/// Kills the daemon on drop, including on an assertion panic.
struct Reaper(Child);

impl Reaper {
    fn wait_with_output(&mut self, timeout: Duration) -> Output {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "child did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
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

fn openwave(data_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openwave"));
    command
        .env("OPENWAVE_DATA_DIR", data_dir)
        .env("OPENWAVE_KEYCHAIN_MOCK", "1")
        .env_remove("OPENWAVE_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY");
    command
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
        .canonicalize()
        .expect("canonical home")
}

/// Endpoint of a live `openwave serve` process.
struct ServeEndpoint {
    url: String,
    token: String,
    /// Keep the daemon alive for the scope of the caller.
    _daemon: Reaper,
    /// Drain stdout so a full pipe cannot stall the child.
    _stdout: std::thread::JoinHandle<()>,
}

/// Start `openwave serve` and return its base URL and bearer token.
fn start_serve(data_dir: &Path) -> ServeEndpoint {
    let mut child = openwave(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn openwave serve");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    let url = format!(
        "http://{}",
        addr_line.rsplit("http://").next().unwrap().trim()
    );
    let token = token_line
        .trim_start_matches("openwave: token ")
        .trim()
        .to_owned();
    // Keep draining so a long-lived daemon cannot block on a full stdout pipe.
    let drain = std::thread::spawn(move || for _ in lines {});
    ServeEndpoint {
        url,
        token,
        _daemon: Reaper(child),
        _stdout: drain,
    }
}

/// Create one chat through a short-lived server, then stop it.
///
/// Most folder tests still want an idle data directory so they can open the
/// broker without contending with a daemon. The concurrent case uses
/// [`start_serve`] and creates the chat while the daemon stays up.
fn create_chat(data_dir: &Path) -> String {
    let endpoint = start_serve(data_dir);
    create_chat_on(&endpoint)
}

/// Create one chat against a live serve endpoint.
fn create_chat_on(endpoint: &ServeEndpoint) -> String {
    let addr = endpoint
        .url
        .strip_prefix("http://")
        .expect("serve url is http");
    let mut stream = TcpStream::connect(addr).unwrap();
    let body = "{}";
    stream
        .write_all(
            format!(
                "POST /chats HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                endpoint.token,
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("201 Created"), "response: {response}");
    let payload = response.split("\r\n\r\n").nth(1).unwrap();
    let start = payload.find('{').expect("chat body");
    let chat: serde_json::Value = serde_json::from_str(&payload[start..])
        .unwrap_or_else(|error| panic!("chat body {payload:?}: {error}"));
    chat["id"].as_str().expect("chat id").to_owned()
}

fn desktop_broker(data_dir: &Path) -> Broker {
    let policy = RootPolicy::for_host(home())
        .unwrap()
        .with_private_directory(&data_dir.canonicalize().unwrap())
        .unwrap();
    Broker::open_with_execute_commands(policy, data_dir, false).unwrap()
}

/// The grant statements the desktop's consent surface reads, from the same
/// state file the CLI wrote.
fn desktop_grant_statements(data_dir: &Path) -> Vec<GrantStatementSummary> {
    let broker = desktop_broker(data_dir);
    let response = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(),
        request: ControlRequest::ListGrantStatements,
    });
    match response.response {
        Response::Ok(ControlResult::ListGrantStatements { grants }) => grants,
        other => panic!("unexpected broker response: {other:?}"),
    }
}

/// Revoke one grant the way the desktop's Permissions surface does.
fn desktop_revoke(data_dir: &Path, subject: GrantSubject, grant_id: GrantId) {
    let broker = desktop_broker(data_dir);
    let response = broker.controller().handle(ControlEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(),
        request: ControlRequest::RevokeGrant(RevokeGrantRequest { subject, grant_id }),
    });
    match response.response {
        Response::Ok(ControlResult::RevokeGrant(result)) => assert!(result.revoked),
        other => panic!("unexpected broker response: {other:?}"),
    }
}

fn run(data_dir: &Path, args: &[&str]) -> (bool, String, String) {
    let output = openwave(data_dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run openwave folder");
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// The provisioning grant is one record: the CLI lists it, the desktop's own
/// query returns it with operator provenance, and a revocation from either side
/// is a revocation for both. A symlinked argument resolves to the directory it
/// names, matching what the broker pins.
#[test]
fn an_operator_grant_is_one_record_both_shells_read_and_either_side_can_revoke() {
    let base = tempfile::tempdir_in(home()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().join("profile");
    let reports = base.path().join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    let link = base.path().join("link");
    std::os::unix::fs::symlink(&reports, &link).unwrap();

    let chat = create_chat(&data_dir);

    let (ok, stdout, stderr) = run(
        &data_dir,
        &["folder", "connect", link.to_str().unwrap(), "--chat", &chat],
    );
    assert!(ok, "connect failed: {stderr}");
    // Canonicalization resolved the link: the folder is named for its target.
    assert!(
        stdout.contains("connected reports to chat") && stdout.contains("operator configuration"),
        "stdout: {stdout}"
    );

    let (ok, listed, stderr) = run(&data_dir, &["folder", "list", "--chat", &chat]);
    assert!(ok, "list failed: {stderr}");
    assert!(
        listed.contains("operator-config") && listed.contains("\tread\treports\t"),
        "listing: {listed}"
    );

    let statements = desktop_grant_statements(&data_dir);
    let read_grant = statements
        .iter()
        .find(|grant| {
            grant.capability == Capability::ReadFiles && matches!(grant.scope, Scope::Root { .. })
        })
        .expect("the desktop consent query must see the operator grant");
    assert_eq!(read_grant.consent_method, ConsentMethod::OperatorConfig);
    assert!(
        listed.contains(&read_grant.grant_id.to_string()),
        "{listed}"
    );

    // Disconnected from the CLI side: nothing the desktop reads still names
    // the folder.
    let (ok, _, stderr) = run(
        &data_dir,
        &[
            "folder",
            "disconnect",
            reports.to_str().unwrap(),
            "--chat",
            &chat,
        ],
    );
    assert!(ok, "disconnect failed: {stderr}");
    let statements = desktop_grant_statements(&data_dir);
    assert!(
        statements
            .iter()
            .all(|grant| !matches!(grant.scope, Scope::Root { .. })),
        "the withdrawn folder still has grants: {statements:?}"
    );

    // And the other direction: a grant withdrawn on the desktop's Permissions
    // surface is gone from the CLI's listing too.
    let (ok, _, stderr) = run(
        &data_dir,
        &[
            "folder",
            "connect",
            reports.to_str().unwrap(),
            "--chat",
            &chat,
        ],
    );
    assert!(ok, "reconnect failed: {stderr}");
    let read_grant = desktop_grant_statements(&data_dir)
        .into_iter()
        .find(|grant| {
            grant.capability == Capability::ReadFiles && matches!(grant.scope, Scope::Root { .. })
        })
        .expect("the reconnected folder must have a read grant");
    desktop_revoke(&data_dir, read_grant.subject, read_grant.grant_id);
    let (ok, listed, stderr) = run(&data_dir, &["folder", "list", "--chat", &chat]);
    assert!(ok, "list failed: {stderr}");
    assert!(
        !listed.contains(&read_grant.grant_id.to_string()),
        "a revoked grant is still listed: {listed}"
    );
}

/// `--server` points a client command at somebody else's server. Folder consent
/// is not a client command: it writes this machine's broker state and this
/// profile's data directory, so accepting the flag would say the grant lands
/// somewhere it does not. It is refused, not ignored.
#[test]
fn folder_commands_refuse_to_pretend_they_target_another_server() {
    let data = tempfile::tempdir().unwrap();
    let output = openwave(data.path())
        .args(["folder", "list", "--server", "http://127.0.0.1:9"])
        .stdin(Stdio::null())
        .output()
        .expect("run openwave folder --server");
    assert_eq!(output.status.code(), Some(2), "expected a usage error");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("folder") && stderr.contains("--server"),
        "stderr: {stderr}"
    );
}

/// A turn boots the engine and runs the tool loop, so it is given more room
/// than a command that only has to refuse and exit. Generous rather than tight:
/// the failure this bounds is a parked call that never resolves, and there is
/// nothing to gain from declaring it early.
const TURN_EXIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Connect one folder as an operator and return its opaque root id.
fn connect_folder(data_dir: &Path, path: &Path, chat: &str) -> String {
    let (ok, stdout, stderr) = run(
        data_dir,
        &["folder", "connect", path.to_str().unwrap(), "--chat", chat],
    );
    assert!(ok, "connect failed: {stderr}");
    // "openwave: connected <name> to chat <id> (root <root id>, operator …)"
    let root = stdout
        .split("(root ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("the connect report names the root id")
        .trim()
        .to_owned();
    assert!(
        uuid::Uuid::parse_str(&root).is_ok(),
        "unexpected root id {root:?} in {stdout:?}"
    );
    root
}

/// Every client-executed folder tool the executor answers, in one script: list
/// the folder, read a file out of it as text, import that file as a source, then
/// answer. Each tool step parks a client-executed call, which is exactly what
/// used to hang.
fn folder_reading_script(root: &str) -> String {
    serde_json::json!([
        {"tool": "list_folder", "input": {"root_id": root, "path": ""}},
        {"tool": "read_connected_file", "input": {"root_id": root, "path": "q3.md"}},
        {"tool": "import_connected_file", "input": {"root_id": root, "path": "q3.md"}},
        {"text": "read the report"}
    ])
    .to_string()
}

/// The folder tools whose calls the script above parks. Every one must come back
/// completed: a single parked call is the whole failure this closes.
const SCRIPTED_FOLDER_TOOLS: &[&str] = &[
    "list_folder",
    "read_connected_file",
    "import_connected_file",
];

/// The journal frames one print-mode run wrote, decoded from its stdout.
fn journal_events(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|frame| frame.get("event").cloned())
        .collect()
}

/// Assert the scripted turn ran every folder tool for real and finished.
fn assert_folder_tools_completed(stdout: &str, stderr: &str) {
    let events = journal_events(stdout);
    for tool in SCRIPTED_FOLDER_TOOLS {
        let call = events
            .iter()
            .find(|event| event["type"] == "tool_call_started" && event["name"] == *tool)
            .unwrap_or_else(|| {
                panic!("{tool} was never called\nstdout: {stdout}\nstderr: {stderr}")
            });
        let call_id = call["call_id"].clone();
        let completed = events
            .iter()
            .find(|event| event["type"] == "tool_call_completed" && event["call_id"] == call_id)
            .unwrap_or_else(|| {
                panic!("{tool} never completed — it parked\nstdout: {stdout}\nstderr: {stderr}")
            });
        assert_eq!(
            completed["status"], "completed",
            "{tool} did not succeed\nstdout: {stdout}\nstderr: {stderr}"
        );
    }
    assert!(
        events.iter().any(|event| event["type"] == "turn_completed"),
        "the turn never completed\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The gap #1884 closed, driven end to end: an operator connects a folder, and
/// an unattended turn *uses* it. `list_folder` and `read_connected_file` are
/// client-executed calls with no client on a headless install, so before this
/// executor existed they parked and the run hung until it was killed. The model
/// is scripted, but the engine, the turn worker, the parked-call lifecycle, the
/// host broker's capability checks, and the journal are all the production path.
#[test]
fn an_unattended_turn_reads_a_folder_the_operator_connected() {
    let base = tempfile::tempdir_in(home()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().join("profile");
    let reports = base.path().join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::write(reports.join("q3.md"), "revenue held flat\n").unwrap();

    let chat = create_chat(&data_dir);
    let root = connect_folder(&data_dir, &reports, &chat);

    let output = openwave(&data_dir)
        .args([
            "-p",
            "summarize the report",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
        ])
        .env("OPENWAVE_SCRIPTED_PROVIDER", folder_reading_script(&root))
        .env_remove("OPENWAVE_SERVER_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a scripted openwave -p");
    let mut child = Reaper(output);
    let output = child.wait_with_output(TURN_EXIT_TIMEOUT);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_folder_tools_completed(&stdout, &stderr);
    // Nothing was written back into the folder: these tools only read it.
    assert_eq!(
        std::fs::read_to_string(reports.join("q3.md")).unwrap(),
        "revenue held flat\n"
    );
}

/// The other half of the scoping decision, and the reason an attached client
/// needs no new authority: the executor belongs to the process that owns the
/// broker state. Here that process is `openwave serve`, and the run driving the
/// turn is a plain `--server` client holding only a bearer token. It cannot
/// execute a folder call — it has no executor credential and no flag grants it
/// one — and it does not have to, because the daemon does.
///
/// The folder is connected *while a serve holds the data directory*, then the
/// scripted daemon is started with that root. Concurrent connect is the
/// operator workflow this change closes; the turn half still proves the daemon
/// executor. (The scripted provider is process env at serve spawn, so the
/// scripted engine cannot be the same process that was up during connect.)
#[test]
fn a_serve_daemon_executes_folder_calls_for_an_attached_client() {
    let base = tempfile::tempdir_in(home()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().join("profile");
    let reports = base.path().join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::write(reports.join("q3.md"), "revenue held flat\n").unwrap();

    let chat = create_chat(&data_dir);
    // Prove connect against a live data-dir lock, then hand the root to a
    // scripted serve for the turn.
    let lock_holder = start_serve(&data_dir);
    let root = connect_folder(&data_dir, &reports, &chat);
    drop(lock_holder);

    let mut daemon = openwave(&data_dir)
        .arg("serve")
        .env("OPENWAVE_SCRIPTED_PROVIDER", folder_reading_script(&root))
        .env_remove("OPENWAVE_SERVER_URL")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn openwave serve");
    let stdout = daemon.stdout.take().unwrap();
    let _daemon = Reaper(daemon);
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    let url = format!(
        "http://{}",
        addr_line.rsplit("http://").next().unwrap().trim()
    );
    let token = token_line
        .trim_start_matches("openwave: token ")
        .trim()
        .to_owned();
    let _drain = std::thread::spawn(move || for _ in lines {});

    let attached = openwave(&data_dir)
        .args([
            "-p",
            "summarize the report",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
            "--server",
        ])
        .arg(&url)
        .env("OPENWAVE_SERVER_TOKEN", &token)
        .env_remove("OPENWAVE_SERVER_URL")
        .env_remove("OPENWAVE_SCRIPTED_PROVIDER")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn an attached openwave -p");
    let mut attached = Reaper(attached);
    let output = attached.wait_with_output(TURN_EXIT_TIMEOUT);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert_folder_tools_completed(&stdout, &stderr);
}

/// #1967: an attached print client holds no executor credential, so a mid-turn
/// `request_folder_access` used to park forever — print left the call for
/// "the attached server's own client executor", and serve's executor did not
/// handle that tool at all. Serve must settle it declined (no grant) so the
/// turn completes instead of hanging until killed. The attach client still
/// cannot claim or grant anything itself.
#[test]
fn a_serve_daemon_declines_folder_access_requests_for_an_attached_client() {
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().join("profile");
    let chat = create_chat(&data_dir);

    let script = serde_json::json!([
        {
            "tool": "request_folder_access",
            "input": {
                "reason": "Read the quarterly reports",
                "requested_capabilities": ["read_files"]
            }
        },
        {"text": "no folder was granted"}
    ])
    .to_string();
    let mut daemon = openwave(&data_dir)
        .arg("serve")
        .env("OPENWAVE_SCRIPTED_PROVIDER", script)
        .env_remove("OPENWAVE_SERVER_URL")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn openwave serve");
    let stdout = daemon.stdout.take().unwrap();
    let _daemon = Reaper(daemon);
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    let url = format!(
        "http://{}",
        addr_line.rsplit("http://").next().unwrap().trim()
    );
    let token = token_line
        .trim_start_matches("openwave: token ")
        .trim()
        .to_owned();
    let _drain = std::thread::spawn(move || for _ in lines {});

    let attached = openwave(&data_dir)
        .args([
            "-p",
            "please connect a folder",
            "--chat",
            &chat,
            "--permission-mode",
            "allow",
            "--output-format",
            "json",
            "--server",
        ])
        .arg(&url)
        .env("OPENWAVE_SERVER_TOKEN", &token)
        .env_remove("OPENWAVE_SERVER_URL")
        .env_remove("OPENWAVE_SCRIPTED_PROVIDER")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn an attached openwave -p");
    let mut attached = Reaper(attached);
    let output = attached.wait_with_output(TURN_EXIT_TIMEOUT);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let events = journal_events(&stdout);
    let call = events
        .iter()
        .find(|event| {
            event["type"] == "tool_call_started" && event["name"] == "request_folder_access"
        })
        .unwrap_or_else(|| {
            panic!("request_folder_access was never called\nstdout: {stdout}\nstderr: {stderr}")
        });
    let call_id = call["call_id"].clone();
    let completed = events
        .iter()
        .find(|event| event["type"] == "tool_call_completed" && event["call_id"] == call_id)
        .unwrap_or_else(|| {
            panic!(
                "request_folder_access never completed — it parked\nstdout: {stdout}\nstderr: {stderr}"
            )
        });
    // Settled as completed (the contract's Declined result is a successful
    // resolution, not a tool failure) — never left pending.
    assert_eq!(
        completed["status"], "completed",
        "request_folder_access did not settle\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        events.iter().any(|event| event["type"] == "turn_completed"),
        "the turn never completed\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Attach still cannot execute; it only observes. The notice names that
    // hand-off when the tool is announced, and the settled tool is reported.
    assert!(
        stderr.contains("left for the attached server's own client executor"),
        "attach must leave the call for serve's executor: {stderr}"
    );
    assert!(
        stderr.contains("tool: request_folder_access ok"),
        "serve must settle the request so the attach client sees completion: {stderr}"
    );
}

/// The contract this change exists for: `openwave folder connect|list|
/// disconnect` must succeed while `openwave serve` holds `openwave.lock`.
/// Before, provisioning embedded a second server and refused. The daemon's
/// folder executor opens the broker only per tool call, so a quiet serve leaves
/// `host-broker.lock` free for these commands.
#[test]
fn folder_connect_while_serve_holds_the_data_directory() {
    let base = tempfile::tempdir_in(home()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().join("profile");
    let reports = base.path().join("reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::write(reports.join("q3.md"), "revenue held flat\n").unwrap();

    let endpoint = start_serve(&data_dir);
    let chat = create_chat_on(&endpoint);

    // The data directory is locked. Connect and list must still work.
    assert!(
        data_dir.join("openwave.lock").exists(),
        "serve should hold openwave.lock"
    );
    let root = connect_folder(&data_dir, &reports, &chat);
    let (ok, listed, stderr) = run(&data_dir, &["folder", "list", "--chat", &chat]);
    assert!(ok, "list while serve is up failed: {stderr}");
    // `folder list` reports grant rows (id, subject, capability, display name,
    // provenance) — not the opaque root id, which only the connect report names.
    assert!(
        listed.contains("operator-config")
            && listed.contains("\tread\treports\t")
            && !root.is_empty(),
        "listing: {listed}"
    );

    // Disconnect while the daemon still owns the profile.
    let (ok, _, stderr) = run(
        &data_dir,
        &[
            "folder",
            "disconnect",
            reports.to_str().unwrap(),
            "--chat",
            &chat,
        ],
    );
    assert!(ok, "disconnect while serve is up failed: {stderr}");
    let statements = desktop_grant_statements(&data_dir);
    assert!(
        statements
            .iter()
            .all(|grant| !matches!(grant.scope, Scope::Root { .. })),
        "grants left after disconnect while serve held the lock: {statements:?}"
    );

    // Keep the endpoint alive until the assertions finish so the lock is real.
    drop(endpoint);
}

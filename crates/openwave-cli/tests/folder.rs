//! Operator-provisioned folder consent, end to end at the process boundary.
//!
//! These exercise the one thing the CLI is uniquely responsible for: that a
//! grant it records is the same record the desktop reads and revokes, not a
//! parallel one. The desktop side is represented by the broker query its
//! consent surface runs (`ListGrantStatements`, behind
//! `list_capability_consents`) against the same data directory.
//!
//! The connected folder lives under the real home directory because that is
//! what the host root policy allows: on macOS a temporary directory
//! canonicalizes under `/private`, which the policy refuses by design.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use openwave_host_broker::{
    Broker, Capability, ConsentMethod, ControlEnvelope, ControlRequest, ControlResult, GrantId,
    GrantStatementSummary, GrantSubject, RequestId, Response, RevokeGrantRequest, RootPolicy,
    Scope, PROTOCOL_VERSION,
};

/// Kills the daemon on drop, including on an assertion panic.
struct Reaper(Child);

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

/// Create one chat through the running server, then stop it: the folder
/// commands need the data directory's locks for themselves.
fn create_chat(data_dir: &Path) -> String {
    let mut child = openwave(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn openwave serve");
    let stdout = child.stdout.take().unwrap();
    let mut reaper = Reaper(child);
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let token_line = lines.next().unwrap().unwrap();
    let addr = addr_line
        .rsplit("http://")
        .next()
        .unwrap()
        .trim()
        .to_owned();
    let token = token_line
        .trim_start_matches("openwave: token ")
        .trim()
        .to_owned();

    let mut stream = TcpStream::connect(&addr).unwrap();
    let body = "{}";
    stream
        .write_all(
            format!(
                "POST /chats HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
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
    let id = chat["id"].as_str().expect("chat id").to_owned();

    reaper.0.kill().ok();
    reaper.0.wait().ok();
    id
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

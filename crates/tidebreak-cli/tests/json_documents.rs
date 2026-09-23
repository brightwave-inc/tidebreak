//! Every JSON document the CLI prints carries `"schema_version": 1`.
//!
//! The headless guide promises this for 1.x, so it is pinned at the process
//! boundary: each command runs as its own process against one scratch
//! profile, embedding a server the way a script's commands do, and its stdout
//! must be one object carrying the version beside the keys the command names.
#![cfg(feature = "keychain")]

use std::path::Path;
use std::process::{Command, Stdio};

/// Run one command and read its stdout as a single JSON document.
fn document(data_dir: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(args)
        .env("TIDEBREAK_DATA_DIR", data_dir)
        .env("TIDEBREAK_PROFILE", "desktop")
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_LISTEN_ADDR")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .output()
        .expect("run tidebreak");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = stdout.lines();
    let line = lines
        .next()
        .unwrap_or_else(|| panic!("{args:?} printed nothing"));
    assert!(
        lines.next().is_none(),
        "{args:?} printed more than one line: {stdout}"
    );
    serde_json::from_str(line).unwrap_or_else(|error| panic!("{args:?} printed {line}: {error}"))
}

#[test]
fn every_json_document_carries_its_schema_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let data = dir.path().join("profile");

    let created = document(&data, &["chat", "create", "--output-format", "json"]);
    assert_eq!(created["schema_version"], 1, "{created}");
    let chat = created["id"]
        .as_str()
        .expect("chat create names the chat")
        .to_owned();

    let cases: [(&[&str], &[&str]); 9] = [
        (&["chat", "list", "--output-format", "json"], &["chats"]),
        (
            &["provider", "list", "--output-format", "json"],
            &["providers"],
        ),
        (&["model", "roles", "--output-format", "json"], &["roles"]),
        (
            &["settings", "show", "--output-format", "json"],
            &["settings", "web_search", "code_execution"],
        ),
        (
            &["mcp-server", "list", "--output-format", "json"],
            &["servers"],
        ),
        (
            &["agent-run", "list", &chat, "--output-format", "json"],
            &["runs"],
        ),
        (
            &["output", "list", &chat, "--output-format", "json"],
            &["deliverables", "truncated"],
        ),
        (&["code", "repo", "list", "--json"], &["repos"]),
        (&["folder", "list", "--output-format", "json"], &["grants"]),
    ];
    for (args, keys) in cases {
        let document = document(&data, args);
        assert_eq!(document["schema_version"], 1, "{args:?}: {document}");
        for key in keys {
            assert!(
                document.get(key).is_some(),
                "{args:?} lost its `{key}` key: {document}"
            );
        }
    }

    // The row a script reads back from `chat list` keeps its keys too.
    let listed = document(&data, &["chat", "list", "--output-format", "json"]);
    let row = &listed["chats"][0];
    for key in ["id", "title", "model", "permission_mode", "created_at"] {
        assert!(row.get(key).is_some(), "chat list rows lost `{key}`: {row}");
    }
    assert_eq!(row["id"], chat.as_str());
}

/// `browser … --json` prints the browser tool's own result, and it carries the
/// version beside it like every other document.
#[test]
fn a_browser_command_prints_a_versioned_document() {
    use std::io::{BufRead as _, BufReader, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("an address").port();
    let endpoint = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("one connection");
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut request_line = String::new();
        reader.read_line(&mut request_line).expect("a request line");
        loop {
            let mut header = String::new();
            let read = reader.read_line(&mut header).expect("a header line");
            if read == 0 || header == "\r\n" {
                break;
            }
        }
        let body = r#"{"sessions":[]}"#;
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("the answer is written");
        request_line
    });
    let dir = tempfile::tempdir().expect("temp dir");
    let capfile = dir.path().join("browser.json");
    std::fs::write(
        &capfile,
        serde_json::json!({
            "version": 1,
            "endpoint": format!("http://127.0.0.1:{port}/code/browser"),
            "token": "tbreak_bt_00000000-0000-0000-0000-000000000000",
        })
        .to_string(),
    )
    .expect("the capfile is written");

    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["browser", "list", "--json"])
        .env("TIDEBREAK_BROWSER_CAPFILE", &capfile)
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .stdin(Stdio::null())
        .output()
        .expect("run tidebreak browser list");
    assert!(
        output.status.success(),
        "browser list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one JSON document");
    assert_eq!(
        document,
        serde_json::json!({ "sessions": [], "schema_version": 1 })
    );
    let request_line = endpoint.join().expect("the endpoint answered");
    assert!(
        request_line.starts_with("GET /code/browser/list "),
        "{request_line}"
    );
}

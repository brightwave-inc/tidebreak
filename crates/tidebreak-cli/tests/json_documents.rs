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

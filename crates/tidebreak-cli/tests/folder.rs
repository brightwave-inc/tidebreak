//! Smoke coverage of the `tidebreak folder` command surface at the process
//! boundary.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};

fn tidebreak(data_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
    command
        .env("TIDEBREAK_DATA_DIR", data_dir)
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY");
    command
}

/// `--server` points a client command at somebody else's server. Folder consent
/// is not a client command: it writes this machine's broker state and this
/// profile's data directory, so accepting the flag would say the grant lands
/// somewhere it does not. It is refused, not ignored.
#[test]
fn folder_commands_refuse_to_pretend_they_target_another_server() {
    let data = tempfile::tempdir().unwrap();
    let output = tidebreak(data.path())
        .args(["folder", "list", "--server", "http://127.0.0.1:9"])
        .stdin(Stdio::null())
        .output()
        .expect("run tidebreak folder --server");
    assert_eq!(output.status.code(), Some(2), "expected a usage error");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("folder") && stderr.contains("--server"),
        "stderr: {stderr}"
    );
}

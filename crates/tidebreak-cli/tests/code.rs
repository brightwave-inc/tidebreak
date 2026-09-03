//! Smoke coverage for `tidebreak code` argument parsing.

use std::process::{Command, Stdio};

#[test]
fn unknown_code_subcommand_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["code", "nope"])
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown code subcommand"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("tidebreak code doctor"),
        "code parse errors should print the code family, not the whole CLI: {stderr}"
    );
    assert!(
        !stderr.contains("tidebreak mcp-server add"),
        "code parse errors must not dump the full CLI usage: {stderr}"
    );
}

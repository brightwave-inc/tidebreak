//! Help and usage-error smoke coverage for the `tidebreak` process.

use std::process::{Command, Stdio};

fn tidebreak(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(args)
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("TIDEBREAK_BROWSER_CAPFILE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn help_flag_prints_to_stdout_and_exits_zero() {
    for args in [
        ["--help"].as_slice(),
        ["-h"].as_slice(),
        ["help"].as_slice(),
    ] {
        let output = tidebreak(args);
        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stdout.contains("Tidebreak command-line interface"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("Agent tools (run inside a Tidebreak session)"),
            "stdout: {stdout}"
        );
        assert!(
            stdout.contains("tidebreak code session mode"),
            "stdout: {stdout}"
        );
        assert!(stdout.contains("tidebreak browser act"), "stdout: {stdout}");
        assert!(
            stdout.contains("tidebreak computer-mcp"),
            "stdout: {stdout}"
        );
        assert!(
            stderr.is_empty(),
            "help must not write usage to stderr: {stderr}"
        );
    }
}

#[test]
fn help_command_prints_a_family() {
    let output = tidebreak(&["help", "code"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tidebreak code doctor"), "stdout: {stdout}");
    assert!(
        !stdout.contains("tidebreak mcp-server add"),
        "family help must not dump every command: {stdout}"
    );
}

#[test]
fn unknown_command_is_a_usage_error_on_stderr() {
    let output = tidebreak(&["nope"]);
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.is_empty(), "usage errors belong on stderr: {stdout}");
    assert!(stderr.contains("unknown command"), "stderr: {stderr}");
}

#[test]
fn output_usage_error_prints_only_the_output_family() {
    let output = tidebreak(&["output"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tidebreak output list"), "stderr: {stderr}");
    assert!(
        !stderr.contains("tidebreak mcp-server add"),
        "output parse errors must not dump the full CLI usage: {stderr}"
    );
    assert!(
        !stderr.contains("tidebreak code doctor"),
        "output parse errors must not dump the code family: {stderr}"
    );
}

#[test]
fn browser_without_capfile_says_it_runs_inside_a_session() {
    let output = tidebreak(&["browser", "list", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TIDEBREAK_BROWSER_CAPFILE is not set"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("inside a Tidebreak session"),
        "stderr: {stderr}"
    );
}

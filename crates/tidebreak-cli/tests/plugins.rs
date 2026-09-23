//! Process-level coverage for `tidebreak plugins install`.
#![cfg(feature = "keychain")]

use std::path::Path;
use std::process::{Command, Stdio};

fn tidebreak(data_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tidebreak"))
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
        .expect("run tidebreak")
}

#[test]
fn plugins_help_names_the_install_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .args(["help", "plugins"])
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .stdin(Stdio::null())
        .output()
        .expect("run tidebreak");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("plugins install --git"), "stdout: {stdout}");
}

#[test]
fn plugins_install_without_git_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = tidebreak(dir.path(), &["plugins", "install"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--git"), "stderr: {stderr}");
}

#[test]
fn plugins_install_refuses_http() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = tidebreak(
        dir.path(),
        &[
            "plugins",
            "install",
            "--git",
            "http://github.com/acme/notes",
            "--ref",
            "v1.0.0",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("https"), "stderr: {stderr}");
}

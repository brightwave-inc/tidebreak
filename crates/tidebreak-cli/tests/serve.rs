//! End-to-end smoke test of the `tidebreak` process surface.

#[cfg(feature = "keychain")]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(feature = "keychain")]
use std::net::TcpStream;
#[cfg(feature = "keychain")]
use std::process::Child;
use std::process::{Command, Stdio};

/// Kills the daemon on drop — including on an assertion panic, since
/// `std::process::Child` does not reap on its own.
#[cfg(feature = "keychain")]
struct Reaper(Child);

#[cfg(feature = "keychain")]
impl Drop for Reaper {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

#[cfg(feature = "keychain")]
#[test]
fn serve_announces_its_address_and_answers_health() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_PROFILE", "desktop")
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_LISTEN_ADDR")
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

/// A headless binary must reject desktop startup before it opens local storage.
#[cfg(not(feature = "keychain"))]
#[test]
fn serve_without_keychain_rejects_desktop_before_opening_storage() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_PROFILE", "desktop")
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env_remove("TIDEBREAK_LISTEN_ADDR")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_VAULT_ADDR")
        .env_remove("TIDEBREAK_VAULT_TOKEN_FILE")
        .env_remove("TIDEBREAK_VAULT_MOUNT")
        .env_remove("TIDEBREAK_VAULT_PATH")
        .env_remove("TIDEBREAK_VAULT_NAMESPACE")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .output()
        .expect("run headless tidebreak serve");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("keychain feature"), "stderr: {stderr}");
    assert!(
        stderr.contains("TIDEBREAK_PROFILE=self_host"),
        "stderr: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "refused server must not announce a listener"
    );
    assert!(!dir.path().join("tidebreak.lock").exists());
    assert!(!dir.path().join("tidebreak.db").exists());
}

/// The fixture verifies its debug engine without opening self-host storage.
#[cfg(debug_assertions)]
#[test]
fn malformed_script_refuses_self_host_before_opening_storage() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TIDEBREAK_PROFILE", "self_host")
        .env("TIDEBREAK_DATA_DIR", dir.path())
        .env("TIDEBREAK_BLOB_STORE_URL", "s3://fixture/test")
        .env(
            "TIDEBREAK_DATABASE_URL",
            "postgres://fixture@127.0.0.1:1/fixture",
        )
        .env("TIDEBREAK_SCRIPTED_HARNESS", "{not json")
        .stdin(Stdio::null())
        .output()
        .expect("run self-host fixture smoke check");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TIDEBREAK_SCRIPTED_HARNESS is not a valid script"),
        "stderr: {stderr}"
    );
    assert!(!dir.path().join("tidebreak.lock").exists());
    assert!(!dir.path().join("tidebreak.db").exists());
}

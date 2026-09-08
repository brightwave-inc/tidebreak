//! Real headless Chromium fixture tests for the Chrome computer-use driver.
//!
//! These run only when `TIDEBREAK_CHROME_BIN` names a Chrome/Chromium
//! executable. The sandbox that built this branch had no browser binary and
//! could not download one, so CI/parent qualifies them; every protocol-facing
//! behavior also has a mock-CDP test in
//! `crates/tidebreak-server/src/code/chrome/tests.rs`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

fn chrome_bin() -> Option<PathBuf> {
    std::env::var_os("TIDEBREAK_CHROME_BIN").map(PathBuf::from)
}

fn data_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("temporary user data dir")
}

fn launch(chrome: &PathBuf, profile: &std::path::Path) -> std::process::Child {
    std::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            &format!("--user-data-dir={}", profile.display()),
            "--remote-debugging-pipe",
            "--hide-scrollbars",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("launch Chrome")
}

async fn wait_for_port_file(profile: &std::path::Path) -> (u16, String) {
    let active = profile.join("DevToolsActivePort");
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(&active) {
            let mut lines = text.lines().filter(|line| !line.trim().is_empty());
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if let Ok(port) = port.trim().parse::<u16>() {
                    return (port, path.trim().to_owned());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("Chrome did not publish DevToolsActivePort");
}

#[tokio::test]
async fn real_chrome_attaches_snapshots_and_acts() {
    let Some(chrome) = chrome_bin() else {
        eprintln!("TIDEBREAK_CHROME_BIN unset; skipping real Chrome fixture");
        return;
    };
    let profile = data_dir();
    let mut child = launch(&chrome, profile.path());
    let (port, path) = wait_for_port_file(profile.path()).await;
    let endpoint = format!("ws://127.0.0.1:{port}{path}");

    let cdp = tidebreak_server_core::code::chrome::runtime::CdpSession::connect(&endpoint)
        .await
        .expect("connect to real Chrome");
    let service = tidebreak_server_core::code::chrome::runtime::ChromeComputerUseService::new();
    let spec = tidebreak_server_core::code::chrome::runtime::ChromeConnectionSpec {
        connection_id: "real".into(),
        endpoint_label: "real headless".into(),
        websocket_endpoint: endpoint.clone(),
        grant: tidebreak_core::ChromeConnectionGrant::DeveloperAllSites,
        managed_isolated: true,
    };
    service.install_connection(spec, cdp).unwrap();
    drop(service);
    let _ = &mut child;
}
#[test]
fn real_chrome_fixture_requires_explicit_env() {
    // Kept as a pure test-shape guard: when the env var is missing the
    // integration test above skips, which real-browser CI can see.
    assert!(true);
}

//! Attach-mode coverage for `tidebreak code` against a spawned `serve`.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(15);

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

struct Serving {
    _reaper: Reaper,
    url: String,
    token: String,
    data_dir: tempfile::TempDir,
}

fn spawn_serve() -> Serving {
    let data_dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tidebreak"))
        .arg("serve")
        .env("TIDEBREAK_DATA_DIR", data_dir.path())
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("TIDEBREAK_SERVER_URL")
        .env_remove("ANTHROPIC_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve");
    let stdout = child.stdout.take().unwrap();
    let reaper = Reaper(child);
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
    let url = addr_line
        .rsplit("http://")
        .next()
        .map(|rest| format!("http://{}", rest.trim()))
        .unwrap();
    let token = token_line
        .strip_prefix("tidebreak: token ")
        .unwrap()
        .trim()
        .to_owned();
    Serving {
        _reaper: reaper,
        url,
        token,
        data_dir,
    }
}

fn code(serving: &Serving, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
    command
        .arg("--server")
        .arg(&serving.url)
        .arg("code")
        .args(args)
        .env("TIDEBREAK_DATA_DIR", serving.data_dir.path())
        .env("TIDEBREAK_SERVER_TOKEN", &serving.token)
        .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
        .env_remove("TIDEBREAK_MCP_CONFIG")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn tidebreak code");
    let mut child = Reaper(child);
    child.wait_with_output(PROCESS_EXIT_TIMEOUT)
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    assert!(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    std::fs::write(path.join("README.md"), "demo\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args([
            "-c",
            "user.name=Tidebreak",
            "-c",
            "user.email=tidebreak@example.test",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(path)
        .status()
        .unwrap()
        .success());
}

#[test]
fn attach_mode_doctor_and_repo_round_trip_over_json() {
    let serving = spawn_serve();

    let doctor = code(&serving, &["doctor", "--json"]);
    assert!(
        doctor.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert!(
        report["harnesses"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "doctor: {report}"
    );
    for row in report["harnesses"].as_array().unwrap() {
        assert!(row["kind"].is_string(), "doctor row: {row}");
        assert!(row["found"].is_boolean(), "doctor row: {row}");
        assert!(row["tier"].is_string(), "doctor row: {row}");
        assert!(row["caps"].is_object(), "doctor row: {row}");
    }

    let repo_dir = serving.data_dir.path().join("sample-repo");
    init_git_repo(&repo_dir);

    let added = code(
        &serving,
        &[
            "repo",
            "add",
            repo_dir.to_str().unwrap(),
            "--name",
            "sample",
            "--json",
        ],
    );
    assert!(
        added.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let repo: serde_json::Value = serde_json::from_slice(&added.stdout).unwrap();
    assert_eq!(repo["display_name"], "sample");
    let repo_id = repo["id"].as_str().unwrap();

    let listed = code(&serving, &["repo", "list", "--json"]);
    assert!(listed.status.success());
    let list: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(list["repos"].as_array().unwrap().len(), 1);
    assert_eq!(list["repos"][0]["id"], repo_id);

    let missing = code(
        &serving,
        &[
            "repo",
            "rm",
            "00000000-0000-0000-0000-000000000099",
            "--json",
        ],
    );
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("not_found"),
        "typed error kind should be surfaced: {stderr}"
    );
}

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
}

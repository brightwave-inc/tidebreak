//! End-to-end smoke test of the `tidebreak` process surface.
//!
//! This binary also pins which data a command uses: the app's own profile by
//! default, never a new one in the folder a command runs from. It is the CLI
//! binary the keychain-enabled CI lane runs, so the tests that need an
//! embedded desktop profile live here too.

#[cfg(feature = "keychain")]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
#[cfg(feature = "keychain")]
use std::process::Child;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    assert!(
        response.contains(r#""status":"ok""#),
        "response: {response}"
    );
    assert!(response.contains(r#""api_level":"#), "response: {response}");
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

// Which data a command uses. Every test below runs with a scratch home, so
// "the app" is a folder under a temporary directory and never the real one.

/// A scratch home, and a project folder to run commands from.
struct Scratch {
    root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            root,
            home,
            project,
        }
    }

    /// A path in the scratch directory, outside the home and the project.
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Where this build looks for the app's data under the scratch home: the
    /// dev app's folder for a debug build, the released app's otherwise.
    fn app_dir(&self) -> PathBuf {
        let identifier = if cfg!(debug_assertions) {
            "io.brightwave.tidebreak.dev"
        } else {
            "io.brightwave.tidebreak"
        };
        let data = if cfg!(target_os = "macos") {
            self.home.join("Library").join("Application Support")
        } else if cfg!(windows) {
            self.home.join("AppData").join("Roaming")
        } else {
            self.home.join(".local").join("share")
        };
        data.join(identifier)
    }

    /// `tidebreak`, run from the project folder with the scratch home, no data
    /// directory named, and nothing else in the environment pointing it at
    /// another profile or server.
    fn tidebreak(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tidebreak"));
        command
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
            .env("APPDATA", self.home.join("AppData").join("Roaming"))
            .env("TIDEBREAK_KEYCHAIN_MOCK", "1")
            .env_remove("TIDEBREAK_DATA_DIR")
            .env_remove("TIDEBREAK_PROFILE")
            .env_remove("TIDEBREAK_SERVER_URL")
            .env_remove("TIDEBREAK_SERVER_TOKEN")
            .env_remove("TIDEBREAK_KEYCHAIN_SERVICE")
            .env_remove("TIDEBREAK_LISTEN_ADDR")
            .env_remove("TIDEBREAK_MCP_CONFIG")
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null());
        command
    }

    /// Publish a `listen.json` in the app's data folder, the way the app does
    /// when it starts.
    fn publish_app_endpoint(&self, base_url: &str, token: &str) {
        let dir = self.app_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let import_token = ["fixture", "import"].join("-");
        let endpoint = serde_json::json!({
            "base_url": base_url,
            "token": token,
            "local_import_token": import_token,
        });
        std::fs::write(dir.join("listen.json"), endpoint.to_string()).unwrap();
    }

    /// Nothing was written into the folder the command ran from.
    fn assert_project_untouched(&self) {
        let entries: Vec<_> = std::fs::read_dir(&self.project)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            entries.is_empty(),
            "the command wrote into the folder it ran from: {entries:?}"
        );
    }
}

/// A made-up bearer. Built from pieces so nothing here reads as a credential.
fn fixture_token() -> String {
    ["fixture", "bearer"].join("-")
}

/// Long enough for a debug build on a loaded runner, and short of nextest's
/// own limit.
const COMMAND_LIMIT: Duration = Duration::from_secs(15);

/// Run `command` to its end, or kill it and fail after `limit`. A command
/// that starts a server instead of finishing is what several of these tests
/// guard against, so none of them may wait forever.
fn output_within(command: &mut Command, limit: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak");
    let collect = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).ok();
            bytes
        })
    };
    let stdout = collect(Box::new(child.stdout.take().unwrap()));
    let stderr = collect(Box::new(child.stderr.take().unwrap()));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > limit {
            child.kill().ok();
            child.wait().ok();
            panic!("tidebreak was still running after {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// A stand-in for the app's server: it answers on loopback and records the
/// head of every request it gets.
struct FakeApp {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeApp {
    /// Answer `/healthz`, an empty chat list, and a `404` for everything else,
    /// `/version` included, the way a server from before the version check
    /// does.
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                answer_one(stream, &seen);
            }
        });
        Self { base_url, requests }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn answer_one(mut stream: TcpStream, seen: &Mutex<Vec<String>>) {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    seen.lock().unwrap().push(head);
    let (status, body) = if path == "/healthz" {
        ("200 OK", r#"{"status":"ok"}"#)
    } else if path == "/chats" {
        ("200 OK", "[]")
    } else {
        (
            "404 Not Found",
            r#"{"kind":"not_found","message":"no route"}"#,
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// A bare `tidebreak` prints help. It used to run `serve` over the folder it
/// ran in, which is how a stray profile appeared in a project.
#[test]
fn a_bare_tidebreak_prints_help_and_starts_nothing() {
    let scratch = Scratch::new();
    let output = output_within(&mut scratch.tidebreak(), COMMAND_LIMIT);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Tidebreak command-line interface"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Which data the CLI uses"),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("listening on"), "stdout: {stdout}");
    scratch.assert_project_untouched();
    assert!(
        !scratch.app_dir().exists(),
        "help must not create the app's data folder"
    );
}

/// With no app running and no data directory named, a client command stops,
/// says what to do instead, and opens no profile anywhere. A `listen.json` an
/// app left behind when it stopped reads the same way.
#[test]
fn without_the_app_a_client_command_says_what_to_do_instead() {
    let scratch = Scratch::new();
    let check = |why: &str| {
        let output = output_within(scratch.tidebreak().args(["chat", "list"]), COMMAND_LIMIT);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{why}: {stderr}");
        assert!(
            stderr.contains("Tidebreak is not running"),
            "{why}: {stderr}"
        );
        assert!(
            stderr.contains("--embed") && stderr.contains("TIDEBREAK_DATA_DIR"),
            "{why}: the message names the ways forward: {stderr}"
        );
        scratch.assert_project_untouched();
        assert!(
            !scratch.app_dir().join("tidebreak.db").exists(),
            "{why}: nothing may open the app's data"
        );
    };

    check("no app");
    assert!(
        !scratch.app_dir().exists(),
        "looking for the app must not create its data folder"
    );

    // The port is free again once the listener is dropped, as it is after
    // the app that published it has stopped.
    let closed = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    scratch.publish_app_endpoint(&format!("http://{closed}"), &fixture_token());
    check("a listen.json left behind");
}

/// A client command with nothing to say otherwise connects to the running
/// app with the bearer the app published, and opens nothing locally.
#[test]
fn a_client_command_connects_to_the_running_app() {
    let scratch = Scratch::new();
    let app = FakeApp::start();
    let token = fixture_token();
    scratch.publish_app_endpoint(&app.base_url, &token);

    let output = output_within(
        scratch
            .tidebreak()
            .args(["chat", "list", "--output-format", "json"]),
        COMMAND_LIMIT,
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("chat list prints one JSON document");
    assert_eq!(document["chats"], serde_json::json!([]), "{document}");

    let requests = app.requests();
    let listed = requests
        .iter()
        .find(|head| head.starts_with("GET /chats "))
        .unwrap_or_else(|| panic!("the app was never asked for its chats: {requests:?}"));
    assert!(
        listed
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {token}")),
        "the request must carry the app's bearer: {listed}"
    );
    scratch.assert_project_untouched();
    assert!(
        !scratch.app_dir().join("tidebreak.db").exists(),
        "connecting to the app opens nothing locally"
    );
}

/// The app only listens on this computer. A `listen.json` that names another
/// host did not come from it, and the command sends nothing there.
#[test]
fn a_listen_file_naming_another_computer_is_not_followed() {
    let scratch = Scratch::new();
    scratch.publish_app_endpoint("https://tidebreak.example.invalid", &fixture_token());
    let output = output_within(scratch.tidebreak().args(["chat", "list"]), COMMAND_LIMIT);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("not on this computer"), "{stderr}");
    scratch.assert_project_untouched();
}

/// agent-mcp drives a server that is already running. A named data directory
/// alone does not start one, or the MCP client that launched it would drive an
/// empty profile without knowing.
#[test]
fn agent_mcp_starts_a_server_only_with_embed() {
    let scratch = Scratch::new();
    let named = scratch.path("named-profile");
    let output = output_within(
        scratch
            .tidebreak()
            .arg("agent-mcp")
            .env("TIDEBREAK_DATA_DIR", &named),
        COMMAND_LIMIT,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    for option in ["--attach", "--server", "--embed"] {
        assert!(
            stderr.contains(option),
            "the refusal names {option}: {stderr}"
        );
    }
    assert!(output.stdout.is_empty(), "stdout carries JSON-RPC only");
    assert!(!named.exists(), "no server may start over the named folder");

    // With nothing named and no app running, it stops the way every client
    // command does.
    let output = output_within(scratch.tidebreak().arg("agent-mcp"), COMMAND_LIMIT);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Tidebreak is not running"), "{stderr}");
    assert!(output.stdout.is_empty(), "stdout carries JSON-RPC only");
    scratch.assert_project_untouched();
}

/// The whole path against a real server. `serve` with nothing named serves the
/// app's own data and publishes `listen.json` there, as the app does, and
/// client commands reach it with no flag. A second server over the same data
/// is refused while it runs.
#[cfg(feature = "keychain")]
#[test]
fn commands_share_the_app_profile_and_never_start_one_in_the_project() {
    let scratch = Scratch::new();
    let mut child = scratch
        .tidebreak()
        .arg("serve")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn tidebreak serve");
    let stdout = child.stdout.take().unwrap();
    let _reaper = Reaper(child);
    // Read both announcement lines and keep the pipe open: the daemon fails
    // if it writes to a stdout nobody reads.
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines.next().unwrap().unwrap();
    let _token_line = lines.next().unwrap().unwrap();
    assert!(
        addr_line.contains("listening on http://127.0.0.1:"),
        "unexpected addr line: {addr_line:?}"
    );
    assert!(
        scratch.app_dir().join("listen.json").is_file(),
        "serve with nothing named serves the app's data"
    );

    let created = output_within(
        scratch
            .tidebreak()
            .args(["chat", "create", "--output-format", "json"]),
        COMMAND_LIMIT,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let chat = created["id"].as_str().expect("chat create names the chat");

    // The next command finds the chat, so both reached the same profile.
    let outputs = output_within(
        scratch
            .tidebreak()
            .args(["output", "list", chat, "--output-format", "json"]),
        COMMAND_LIMIT,
    );
    assert!(
        outputs.status.success(),
        "{}",
        String::from_utf8_lossy(&outputs.stderr)
    );

    let embedded = output_within(
        scratch.tidebreak().args(["--embed", "chat", "list"]),
        COMMAND_LIMIT,
    );
    let stderr = String::from_utf8_lossy(&embedded.stderr);
    assert!(!embedded.status.success(), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");
    scratch.assert_project_untouched();
    drop(lines);
}

/// `--embed` opens the app's data in this process while the app is closed,
/// instead of a profile in the folder the command ran from.
#[cfg(feature = "keychain")]
#[test]
fn embed_works_on_the_app_data_while_the_app_is_closed() {
    let scratch = Scratch::new();
    let created = output_within(
        scratch
            .tidebreak()
            .args(["--embed", "chat", "create", "--output-format", "json"]),
        COMMAND_LIMIT,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let chat = created["id"].as_str().expect("chat create names the chat");
    assert!(scratch.app_dir().join("tidebreak.db").is_file());
    scratch.assert_project_untouched();

    let outputs = output_within(
        scratch
            .tidebreak()
            .args(["--embed", "output", "list", chat, "--output-format", "json"]),
        COMMAND_LIMIT,
    );
    assert!(
        outputs.status.success(),
        "the chat is in the app's data: {}",
        String::from_utf8_lossy(&outputs.stderr)
    );
}

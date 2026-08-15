//! Capture a real engine invocation into `fixtures/<harness>/<version>/`.
//!
//! Usage (from the crate root, with the `capture` feature):
//!
//! ```text
//! cargo run -p tidebreak-harness --features capture --bin tidebreak-harness-capture -- \
//!   --harness claude-code --scenario plain-text --prompt "reply with exactly: hello from fixture"
//!
//! cargo run -p tidebreak-harness --features capture --bin tidebreak-harness-capture -- \
//!   --harness codex --scenario plain-text --prompt "reply with exactly: hello from fixture"
//! ```
//!
//! Writes `<scenario>.ndjson` and a `manifest.toml`. Redact the stream before
//! committing — see `fixtures/README.md`. Codex captures are framed JSON-RPC
//! (`{"dir":"in"|"out","msg":…}`). opencode captures are framed HTTP + SSE
//! (`{"dir":"in"|"out","msg":{"kind":"http"|"sse",…}}`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let mut harness = "claude-code".to_owned();
    let mut scenario = "plain-text".to_owned();
    let mut prompt = "reply with exactly: hello from fixture".to_owned();
    let mut extra: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--harness" => harness = args.next().expect("--harness needs a value"),
            "--scenario" => scenario = args.next().expect("--scenario needs a value"),
            "--prompt" => prompt = args.next().expect("--prompt needs a value"),
            "--" => {
                extra.extend(args);
                break;
            }
            other => extra.push(other.to_owned()),
        }
    }

    match harness.as_str() {
        "codex" => capture_codex(&scenario, &prompt, &extra),
        "opencode" => capture_opencode(&scenario, &prompt, &extra),
        _ => capture_claude(&harness, &scenario, &prompt, &extra),
    }
}

fn capture_claude(harness: &str, scenario: &str, prompt: &str, extra: &[String]) {
    let workspace = tempfile_workspace();
    let binary = resolve_binary("claude");
    let version = claude_version(&binary);
    let mut argv = vec![
        binary.clone(),
        "-p".into(),
        prompt.to_owned(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
    ];
    argv.extend(extra.iter().cloned());

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {binary}: {err}"));
    let stdout = child.stdout.take().expect("stdout");
    let dest_dir = fixture_dir(harness, &version);
    let ndjson_path = dest_dir.join(format!("{scenario}.ndjson"));
    let mut out = std::fs::File::create(&ndjson_path).unwrap();
    std::io::copy(&mut std::io::BufReader::new(stdout), &mut out).unwrap();
    let status = child.wait().unwrap();
    write_manifest(
        &dest_dir, harness, &version, scenario, &argv, &workspace, &status,
    );
    eprintln!("wrote {}", ndjson_path.display());
}

fn capture_codex(scenario: &str, prompt: &str, extra: &[String]) {
    let workspace = tempfile_workspace();
    let binary = resolve_binary("codex");
    let version = codex_version(&binary);
    let mut argv = vec![binary.clone(), "app-server".into(), "--stdio".into()];
    argv.extend(extra.iter().cloned());

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {binary}: {err}"));
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let dest_dir = fixture_dir("codex", &version);
    let ndjson_path = dest_dir.join(format!("{scenario}.ndjson"));
    let mut out = std::fs::File::create(&ndjson_path).unwrap();
    let mut reader = BufReader::new(stdout);

    write_rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": { "name": "tidebreak-harness-capture", "version": "0.0.0" },
                "capabilities": { "experimentalApi": false }
            }
        }),
    );
    wait_for_rpc_id(&mut reader, &mut out, 1);
    write_rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({ "method": "initialized" }),
    );
    write_rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "id": 2,
            "method": "thread/start",
            "params": {
                "cwd": workspace,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "ephemeral": true
            }
        }),
    );
    let thread = wait_for_rpc_id(&mut reader, &mut out, 2);
    let thread_id = thread
        .pointer("/result/thread/id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    if thread_id.is_empty() {
        let _ = child.kill();
        panic!("thread/start did not return a thread id: {thread}");
    }
    write_rpc(
        &mut stdin,
        &mut out,
        serde_json::json!({
            "id": 3,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": [{ "type": "text", "text": prompt }]
            }
        }),
    );
    wait_for_rpc_id(&mut reader, &mut out, 3);
    wait_for_turn_completed(&mut reader, &mut out);
    drop(stdin);
    let status = child.wait().unwrap();
    write_manifest(
        &dest_dir, "codex", &version, scenario, &argv, &workspace, &status,
    );
    eprintln!("wrote {}", ndjson_path.display());
}

fn capture_opencode(scenario: &str, prompt: &str, extra: &[String]) {
    let workspace = tempfile_workspace();
    let binary = resolve_binary("opencode");
    let version = opencode_version(&binary);
    let port = pick_ephemeral_port();
    let mut argv = vec![
        binary.clone(),
        "serve".into(),
        "--hostname".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ];
    argv.extend(extra.iter().cloned());

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {binary}: {err}"));
    wait_for_health(port);

    let dest_dir = fixture_dir("opencode", &version);
    let ndjson_path = dest_dir.join(format!("{scenario}.ndjson"));
    let mut out = std::fs::File::create(&ndjson_path).unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let dir_q = format!(
        "directory={}",
        url_encode(workspace.to_string_lossy().as_ref())
    );

    let sse = std::thread::spawn({
        let base = base.clone();
        let dir_q = dir_q.clone();
        let path = ndjson_path.clone();
        move || tee_sse(&base, &dir_q, &path)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));

    let create = serde_json::json!({
        "title": scenario,
        "agent": "build",
        "model": { "providerID": "opencode", "id": "big-pickle" }
    });
    let session = http_json(
        &mut out,
        "POST",
        "/session",
        &format!("{base}/session?{dir_q}"),
        Some(&create),
    );
    let session_id = session
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    if session_id.is_empty() {
        let _ = child.kill();
        panic!("POST /session did not return an id: {session}");
    }
    let body = serde_json::json!({
        "model": { "providerID": "opencode", "modelID": "big-pickle" },
        "parts": [{ "type": "text", "text": prompt }]
    });
    let prompt_path = format!("/session/{session_id}/prompt_async");
    http_json(
        &mut out,
        "POST",
        &prompt_path,
        &format!("{base}{prompt_path}?{dir_q}"),
        Some(&body),
    );
    wait_for_idle_file(&ndjson_path);
    let _ = child.kill();
    let status = child.wait().unwrap();
    let _ = sse.join();
    write_manifest(
        &dest_dir, "opencode", &version, scenario, &argv, &workspace, &status,
    );
    eprintln!("wrote {}", ndjson_path.display());
}

fn pick_ephemeral_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

fn wait_for_health(port: u16) {
    let url = format!("http://127.0.0.1:{port}/global/health");
    for _ in 0..50 {
        let output = Command::new("curl")
            .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", &url])
            .output();
        if let Ok(output) = output {
            if String::from_utf8_lossy(&output.stdout).starts_with('2') {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("opencode serve on :{port} never became healthy");
}

fn wait_for_idle_file(path: &Path) {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains("\"session.idle\"") || text.contains("\"session.error\"") {
                std::thread::sleep(std::time::Duration::from_millis(200));
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timed out waiting for session.idle in {}", path.display());
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn write_http_frame(
    out: &mut std::fs::File,
    _dir: &str,
    method_or_status: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    inbound: bool,
) {
    let encoded_body = body
        .map(|value| serde_json::to_string(value).expect("body"))
        .unwrap_or_else(|| "null".into());
    if inbound {
        let status: u16 = method_or_status.parse().unwrap_or(0);
        writeln!(
            out,
            r#"{{"dir":"in","msg":{{"kind":"http","status":{status},"path":{path},"body":{encoded_body}}}}}"#,
            path = serde_json::to_string(path).unwrap(),
        )
        .unwrap();
    } else {
        writeln!(
            out,
            r#"{{"dir":"out","msg":{{"kind":"http","method":{method},"path":{path},"body":{encoded_body}}}}}"#,
            method = serde_json::to_string(method_or_status).unwrap(),
            path = serde_json::to_string(path).unwrap(),
        )
        .unwrap();
    }
}

fn http_json(
    out: &mut std::fs::File,
    method: &str,
    path: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> serde_json::Value {
    write_http_frame(out, "out", method, path, body, false);
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-D", "-", "-o", "-"]);
    cmd.arg("-X").arg(method);
    if let Some(body) = body {
        cmd.args(["-H", "content-type: application/json"]);
        cmd.arg("--data").arg(serde_json::to_string(body).unwrap());
    }
    cmd.arg(url);
    let output = cmd.output().expect("curl");
    let raw = String::from_utf8_lossy(&output.stdout);
    let (head, payload) = raw.split_once("\r\n\r\n").unwrap_or(("", raw.as_ref()));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("0");
    let parsed: serde_json::Value = if payload.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(payload).unwrap_or(serde_json::Value::Null)
    };
    write_http_frame(out, "in", status, path, Some(&parsed), true);
    parsed
}

fn tee_sse(base: &str, dir_q: &str, dest: &Path) {
    let mut child = Command::new("curl")
        .args(["-sS", "--no-buffer", &format!("{base}/event?{dir_q}")])
        .stdout(Stdio::piped())
        .spawn()
        .expect("curl sse");
    let stdout = child.stdout.take().expect("sse stdout");
    let mut reader = BufReader::new(stdout);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dest)
        .expect("append sse");
    let mut pending = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.trim().is_empty() {
            if let Some(data) = pending
                .lines()
                .filter_map(|item| item.strip_prefix("data:"))
                .map(str::trim)
                .find(|item| !item.is_empty())
            {
                writeln!(
                    file,
                    r#"{{"dir":"in","msg":{{"kind":"sse","event":{data}}}}}"#
                )
                .ok();
                let _ = file.flush();
            }
            pending.clear();
            continue;
        }
        pending.push_str(&line);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn write_rpc(
    stdin: &mut std::process::ChildStdin,
    out: &mut std::fs::File,
    msg: serde_json::Value,
) {
    let encoded = serde_json::to_string(&msg).expect("rpc request");
    writeln!(out, r#"{{"dir":"out","msg":{encoded}}}"#).unwrap();
    writeln!(stdin, "{encoded}").unwrap();
    stdin.flush().unwrap();
}

fn wait_for_rpc_id(
    reader: &mut BufReader<std::process::ChildStdout>,
    out: &mut std::fs::File,
    id: i64,
) -> serde_json::Value {
    loop {
        let value = read_inbound_frame(reader, out);
        if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
            return value;
        }
    }
}

fn wait_for_turn_completed(
    reader: &mut BufReader<std::process::ChildStdout>,
    out: &mut std::fs::File,
) {
    loop {
        let value = read_inbound_frame(reader, out);
        if value.get("method").and_then(serde_json::Value::as_str) == Some("turn/completed") {
            return;
        }
        if value.get("error").is_some() && value.get("id").is_some() {
            return;
        }
        // Auto-decline parked approvals so a capture does not hang.
        if value.get("method").and_then(serde_json::Value::as_str)
            == Some("item/commandExecution/requestApproval")
        {
            eprintln!("approval request observed; capture.bin does not decide it");
        }
    }
}

fn read_inbound_frame(
    reader: &mut BufReader<std::process::ChildStdout>,
    out: &mut std::fs::File,
) -> serde_json::Value {
    let mut line = String::new();
    let n = reader.read_line(&mut line).expect("app-server stdout");
    if n == 0 {
        panic!("app-server stdout closed before the capture finished");
    }
    let line = line.trim();
    writeln!(out, r#"{{"dir":"in","msg":{line}}}"#).unwrap();
    serde_json::from_str(line).unwrap_or_else(|err| panic!("inbound json: {err}: {line}"))
}

fn fixture_dir(harness: &str, version: &str) -> PathBuf {
    let dest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(harness)
        .join(version);
    std::fs::create_dir_all(&dest_dir).unwrap();
    dest_dir
}

fn write_manifest(
    dest_dir: &Path,
    harness: &str,
    version: &str,
    scenario: &str,
    argv: &[String],
    workspace: &Path,
    status: &std::process::ExitStatus,
) {
    let date = iso_date();
    let manifest = format!(
        "harness = \"{harness}\"\n\
         version = \"{version}\"\n\
         scenario = \"{scenario}\"\n\
         date = \"{date}\"\n\
         argv = {argv:?}\n\
         cwd = \"{cwd}\"\n\
         exit_status = \"{status}\"\n\
         redaction_notes = \"Not redacted by capture. Follow fixtures/README.md before committing.\"\n",
        cwd = workspace.display(),
    );
    std::fs::write(dest_dir.join("manifest.toml"), manifest).unwrap();
}

fn resolve_binary(name: &str) -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let output = Command::new(shell)
        .args(["-ilc", &format!("command -v {name}")])
        .output()
        .expect("login shell");
    let path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    if path.is_empty() {
        panic!("{name} not found on the login-shell PATH");
    }
    path
}

fn claude_version(binary: &str) -> String {
    first_version_token(binary, true)
}

fn codex_version(binary: &str) -> String {
    first_version_token(binary, false)
}

fn opencode_version(binary: &str) -> String {
    first_version_token(binary, true)
}

fn first_version_token(binary: &str, first_word: bool) -> String {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|err| panic!("{binary} --version: {err}"));
    let line = String::from_utf8_lossy(&output.stdout);
    let line = line
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown");
    if first_word {
        line.split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_owned()
    } else {
        line.split_whitespace()
            .last()
            .unwrap_or("unknown")
            .to_owned()
    }
}

fn tempfile_workspace() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let dir = std::env::temp_dir().join(format!("tidebreak-harness-capture-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    init_git(&dir);
    dir
}

fn init_git(dir: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .expect("git init");
    if !status.success() {
        panic!("git init failed");
    }
    std::fs::write(
        dir.join("README.md"),
        "demo\n\nA tiny fixture repository.\n",
    )
    .unwrap();
    let _ = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(dir)
        .status();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=capture@example.invalid",
            "-c",
            "user.name=capture",
            "commit",
            "-qm",
            "init",
        ])
        .current_dir(dir)
        .status();
}

fn iso_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    format!("{secs}")
}

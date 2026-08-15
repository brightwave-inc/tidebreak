//! Capture a real engine invocation into `fixtures/<harness>/<version>/`.
//!
//! Usage (from the crate root, with the `capture` feature):
//!
//! ```text
//! cargo run -p tidebreak-harness --features capture --bin tidebreak-harness-capture -- \
//!   --harness claude-code --scenario plain-text --prompt "reply with exactly: hello from fixture"
//! ```
//!
//! Writes `<scenario>.ndjson` and a `manifest.toml`. Redact the stream before
//! committing — see `fixtures/README.md`.

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

    let workspace = tempfile_workspace();
    let binary = resolve_claude();
    let version = claude_version(&binary);
    let mut argv = vec![
        binary.clone(),
        "-p".into(),
        prompt.clone(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
    ];
    argv.extend(extra);

    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {binary}: {err}"));
    let stdout = child.stdout.take().expect("stdout");
    let dest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(&harness)
        .join(&version);
    std::fs::create_dir_all(&dest_dir).unwrap();
    let ndjson_path = dest_dir.join(format!("{scenario}.ndjson"));
    let mut out = std::fs::File::create(&ndjson_path).unwrap();
    std::io::copy(&mut std::io::BufReader::new(stdout), &mut out).unwrap();
    let status = child.wait().unwrap();
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
    eprintln!("wrote {}", ndjson_path.display());
}

fn resolve_claude() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let output = Command::new(shell)
        .args(["-lc", "command -v claude"])
        .output()
        .expect("login shell");
    let path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    if path.is_empty() {
        panic!("claude not found on the login-shell PATH");
    }
    path
}

fn claude_version(binary: &str) -> String {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("claude --version");
    let line = String::from_utf8_lossy(&output.stdout);
    line.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown")
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_owned()
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

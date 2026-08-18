//! Interactive-login-shell binary resolution, version detection, and
//! environment capture.
//!
//! GUI processes on macOS do not inherit the user's shell PATH or profile
//! environment. Resolution therefore asks `$SHELL -ilc '…'` — login *and*
//! interactive, so zsh sources `.zshrc` where version managers and gateway
//! config commonly live — and accepts only an absolute, executable result.
//! The same probe captures the shell's resolved environment so children
//! run under that snapshot, not the GUI process env.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::Command;
use tokio::time::timeout;

use crate::is_absolute_executable;

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STDERR_BYTES: usize = 4_096;

/// Host environment used for discovery. Tests inject a shim shell and PATH.
#[derive(Debug, Clone)]
pub struct HostEnv {
    /// Login shell. Defaults to `$SHELL`, then `/bin/sh`.
    pub shell: PathBuf,
    /// Extra environment for the probe child (typically a test PATH).
    pub env: Vec<(OsString, OsString)>,
    /// When set, replace the process environment entirely with `env`.
    pub clear_env: bool,
    /// App data directory. When set, probe prefers a pinned install under it.
    pub data_dir: Option<PathBuf>,
}

impl Default for HostEnv {
    fn default() -> Self {
        Self::from_process()
    }
}

impl HostEnv {
    /// The current process's login shell.
    #[must_use]
    pub fn from_process() -> Self {
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Self {
            shell,
            env: Vec::new(),
            clear_env: false,
            data_dir: None,
        }
    }
}

/// What one interactive-login probe recovered.
#[derive(Debug, Clone)]
pub struct ProbeCapture {
    /// Absolute, executable path.
    pub binary: PathBuf,
    /// The shell's resolved environment, unfiltered.
    pub env: Vec<(OsString, OsString)>,
    /// Bounded stderr from the probe, for the doctor surface.
    pub stderr: String,
}

/// Probe failure.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The login shell printed nothing useful, or the binary is missing.
    #[error("binary {0} not found on the login-shell PATH")]
    NotFound(String),
    /// `command -v` returned a relative path; only absolute paths are accepted.
    #[error("binary resolution for {name} returned a relative path: {path}")]
    RelativePath {
        /// Binary name that was requested.
        name: String,
        /// The relative path the shell printed.
        path: String,
    },
    /// The resolved path exists but is not executable.
    #[error("resolved path {0} is not an executable file")]
    NotExecutable(PathBuf),
    /// The login shell itself failed.
    #[error("login shell failed: {0}")]
    Shell(String),
}

/// Resolve `name` through the user's interactive login shell.
pub async fn resolve_binary(host: &HostEnv, name: &str) -> Result<PathBuf, ProbeError> {
    Ok(probe_shell(host, name).await?.binary)
}

/// Resolve `name` and capture the shell's environment in one probe.
///
/// The command is `$SHELL -ilc '…'` with unique sentinel markers so
/// profile banners cannot forge the result.
pub async fn probe_shell(host: &HostEnv, name: &str) -> Result<ProbeCapture, ProbeError> {
    if name.is_empty() || name.contains(['/', '\\', '\0', '\'', '"', ';', '|', '&']) {
        return Err(ProbeError::NotFound(name.to_owned()));
    }
    if let Some(data_dir) = &host.data_dir {
        if let Some(kind) = kind_for_command(name) {
            if let Some(binary) = crate::managed_binary(data_dir, kind) {
                let env = capture_shell_env(host).await.unwrap_or_default();
                return Ok(ProbeCapture {
                    binary,
                    env,
                    stderr: String::new(),
                });
            }
            // Do not npm-install or fall through to PATH here. Listing
            // workspaces must not wait on a download. Create-session and
            // doctor refresh install the pin.
            return Err(ProbeError::NotFound(name.to_owned()));
        }
    }
    let token = sentinel_token();
    let begin = format!("TIDEBREAK_PROBE_BEGIN_{token}");
    let env_mark = format!("TIDEBREAK_PROBE_ENV_{token}");
    let end = format!("TIDEBREAK_PROBE_END_{token}");
    let script = format!(
        "printf '%s\\n' '{begin}'; command -v {name} || true; printf '%s\\n' '{env_mark}'; \
         if env -0 >/dev/null 2>&1; then env -0; else env; fi; printf '\\n%s\\n' '{end}'"
    );
    let output = run_interactive_login_shell(host, &script).await?;
    let parsed =
        parse_sentinel_output(&output.stdout, &begin, &env_mark, &end).ok_or_else(|| {
            if output.stdout.trim().is_empty() && !output.status_ok {
                ProbeError::NotFound(name.to_owned())
            } else {
                ProbeError::Shell("probe sentinels missing from shell output".into())
            }
        })?;
    if parsed.path.is_empty() {
        return Err(ProbeError::NotFound(name.to_owned()));
    }
    let path = PathBuf::from(&parsed.path);
    if !path.is_absolute() {
        return Err(ProbeError::RelativePath {
            name: name.to_owned(),
            path: parsed.path,
        });
    }
    if !is_absolute_executable(&path) {
        return Err(ProbeError::NotExecutable(path));
    }
    Ok(ProbeCapture {
        binary: path,
        env: parsed.env,
        stderr: output.stderr,
    })
}

fn kind_for_command(name: &str) -> Option<tidebreak_core::HarnessKind> {
    match name {
        "claude" => Some(tidebreak_core::HarnessKind::ClaudeCode),
        "codex" => Some(tidebreak_core::HarnessKind::Codex),
        "opencode" => Some(tidebreak_core::HarnessKind::Opencode),
        "grok" => Some(tidebreak_core::HarnessKind::Grok),
        _ => None,
    }
}

async fn capture_shell_env(host: &HostEnv) -> Result<Vec<(OsString, OsString)>, ProbeError> {
    let token = sentinel_token();
    let begin = format!("TIDEBREAK_PROBE_BEGIN_{token}");
    let env_mark = format!("TIDEBREAK_PROBE_ENV_{token}");
    let end = format!("TIDEBREAK_PROBE_END_{token}");
    let script = format!(
        "printf '%s\\n' '{begin}'; printf '%s\\n' '{env_mark}'; \
         if env -0 >/dev/null 2>&1; then env -0; else env; fi; printf '\\n%s\\n' '{end}'"
    );
    let output = run_interactive_login_shell(host, &script).await?;
    let parsed = parse_sentinel_output(&output.stdout, &begin, &env_mark, &end)
        .ok_or_else(|| ProbeError::Shell("probe sentinels missing from shell output".into()))?;
    Ok(parsed.env)
}

/// Drop Tidebreak-prefixed variables from a captured shell snapshot.
#[must_use]
pub fn filter_child_env<I, K, V>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    vars.into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .filter(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        })
        .collect()
}

/// Run `<binary> --version` and return the first line, trimmed.
pub async fn observe_version(binary: &Path) -> Result<String, ProbeError> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| ProbeError::Shell("version probe timed out".into()))?
        .map_err(|err| ProbeError::Shell(err.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    if line.is_empty() {
        return Err(ProbeError::Shell("version probe produced no output".into()));
    }
    Ok(line)
}

/// One model a harness CLI listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedHarnessModel {
    /// Token the engine accepts on `--model`.
    pub id: String,
    /// Display label, when the CLI printed one.
    pub label: String,
    /// Whether this row is the engine's current or default model.
    pub default: bool,
}

/// Run `<binary> models` (or `args`) and parse one model per remaining line.
pub async fn list_cli_models(
    binary: &Path,
    args: &[&str],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<ListedHarnessModel> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let Ok(Ok(output)) = timeout(PROBE_TIMEOUT, command.output()).await else {
        return Vec::new();
    };
    parse_cli_models(&String::from_utf8_lossy(&output.stdout), env)
}

/// Mark the engine's current model from listing markers or its own env.
pub fn infer_listed_default(
    models: &mut [ListedHarnessModel],
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) {
    if models.iter().any(|model| model.default) {
        return;
    }
    const KEYS: &[&str] = &[
        "ANTHROPIC_MODEL",
        "CLAUDE_MODEL",
        "OPENAI_MODEL",
        "CODEX_MODEL",
        "GROK_MODEL",
        "XAI_MODEL",
        "OPENCODE_MODEL",
    ];
    let Some(current) = env.iter().find_map(|(key, value)| {
        let name = key.to_string_lossy();
        if !KEYS.contains(&name.as_ref()) {
            return None;
        }
        let id = value.to_string_lossy().trim().to_owned();
        (!id.is_empty()).then_some(id)
    }) else {
        return;
    };
    if let Some(model) = models.iter_mut().find(|model| {
        model.id == current || current.ends_with(&model.id) || model.id.ends_with(&current)
    }) {
        model.default = true;
    }
}

fn parse_cli_models(
    stdout: &str,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<ListedHarnessModel> {
    let mut models: Vec<ListedHarnessModel> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("you are ") || lower.contains("not authenticated") {
            continue;
        }
        if lower.starts_with("available models") {
            continue;
        }
        if let Some(rest) = lower
            .strip_prefix("default model:")
            .or_else(|| lower.strip_prefix("current model:"))
        {
            if let Some(id) = rest
                .split_whitespace()
                .next()
                .and_then(|token| normalize_model_id(token))
            {
                push_listed(&mut models, id, true);
            }
            continue;
        }
        let default = lower.contains("(current)")
            || lower.contains("(default)")
            || trimmed.contains('*')
            || trimmed.contains('✓');
        let token = trimmed
            .trim_start_matches(|ch: char| matches!(ch, '-' | '*' | '•' | '✓' | '►'))
            .trim();
        let Some(id) = token
            .split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ',')
            .next()
            .and_then(normalize_model_id)
        else {
            continue;
        };
        push_listed(&mut models, id, default);
    }
    infer_listed_default(&mut models, env);
    models
}

fn normalize_model_id(token: &str) -> Option<String> {
    let id = token
        .trim_matches(|ch: char| matches!(ch, '*' | '-' | '•' | ':' | '"' | '\''))
        .to_owned();
    looks_like_model_id(&id).then_some(id)
}

fn looks_like_model_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let lower = id.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "current" | "usage" | "default" | "available" | "model" | "models" | "name"
    ) {
        return false;
    }
    if matches!(lower.as_str(), "sonnet" | "opus" | "haiku" | "fable") {
        return true;
    }
    let has_digit = id.chars().any(|ch| ch.is_ascii_digit());
    let has_sep = id.contains('-') || id.contains('/') || id.contains('.');
    has_digit
        && has_sep
        && id.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.' | ':' | '[' | ']')
        })
}

fn push_listed(models: &mut Vec<ListedHarnessModel>, id: String, default: bool) {
    if let Some(existing) = models.iter_mut().find(|model| model.id == id) {
        existing.default |= default;
        return;
    }
    models.push(ListedHarnessModel {
        label: display_model_label(&id),
        id,
        default,
    });
}

/// Human label from a CLI id: `claude-sonnet-5` → `Claude Sonnet 5`.
pub fn display_model_label(id: &str) -> String {
    let leaf = id.rsplit('/').next().unwrap_or(id);
    leaf.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.eq_ignore_ascii_case("gpt") {
                "GPT".to_owned()
            } else {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Prefer gateway ids when the listing includes any.
pub fn prefer_gateway_models(models: Vec<ListedHarnessModel>) -> Vec<ListedHarnessModel> {
    if models
        .iter()
        .any(|model| model.id.contains("model-gateway"))
    {
        return models
            .into_iter()
            .filter(|model| model.id.contains("model-gateway"))
            .collect();
    }
    models
}

#[cfg(test)]
mod list_cli_models_tests {
    use super::parse_cli_models;

    #[test]
    fn skips_login_headers_and_dedupes() {
        let listed = parse_cli_models(
            "You are logged in with grok.com.\n\
             grok-4.5\n\
             grok-4\n\
             grok-4.5\n",
            &[],
        );
        assert_eq!(
            listed
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["grok-4.5", "grok-4"]
        );
        assert!(!listed.iter().any(|model| model.default));
    }

    #[test]
    fn marks_the_current_row_and_env_fallback() {
        let marked = parse_cli_models("* sonnet (current)\nopus\nhaiku\n", &[]);
        assert_eq!(
            marked
                .iter()
                .find(|model| model.default)
                .map(|model| model.id.as_str()),
            Some("sonnet")
        );
        let from_env = parse_cli_models(
            "grok-4.5\ngrok-4\n",
            &[(
                std::ffi::OsString::from("GROK_MODEL"),
                std::ffi::OsString::from("grok-4"),
            )],
        );
        assert_eq!(
            from_env
                .iter()
                .find(|model| model.default)
                .map(|model| model.id.as_str()),
            Some("grok-4")
        );
    }

    #[test]
    fn ignores_tui_chrome_and_keeps_real_ids() {
        let listed = parse_cli_models(
            "Current\n\
             Usage\n\
             Default model: model-gateway-model-gateway/grok-4.6\n\
             Available models:\n\
               - grok-4.5\n\
               * model-gateway-model-gateway/grok-4.6 (default)\n",
            &[],
        );
        assert_eq!(
            listed
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["model-gateway-model-gateway/grok-4.6", "grok-4.5",]
        );
        assert_eq!(
            listed
                .iter()
                .find(|model| model.id.ends_with("grok-4.6"))
                .map(|model| (model.label.as_str(), model.default)),
            Some(("Grok 4.6", true))
        );
    }
}

struct ShellOutput {
    stdout: String,
    stderr: String,
    status_ok: bool,
}

struct SentinelPayload {
    path: String,
    env: Vec<(OsString, OsString)>,
}

fn sentinel_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", std::process::id(), nanos)
}

fn parse_sentinel_output(
    stdout: &str,
    begin: &str,
    env_mark: &str,
    end: &str,
) -> Option<SentinelPayload> {
    let after_begin = stdout.split_once(begin)?.1;
    let (between, after_env) = after_begin.split_once(env_mark)?;
    let env_block = after_env.split_once(end)?.0;
    let path = between
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned();
    let env = if env_block.contains('\0') {
        env_block
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .filter_map(split_env_entry)
            .collect()
    } else {
        env_block
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(split_env_entry)
            .collect()
    };
    Some(SentinelPayload { path, env })
}

fn split_env_entry(entry: &str) -> Option<(OsString, OsString)> {
    let (key, value) = entry.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((OsString::from(key), OsString::from(value)))
}

async fn run_interactive_login_shell(
    host: &HostEnv,
    script: &str,
) -> Result<ShellOutput, ProbeError> {
    let mut command = Command::new(&host.shell);
    command
        .arg("-ilc")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if host.clear_env {
        command.env_clear();
    }
    for (key, value) in &host.env {
        command.env(key, value);
    }
    let output = timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| ProbeError::Shell("login shell timed out".into()))?
        .map_err(|err| ProbeError::Shell(err.to_string()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    crate::text::truncate_on_char_boundary(&mut stderr, MAX_STDERR_BYTES);
    Ok(ShellOutput {
        stdout,
        stderr,
        status_ok: output.status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exec(path: &Path, body: &str) {
        // Write a sibling inode, fsync, then rename over `path` so execve
        // never sees a file that still has a writer (Linux ETXTBSY).
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let staging = path.with_extension("writing");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o755)
            .open(&staging)
            .unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::rename(&staging, path).unwrap();
        if let Some(parent) = path.parent() {
            let dir = std::fs::File::open(parent).unwrap();
            dir.sync_all().unwrap();
        }
    }

    /// Shim that prints profile noise, honors `-ilc`/`-c`, then evals the script.
    fn write_profile_shim(path: &Path, preamble: &str) {
        write_exec(
            path,
            &format!(
                r#"#!/bin/sh
echo 'welcome to my profile'
cmd=
while [ $# -gt 0 ]; do
  case "$1" in
    -c) shift; cmd=$1; break ;;
    -*c)
      rest=${{1#*c}}
      shift
      if [ -n "$rest" ]; then cmd=$rest; else cmd=$1; fi
      break
      ;;
    -*) shift ;;
    *) break ;;
  esac
done
{preamble}
eval "$cmd"
"#
            ),
        );
    }

    fn host_with_path(dir: &Path) -> HostEnv {
        HostEnv {
            shell: PathBuf::from("/bin/sh"),
            env: vec![("PATH".into(), dir.as_os_str().to_owned())],
            clear_env: true,
            data_dir: None,
        }
    }

    #[tokio::test]
    async fn missing_binary_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_binary(&host_with_path(dir.path()), "tb_missing_harness_bin")
            .await
            .unwrap_err();
        assert!(matches!(err, ProbeError::NotFound(_)));
    }

    async fn diagnose_probe_shim(host: &HostEnv) -> String {
        let script = "printf '%s\\n' 'TIDEBREAK_PROBE_BEGIN_diag'; command -v claude || true; \
             printf '%s\\n' 'TIDEBREAK_PROBE_ENV_diag'; \
             if env -0 >/dev/null 2>&1; then env -0; else env; fi; \
             printf '\\n%s\\n' 'TIDEBREAK_PROBE_END_diag'";
        match run_interactive_login_shell(host, script).await {
            Ok(out) => format!(
                "shim status_ok={} stdout={:?} stderr={:?}",
                out.status_ok, out.stdout, out.stderr
            ),
            Err(err) => format!("shim spawn failed: {err}"),
        }
    }

    #[tokio::test]
    async fn relative_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let empty_path = dir.path().join("empty-path");
        let home = dir.path().join("home");
        let tmp = dir.path().join("tmp");
        std::fs::create_dir(&empty_path).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&tmp).unwrap();
        let shell = dir.path().join("shell");
        // Closed-world fake shell: exact `-ilc`/`-c` matches, peel sentinels
        // with simple POSIX expansions, print a relative path. Never eval the
        // probe script or call host `command`/`env` — those differ by /bin/sh.
        write_exec(
            &shell,
            r#"#!/bin/sh
cmd=
while [ $# -gt 0 ]; do
  case "$1" in
    -c|-ilc|-lic|-icl|-lci|-cil|-cli)
      shift
      cmd=$1
      break
      ;;
    -i|-l|-il|-li)
      shift
      ;;
    *)
      break
      ;;
  esac
done
begin_tail=${cmd#*TIDEBREAK_PROBE_BEGIN_}
begin_tok=${begin_tail%%[!0-9a-f]*}
env_tail=${cmd#*TIDEBREAK_PROBE_ENV_}
env_tok=${env_tail%%[!0-9a-f]*}
end_tail=${cmd#*TIDEBREAK_PROBE_END_}
end_tok=${end_tail%%[!0-9a-f]*}
printf '%s\n' "TIDEBREAK_PROBE_BEGIN_${begin_tok}"
printf '%s\n' "claude"
printf '%s\n' "TIDEBREAK_PROBE_ENV_${env_tok}"
printf '\n%s\n' "TIDEBREAK_PROBE_END_${end_tok}"
"#,
        );
        let host = HostEnv {
            shell: shell.clone(),
            env: vec![
                ("SHELL".into(), shell.as_os_str().to_owned()),
                ("PATH".into(), empty_path.as_os_str().to_owned()),
                ("HOME".into(), home.as_os_str().to_owned()),
                ("TMPDIR".into(), tmp.as_os_str().to_owned()),
                ("LANG".into(), "C".into()),
                ("LC_ALL".into(), "C".into()),
            ],
            clear_env: true,
            data_dir: None,
        };
        match resolve_binary(&host, "claude").await {
            Err(ProbeError::RelativePath { name, path })
                if name == "claude" && path == "claude" => {}
            other => panic!(
                "expected RelativePath {{ name: \"claude\", path: \"claude\" }}, got {other:?}; {}",
                diagnose_probe_shim(&host).await
            ),
        }
    }

    #[tokio::test]
    async fn noisy_profile_still_resolves_inside_sentinels() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho 2.1.233\n");
        let shell = dir.path().join("shell");
        write_profile_shim(&shell, "");
        let host = HostEnv {
            shell,
            env: vec![("PATH".into(), dir.path().as_os_str().to_owned())],
            clear_env: true,
            data_dir: None,
        };
        let resolved = resolve_binary(&host, "claude").await.unwrap();
        assert_eq!(resolved, bin);
    }

    #[tokio::test]
    async fn interactive_profile_env_is_captured_and_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho 2.1.233\n");
        let host = HostEnv {
            shell: {
                let path = dir.path().join("interactive-shell");
                write_exec(
                    &path,
                    &format!(
                        r#"#!/bin/sh
echo 'welcome to my profile'
interactive=0
for arg in "$@"; do
  case "$arg" in -i*|-*i*) interactive=1 ;; esac
done
cmd=
while [ $# -gt 0 ]; do
  case "$1" in
    -c) shift; cmd=$1; break ;;
    -*c)
      rest=${{1#*c}}
      shift
      if [ -n "$rest" ]; then cmd=$rest; else cmd=$1; fi
      break
      ;;
    -*) shift ;;
    *) break ;;
  esac
done
if [ "$interactive" = 1 ]; then
  export PROFILE_ONLY_VAR=from-profile
fi
export TIDEBREAK_SECRET=nope
export PATH="{bin_dir}:$PATH"
eval "$cmd"
"#,
                        bin_dir = dir.path().display()
                    ),
                );
                path
            },
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
        };
        let capture = probe_shell(&host, "claude").await.unwrap();
        assert_eq!(capture.binary, bin);
        let profile = capture
            .env
            .iter()
            .find(|(key, _)| key == "PROFILE_ONLY_VAR")
            .map(|(_, value)| value.to_string_lossy().into_owned());
        assert_eq!(profile.as_deref(), Some("from-profile"));
        assert!(std::env::var_os("PROFILE_ONLY_VAR").is_none());
        let filtered = filter_child_env(capture.env);
        assert!(filtered
            .iter()
            .any(|(key, value)| { key == "PROFILE_ONLY_VAR" && value == "from-profile" }));
        assert!(filtered.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));

        let mut child = Command::new("/bin/sh");
        child
            .arg("-c")
            .arg("printf %s \"$PROFILE_ONLY_VAR\"")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .kill_on_drop(true);
        for (key, value) in &filtered {
            child.env(key, value);
        }
        let output = child.output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "from-profile");
    }

    #[tokio::test]
    async fn version_variants_are_first_nonempty_line() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho\necho '2.1.233 (Claude Code)'\n");
        let version = observe_version(&bin).await.unwrap();
        assert!(version.contains("2.1.233"));
    }
}

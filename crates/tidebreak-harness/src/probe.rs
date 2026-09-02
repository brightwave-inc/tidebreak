//! Host binary resolution, version detection, and environment capture.
//!
//! GUI processes on macOS do not inherit the user's shell PATH or profile
//! environment. Resolution therefore asks `$SHELL -ilc '…'` — login *and*
//! interactive, so zsh sources `.zshrc` where version managers and gateway
//! config commonly live — and accepts only an absolute, executable result.
//! The same probe captures the shell's resolved environment so children
//! run under that snapshot, not the GUI process env.
//!
//! A native Windows GUI process already receives the user's environment.
//! Windows therefore captures that process environment directly, merges test
//! or caller overrides case-insensitively, and resolves through `PATH` plus
//! `PATHEXT` without running a shell profile script.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tidebreak_managed_node::{managed_node_executable, managed_node_path_dir};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{is_absolute_executable, spawn_process_tree};

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_STDERR_BYTES: usize = 4_096;

/// Host environment used for discovery. Tests inject a shim shell and PATH.
#[derive(Debug, Clone)]
pub struct HostEnv {
    /// Login shell on Unix. Unused on Windows.
    pub shell: PathBuf,
    /// Extra environment for the probe child (typically a test PATH).
    pub env: Vec<(OsString, OsString)>,
    /// When set, replace the process environment entirely with `env`.
    pub clear_env: bool,
    /// App data directory. When set, probe prefers a pinned install under it.
    pub data_dir: Option<PathBuf>,
    /// Host-verified managed Node root. Pinned npm harnesses need its runtime
    /// directory first on `PATH`; their entrypoints resolve `node`/`node.exe`.
    pub managed_node_root: Option<PathBuf>,
    /// The managed install to drive per engine when it is not the pin: the
    /// exact version a reader on the `latest` update channel moved to. An
    /// engine with no entry resolves to its pin. Nothing here is discovery —
    /// the version must already be installed under `data_dir` with a marker
    /// that names it, or the engine probes as not found.
    pub harness_versions: Vec<(tidebreak_core::HarnessKind, String)>,
    /// Binaries the embedding environment provides and vouches for. A
    /// declared entry wins over both the pinned install and the login-shell
    /// PATH; the declared version is authoritative, so probe never runs
    /// `--version` against it.
    pub declared_binaries: Vec<(tidebreak_core::HarnessKind, DeclaredBinary)>,
    /// Complete child environment the embedding environment asserts. When
    /// set, probes never run the login shell: commands resolve against this
    /// environment's `PATH`, and environment captures return it verbatim.
    pub declared_env: Option<Vec<(OsString, OsString)>>,
}

impl HostEnv {
    /// The managed install version selected for `kind`, when it is not the
    /// pin.
    #[must_use]
    pub fn harness_version(&self, kind: tidebreak_core::HarnessKind) -> Option<&str> {
        self.harness_versions
            .iter()
            .find(|(candidate, _)| *candidate == kind)
            .map(|(_, version)| version.as_str())
    }
}

/// A harness binary the embedding environment installed itself.
///
/// The caller asserts provenance: the path must be absolute and executable,
/// and the version must be the exact version the environment installed.
/// Declaring a wrong version misinforms capability detection.
#[derive(Debug, Clone)]
pub struct DeclaredBinary {
    /// Absolute path to the engine entrypoint.
    pub path: PathBuf,
    /// Exact engine version at that path, as `--version` would print it.
    pub version: String,
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
        #[cfg(unix)]
        let shell = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        #[cfg(windows)]
        let shell = PathBuf::new();
        Self {
            shell,
            env: Vec::new(),
            clear_env: false,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
    }

    /// Declare a preinstalled engine binary and its exact version. Replaces
    /// any earlier declaration for the same kind.
    #[must_use]
    pub fn with_declared_binary(
        mut self,
        kind: tidebreak_core::HarnessKind,
        path: impl Into<PathBuf>,
        version: impl Into<String>,
    ) -> Self {
        self.declared_binaries
            .retain(|(existing, _)| *existing != kind);
        self.declared_binaries.push((
            kind,
            DeclaredBinary {
                path: path.into(),
                version: version.into(),
            },
        ));
        self
    }

    /// The declared binary for `kind`, when the embedding environment set one.
    #[must_use]
    pub fn declared(&self, kind: tidebreak_core::HarnessKind) -> Option<&DeclaredBinary> {
        self.declared_binaries
            .iter()
            .rev()
            .find(|(existing, _)| *existing == kind)
            .map(|(_, declared)| declared)
    }

    /// The declared version for `kind`, when one was declared.
    #[must_use]
    pub fn declared_version(&self, kind: tidebreak_core::HarnessKind) -> Option<&str> {
        self.declared(kind)
            .map(|declared| declared.version.as_str())
    }

    /// Assert the complete probe environment. Probes then skip the login
    /// shell entirely: resolution scans this environment's `PATH`, and the
    /// captured child environment is this one, verbatim.
    #[must_use]
    pub fn with_declared_env(mut self, env: Vec<(OsString, OsString)>) -> Self {
        self.declared_env = Some(env);
        self
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
    if let Some(declared) = kind_for_command(name).and_then(|kind| host.declared(kind)) {
        return declared_probe_capture(host, name, declared).await;
    }
    if let Some(data_dir) = &host.data_dir {
        if let Some(kind) = kind_for_command(name) {
            let binary = match host.harness_version(kind) {
                Some(version) => crate::managed_binary_version(data_dir, kind, version),
                None => crate::managed_binary(data_dir, kind),
            };
            if let Some(binary) = binary {
                let node = host
                    .managed_node_root
                    .as_deref()
                    .map(managed_node_executable);
                if !node.as_deref().is_some_and(crate::is_absolute_executable) {
                    return Err(ProbeError::NotFound(name.to_owned()));
                }
                return Ok(pinned_probe_capture(host, binary).await);
            }
            // Do not npm-install or fall through to PATH here. Listing
            // workspaces must not wait on a download. Create-session and
            // doctor refresh install the pin.
            return Err(ProbeError::NotFound(name.to_owned()));
        }
    }
    // A declared environment replaces the login shell: resolve against its
    // PATH directly, and return it as the capture.
    if let Some(env) = &host.declared_env {
        #[cfg(windows)]
        let resolved = resolve_windows_command(env, name);
        #[cfg(not(windows))]
        let resolved = resolve_unix_command(env, name);
        let binary = resolved.ok_or_else(|| ProbeError::NotFound(name.to_owned()))?;
        return Ok(ProbeCapture {
            binary,
            env: env.clone(),
            stderr: String::new(),
        });
    }
    #[cfg(windows)]
    {
        probe_windows_process_env(host, name)
    }
    #[cfg(not(windows))]
    {
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
}

#[cfg(windows)]
fn probe_windows_process_env(host: &HostEnv, name: &str) -> Result<ProbeCapture, ProbeError> {
    let env = windows_process_env(host);
    let path =
        resolve_windows_command(&env, name).ok_or_else(|| ProbeError::NotFound(name.to_owned()))?;
    Ok(ProbeCapture {
        binary: path,
        env,
        stderr: String::new(),
    })
}

#[cfg(windows)]
fn resolve_windows_command(env: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    let path = env_value(env, std::ffi::OsStr::new("PATH"))?;
    let extensions = env_value(env, std::ffi::OsStr::new("PATHEXT"))
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(|extension| {
                    if extension.starts_with('.') {
                        extension.to_owned()
                    } else {
                        format!(".{extension}")
                    }
                })
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| [".COM", ".EXE", ".BAT", ".CMD"].map(str::to_owned).to_vec());
    let explicit_extension = Path::new(name).extension().is_some();
    for dir in std::env::split_paths(path) {
        if !dir.is_absolute() {
            continue;
        }
        if explicit_extension {
            let candidate = dir.join(name);
            if is_absolute_executable(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if is_absolute_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
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

#[cfg(not(windows))]
fn resolve_unix_command(env: &[(OsString, OsString)], name: &str) -> Option<PathBuf> {
    let path = env_value(env, std::ffi::OsStr::new("PATH"))?;
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_absolute_executable(candidate))
}

/// Capture for a binary the embedding environment declared.
///
/// The path is validated, never resolved: a declaration bypasses both the
/// pinned install and the login-shell PATH. The child environment still comes
/// from the shell capture when the shell works, with the process environment
/// as the fallback, but the managed Node runtime is never prepended — the
/// embedding environment provides whatever interpreter its binary needs.
async fn declared_probe_capture(
    host: &HostEnv,
    name: &str,
    declared: &DeclaredBinary,
) -> Result<ProbeCapture, ProbeError> {
    if !declared.path.is_absolute() {
        return Err(ProbeError::RelativePath {
            name: name.to_owned(),
            path: declared.path.to_string_lossy().into_owned(),
        });
    }
    if !is_absolute_executable(&declared.path) {
        return Err(ProbeError::NotExecutable(declared.path.clone()));
    }
    Ok(match capture_shell_env(host).await {
        Ok(env) => ProbeCapture {
            binary: declared.path.clone(),
            env,
            stderr: String::new(),
        },
        Err(err) => ProbeCapture {
            binary: declared.path.clone(),
            env: process_env_without_tidebreak(),
            stderr: err.to_string(),
        },
    })
}

async fn pinned_probe_capture(host: &HostEnv, binary: PathBuf) -> ProbeCapture {
    match capture_shell_env(host).await {
        Ok(env) => ProbeCapture {
            binary,
            env: prepend_managed_node_path(env, host.managed_node_root.as_deref()),
            stderr: String::new(),
        },
        Err(err) => ProbeCapture {
            binary,
            env: prepend_managed_node_path(
                process_env_without_tidebreak(),
                host.managed_node_root.as_deref(),
            ),
            stderr: err.to_string(),
        },
    }
}

fn prepend_managed_node_path(
    mut env: Vec<(OsString, OsString)>,
    managed_node_root: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let Some(root) = managed_node_root else {
        return env;
    };
    let runtime_dir = managed_node_path_dir(root);
    let prior = env_value(&env, std::ffi::OsStr::new("PATH"))
        .cloned()
        .unwrap_or_default();
    env.retain(|(key, _)| !env_key_eq(key, std::ffi::OsStr::new("PATH"), true));
    let mut paths = vec![runtime_dir.clone()];
    paths.extend(std::env::split_paths(&prior));
    let path =
        std::env::join_paths(paths).unwrap_or_else(|_| runtime_dir.as_os_str().to_os_string());
    env.push((OsString::from("PATH"), path));
    env
}

fn process_env_without_tidebreak() -> Vec<(OsString, OsString)> {
    strip_tidebreak_env(std::env::vars_os())
}

/// Drop only the reserved `TIDEBREAK_` namespace and malformed names from a
/// captured snapshot. This keeps the stored probe environment complete —
/// auth-override detection reads credentials like `ANTHROPIC_API_KEY` out
/// of it — while [`filter_child_env`] separately narrows what a spawned
/// child may inherit.
fn strip_tidebreak_env<I, K, V>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let filtered = vars.into_iter().filter_map(|(key, value)| {
        let key = key.into();
        let name = key.to_string_lossy();
        if name.is_empty()
            || name.contains('=')
            || name.to_ascii_uppercase().starts_with("TIDEBREAK_")
        {
            return None;
        }
        Some((key, value.into()))
    });
    merge_environment(Vec::new(), filtered, cfg!(windows))
}

async fn capture_shell_env(host: &HostEnv) -> Result<Vec<(OsString, OsString)>, ProbeError> {
    if let Some(env) = &host.declared_env {
        return Ok(env.clone());
    }
    #[cfg(windows)]
    {
        Ok(windows_process_env(host))
    }
    #[cfg(not(windows))]
    {
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
}

#[cfg(windows)]
fn windows_process_env(host: &HostEnv) -> Vec<(OsString, OsString)> {
    let base = if host.clear_env {
        Vec::new()
    } else {
        std::env::vars_os().collect()
    };
    strip_tidebreak_env(merge_environment(base, host.env.iter().cloned(), true))
}

/// Environment variables Claude Code's auth-mode detection
/// (`crate::claude::observe_auth_override`) reads as the engine's own live
/// auth: a key, token, or endpoint override, or a Bedrock/Vertex switch.
///
/// Defined here, next to [`CHILD_ENV_ALLOWED_NAMES`], so detection and the
/// child filter cannot drift: whatever detection counts as working auth,
/// [`filter_engine_child_env`] hands to this engine's session child — and
/// only this engine's.
pub(crate) const CLAUDE_AUTH_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
];

/// Environment variables Codex's auth-mode detection
/// (`crate::codex::observe_auth_override`) reads as the engine's own live
/// auth, shared with the child filter for the same no-drift reason as
/// [`CLAUDE_AUTH_ENV_VARS`]. Codex's config-file admission surface names
/// its own environment keys inside `$CODEX_HOME/config.toml`; the filter
/// reads those through [`crate::codex::config_provider_env_keys`] rather
/// than this static list.
pub(crate) const CODEX_AUTH_ENV_VARS: &[&str] = &["OPENAI_API_KEY", "OPENAI_BASE_URL"];

/// Variables a spawned child may inherit from a captured shell snapshot,
/// matched on the ASCII-uppercased name.
///
/// This is an allowlist for the same reason the exec path in
/// `tidebreak-code-execution` builds on `env_clear()`: a coding-engine turn
/// can read its whole environment with one `env` call, so every ambient
/// credential the user's shell rc exports — `GITHUB_TOKEN`, an `AWS_*`
/// secret for unrelated tooling, a random rc export — is one prompt
/// injection away from exfiltration. A variable missing from this list
/// fails visibly in the child; a leaked secret fails silently, so anything
/// not demonstrably required stays out. Everything Tidebreak itself wires
/// into a child — settings `extra_env`, the session relay key, the browser
/// capability file — is applied after this filter (see
/// [`crate::browser_channel::apply_child_env_tokio`]) and never needs an
/// entry here.
///
/// The deliberate exception is the engine's own provider auth — but scoped
/// to the engine that authenticates with it, never this shared base list.
/// Session create (`refuse_signed_out_harness`) and the doctor
/// (`resolve_auth_mode`) read [`CLAUDE_AUTH_ENV_VARS`] and
/// [`CODEX_AUTH_ENV_VARS`] out of the unfiltered snapshot and treat a hit
/// as that engine's working auth mode: a signed-out engine with a shell-rc
/// `ANTHROPIC_API_KEY` is allowed to start a session on that basis, so the
/// session child must actually receive the variable or its first turn 401s
/// — the failure issue 2653 asked create to refuse. That per-engine
/// invariant lives in [`filter_engine_child_env`]: whatever detection
/// counts as working auth for engine X reaches engine X's session child,
/// and nothing else's — an Anthropic credential handed to a Codex, Grok,
/// or opencode child (or any probe/`gh` spawn under plain
/// [`filter_child_env`]) would be exactly the cross-context disclosure this
/// allowlist exists to stop.
const CHILD_ENV_ALLOWED_NAMES: &[&str] = &[
    // Command resolution: the probe prepends the managed Node runtime and
    // the login-shell PATH here, and every pinned engine entrypoint needs
    // its interpreter found through it.
    "PATH",
    // Engine state lives under the home directory: `~/.claude`, `~/.codex`,
    // and the `~/.config` fallbacks all resolve through HOME.
    "HOME",
    // POSIX session identity and terminal basics.
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "TMPDIR",
    // `tidebreak-server` spawns `gh` under this filter, and gh resolves a
    // relocated config directory through this path (not a secret).
    "GH_CONFIG_DIR",
    // Codex resolves its config directory through this path (not a secret).
    // Auth-mode detection reads the same `$CODEX_HOME/config.toml` for a
    // custom model provider, so the child must look where detection looked.
    "CODEX_HOME",
    // Outbound-trust configuration `tidebreak-supervised-agent` merges into
    // the session snapshot (`Trust::environment` via `HarnessEngine::launch`):
    // behind the sidecar's TLS-intercepting egress, an engine child without
    // these loses every HTTPS call. They carry CA-bundle paths, not secrets.
    "SSL_CERT_FILE",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "NODE_EXTRA_CA_CERTS",
    // Windows children cannot start, resolve commands, or write temp files
    // without these; on Unix they are simply absent.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "TEMP",
    "TMP",
];

/// Allowed name prefixes: locale configuration and the XDG base directories
/// engines use to find their config and cache trees.
const CHILD_ENV_ALLOWED_PREFIXES: &[&str] = &["LC_", "XDG_"];

fn child_env_allowed(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    CHILD_ENV_ALLOWED_NAMES.contains(&upper.as_str())
        || CHILD_ENV_ALLOWED_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// Whether `name` is an auth variable `kind`'s own detection reads as this
/// machine's working auth, mirrored exactly: opencode and Grok detection
/// reads no environment names ([`crate::auth_override_present`] grants them
/// the benefit of the doubt without looking), so their children receive no
/// auth variables at all.
///
/// The Bedrock/Vertex switches are pure flags whose supporting credentials
/// live under other names, so those pass only when the user set the switch:
/// with `CLAUDE_CODE_USE_BEDROCK` the engine's inference runs on the AWS
/// credential chain, so `AWS_*` is its configured auth; without the switch
/// (or for any other engine) the same variables are unrelated secrets and
/// stay out. Vertex likewise needs its project/region variables and the ADC
/// pointer (`GOOGLE_APPLICATION_CREDENTIALS`).
fn engine_auth_env_allowed(
    kind: tidebreak_core::HarnessKind,
    name: &str,
    bedrock: bool,
    vertex: bool,
) -> bool {
    let upper = name.to_ascii_uppercase();
    match kind {
        tidebreak_core::HarnessKind::ClaudeCode => {
            CLAUDE_AUTH_ENV_VARS.contains(&upper.as_str())
                || (bedrock && upper.starts_with("AWS_"))
                || (vertex
                    && (upper.starts_with("GOOGLE_")
                        || upper.starts_with("ANTHROPIC_VERTEX_")
                        || upper == "CLOUD_ML_REGION"))
        }
        tidebreak_core::HarnessKind::Codex => CODEX_AUTH_ENV_VARS.contains(&upper.as_str()),
        tidebreak_core::HarnessKind::Opencode
        | tidebreak_core::HarnessKind::Grok
        | tidebreak_core::HarnessKind::Internal => false,
    }
}

/// Narrow a captured shell snapshot to the environment a spawned child may
/// inherit: the [`CHILD_ENV_ALLOWED_NAMES`] allowlist, minus malformed
/// names. The reserved `TIDEBREAK_` namespace is excluded by construction —
/// nothing in the allowlist matches it.
///
/// No auth variable survives this filter. It is the right shape for probe
/// spawns (`--version`, model listings, auth-status checks) and non-engine
/// children (`gh`, the gateway usage CLI); an engine *session* child goes
/// through [`filter_engine_child_env`] instead, which adds that one
/// engine's own auth signals.
#[must_use]
pub fn filter_child_env<I, K, V>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let filtered = vars.into_iter().filter_map(|(key, value)| {
        let key = key.into();
        let name = key.to_string_lossy();
        if name.is_empty() || name.contains('=') || !child_env_allowed(&name) {
            return None;
        }
        Some((key, value.into()))
    });
    merge_environment(Vec::new(), filtered, cfg!(windows))
}

/// [`filter_child_env`] plus the auth variables `kind`'s own auth-mode
/// detection reads, for spawning that engine's session child.
///
/// This is where the create/doctor consistency invariant lives: a session
/// that `refuse_signed_out_harness` admitted because detection saw
/// engine-auth variables in the snapshot must hand those same variables to
/// the engine child, or its first turn 401s (issue 2653). The pass-through
/// is per engine — [`engine_auth_env_allowed`] — so one engine's credential
/// is never disclosed to another engine's prompt-injectable child.
///
/// Codex has a second admission surface with the same obligation: detection
/// admits on a `$CODEX_HOME/config.toml` that declares a model provider,
/// and such a provider reads its credential from whatever environment key
/// the config names (`env_key`, `env_http_headers`). Those config-named
/// variables ([`crate::codex::config_provider_env_keys`], resolved from
/// this same snapshot) are forwarded to a Codex child only — bounded to
/// exactly the names the config declares.
#[must_use]
pub fn filter_engine_child_env<I, K, V>(
    kind: tidebreak_core::HarnessKind,
    vars: I,
) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let vars: Vec<(OsString, OsString)> = vars
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    // The same non-empty rule detection's `env_sets_any` applies to the
    // auth variables themselves.
    let bedrock = kind == tidebreak_core::HarnessKind::ClaudeCode
        && env_sets_any(&vars, &["CLAUDE_CODE_USE_BEDROCK"]);
    let vertex = kind == tidebreak_core::HarnessKind::ClaudeCode
        && env_sets_any(&vars, &["CLAUDE_CODE_USE_VERTEX"]);
    let config_auth_keys = if kind == tidebreak_core::HarnessKind::Codex {
        crate::codex::config_provider_env_keys(&vars)
    } else {
        Vec::new()
    };
    let filtered = vars.into_iter().filter(|(key, _)| {
        let name = key.to_string_lossy();
        !name.is_empty()
            && !name.contains('=')
            && (child_env_allowed(&name)
                || engine_auth_env_allowed(kind, &name, bedrock, vertex)
                || config_auth_keys
                    .iter()
                    .any(|named| env_key_eq(std::ffi::OsStr::new(named), key, cfg!(windows))))
    });
    merge_environment(Vec::new(), filtered, cfg!(windows))
}

fn merge_environment<I>(
    mut base: Vec<(OsString, OsString)>,
    overrides: I,
    case_insensitive: bool,
) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    for (key, value) in overrides {
        if let Some(index) = base
            .iter()
            .position(|(existing, _)| env_key_eq(existing, &key, case_insensitive))
        {
            base.remove(index);
        }
        base.push((key, value));
    }
    base
}

fn env_key_eq(left: &std::ffi::OsStr, right: &std::ffi::OsStr, case_insensitive: bool) -> bool {
    if case_insensitive {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

pub(crate) fn env_value<'a>(
    env: &'a [(OsString, OsString)],
    key: &std::ffi::OsStr,
) -> Option<&'a OsString> {
    env.iter()
        .rev()
        .find(|(candidate, _)| env_key_eq(candidate, key, cfg!(windows)))
        .map(|(_, value)| value)
}

/// Whether the captured environment assigns any of `names` a non-empty value.
pub(crate) fn env_sets_any(env: &[(OsString, OsString)], names: &[&str]) -> bool {
    names.iter().any(|name| {
        env_value(env, std::ffi::OsStr::new(name)).is_some_and(|value| !value.is_empty())
    })
}

/// The first `major.minor` a version line states. Scans whitespace tokens for
/// one whose leading `v`-stripped form starts `<digits>.<digits>`, so banner
/// words and build hashes around the number do not matter.
pub(crate) fn version_minor_line(version: Option<&str>) -> Option<(u64, u64)> {
    version
        .into_iter()
        .flat_map(str::split_whitespace)
        .find_map(|candidate| {
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            let mut parts = candidate.split('.');
            let major = parts.next()?.parse::<u64>().ok()?;
            let minor = parts.next()?.parse::<u64>().ok()?;
            Some((major, minor))
        })
}

/// The first `major.minor.patch` a version line states.
pub(crate) fn version_patch_line(version: Option<&str>) -> Option<(u64, u64, u64)> {
    version
        .into_iter()
        .flat_map(str::split_whitespace)
        .find_map(|candidate| {
            let candidate = candidate.strip_prefix('v').unwrap_or(candidate);
            let mut parts = candidate.split('.');
            let major = parts.next()?.parse::<u64>().ok()?;
            let minor = parts.next()?.parse::<u64>().ok()?;
            let patch = parts
                .next()?
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u64>()
                .ok()?;
            Some((major, minor, patch))
        })
}

/// Whether a probed version states a `major.minor` other than the pinned
/// line. A missing or unparseable version is not off-line: with nothing to
/// compare, an adapter keeps the pinned capture's flags rather than degrading
/// the product it ships today.
pub(crate) fn off_pinned_line(version: Option<&str>, pinned: (u64, u64)) -> bool {
    version_minor_line(version).is_some_and(|line| line != pinned)
}

fn apply_captured_env(command: &mut Command, env: &[(std::ffi::OsString, std::ffi::OsString)]) {
    command.env_clear();
    for (key, value) in filter_child_env(env.iter().cloned()) {
        command.env(key, value);
    }
}

/// Run `<binary> --version` under the captured environment and return the
/// first line, trimmed.
pub async fn observe_version(
    binary: &Path,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<String, ProbeError> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_captured_env(&mut command, env);
    let child =
        spawn_process_tree(&mut command).map_err(|err| ProbeError::Shell(err.to_string()))?;
    let output = timeout(PROBE_TIMEOUT, child.wait_with_output())
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
    /// Effort levels this row accepts, ascending. Empty means the engine
    /// takes no effort control for it, and the picker hides the choice.
    ///
    /// Per model, not per engine: Codex advertises a different ladder for a
    /// gateway row than for its own, and only some rows reach `ultra`.
    pub reasoning_efforts: Vec<tidebreak_core::ReasoningEffort>,
    /// Whether this row can serve the engine's fast mode.
    ///
    /// Per model, not per engine, for the same reason the effort ladder is:
    /// Anthropic serves fast mode on part of the Opus line only, and Codex
    /// advertises the tier per row. `false` hides the toggle rather than
    /// letting a user arm a premium that the selected model would ignore.
    pub fast_mode: bool,
}

/// Stamp one engine-wide effort ladder over every listed row.
///
/// For an engine whose `--effort` flag takes the same levels whatever model is
/// selected. Codex is the exception and states a ladder per row instead.
#[must_use]
pub fn with_reasoning_efforts(
    mut models: Vec<ListedHarnessModel>,
    levels: &[tidebreak_core::ReasoningEffort],
) -> Vec<ListedHarnessModel> {
    for model in &mut models {
        model.reasoning_efforts = levels.to_vec();
    }
    models
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
        .stderr(Stdio::piped());
    apply_captured_env(&mut command, env);
    let Ok(child) = spawn_process_tree(&mut command) else {
        return Vec::new();
    };
    let Ok(Ok(output)) = timeout(PROBE_TIMEOUT, child.wait_with_output()).await else {
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
        if !KEYS.iter().any(|key| name.eq_ignore_ascii_case(key)) {
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
            if let Some(id) = rest.split_whitespace().next().and_then(normalize_model_id) {
                push_listed(&mut models, id, true);
            }
            continue;
        }
        let default = lower.contains("(current)")
            || lower.contains("(default)")
            || trimmed.contains('*')
            || trimmed.contains('✓');
        let token = trimmed.trim_start_matches(['-', '*', '•', '✓', '►']).trim();
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
        // A line of `<engine> models` output says nothing about effort. An
        // adapter with a ladder fills this in over the parsed rows.
        reasoning_efforts: Vec::new(),
        // Nor does it say anything about fast mode. Off is the safe read:
        // arming a premium the row cannot serve would spend nothing extra but
        // would tell the user something untrue about the turn.
        fast_mode: false,
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
        .stderr(Stdio::piped());
    if host.clear_env {
        command.env_clear();
    }
    for (key, value) in &host.env {
        command.env(key, value);
    }
    let child =
        spawn_process_tree(&mut command).map_err(|err| ProbeError::Shell(err.to_string()))?;
    let output = timeout(PROBE_TIMEOUT, child.wait_with_output())
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

#[cfg(all(test, unix))]
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
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
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
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
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
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
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
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
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
        // The snapshot keeps profile-sourced variables for detection, but a
        // child inherits only the allowlist: an arbitrary profile export
        // stays out while the profile-built PATH survives.
        assert!(filtered.iter().all(|(key, _)| key != "PROFILE_ONLY_VAR"));
        assert!(filtered.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));
        let path = filtered
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .expect("profile-built PATH reaches the child");
        assert!(std::env::split_paths(&path).any(|entry| entry == dir.path()));

        let mut child = Command::new("/bin/sh");
        child
            .arg("-c")
            .arg("printf %s \"${PROFILE_ONLY_VAR-unset}\"")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear()
            .kill_on_drop(true);
        for (key, value) in &filtered {
            child.env(key, value);
        }
        let output = child.output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset");
    }

    #[tokio::test]
    async fn version_variants_are_first_nonempty_line() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        write_exec(&bin, "#!/bin/sh\necho\necho '2.1.233 (Claude Code)'\n");
        let version = observe_version(&bin, &[]).await.unwrap();
        assert!(version.contains("2.1.233"));
    }

    #[tokio::test]
    async fn pin_probe_falls_back_to_process_env_when_the_shell_fails() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("claude");
        write_exec(&binary, "#!/bin/sh\necho 2.1.234\n");
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        };
        let capture = pinned_probe_capture(&host, binary.clone()).await;
        assert_eq!(capture.binary, binary);
        assert!(!capture.stderr.is_empty());
        assert!(capture
            .env
            .iter()
            .any(|(key, _)| key == "PATH" || key == "HOME"));
        assert!(capture.env.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));
    }

    #[tokio::test]
    async fn pinned_entrypoint_runs_with_only_managed_node_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let node_root = dir.path().join("verified-node");
        let node_bin = node_root.join("bin");
        std::fs::create_dir_all(&node_bin).unwrap();
        write_exec(
            &node_bin.join("node"),
            "#!/bin/sh\nprintf '%s\\n' managed-node-version\n",
        );
        let binary = dir.path().join("claude");
        write_exec(&binary, "#!/usr/bin/env node\n");
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: Some(node_root),
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        };

        let capture = pinned_probe_capture(&host, binary.clone()).await;
        let path_values = capture
            .env
            .iter()
            .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        assert_eq!(path_values.len(), 1);
        assert_eq!(
            std::env::split_paths(path_values[0]).next().as_deref(),
            Some(node_bin.as_path())
        );
        assert_eq!(
            observe_version(&binary, &capture.env).await.unwrap(),
            "managed-node-version"
        );
    }

    #[tokio::test]
    async fn declared_binary_resolves_without_a_login_shell() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("engine-claude");
        write_exec(&binary, "#!/bin/sh\nexit 1\n");
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
        .with_declared_binary(tidebreak_core::HarnessKind::ClaudeCode, &binary, "9.9.9");
        let capture = probe_shell(&host, "claude").await.unwrap();
        assert_eq!(capture.binary, binary);
        assert!(!capture.stderr.is_empty());
        assert!(capture
            .env
            .iter()
            .any(|(key, _)| key == "PATH" || key == "HOME"));
        assert!(capture.env.iter().all(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("TIDEBREAK_")
        }));
    }

    #[tokio::test]
    async fn declared_relative_path_is_rejected() {
        let host = HostEnv::from_process().with_declared_binary(
            tidebreak_core::HarnessKind::ClaudeCode,
            "relative/claude",
            "9.9.9",
        );
        match probe_shell(&host, "claude").await {
            Err(ProbeError::RelativePath { name, path })
                if name == "claude" && path == "relative/claude" => {}
            other => panic!("expected RelativePath, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn declared_non_executable_path_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("claude");
        std::fs::write(&binary, "not executable").unwrap();
        let host = HostEnv::from_process().with_declared_binary(
            tidebreak_core::HarnessKind::ClaudeCode,
            &binary,
            "9.9.9",
        );
        match probe_shell(&host, "claude").await {
            Err(ProbeError::NotExecutable(path)) if path == binary => {}
            other => panic!("expected NotExecutable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn declared_binary_wins_over_the_pinned_install() {
        let data_dir = tempfile::tempdir().unwrap();
        let pin = crate::pin_for(tidebreak_core::HarnessKind::ClaudeCode).unwrap();
        let install_dir = crate::pin::install_dir(data_dir.path(), pin);
        let pinned = install_dir.join("node_modules/.bin/claude");
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        write_exec(&pinned, "#!/usr/bin/env node\n");
        std::fs::write(
            install_dir.join("installed.json"),
            serde_json::json!({"package": pin.package, "version": pin.version}).to_string(),
        )
        .unwrap();
        let declared = data_dir.path().join("declared-claude");
        write_exec(&declared, "#!/bin/sh\nexit 1\n");
        // No managed Node root: the pinned path would refuse, the declared
        // binary must not need one.
        let host = HostEnv {
            shell: data_dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: Some(data_dir.path().to_path_buf()),
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
        .with_declared_binary(tidebreak_core::HarnessKind::ClaudeCode, &declared, "9.9.9");
        let capture = probe_shell(&host, "claude").await.unwrap();
        assert_eq!(capture.binary, declared);
    }

    #[test]
    fn version_lines_parse_from_banner_tokens() {
        assert_eq!(
            version_minor_line(Some("2.1.233 (Claude Code)")),
            Some((2, 1))
        );
        assert_eq!(
            version_minor_line(Some("grok 1.0.4 (d846eb93d94d) [stable]")),
            Some((1, 0))
        );
        assert_eq!(version_minor_line(Some("v1.18.18")), Some((1, 18)));
        assert_eq!(version_minor_line(Some("v1.18")), Some((1, 18)));
        assert_eq!(
            version_patch_line(Some("grok 1.0.5 (5115b46bc909) [stable]")),
            Some((1, 0, 5))
        );
        assert_eq!(version_patch_line(Some("v1.18")), None);
        assert_eq!(version_minor_line(Some("development build")), None);
        assert_eq!(version_minor_line(None), None);
        assert!(off_pinned_line(Some("3.0.1 (Claude Code)"), (2, 1)));
        assert!(!off_pinned_line(Some("2.1.300 (Claude Code)"), (2, 1)));
        // Nothing to compare: keep the pinned capture's flags.
        assert!(!off_pinned_line(None, (2, 1)));
        assert!(!off_pinned_line(Some("development build"), (2, 1)));
    }

    #[tokio::test]
    async fn declared_env_resolves_on_its_own_path_without_a_login_shell() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("claude");
        write_exec(&binary, "#!/bin/sh\nexit 1\n");
        let declared_env = vec![
            (
                OsString::from("PATH"),
                dir.path().as_os_str().to_os_string(),
            ),
            (OsString::from("SANDBOX_MARKER"), OsString::from("1")),
        ];
        // A shell that cannot run: only the declared environment can
        // produce this capture.
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
        .with_declared_env(declared_env.clone());
        let capture = probe_shell(&host, "claude").await.unwrap();
        assert_eq!(capture.binary, binary);
        assert_eq!(capture.env, declared_env);
        assert!(matches!(
            probe_shell(&host, "codex").await,
            Err(ProbeError::NotFound(name)) if name == "codex"
        ));
    }

    #[tokio::test]
    async fn declared_binary_captures_the_declared_env_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("declared-grok");
        write_exec(&binary, "#!/bin/sh\nexit 1\n");
        let declared_env = vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
        let host = HostEnv {
            shell: dir.path().join("missing-shell"),
            env: Vec::new(),
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        }
        .with_declared_binary(tidebreak_core::HarnessKind::Grok, &binary, "1.0.4")
        .with_declared_env(declared_env.clone());
        let capture = probe_shell(&host, "grok").await.unwrap();
        assert_eq!(capture.binary, binary);
        assert_eq!(capture.env, declared_env);
        assert!(capture.stderr.is_empty(), "no shell ran, so no shell error");
    }

    #[tokio::test]
    async fn pinned_harness_is_not_found_without_a_verified_node_root() {
        let data_dir = tempfile::tempdir().unwrap();
        let pin = crate::pin_for(tidebreak_core::HarnessKind::ClaudeCode).unwrap();
        let install_dir = crate::pin::install_dir(data_dir.path(), pin);
        let binary = install_dir.join("node_modules/.bin/claude");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        write_exec(&binary, "#!/usr/bin/env node\n");
        std::fs::write(
            install_dir.join("installed.json"),
            serde_json::json!({"package": pin.package, "version": pin.version}).to_string(),
        )
        .unwrap();
        let host = HostEnv {
            shell: PathBuf::from("/bin/sh"),
            env: Vec::new(),
            clear_env: true,
            data_dir: Some(data_dir.path().to_path_buf()),
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        };

        assert!(matches!(
            probe_shell(&host, "claude").await,
            Err(ProbeError::NotFound(name)) if name == "claude"
        ));
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;

    #[test]
    fn case_insensitive_environment_merge_keeps_the_last_spelling_and_value() {
        let merged = merge_environment(
            vec![
                (OsString::from("Path"), OsString::from("old")),
                (OsString::from("KEEP"), OsString::from("yes")),
            ],
            [(OsString::from("PATH"), OsString::from("managed"))],
            true,
        );
        assert_eq!(
            merged,
            vec![
                (OsString::from("KEEP"), OsString::from("yes")),
                (OsString::from("PATH"), OsString::from("managed")),
            ]
        );
    }

    #[test]
    fn filter_rejects_case_varied_tidebreak_and_invalid_environment_keys() {
        let filtered = filter_child_env([
            ("tidebreak_secret", "nope"),
            ("HOME", "/home/probe"),
            ("BAD=KEY", "nope"),
        ]);
        assert_eq!(
            filtered,
            [(OsString::from("HOME"), OsString::from("/home/probe"))]
        );
    }

    #[test]
    fn child_env_is_an_allowlist_that_drops_planted_secrets() {
        // A shell rc that exports ambient credentials must not hand them to
        // a probe or non-engine child, while the session basics still
        // arrive. Engine-auth variables pass only through the per-engine
        // filter below, never this base one.
        let filtered = filter_child_env([
            ("PATH", "/usr/bin"),
            ("HOME", "/home/probe"),
            ("LC_ALL", "en_US.UTF-8"),
            ("XDG_CONFIG_HOME", "/home/probe/.config"),
            ("SSL_CERT_FILE", "/run/trust/bundle.pem"),
            ("NODE_EXTRA_CA_CERTS", "/run/trust/sidecar-ca.pem"),
            ("AWS_SECRET_ACCESS_KEY", "planted"),
            ("GITHUB_TOKEN", "planted"),
            ("ANTHROPIC_API_KEY", "planted"),
            ("OPENAI_API_KEY", "planted"),
        ]);
        for name in [
            "PATH",
            "HOME",
            "LC_ALL",
            "XDG_CONFIG_HOME",
            // The supervised runtime merges CA trust into the snapshot; a
            // child that loses it cannot make outbound HTTPS calls.
            "SSL_CERT_FILE",
            "NODE_EXTRA_CA_CERTS",
        ] {
            assert!(
                filtered.iter().any(|(key, _)| key == name),
                "{name} must survive the child filter"
            );
        }
        for name in [
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
        ] {
            assert!(
                filtered.iter().all(|(key, _)| key != name),
                "{name} must not reach a non-engine child environment"
            );
        }
    }

    #[test]
    fn engine_children_receive_only_their_own_auth_signals() {
        // Auth-mode detection reads each engine's variables as its live auth
        // (issue 2653): a session admitted on their evidence must hand them
        // to that engine's child, or its first turn 401s — and to no other
        // engine's child, or one provider's credential is disclosed to an
        // unrelated prompt-injectable process.
        use tidebreak_core::HarnessKind;
        let snapshot = [
            ("HOME", "/home/probe"),
            ("SSL_CERT_FILE", "/run/trust/bundle.pem"),
            ("ANTHROPIC_API_KEY", "sk-ant"),
            ("ANTHROPIC_BASE_URL", "https://gateway.example"),
            ("OPENAI_API_KEY", "sk-oai"),
            ("OPENAI_BASE_URL", "https://oai-gateway.example"),
            ("GITHUB_TOKEN", "planted"),
        ];
        let claude = filter_engine_child_env(HarnessKind::ClaudeCode, snapshot);
        for name in ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "SSL_CERT_FILE"] {
            assert!(
                claude.iter().any(|(key, _)| key == name),
                "{name} must survive for a Claude Code launch"
            );
        }
        for name in ["OPENAI_API_KEY", "OPENAI_BASE_URL", "GITHUB_TOKEN"] {
            assert!(
                claude.iter().all(|(key, _)| key != name),
                "{name} must not reach a Claude Code child"
            );
        }
        let codex = filter_engine_child_env(HarnessKind::Codex, snapshot);
        for name in ["OPENAI_API_KEY", "OPENAI_BASE_URL", "SSL_CERT_FILE"] {
            assert!(
                codex.iter().any(|(key, _)| key == name),
                "{name} must survive for a Codex launch"
            );
        }
        for name in ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "GITHUB_TOKEN"] {
            assert!(
                codex.iter().all(|(key, _)| key != name),
                "{name} must not reach a Codex child"
            );
        }
        // Detection reads no environment names for these engines, so their
        // children receive no auth variables at all.
        for kind in [HarnessKind::Opencode, HarnessKind::Grok] {
            let filtered = filter_engine_child_env(kind, snapshot);
            for name in [
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_BASE_URL",
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
                "GITHUB_TOKEN",
            ] {
                assert!(
                    filtered.iter().all(|(key, _)| key != name),
                    "{name} must not reach a {kind:?} child"
                );
            }
            assert!(
                filtered.iter().any(|(key, _)| key == "HOME"),
                "session basics still reach a {kind:?} child"
            );
        }
    }

    #[test]
    fn provider_mode_flags_gate_their_supporting_credentials() {
        use tidebreak_core::HarnessKind;
        // Without the Bedrock switch, AWS credentials are unrelated secrets
        // even for the engine that could use them.
        let unrelated = filter_engine_child_env(
            HarnessKind::ClaudeCode,
            [("AWS_SECRET_ACCESS_KEY", "planted")],
        );
        assert!(unrelated
            .iter()
            .all(|(key, _)| key != "AWS_SECRET_ACCESS_KEY"));

        // With the switch set, the engine's inference runs on the AWS
        // credential chain: detection reads the flag as working auth, so the
        // chain the flag needs must reach the child with it.
        let bedrock_snapshot = [
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
            ("AWS_REGION", "us-east-1"),
            ("GITHUB_TOKEN", "planted"),
        ];
        let bedrock = filter_engine_child_env(HarnessKind::ClaudeCode, bedrock_snapshot);
        for name in [
            "CLAUDE_CODE_USE_BEDROCK",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_REGION",
        ] {
            assert!(
                bedrock.iter().any(|(key, _)| key == name),
                "{name} must survive the child filter in Bedrock mode"
            );
        }
        assert!(
            bedrock.iter().all(|(key, _)| key != "GITHUB_TOKEN"),
            "a mode switch must not widen the filter beyond its own provider"
        );
        // The switch is Claude Code's auth mode: the same snapshot hands no
        // AWS credential to another engine's child.
        let codex = filter_engine_child_env(HarnessKind::Codex, bedrock_snapshot);
        for name in ["CLAUDE_CODE_USE_BEDROCK", "AWS_SECRET_ACCESS_KEY"] {
            assert!(
                codex.iter().all(|(key, _)| key != name),
                "{name} must not reach a Codex child"
            );
        }

        let vertex = filter_engine_child_env(
            HarnessKind::ClaudeCode,
            [
                ("CLAUDE_CODE_USE_VERTEX", "1"),
                ("GOOGLE_APPLICATION_CREDENTIALS", "/home/probe/adc.json"),
                ("CLOUD_ML_REGION", "us-east5"),
                ("ANTHROPIC_VERTEX_PROJECT_ID", "probe-project"),
            ],
        );
        for name in [
            "CLAUDE_CODE_USE_VERTEX",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "CLOUD_ML_REGION",
            "ANTHROPIC_VERTEX_PROJECT_ID",
        ] {
            assert!(
                vertex.iter().any(|(key, _)| key == name),
                "{name} must survive the child filter in Vertex mode"
            );
        }
        // An empty switch is not set — the same non-empty rule detection's
        // `env_sets_any` applies.
        let unset = filter_engine_child_env(
            HarnessKind::ClaudeCode,
            [
                ("CLAUDE_CODE_USE_BEDROCK", ""),
                ("AWS_SECRET_ACCESS_KEY", "planted"),
            ],
        );
        assert!(unset.iter().all(|(key, _)| key != "AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn codex_config_declared_provider_keys_reach_only_codex_children() {
        use tidebreak_core::HarnessKind;
        // A gateway-managed machine authenticates Codex through a
        // config-declared provider whose credential lives under whatever
        // environment key the config names. Detection admits the session on
        // that declaration, so the child must receive exactly the named
        // variables (issue 2653) — and no other engine's child may.
        let codex_home = tempfile::tempdir().unwrap();
        std::fs::write(
            codex_home.path().join("config.toml"),
            r#"model_provider = "gateway"

[model_providers.gateway]
name = "Gateway"
base_url = "https://gateway.example/v1"
env_key = "GATEWAY_TEST_KEY"
env_http_headers = { "X-Gateway-Auth" = "GATEWAY_TEST_HEADER" }
"#,
        )
        .unwrap();
        let os = OsString::from;
        let snapshot = vec![
            (os("CODEX_HOME"), codex_home.path().as_os_str().to_owned()),
            (os("GATEWAY_TEST_KEY"), os("gw-secret")),
            (os("GATEWAY_TEST_HEADER"), os("gw-header-secret")),
            (os("GITHUB_TOKEN"), os("planted")),
        ];
        let codex = filter_engine_child_env(HarnessKind::Codex, snapshot.clone());
        for name in ["CODEX_HOME", "GATEWAY_TEST_KEY", "GATEWAY_TEST_HEADER"] {
            assert!(
                codex.iter().any(|(key, _)| key == name),
                "{name} must survive for a Codex launch its config names"
            );
        }
        assert!(
            codex.iter().all(|(key, _)| key != "GITHUB_TOKEN"),
            "a config-named key must not widen the filter beyond the config"
        );
        let claude = filter_engine_child_env(HarnessKind::ClaudeCode, snapshot);
        for name in ["GATEWAY_TEST_KEY", "GATEWAY_TEST_HEADER"] {
            assert!(
                claude.iter().all(|(key, _)| key != name),
                "{name} must not reach a Claude Code child"
            );
        }
    }

    #[test]
    fn a_declared_provider_without_env_keys_widens_nothing() {
        // A provider that names no environment key (OAuth, an auth helper,
        // an unauthenticated local endpoint) authenticates through files
        // under `$CODEX_HOME`; its declaration must not let ambient
        // variables through.
        let codex_home = tempfile::tempdir().unwrap();
        std::fs::write(
            codex_home.path().join("config.toml"),
            "[model_providers.local]\nname = \"Local\"\nbase_url = \"http://127.0.0.1:8080/v1\"\n",
        )
        .unwrap();
        let os = OsString::from;
        let filtered = filter_engine_child_env(
            tidebreak_core::HarnessKind::Codex,
            vec![
                (os("CODEX_HOME"), codex_home.path().as_os_str().to_owned()),
                (os("AMBIENT_SECRET"), os("planted")),
            ],
        );
        assert!(filtered.iter().all(|(key, _)| key != "AMBIENT_SECRET"));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[tokio::test]
    async fn resolves_and_executes_cmd_shims_from_case_varied_path_and_pathext() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        std::fs::write(&shim, "@echo 2.1.234\r\n").unwrap();
        let host = HostEnv {
            shell: PathBuf::new(),
            env: vec![
                (OsString::from("Path"), dir.path().as_os_str().to_owned()),
                (OsString::from("pathext"), OsString::from(".CMD;.EXE")),
            ],
            clear_env: true,
            data_dir: None,
            managed_node_root: None,
            harness_versions: Vec::new(),
            declared_binaries: Vec::new(),
            declared_env: None,
        };

        let capture = probe_shell(&host, "claude").await.unwrap();
        assert!(
            capture
                .binary
                .to_string_lossy()
                .eq_ignore_ascii_case(&shim.to_string_lossy()),
            "expected {shim:?}, got {:?}",
            capture.binary
        );
        assert_eq!(
            observe_version(&capture.binary, &capture.env)
                .await
                .unwrap(),
            "2.1.234"
        );
        assert_eq!(
            capture
                .env
                .iter()
                .filter(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("PATH"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn from_process_captures_windows_environment_without_a_shell() {
        let host = HostEnv::from_process();
        assert!(host.shell.as_os_str().is_empty());
        let captured = capture_shell_env(&host).await.unwrap();
        assert!(env_value(&captured, std::ffi::OsStr::new("PATH")).is_some());
    }
}

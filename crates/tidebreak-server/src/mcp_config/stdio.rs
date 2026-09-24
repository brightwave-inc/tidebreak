//! Host PATH resolution for user-configured stdio MCP commands.
//!
//! A GUI process on macOS inherits launchd's minimal PATH, so a bare name
//! like `npx` is not found unless we extend the process PATH with the
//! login-shell PATH the harness probe already captures. Resolution never
//! invokes a shell to run the server: it scans directories and spawns the
//! absolute path with `Command::new`.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tidebreak_core::{AgentError, Result};
use tidebreak_harness::{
    capture_login_env, env_value, is_absolute_executable, resolve_command_on_path, HostEnv,
};
use tokio::sync::OnceCell;

/// Directories named in a "command not found" diagnostic. Enough to show
/// Homebrew / nvm / volta roots without dumping an unbounded PATH.
const MAX_SEARCHED_DIRS: usize = 8;

/// The environment names every user-configured stdio child gets by default:
/// the desktop process's HOME, and the host search PATH its command was
/// resolved on. A name the definition declares itself, in `env` or
/// `env_from`, never gets the default; see [`defaulted_names`].
pub const FORWARDED_BY_DEFAULT: [&str; 2] = ["HOME", "PATH"];

/// How a spawn failure starts when a bare command would run a program the
/// desktop's native dialog did not approve. Settings shows the sentence as it
/// is, and the supervisor stops retrying until a settings change or a manual
/// reconnect.
pub(super) const NEEDS_APPROVAL: &str = "Needs approval:";

/// Whether the desktop's native dialog guards this server's local commands
/// (decision 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandApproval {
    /// No native dialog guards local commands here, as in the CLI, on a
    /// self-hosted server, or on a machine a desktop window attaches to. A
    /// bare name runs the program it resolves to at each spawn, unless its
    /// definition records an approved program.
    Optional,
    /// The desktop app's native dialog guards local commands. A bare name runs
    /// only the program the dialog approved.
    Required,
}

static PATH_OVERRIDE: Mutex<Option<OsString>> = Mutex::new(None);
static LOGIN_PATH: OnceCell<Option<OsString>> = OnceCell::const_new();

/// Serializes the tests that replace the host search PATH, which is process
/// state every stdio test reads.
#[cfg(test)]
static HOST_PATH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Test seam: replace the host search PATH (process + login-shell merge)
/// while the guard lives, and restore the process PATH merge when it drops.
#[cfg(test)]
pub(super) struct HostPathGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl HostPathGuard {
    pub(super) async fn set(path: Option<OsString>) -> Self {
        let lock = HOST_PATH_TEST_LOCK.lock().await;
        *PATH_OVERRIDE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
        Self { _lock: lock }
    }
}

#[cfg(test)]
impl Drop for HostPathGuard {
    fn drop(&mut self) {
        *PATH_OVERRIDE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Why a user-typed stdio command could not be turned into an executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StdioResolveError {
    Relative { command: String },
    NotFound { command: String, searched: String },
    NotExecutable { path: PathBuf },
    PermissionDenied { path: PathBuf },
}

impl StdioResolveError {
    pub(super) fn diagnostic(&self) -> String {
        match self {
            Self::Relative { command } => format!(
                "Relative executable path {command:?} is not allowed. Use an absolute path \
                 or a command name with no path separators."
            ),
            Self::NotFound { command, searched } => {
                format!(
                    "Command not found: {command:?} is not on the host PATH. Searched: {searched}."
                )
            }
            Self::NotExecutable { path } => format!(
                "Not executable: {} exists but is not executable by this user.",
                path.display()
            ),
            Self::PermissionDenied { path } => {
                format!("Permission denied: cannot execute {}.", path.display())
            }
        }
    }
}

/// Absolute path to show after a successful stdio verify, when the
/// definition still stores the user-typed command.
pub(super) async fn resolved_display(
    definition: &super::types::McpServerDefinition,
) -> Option<String> {
    if definition.launch.is_some() {
        return None;
    }
    let command = definition.command.as_deref()?;
    resolve_stdio_command(command)
        .await
        .ok()
        .map(|path| path.display().to_string())
}

/// Resolve a user-configured stdio `command` at verify and launch time.
///
/// The definition keeps what the user typed. Absolute paths are used as-is.
/// A bare name is found on the process PATH extended with the login-shell
/// PATH (and `PATHEXT` on Windows). A relative path with separators is
/// refused. Plugin `./` commands do not go through this function.
pub(super) async fn resolve_stdio_command(command: &str) -> Result<PathBuf> {
    resolve_stdio_executable(command)
        .await
        .map_err(AgentError::config)
}

/// Whether `command` is a bare program name, such as `npx`, that is looked
/// up on the host search path, rather than a path to a program.
pub fn is_bare_command(command: &str) -> bool {
    !command.is_empty()
        && !Path::new(command).is_absolute()
        && !command.contains('/')
        && !command.contains('\\')
}

/// Resolve a user-configured command for one spawn, holding a bare name to
/// the program the desktop's native dialog approved.
///
/// `approved` is the absolute path the dialog showed when the person allowed
/// the server. The name resolves again here, on the host search path as it
/// stands now, and the spawn goes ahead only when it lands on that same path.
/// A name that now resolves elsewhere, for example to a program that appeared
/// in an earlier directory on the path, is refused: nobody approved that
/// program. A bare name with no approved path runs where it resolves only
/// where no native dialog guards local commands. An absolute command names
/// its program itself, so it resolves as it always has.
pub(super) async fn resolve_approved_command(
    command: &str,
    approved: Option<&str>,
    approval: CommandApproval,
) -> Result<PathBuf> {
    let resolved = resolve_stdio_command(command).await?;
    if !is_bare_command(command) {
        return Ok(resolved);
    }
    match approved {
        Some(approved) if Path::new(approved) == resolved => Ok(resolved),
        Some(approved) => Err(AgentError::config(format!(
            "{NEEDS_APPROVAL} {command:?} now resolves to {}, not to {approved}, the program you \
             allowed. Tidebreak did not start it. To run the new program, save the server again \
             and allow it in the dialog.",
            resolved.display()
        ))),
        None if approval == CommandApproval::Required => Err(AgentError::config(format!(
            "{NEEDS_APPROVAL} {command:?} resolves to {}, a program you have not allowed on this \
             computer. Tidebreak did not start it. To run it, save the server again and allow it \
             in the dialog.",
            resolved.display()
        ))),
        None => Ok(resolved),
    }
}

/// Resolve a user-typed stdio command exactly as verify and launch do, for a
/// caller that shows the answer, such as the desktop's native confirmation.
/// The error is the sentence Settings shows for the same failure.
pub async fn resolve_stdio_executable(command: &str) -> std::result::Result<PathBuf, String> {
    let path = Path::new(command);
    let search_path = if path.is_absolute() || command.contains('/') || command.contains('\\') {
        OsString::new()
    } else {
        host_search_path().await
    };
    resolve_stdio_executable_on(command, &search_path)
}

/// [`resolve_stdio_executable`] against an explicit search path instead of
/// the host's, so a caller's tests do not depend on the machine they run on.
pub fn resolve_stdio_executable_on(
    command: &str,
    search_path: &OsStr,
) -> std::result::Result<PathBuf, String> {
    resolve_stdio_command_on_path(command, search_path).map_err(|error| error.diagnostic())
}

/// The [`FORWARDED_BY_DEFAULT`] names a local server's child gets by
/// default, for a definition that declares the environment names `declared`
/// in `env` or `env_from`.
///
/// A declared name never gets the default, even while its stored value is
/// missing, as it is after an import: the child gets the value the
/// definition names, or none. The spawn and the desktop's native dialog both
/// ask this function, so the dialog lists exactly the names the child gets.
pub fn defaulted_names<'a>(declared: impl IntoIterator<Item = &'a str>) -> Vec<&'static str> {
    let declared: Vec<&str> = declared.into_iter().collect();
    FORWARDED_BY_DEFAULT
        .into_iter()
        .filter(|name| {
            !declared
                .iter()
                .any(|declared| same_environment_name(declared, name))
        })
        .collect()
}

/// Environment names compare without case on Windows, whose process
/// environment ignores case, and exactly everywhere else.
fn same_environment_name(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// The values of the [`defaulted_names`] for a definition that declares
/// `declared`. HOME is this process's. PATH is the host search path a bare
/// command resolves on, so a script such as `npx` finds the `node` beside
/// it. A name with no value here stays unset.
pub(super) async fn forwarded_by_default<'a>(
    declared: impl IntoIterator<Item = &'a str>,
) -> Vec<(&'static str, OsString)> {
    let mut forwarded = Vec::with_capacity(FORWARDED_BY_DEFAULT.len());
    for name in defaulted_names(declared) {
        let value = match name {
            "HOME" => std::env::var_os("HOME"),
            "PATH" => Some(host_search_path().await),
            _ => None,
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            forwarded.push((name, value));
        }
    }
    forwarded
}

pub(super) fn resolve_stdio_command_on_path(
    command: &str,
    search_path: &OsStr,
) -> std::result::Result<PathBuf, StdioResolveError> {
    let path = Path::new(command);
    if path.is_absolute() {
        return classify_absolute(path);
    }
    if command.contains('/') || command.contains('\\') {
        return Err(StdioResolveError::Relative {
            command: command.to_string(),
        });
    }
    match resolve_command_on_path(command, search_path) {
        Some(resolved) => Ok(resolved),
        None => Err(StdioResolveError::NotFound {
            command: command.to_string(),
            searched: format_searched_directories(search_path),
        }),
    }
}

fn classify_absolute(path: &Path) -> std::result::Result<PathBuf, StdioResolveError> {
    match path.metadata() {
        Ok(_) => {
            if is_absolute_executable(path) {
                Ok(path.to_path_buf())
            } else {
                Err(StdioResolveError::NotExecutable {
                    path: path.to_path_buf(),
                })
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(StdioResolveError::PermissionDenied {
                path: path.to_path_buf(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Missing absolute paths keep the spawn-time "command not found"
            // path so existing verify diagnostics stay stable.
            Ok(path.to_path_buf())
        }
        Err(_) => Ok(path.to_path_buf()),
    }
}

async fn host_search_path() -> OsString {
    // A test's replacement stands in for the process PATH, and goes through
    // the same merge, so it is cleaned the way a real one is.
    let overridden = PATH_OVERRIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(overridden) = overridden {
        return merge_search_path(&overridden, None);
    }
    let process = std::env::var_os("PATH").unwrap_or_default();
    // Unit tests never start the person's login shell. A test that needs a
    // particular search path sets one with `HostPathGuard`.
    if cfg!(test) {
        return merge_search_path(&process, None);
    }
    let login = LOGIN_PATH
        .get_or_init(|| async {
            let env = capture_login_env(&HostEnv::from_process()).await.ok()?;
            env_value(&env, OsStr::new("PATH")).cloned()
        })
        .await
        .clone();
    merge_search_path(&process, login.as_deref())
}

/// The process PATH followed by the login-shell PATH, each directory once.
///
/// Only absolute directories stay. Resolution never searches a relative one,
/// and a child given `.` or `node_modules/.bin` would look names up against
/// its own working directory: a script's `#!/usr/bin/env node` could run a
/// `node` from the checkout the server starts in.
fn merge_search_path(process: &OsStr, login: Option<&OsStr>) -> OsString {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for source in [Some(process), login].into_iter().flatten() {
        for dir in std::env::split_paths(source) {
            if !dir.is_absolute() || !seen.insert(dir.clone()) {
                continue;
            }
            dirs.push(dir);
        }
    }
    // `join_paths` refuses only a directory that contains the separator,
    // which `split_paths` never yields.
    std::env::join_paths(dirs).unwrap_or_default()
}

fn format_searched_directories(search_path: &OsStr) -> String {
    let searched: Vec<String> = std::env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.display().to_string())
        .collect();
    if searched.is_empty() {
        return "(no directories on the host PATH)".to_string();
    }
    let shown = searched
        .iter()
        .take(MAX_SEARCHED_DIRS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if searched.len() > MAX_SEARCHED_DIRS {
        format!("{shown}, and {} more", searched.len() - MAX_SEARCHED_DIRS)
    } else {
        shown
    }
}

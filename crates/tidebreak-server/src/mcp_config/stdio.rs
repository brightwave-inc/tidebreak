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
/// resolved on. A name the definition sets itself, in `env` or `env_from`,
/// replaces the default.
pub const FORWARDED_BY_DEFAULT: [&str; 2] = ["HOME", "PATH"];

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

/// The values of the names [`FORWARDED_BY_DEFAULT`] lists. HOME is this
/// process's. PATH is the host search path a bare command resolves on, so a
/// script such as `npx` finds the `node` beside it. A name with no value
/// here stays unset.
pub(super) async fn forwarded_by_default() -> Vec<(&'static str, OsString)> {
    let mut forwarded = Vec::with_capacity(FORWARDED_BY_DEFAULT.len());
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        forwarded.push(("HOME", home));
    }
    let path = host_search_path().await;
    if !path.is_empty() {
        forwarded.push(("PATH", path));
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
    if let Some(overridden) = PATH_OVERRIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return overridden;
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

fn merge_search_path(process: &OsStr, login: Option<&OsStr>) -> OsString {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for source in [Some(process), login] {
        let Some(source) = source else {
            continue;
        };
        for dir in std::env::split_paths(source) {
            if dir.as_os_str().is_empty() || !seen.insert(dir.clone()) {
                continue;
            }
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| process.to_os_string())
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

//! The `tidebreak` command, installed where a shell finds it.
//!
//! Every desktop package carries the CLI as a sidecar beside the app's own
//! executable (`Contents/MacOS/tidebreak` in the macOS bundle), but nothing
//! puts it on a shell's PATH. This module links it there the way editors
//! install their launcher: a symbolic link named `tidebreak` in
//! `~/.local/bin`, or in `/usr/local/bin` through an explicit administrator
//! prompt.
//!
//! A link is Tidebreak's when it points at an app bundle's `tidebreak`
//! sidecar. Anything else at that path, such as a file, a folder, or a link to
//! a build someone made themselves, belongs to the person: install and
//! uninstall both refuse to touch it. A Tidebreak link that points at another
//! copy of the app (the app moved, was renamed, or an old copy was deleted) is
//! repaired in place.
//!
//! Only the macOS app offers the install. It is the one package that ships,
//! and it is the one whose sidecar sits nowhere near a PATH folder.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Webview};

/// The name the command installs under, which is also the sidecar's name.
const COMMAND: &str = "tidebreak";

/// Where an install for this account links the command, under the home
/// folder. Created when it does not exist.
const USER_BIN: &str = ".local/bin";

/// Where an install for every account links the command.
const SYSTEM_BIN: &str = "/usr/local/bin";

/// One of the two places the command can be installed.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliLocation {
    /// `~/.local/bin`, written as the person without a prompt.
    User,
    /// `/usr/local/bin`, written through the macOS administrator prompt.
    System,
}

/// What is at one install location.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum LinkState {
    /// Nothing is there.
    Missing,
    /// Tidebreak's link to this copy of the app.
    Installed,
    /// Tidebreak's link to another copy of the app. Install repairs it, and
    /// uninstall removes it.
    Stale { target: String },
    /// Something Tidebreak did not put there. Install and uninstall leave it.
    Foreign { target: Option<String> },
}

/// One install location as the settings page shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CliLink {
    /// The link's full path.
    path: String,
    #[serde(flatten)]
    state: LinkState,
    /// Whether a login shell's PATH includes the link's folder. `None` when
    /// the PATH could not be read.
    on_path: Option<bool>,
}

/// Why this copy of Tidebreak cannot install the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnavailableReason {
    /// Only the macOS app installs the command.
    Unsupported,
    /// This build carries no CLI sidecar, such as a development build.
    NotBundled,
    /// The app runs from a disk image or from the read-only copy macOS makes
    /// of an app that was never moved, so a link would break as soon as the
    /// image is ejected or the app is quit.
    TemporaryLocation,
}

/// The `tidebreak` a new login shell runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResolvedCommand {
    /// Where the shell finds it.
    path: String,
    /// It runs this app's command. Judged by where the path leads, not by
    /// its name, so Tidebreak's link in either folder counts.
    this_app: bool,
}

/// Everything the settings page needs to say about the command.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum CliCommandStatus {
    Unavailable {
        reason: UnavailableReason,
    },
    Available {
        /// The bundled command every link points at.
        command: String,
        user: CliLink,
        system: CliLink,
        /// What `tidebreak` runs in a new login shell, when anything.
        resolved: Option<ResolvedCommand>,
    },
}

/// What an install or uninstall did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeOutcome {
    /// A new link.
    Created,
    /// A Tidebreak link to another copy of the app now points at this one.
    Updated,
    /// The link was already right.
    Unchanged,
    /// Tidebreak's link is gone.
    Removed,
    /// There was no Tidebreak link to remove.
    Absent,
    /// The person cancelled the administrator prompt, so nothing changed.
    Cancelled,
}

/// The answer to an install or uninstall: what changed, and the state after.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CliCommandChange {
    location: CliLocation,
    outcome: ChangeOutcome,
    /// The install created the link's folder.
    folder_created: bool,
    status: CliCommandStatus,
}

/// Why an install or uninstall changed nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CliCommandError {
    #[error("{0} is not a link Tidebreak made, so Tidebreak left it in place. Move or delete it, then try again.")]
    Foreign(String),
    #[error("This copy of Tidebreak has no tidebreak command to install.")]
    NotBundled,
    #[error("Move Tidebreak to your Applications folder and open it from there, then install the command.")]
    TemporaryLocation,
    #[error("Installing the tidebreak command requires the macOS app.")]
    Unsupported,
    #[error("Tidebreak could not change {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("Something else appeared at {0} while Tidebreak was changing it, so nothing changed.")]
    Changed(String),
    #[error("{0} is a symbolic link, so Tidebreak will not change the tidebreak command there as an administrator. Install the command for your account instead.")]
    LinkedFolder(String),
    #[error("Tidebreak could not change {0} as an administrator.")]
    AdministratorFailed(String),
}

impl CliCommandError {
    fn io(path: &Path, error: &std::io::Error) -> Self {
        Self::Io {
            path: display(path),
            reason: error.to_string(),
        }
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The link in `folder`, and what it points at as written.
fn link_in(folder: &Path) -> PathBuf {
    folder.join(COMMAND)
}

/// What the link at `link` is, compared with the `command` this app bundles.
///
/// The raw target is returned too, because an administrator change re-checks
/// the link against exactly what was read here before it replaces anything.
fn inspect(link: &Path, command: &Path) -> (LinkState, Option<PathBuf>) {
    let Ok(metadata) = std::fs::symlink_metadata(link) else {
        return (LinkState::Missing, None);
    };
    if !metadata.file_type().is_symlink() {
        return (LinkState::Foreign { target: None }, None);
    }
    let Ok(raw) = std::fs::read_link(link) else {
        return (LinkState::Foreign { target: None }, None);
    };
    let target = if raw.is_absolute() {
        raw.clone()
    } else {
        link.parent()
            .map_or_else(|| raw.clone(), |parent| parent.join(&raw))
    };
    let state = if same_file(&target, command) {
        LinkState::Installed
    } else if is_bundled_command(&target) {
        LinkState::Stale {
            target: display(&target),
        }
    } else {
        LinkState::Foreign {
            target: Some(display(&target)),
        }
    };
    (state, Some(raw))
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether `path` names the command inside an app bundle:
/// `…/<Name>.app/Contents/MacOS/tidebreak`. Only Tidebreak ships a sidecar by
/// that name, so a link of this shape is one Tidebreak made, even when the
/// copy of the app it names is gone.
fn is_bundled_command(path: &Path) -> bool {
    let named =
        |path: Option<&Path>, name: &str| path.and_then(Path::file_name) == Some(OsStr::new(name));
    let macos = path.parent();
    let contents = macos.and_then(Path::parent);
    let bundle = contents.and_then(Path::parent);
    named(Some(path), COMMAND)
        && named(macos, "MacOS")
        && named(contents, "Contents")
        && bundle
            .and_then(Path::extension)
            .is_some_and(|extension| extension == "app")
}

/// Whether `command` runs from the randomized read-only copy App
/// Translocation makes of an app that was opened before it was moved. The
/// copy goes away when the app quits.
fn is_translocated(command: &Path) -> bool {
    command
        .components()
        .any(|component| component.as_os_str() == "AppTranslocation")
}

/// The bundled command beside this app's own executable, if it is one a link
/// can point at for good. A disk image is checked apart, in
/// [`current_command`], because telling one from an external drive asks
/// `hdiutil`.
fn bundled_command(executable: &Path) -> Result<PathBuf, UnavailableReason> {
    if !cfg!(target_os = "macos") {
        return Err(UnavailableReason::Unsupported);
    }
    let command = executable
        .parent()
        .map(|folder| folder.join(COMMAND))
        .ok_or(UnavailableReason::NotBundled)?;
    if !command.is_file() {
        return Err(UnavailableReason::NotBundled);
    }
    if is_translocated(&command) {
        return Err(UnavailableReason::TemporaryLocation);
    }
    Ok(command)
}

/// Whether `command` sits on one of the disk images mounted at
/// `mount_points`. An app on an external drive is on none of them: a link to
/// it works whenever the drive is connected.
#[cfg(any(target_os = "macos", test))]
fn on_disk_image(command: &Path, mount_points: &[PathBuf]) -> bool {
    mount_points
        .iter()
        .any(|mount_point| command.starts_with(mount_point))
}

/// The folders the disk images attached right now are mounted at, read from
/// `hdiutil info -plist`.
#[cfg(target_os = "macos")]
fn disk_image_mount_points(info: &[u8]) -> Vec<PathBuf> {
    let Ok(info) = plist::Value::from_reader(std::io::Cursor::new(info)) else {
        return Vec::new();
    };
    let images = info
        .as_dictionary()
        .and_then(|info| info.get("images"))
        .and_then(plist::Value::as_array);
    images
        .into_iter()
        .flatten()
        .filter_map(|image| image.as_dictionary()?.get("system-entities")?.as_array())
        .flatten()
        .filter_map(|entity| entity.as_dictionary()?.get("mount-point")?.as_string())
        .map(PathBuf::from)
        .collect()
}

/// Whether `command` runs from a mounted disk image, such as the one the app
/// was downloaded in. Ejecting the image would break a link to it.
///
/// Only a path under `/Volumes`, where the Finder mounts an image, asks
/// `hdiutil`; an app anywhere else is on the startup disk. When `hdiutil`
/// cannot answer, the install goes ahead: a link that later breaks reads as
/// pointing to another copy, and the page offers the repair.
#[cfg(target_os = "macos")]
async fn runs_from_disk_image(command: &Path) -> bool {
    if !command.starts_with("/Volumes") {
        return false;
    }
    let info = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("/usr/bin/hdiutil")
            .args(["info", "-plist"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match info {
        Ok(Ok(output)) if output.status.success() => {
            on_disk_image(command, &disk_image_mount_points(&output.stdout))
        }
        _ => false,
    }
}

#[cfg(not(target_os = "macos"))]
async fn runs_from_disk_image(_command: &Path) -> bool {
    false
}

fn unavailable_error(reason: UnavailableReason) -> CliCommandError {
    match reason {
        UnavailableReason::Unsupported => CliCommandError::Unsupported,
        UnavailableReason::NotBundled => CliCommandError::NotBundled,
        UnavailableReason::TemporaryLocation => CliCommandError::TemporaryLocation,
    }
}

/// Whether `folder` is one of the entries in a PATH value.
fn folder_on_path(folder: &Path, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|entry| entry == folder || same_file(&entry, folder))
}

/// The first `tidebreak` a PATH value finds, when one is there.
fn resolve_on_path(path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|entry| entry.is_absolute())
        .map(|entry| entry.join(COMMAND))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The status of both locations, read from disk and from a login shell's PATH.
fn status_for(
    command: Result<PathBuf, UnavailableReason>,
    user_folder: &Path,
    system_folder: &Path,
    login_path: Option<&OsStr>,
) -> CliCommandStatus {
    let command = match command {
        Ok(command) => command,
        Err(reason) => return CliCommandStatus::Unavailable { reason },
    };
    let describe = |folder: &Path| {
        let link = link_in(folder);
        CliLink {
            path: display(&link),
            state: inspect(&link, &command).0,
            on_path: login_path.map(|path| folder_on_path(folder, path)),
        }
    };
    CliCommandStatus::Available {
        user: describe(user_folder),
        system: describe(system_folder),
        resolved: login_path
            .and_then(resolve_on_path)
            .map(|resolved| ResolvedCommand {
                this_app: same_file(&resolved, &command),
                path: display(&resolved),
            }),
        command: display(&command),
    }
}

/// Link `command` into `folder` as the person running Tidebreak.
///
/// Creates the folder when needed, leaves a correct link alone, repairs a
/// Tidebreak link to another copy of the app, and refuses anything else.
fn install_into(folder: &Path, command: &Path) -> Result<(ChangeOutcome, bool), CliCommandError> {
    let link = link_in(folder);
    let (state, _) = inspect(&link, command);
    match state {
        LinkState::Installed => return Ok((ChangeOutcome::Unchanged, false)),
        LinkState::Foreign { .. } => return Err(CliCommandError::Foreign(display(&link))),
        LinkState::Missing | LinkState::Stale { .. } => {}
    }
    let folder_created = !folder.is_dir();
    std::fs::create_dir_all(folder).map_err(|error| CliCommandError::io(folder, &error))?;
    match state {
        LinkState::Stale { .. } => Ok((replace_link(command, &link)?, folder_created)),
        _ => {
            // Creating a link never replaces one, so something that appeared
            // since the look above fails here rather than being overwritten.
            symlink(command, &link).map_err(|error| CliCommandError::io(&link, &error))?;
            Ok((ChangeOutcome::Created, folder_created))
        }
    }
}

/// Remove Tidebreak's link from `folder`, and nothing else.
fn uninstall_from(folder: &Path, command: &Path) -> Result<ChangeOutcome, CliCommandError> {
    let link = link_in(folder);
    match inspect(&link, command).0 {
        LinkState::Missing => Ok(ChangeOutcome::Absent),
        LinkState::Foreign { .. } => Err(CliCommandError::Foreign(display(&link))),
        LinkState::Installed | LinkState::Stale { .. } => {
            std::fs::remove_file(&link).map_err(|error| CliCommandError::io(&link, &error))?;
            Ok(ChangeOutcome::Removed)
        }
    }
}

/// Point an existing Tidebreak link at `command` in one step: a new link is
/// made beside it and renamed over it, so a shell never finds the command
/// missing halfway through.
///
/// A rename replaces whatever is at `link`, so the link is looked at again
/// once the new one is ready, right before the rename. Something the person
/// put there since the first look is theirs, and stays.
fn replace_link(command: &Path, link: &Path) -> Result<ChangeOutcome, CliCommandError> {
    let folder = link.parent().unwrap_or_else(|| Path::new("."));
    let staged = folder.join(format!(".{COMMAND}.{}.new", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    symlink(command, &staged).map_err(|error| CliCommandError::io(link, &error))?;
    let discard = || {
        let _ = std::fs::remove_file(&staged);
    };
    let outcome = match inspect(link, command).0 {
        LinkState::Stale { .. } => ChangeOutcome::Updated,
        LinkState::Missing => ChangeOutcome::Created,
        LinkState::Installed => {
            discard();
            return Ok(ChangeOutcome::Unchanged);
        }
        LinkState::Foreign { .. } => {
            discard();
            return Err(CliCommandError::Foreign(display(link)));
        }
    };
    std::fs::rename(&staged, link).map_err(|error| {
        discard();
        CliCommandError::io(link, &error)
    })?;
    Ok(outcome)
}

#[cfg(unix)]
fn symlink(command: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(command, link)
}

#[cfg(not(unix))]
fn symlink(_command: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symbolic links are not supported here",
    ))
}

/// Quote a string for `/bin/sh`.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Quote a string for AppleScript.
#[cfg(any(target_os = "macos", test))]
fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', r"\\").replace('"', "\\\""))
}

/// The exit status an administrator script uses when the link changed since
/// Tidebreak looked at it.
const CHANGED_EXIT: i32 = 3;

/// The exit status an administrator script uses when the folder, or a folder
/// above it, is a symbolic link.
const LINKED_FOLDER_EXIT: i32 = 4;

/// `folder` and each folder above it, up to but not including `/`: the paths
/// root must not follow a symbolic link through.
fn folder_and_parents(folder: &Path) -> impl Iterator<Item = &Path> {
    folder
        .ancestors()
        .filter(|ancestor| ancestor.parent().is_some())
}

/// The first of `folder` and the folders above it that is a symbolic link.
fn linked_folder(folder: &Path) -> Option<&Path> {
    folder_and_parents(folder).find(|ancestor| {
        std::fs::symlink_metadata(ancestor).is_ok_and(|metadata| metadata.file_type().is_symlink())
    })
}

/// The shell script that makes an administrator change, guarded so it touches
/// the link only if it is still exactly what Tidebreak inspected. Anything
/// else exits with [`CHANGED_EXIT`] before changing a byte.
///
/// Root follows a symbolic link wherever it leads, so the script also refuses
/// with [`LINKED_FOLDER_EXIT`] when the folder, or a folder above it, is one:
/// the link must land in the real folder and nowhere else.
fn administrator_script(
    change: AdministratorChange,
    folder: &Path,
    command: &Path,
    observed: Option<&Path>,
) -> String {
    let link = shell_quote(&display(&link_in(folder)));
    let no_linked_folders = folder_and_parents(folder)
        .map(|ancestor| format!("[ ! -L {} ]", shell_quote(&display(ancestor))))
        .collect::<Vec<_>>()
        .join(" && ");
    let not_linked = format!("{no_linked_folders} || exit {LINKED_FOLDER_EXIT}");
    let folder = shell_quote(&display(folder));
    let command = shell_quote(&display(command));
    let unchanged = match observed {
        None => format!("if [ -e {link} ] || [ -L {link} ]; then exit {CHANGED_EXIT}; fi"),
        Some(target) => format!(
            "if [ ! -L {link} ] || [ \"$(/usr/bin/readlink {link})\" != {} ]; then exit {CHANGED_EXIT}; fi",
            shell_quote(&display(target))
        ),
    };
    match change {
        AdministratorChange::Install => [
            not_linked,
            format!("/bin/mkdir -p {folder} || exit 1"),
            format!("[ -d {folder} ] && [ ! -L {folder} ] || exit {LINKED_FOLDER_EXIT}"),
            unchanged,
            format!("/bin/ln -sfh {command} {link}"),
        ]
        .join("; "),
        AdministratorChange::Uninstall => {
            [not_linked, unchanged, format!("/bin/rm -f {link}")].join("; ")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdministratorChange {
    Install,
    Uninstall,
}

/// How an administrator script ended.
// Off macOS nothing runs a script, so only the tests build most of these.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScriptEnd {
    /// It ran to the end.
    Done,
    /// The person cancelled the password prompt, so it never ran.
    Cancelled,
    /// It found something other than what Tidebreak inspected at the link.
    Changed,
    /// It found a symbolic link in place of the folder or a folder above it.
    LinkedFolder,
    /// It could not run, or failed partway.
    Failed,
}

/// What runs an administrator script. The app asks macOS for an
/// administrator password; tests stand in for it, so no test can put a real
/// password prompt in front of whoever runs them.
#[async_trait::async_trait]
trait Administrator: Sync {
    async fn run(&self, script: &str) -> ScriptEnd;
}

/// The macOS administrator prompt.
struct PasswordPrompt;

#[async_trait::async_trait]
impl Administrator for PasswordPrompt {
    async fn run(&self, script: &str) -> ScriptEnd {
        // A test that reached this would ask whoever runs the suite for their
        // password. Tests pass a stand-in instead.
        if cfg!(test) {
            panic!("a test reached the real administrator prompt");
        }
        run_with_password(script).await
    }
}

/// Run `script` as root through the macOS administrator prompt.
#[cfg(target_os = "macos")]
async fn run_with_password(script: &str) -> ScriptEnd {
    let prompt = "Tidebreak wants to change the tidebreak command for all users of this Mac.";
    let source = format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(script),
        applescript_string(prompt)
    );
    match tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(source)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
    {
        Ok(output) => script_end(
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Err(_) => ScriptEnd::Failed,
    }
}

#[cfg(not(target_os = "macos"))]
async fn run_with_password(_script: &str) -> ScriptEnd {
    ScriptEnd::Failed
}

/// How `osascript` reported an administrator script. AppleScript reports a
/// cancelled password prompt as error -128, and a script that failed by its
/// exit status.
#[cfg(any(target_os = "macos", test))]
fn script_end(success: bool, stderr: &str) -> ScriptEnd {
    if success {
        ScriptEnd::Done
    } else if stderr.contains("(-128)") {
        ScriptEnd::Cancelled
    } else if stderr.contains(&format!("({CHANGED_EXIT})")) {
        ScriptEnd::Changed
    } else if stderr.contains(&format!("({LINKED_FOLDER_EXIT})")) {
        ScriptEnd::LinkedFolder
    } else {
        ScriptEnd::Failed
    }
}

/// An administrator change to `/usr/local/bin`, checked first as the person so
/// a refusal never costs a password prompt.
async fn change_as_administrator(
    change: AdministratorChange,
    folder: &Path,
    command: &Path,
    administrator: &impl Administrator,
) -> Result<(ChangeOutcome, bool), CliCommandError> {
    let link = link_in(folder);
    let (state, observed) = inspect(&link, command);
    let outcome = match (change, &state) {
        (_, LinkState::Foreign { .. }) => return Err(CliCommandError::Foreign(display(&link))),
        (AdministratorChange::Install, LinkState::Installed) => {
            return Ok((ChangeOutcome::Unchanged, false))
        }
        (AdministratorChange::Uninstall, LinkState::Missing) => {
            return Ok((ChangeOutcome::Absent, false))
        }
        (AdministratorChange::Install, LinkState::Missing) => ChangeOutcome::Created,
        (AdministratorChange::Install, LinkState::Stale { .. }) => ChangeOutcome::Updated,
        (AdministratorChange::Uninstall, _) => ChangeOutcome::Removed,
    };
    if let Some(linked) = linked_folder(folder) {
        return Err(CliCommandError::LinkedFolder(display(linked)));
    }
    let folder_created = change == AdministratorChange::Install && !folder.is_dir();
    let script = administrator_script(change, folder, command, observed.as_deref());
    match administrator.run(&script).await {
        ScriptEnd::Done => Ok((outcome, folder_created)),
        ScriptEnd::Cancelled => Ok((ChangeOutcome::Cancelled, false)),
        ScriptEnd::Changed => Err(CliCommandError::Changed(display(&link))),
        ScriptEnd::LinkedFolder => Err(CliCommandError::LinkedFolder(display(folder))),
        ScriptEnd::Failed => Err(CliCommandError::AdministratorFailed(display(&link))),
    }
}

/// A login shell's PATH, which is what a new terminal gets. The app's own
/// PATH is the minimal one macOS gives apps opened from the Finder or Dock.
async fn login_path() -> Option<std::ffi::OsString> {
    let env = tidebreak_harness::probe::capture_login_env(
        &tidebreak_harness::probe::HostEnv::from_process(),
    )
    .await
    .ok()?;
    env.into_iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
}

fn require_main_webview(label: &str) -> Result<(), String> {
    if label == "main" {
        Ok(())
    } else {
        Err("The tidebreak command can be installed only from Tidebreak Settings.".to_owned())
    }
}

fn user_folder(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .home_dir()
        .map(|home| home.join(USER_BIN))
        .map_err(|_| "Tidebreak could not find your home folder.".to_owned())
}

async fn current_command() -> Result<PathBuf, UnavailableReason> {
    let command = std::env::current_exe()
        .map_err(|_| UnavailableReason::NotBundled)
        .and_then(|executable| bundled_command(&executable))?;
    if runs_from_disk_image(&command).await {
        return Err(UnavailableReason::TemporaryLocation);
    }
    Ok(command)
}

async fn current_status(app: &AppHandle) -> Result<CliCommandStatus, String> {
    let user = user_folder(app)?;
    let command = current_command().await;
    let path = if command.is_ok() {
        login_path().await
    } else {
        None
    };
    Ok(status_for(
        command,
        &user,
        Path::new(SYSTEM_BIN),
        path.as_deref(),
    ))
}

/// Where the command is installed, and whether a new terminal will find it.
#[tauri::command]
pub(crate) async fn cli_command_status(
    app: AppHandle,
    webview: Webview,
) -> Result<CliCommandStatus, String> {
    require_main_webview(webview.label())?;
    current_status(&app).await
}

/// Link the bundled command into `location`.
#[tauri::command]
pub(crate) async fn install_cli_command(
    app: AppHandle,
    webview: Webview,
    location: CliLocation,
) -> Result<CliCommandChange, String> {
    require_main_webview(webview.label())?;
    let command = current_command()
        .await
        .map_err(|reason| unavailable_error(reason).to_string())?;
    let (outcome, folder_created) = match location {
        CliLocation::User => install_into(&user_folder(&app)?, &command),
        CliLocation::System => {
            change_as_administrator(
                AdministratorChange::Install,
                Path::new(SYSTEM_BIN),
                &command,
                &PasswordPrompt,
            )
            .await
        }
    }
    .map_err(|error| error.to_string())?;
    Ok(CliCommandChange {
        location,
        outcome,
        folder_created,
        status: current_status(&app).await?,
    })
}

/// Remove Tidebreak's link from `location`.
#[tauri::command]
pub(crate) async fn uninstall_cli_command(
    app: AppHandle,
    webview: Webview,
    location: CliLocation,
) -> Result<CliCommandChange, String> {
    require_main_webview(webview.label())?;
    let command = current_command()
        .await
        .map_err(|reason| unavailable_error(reason).to_string())?;
    let outcome = match location {
        CliLocation::User => uninstall_from(&user_folder(&app)?, &command),
        CliLocation::System => change_as_administrator(
            AdministratorChange::Uninstall,
            Path::new(SYSTEM_BIN),
            &command,
            &PasswordPrompt,
        )
        .await
        .map(|(outcome, _)| outcome),
    }
    .map_err(|error| error.to_string())?;
    Ok(CliCommandChange {
        location,
        outcome,
        folder_created: false,
        status: current_status(&app).await?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A temporary home with an installed app at `Applications/<app>` whose
    /// bundle carries the command.
    struct Fixture {
        _root: tempfile::TempDir,
        /// The temporary folder with every symbolic link resolved (macOS
        /// keeps temporary files behind `/var`, a link to `/private/var`), so
        /// the administrator guards see only the links a test makes.
        base: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(root.path()).unwrap();
            Self { _root: root, base }
        }

        fn home(&self) -> PathBuf {
            self.base.join("home")
        }

        /// Where these tests put the folder an install for all users uses.
        fn system(&self) -> PathBuf {
            self.base.join("usr/local/bin")
        }

        fn bin(&self) -> PathBuf {
            self.home().join(USER_BIN)
        }

        fn link(&self) -> PathBuf {
            self.bin().join(COMMAND)
        }

        /// Install a copy of the app named `app` and return its command.
        fn app(&self, app: &str) -> PathBuf {
            let macos = self
                .base
                .join("Applications")
                .join(app)
                .join("Contents/MacOS");
            std::fs::create_dir_all(&macos).unwrap();
            let command = macos.join(COMMAND);
            std::fs::write(&command, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(
                &command,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
            command
        }
    }

    #[test]
    fn install_links_the_bundled_command_into_a_new_local_bin() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        assert!(!fixture.bin().exists());

        let (outcome, folder_created) = install_into(&fixture.bin(), &command).unwrap();

        assert_eq!(outcome, ChangeOutcome::Created);
        assert!(folder_created);
        assert!(std::fs::symlink_metadata(fixture.link())
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(fixture.link()).unwrap(), command);
        assert_eq!(inspect(&fixture.link(), &command).0, LinkState::Installed);
    }

    #[test]
    fn a_second_install_changes_nothing() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        install_into(&fixture.bin(), &command).unwrap();

        assert_eq!(
            install_into(&fixture.bin(), &command).unwrap(),
            (ChangeOutcome::Unchanged, false)
        );
    }

    #[test]
    fn install_refuses_a_file_it_did_not_make() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        std::fs::create_dir_all(fixture.bin()).unwrap();
        std::fs::write(fixture.link(), b"someone else's tidebreak").unwrap();

        let error = install_into(&fixture.bin(), &command).unwrap_err();

        assert_eq!(error, CliCommandError::Foreign(display(&fixture.link())));
        assert_eq!(
            std::fs::read(fixture.link()).unwrap(),
            b"someone else's tidebreak"
        );
    }

    #[test]
    fn install_refuses_a_link_to_a_build_someone_made() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let build = fixture.base.join("src/tidebreak/target/release");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join(COMMAND), b"a local build").unwrap();
        std::fs::create_dir_all(fixture.bin()).unwrap();
        symlink(&build.join(COMMAND), &fixture.link()).unwrap();

        assert!(matches!(
            inspect(&fixture.link(), &command).0,
            LinkState::Foreign { target: Some(_) }
        ));
        assert!(matches!(
            install_into(&fixture.bin(), &command),
            Err(CliCommandError::Foreign(_))
        ));
        assert_eq!(
            std::fs::read_link(fixture.link()).unwrap(),
            build.join(COMMAND)
        );
    }

    /// Runs administrator scripts in place of the macOS prompt and counts
    /// them. With no `answer` it runs the script as the person running the
    /// tests, which is enough to exercise its guards in a temporary folder.
    struct StandIn {
        runs: AtomicUsize,
        answer: Option<ScriptEnd>,
    }

    impl StandIn {
        fn answering(answer: ScriptEnd) -> Self {
            Self {
                runs: AtomicUsize::new(0),
                answer: Some(answer),
            }
        }

        fn running() -> Self {
            Self {
                runs: AtomicUsize::new(0),
                answer: None,
            }
        }

        fn runs(&self) -> usize {
            self.runs.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Administrator for StandIn {
        async fn run(&self, script: &str) -> ScriptEnd {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.answer.unwrap_or_else(|| run_as_self(script))
        }
    }

    /// Run an administrator script without elevation.
    fn run_as_self(script: &str) -> ScriptEnd {
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .status()
            .unwrap();
        match status.code() {
            Some(0) => ScriptEnd::Done,
            Some(CHANGED_EXIT) => ScriptEnd::Changed,
            Some(LINKED_FOLDER_EXIT) => ScriptEnd::LinkedFolder,
            _ => ScriptEnd::Failed,
        }
    }

    fn administrator_change(
        change: AdministratorChange,
        folder: &Path,
        command: &Path,
        administrator: &StandIn,
    ) -> Result<(ChangeOutcome, bool), CliCommandError> {
        tauri::async_runtime::block_on(change_as_administrator(
            change,
            folder,
            command,
            administrator,
        ))
    }

    #[test]
    fn install_repairs_a_link_to_an_old_app_location() {
        let fixture = Fixture::new();
        let old = fixture.app("Tidebreak old.app");
        let current = fixture.app("Tidebreak.app");
        install_into(&fixture.bin(), &old).unwrap();
        // The old copy is gone, so the link dangles.
        std::fs::remove_dir_all(old.ancestors().nth(3).unwrap()).unwrap();

        assert_eq!(
            inspect(&fixture.link(), &current).0,
            LinkState::Stale {
                target: display(&old)
            }
        );
        let (outcome, folder_created) = install_into(&fixture.bin(), &current).unwrap();

        assert_eq!(outcome, ChangeOutcome::Updated);
        assert!(!folder_created);
        assert_eq!(std::fs::read_link(fixture.link()).unwrap(), current);
        // The staged link was renamed into place, not left behind.
        let leftovers: Vec<_> = std::fs::read_dir(fixture.bin())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(COMMAND)]);
    }

    /// The repair renames a new link over the old one, and a rename replaces
    /// whatever is there. A file that took the link's place after the first
    /// look stays, and the new link is cleared away.
    #[test]
    fn a_repair_refuses_a_file_that_appeared_since_the_first_look() {
        let fixture = Fixture::new();
        let old = fixture.app("Tidebreak old.app");
        let current = fixture.app("Tidebreak.app");
        install_into(&fixture.bin(), &old).unwrap();
        assert!(matches!(
            inspect(&fixture.link(), &current).0,
            LinkState::Stale { .. }
        ));

        // The person replaces the link with their own file after the look.
        std::fs::remove_file(fixture.link()).unwrap();
        std::fs::write(fixture.link(), b"someone else's tidebreak").unwrap();

        assert_eq!(
            replace_link(&current, &fixture.link()),
            Err(CliCommandError::Foreign(display(&fixture.link())))
        );
        assert_eq!(
            std::fs::read(fixture.link()).unwrap(),
            b"someone else's tidebreak"
        );
        let leftovers: Vec<_> = std::fs::read_dir(fixture.bin())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(COMMAND)]);
    }

    #[test]
    fn uninstall_removes_only_its_own_link() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        install_into(&fixture.bin(), &command).unwrap();

        assert_eq!(
            uninstall_from(&fixture.bin(), &command).unwrap(),
            ChangeOutcome::Removed
        );
        assert!(std::fs::symlink_metadata(fixture.link()).is_err());
        assert!(command.is_file(), "the bundled command stays");
        assert_eq!(
            uninstall_from(&fixture.bin(), &command).unwrap(),
            ChangeOutcome::Absent
        );

        std::fs::write(fixture.link(), b"someone else's tidebreak").unwrap();
        assert!(matches!(
            uninstall_from(&fixture.bin(), &command),
            Err(CliCommandError::Foreign(_))
        ));
        assert!(fixture.link().is_file());
    }

    #[test]
    fn uninstall_also_removes_a_link_to_an_old_app_location() {
        let fixture = Fixture::new();
        let old = fixture.app("Tidebreak old.app");
        let current = fixture.app("Tidebreak.app");
        install_into(&fixture.bin(), &old).unwrap();

        assert_eq!(
            uninstall_from(&fixture.bin(), &current).unwrap(),
            ChangeOutcome::Removed
        );
        assert!(old.is_file());
    }

    #[test]
    fn only_an_app_bundle_sidecar_counts_as_tidebreaks() {
        for owned in [
            "/Applications/Tidebreak.app/Contents/MacOS/tidebreak",
            "/Users/me/Applications/Tidebreak Staging.app/Contents/MacOS/tidebreak",
        ] {
            assert!(is_bundled_command(Path::new(owned)), "{owned}");
        }
        for foreign in [
            "/Users/me/.cargo/bin/tidebreak",
            "/Applications/Tidebreak.app/Contents/Resources/tidebreak",
            "/Applications/Tidebreak/Contents/MacOS/tidebreak",
            "/Applications/Tidebreak.app/Contents/MacOS/tidebreak-desktop",
            "tidebreak",
        ] {
            assert!(!is_bundled_command(Path::new(foreign)), "{foreign}");
        }
    }

    #[test]
    fn status_reports_each_location_and_whether_a_shell_finds_it() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let system = fixture.system();
        install_into(&fixture.bin(), &command).unwrap();

        let path = std::env::join_paths([system.clone(), fixture.bin()]).unwrap();
        let CliCommandStatus::Available {
            user,
            system: system_link,
            resolved,
            ..
        } = status_for(Ok(command.clone()), &fixture.bin(), &system, Some(&path))
        else {
            panic!("an installed app can install the command");
        };
        assert_eq!(user.state, LinkState::Installed);
        assert_eq!(user.on_path, Some(true));
        assert_eq!(system_link.state, LinkState::Missing);
        assert_eq!(
            resolved,
            Some(ResolvedCommand {
                path: display(&fixture.link()),
                this_app: true,
            })
        );

        let elsewhere = std::env::join_paths([system.clone()]).unwrap();
        let CliCommandStatus::Available { user, resolved, .. } = status_for(
            Ok(command.clone()),
            &fixture.bin(),
            &system,
            Some(&elsewhere),
        ) else {
            panic!("an installed app can install the command");
        };
        assert_eq!(user.on_path, Some(false));
        assert_eq!(resolved, None);

        let CliCommandStatus::Available { user, .. } =
            status_for(Ok(command), &fixture.bin(), &system, None)
        else {
            panic!("an installed app can install the command");
        };
        assert_eq!(
            user.on_path, None,
            "an unread PATH is not reported as missing"
        );
    }

    /// What a new terminal runs is judged by where it leads. Tidebreak's link
    /// in `/usr/local/bin` ahead of the one in `~/.local/bin` still runs this
    /// app; a build of someone's own ahead of both does not.
    #[test]
    fn status_judges_what_a_shell_runs_by_where_it_leads() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let system = fixture.system();
        install_into(&fixture.bin(), &command).unwrap();
        install_into(&system, &command).unwrap();

        let system_first = std::env::join_paths([system.clone(), fixture.bin()]).unwrap();
        let CliCommandStatus::Available { resolved, .. } = status_for(
            Ok(command.clone()),
            &fixture.bin(),
            &system,
            Some(&system_first),
        ) else {
            panic!("an installed app can install the command");
        };
        assert_eq!(
            resolved,
            Some(ResolvedCommand {
                path: display(&link_in(&system)),
                this_app: true,
            })
        );

        let cargo = fixture.home().join(".cargo/bin");
        std::fs::create_dir_all(&cargo).unwrap();
        let build = cargo.join(COMMAND);
        std::fs::write(&build, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&build, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let shadowed = std::env::join_paths([cargo, system.clone()]).unwrap();
        let CliCommandStatus::Available { resolved, .. } =
            status_for(Ok(command), &fixture.bin(), &system, Some(&shadowed))
        else {
            panic!("an installed app can install the command");
        };
        assert_eq!(
            resolved,
            Some(ResolvedCommand {
                path: display(&build),
                this_app: false,
            })
        );
    }

    #[test]
    fn a_build_without_the_sidecar_or_a_temporary_copy_cannot_install() {
        let fixture = Fixture::new();
        let bare = fixture.base.join("Bare.app/Contents/MacOS");
        std::fs::create_dir_all(&bare).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(
                bundled_command(&bare.join("tidebreak-desktop")),
                Err(UnavailableReason::NotBundled)
            );
            let command = fixture.app("Tidebreak.app");
            assert_eq!(
                bundled_command(&command.with_file_name("tidebreak-desktop")),
                Ok(command)
            );
        } else {
            assert_eq!(
                bundled_command(&bare.join("tidebreak-desktop")),
                Err(UnavailableReason::Unsupported)
            );
        }
        assert!(is_translocated(Path::new(
            "/private/var/folders/xy/T/AppTranslocation/1234/d/Tidebreak.app/Contents/MacOS/tidebreak"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/Tidebreak.app/Contents/MacOS/tidebreak"
        )));

        // A disk image is temporary; an external drive is not.
        let images = [PathBuf::from("/Volumes/Tidebreak")];
        assert!(on_disk_image(
            Path::new("/Volumes/Tidebreak/Tidebreak.app/Contents/MacOS/tidebreak"),
            &images
        ));
        for kept in [
            "/Volumes/Backup/Applications/Tidebreak.app/Contents/MacOS/tidebreak",
            "/Volumes/Tidebreak 2/Tidebreak.app/Contents/MacOS/tidebreak",
            "/Applications/Tidebreak.app/Contents/MacOS/tidebreak",
        ] {
            assert!(!on_disk_image(Path::new(kept), &images), "{kept}");
        }
    }

    /// `hdiutil info -plist` lists each attached image with the volumes it
    /// mounted; only mounted volumes carry a mount point.
    #[cfg(target_os = "macos")]
    #[test]
    fn disk_images_are_read_from_hdiutil() {
        let info = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>framework</key><string>683.160.3</string>
  <key>images</key>
  <array>
    <dict>
      <key>image-path</key><string>/Users/avery/Downloads/Tidebreak.dmg</string>
      <key>system-entities</key>
      <array>
        <dict><key>content-hint</key><string>GUID_partition_scheme</string><key>dev-entry</key><string>/dev/disk4</string></dict>
        <dict><key>content-hint</key><string>Apple_HFS</string><key>dev-entry</key><string>/dev/disk4s1</string><key>mount-point</key><string>/Volumes/Tidebreak</string></dict>
      </array>
    </dict>
  </array>
</dict>
</plist>"#;
        assert_eq!(
            disk_image_mount_points(info),
            vec![PathBuf::from("/Volumes/Tidebreak")]
        );
        assert!(disk_image_mount_points(b"not a plist").is_empty());
    }

    #[test]
    fn administrator_scripts_change_only_the_link_that_was_inspected() {
        let folder = Path::new("/usr/local/bin");
        let command = Path::new("/Applications/Tide break's.app/Contents/MacOS/tidebreak");

        let fresh = administrator_script(AdministratorChange::Install, folder, command, None);
        assert_eq!(
            fresh,
            "[ ! -L '/usr/local/bin' ] && [ ! -L '/usr/local' ] && [ ! -L '/usr' ] || exit 4; \
             /bin/mkdir -p '/usr/local/bin' || exit 1; \
             [ -d '/usr/local/bin' ] && [ ! -L '/usr/local/bin' ] || exit 4; \
             if [ -e '/usr/local/bin/tidebreak' ] || [ -L '/usr/local/bin/tidebreak' ]; then exit 3; fi; \
             /bin/ln -sfh '/Applications/Tide break'\\''s.app/Contents/MacOS/tidebreak' '/usr/local/bin/tidebreak'"
        );

        let old = Path::new("/Applications/Old.app/Contents/MacOS/tidebreak");
        let repair = administrator_script(AdministratorChange::Install, folder, command, Some(old));
        assert!(repair.contains(
            "[ \"$(/usr/bin/readlink '/usr/local/bin/tidebreak')\" != '/Applications/Old.app/Contents/MacOS/tidebreak' ]"
        ));
        assert!(repair.ends_with("/bin/ln -sfh '/Applications/Tide break'\\''s.app/Contents/MacOS/tidebreak' '/usr/local/bin/tidebreak'"));

        let remove =
            administrator_script(AdministratorChange::Uninstall, folder, command, Some(old));
        assert!(remove.starts_with(
            "[ ! -L '/usr/local/bin' ] && [ ! -L '/usr/local' ] && [ ! -L '/usr' ] || exit 4; if [ ! -L '/usr/local/bin/tidebreak' ]"
        ));
        assert!(remove.ends_with("; /bin/rm -f '/usr/local/bin/tidebreak'"));

        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn a_foreign_file_is_refused_before_any_administrator_prompt() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        std::fs::create_dir_all(fixture.system()).unwrap();
        let link = link_in(&fixture.system());
        std::fs::write(&link, b"someone else's tidebreak").unwrap();
        let administrator = StandIn::answering(ScriptEnd::Done);

        let refused = administrator_change(
            AdministratorChange::Install,
            &fixture.system(),
            &command,
            &administrator,
        );

        assert_eq!(refused, Err(CliCommandError::Foreign(display(&link))));
        assert_eq!(administrator.runs(), 0, "no password prompt for a refusal");
    }

    #[test]
    fn a_cancelled_administrator_prompt_changes_nothing() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let administrator = StandIn::answering(ScriptEnd::Cancelled);

        assert_eq!(
            administrator_change(
                AdministratorChange::Install,
                &fixture.system(),
                &command,
                &administrator,
            ),
            Ok((ChangeOutcome::Cancelled, false))
        );
        assert_eq!(administrator.runs(), 1);
        assert!(std::fs::symlink_metadata(link_in(&fixture.system())).is_err());
    }

    #[test]
    fn osascript_reports_map_to_how_the_script_ended() {
        assert_eq!(script_end(true, ""), ScriptEnd::Done);
        assert_eq!(
            script_end(false, "execution error: User canceled. (-128)"),
            ScriptEnd::Cancelled
        );
        assert_eq!(
            script_end(
                false,
                "execution error: The command exited with a non-zero status. (3)"
            ),
            ScriptEnd::Changed
        );
        assert_eq!(
            script_end(
                false,
                "execution error: The command exited with a non-zero status. (4)"
            ),
            ScriptEnd::LinkedFolder
        );
        assert_eq!(
            script_end(false, "execution error: ln: Permission denied (1)"),
            ScriptEnd::Failed
        );
    }

    /// Root follows a symbolic link wherever it leads, so a link planted in
    /// place of the folder, or a folder above it, is refused before any
    /// prompt, and the script refuses it again if one appears after.
    #[test]
    fn an_administrator_change_never_follows_a_linked_folder() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let elsewhere = fixture.base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(fixture.system().parent().unwrap()).unwrap();
        symlink(&elsewhere, &fixture.system()).unwrap();
        let administrator = StandIn::running();

        assert_eq!(
            administrator_change(
                AdministratorChange::Install,
                &fixture.system(),
                &command,
                &administrator,
            ),
            Err(CliCommandError::LinkedFolder(display(&fixture.system())))
        );
        assert_eq!(administrator.runs(), 0, "no password prompt for a refusal");
        assert!(!elsewhere.join(COMMAND).exists());
    }

    // The scripts use the macOS `ln -h`, so they run only there.
    #[cfg(target_os = "macos")]
    #[test]
    fn administrator_scripts_refuse_a_linked_folder_and_a_changed_link() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let install = |folder: &Path, observed: Option<&Path>| {
            run_as_self(&administrator_script(
                AdministratorChange::Install,
                folder,
                &command,
                observed,
            ))
        };

        // The folder, and a folder above it, turned into links after the look.
        let elsewhere = fixture.base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(fixture.system().parent().unwrap()).unwrap();
        symlink(&elsewhere, &fixture.system()).unwrap();
        assert_eq!(install(&fixture.system(), None), ScriptEnd::LinkedFolder);
        assert!(!elsewhere.join(COMMAND).exists());

        let parent = fixture.base.join("opt");
        symlink(&elsewhere, &parent).unwrap();
        let below = parent.join("bin");
        assert_eq!(install(&below, None), ScriptEnd::LinkedFolder);
        assert!(!elsewhere.join("bin").exists(), "mkdir never ran");

        // Something appeared at the link after the look.
        let real = fixture.base.join("real/bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(link_in(&real), b"someone else's tidebreak").unwrap();
        assert_eq!(install(&real, None), ScriptEnd::Changed);
        assert_eq!(
            std::fs::read(link_in(&real)).unwrap(),
            b"someone else's tidebreak"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_administrator_install_and_uninstall_change_only_the_link() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        let system = fixture.system();
        let administrator = StandIn::running();

        assert_eq!(
            administrator_change(
                AdministratorChange::Install,
                &system,
                &command,
                &administrator
            ),
            Ok((ChangeOutcome::Created, true))
        );
        assert_eq!(std::fs::read_link(link_in(&system)).unwrap(), command);
        assert_eq!(
            administrator_change(
                AdministratorChange::Install,
                &system,
                &command,
                &administrator
            ),
            Ok((ChangeOutcome::Unchanged, false))
        );
        assert_eq!(administrator.runs(), 1, "a correct link needs no prompt");

        assert_eq!(
            administrator_change(
                AdministratorChange::Uninstall,
                &system,
                &command,
                &administrator
            ),
            Ok((ChangeOutcome::Removed, false))
        );
        assert!(std::fs::symlink_metadata(link_in(&system)).is_err());
        assert!(command.is_file(), "the bundled command stays");
    }

    #[test]
    fn install_is_reachable_only_from_the_main_window() {
        assert!(require_main_webview("main").is_ok());
        for label in ["browser", "main-child", "", "Main"] {
            assert!(require_main_webview(label).is_err(), "{label}");
        }
    }
}

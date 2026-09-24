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
        resolved: Option<String>,
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
    // The administrator prompt exists only on macOS; elsewhere these three
    // are never built.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    #[error("The administrator prompt was cancelled, so nothing changed.")]
    AdministratorCancelled,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    #[error("Something else appeared at {0} while Tidebreak was changing it, so nothing changed.")]
    Changed(String),
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

/// A path macOS may take away from under a link: a mounted disk image, or the
/// randomized read-only copy Gatekeeper runs an app from until it is moved.
fn is_temporary_location(command: &Path) -> bool {
    command.starts_with("/Volumes")
        || command
            .components()
            .any(|component| component.as_os_str() == "AppTranslocation")
}

/// The bundled command beside this app's own executable, if it is one a link
/// can point at for good.
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
    if is_temporary_location(&command) {
        return Err(UnavailableReason::TemporaryLocation);
    }
    Ok(command)
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
        command: display(&command),
        user: describe(user_folder),
        system: describe(system_folder),
        resolved: login_path
            .and_then(resolve_on_path)
            .map(|resolved| display(&resolved)),
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
        LinkState::Stale { .. } => {
            replace_link(command, &link)?;
            Ok((ChangeOutcome::Updated, folder_created))
        }
        _ => {
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
fn replace_link(command: &Path, link: &Path) -> Result<(), CliCommandError> {
    let folder = link.parent().unwrap_or_else(|| Path::new("."));
    let staged = folder.join(format!(".{COMMAND}.{}.new", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    symlink(command, &staged).map_err(|error| CliCommandError::io(link, &error))?;
    std::fs::rename(&staged, link).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        CliCommandError::io(link, &error)
    })
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

/// The shell script that makes an administrator change, guarded so it touches
/// the link only if it is still exactly what Tidebreak inspected. Anything
/// else exits with [`CHANGED_EXIT`] before changing a byte.
fn administrator_script(
    change: AdministratorChange,
    folder: &Path,
    command: &Path,
    observed: Option<&Path>,
) -> String {
    let link = shell_quote(&display(&link_in(folder)));
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
        AdministratorChange::Install => {
            format!("/bin/mkdir -p {folder} && {unchanged} && /bin/ln -sfh {command} {link}")
        }
        AdministratorChange::Uninstall => format!("{unchanged} && /bin/rm -f {link}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdministratorChange {
    Install,
    Uninstall,
}

/// Run `script` through the macOS administrator prompt.
#[cfg(target_os = "macos")]
async fn run_as_administrator(script: &str, link: &Path) -> Result<(), CliCommandError> {
    let prompt = "Tidebreak wants to change the tidebreak command for all users of this Mac.";
    let source = format!(
        "do shell script {} with prompt {} with administrator privileges",
        applescript_string(script),
        applescript_string(prompt)
    );
    let output = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(source)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| CliCommandError::AdministratorFailed(display(link)))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // AppleScript reports a cancelled authentication dialog as error -128,
    // and a failing shell script by its exit status.
    if stderr.contains("(-128)") {
        Err(CliCommandError::AdministratorCancelled)
    } else if stderr.contains(&format!("({CHANGED_EXIT})")) {
        Err(CliCommandError::Changed(display(link)))
    } else {
        Err(CliCommandError::AdministratorFailed(display(link)))
    }
}

#[cfg(not(target_os = "macos"))]
async fn run_as_administrator(_script: &str, _link: &Path) -> Result<(), CliCommandError> {
    Err(CliCommandError::Unsupported)
}

/// An administrator change to `/usr/local/bin`, checked first as the person so
/// a refusal never costs a password prompt.
async fn change_as_administrator(
    change: AdministratorChange,
    folder: &Path,
    command: &Path,
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
    let folder_created = change == AdministratorChange::Install && !folder.is_dir();
    let script = administrator_script(change, folder, command, observed.as_deref());
    run_as_administrator(&script, &link).await?;
    Ok((outcome, folder_created))
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

fn current_command() -> Result<PathBuf, UnavailableReason> {
    std::env::current_exe()
        .map_err(|_| UnavailableReason::NotBundled)
        .and_then(|executable| bundled_command(&executable))
}

async fn current_status(app: &AppHandle) -> Result<CliCommandStatus, String> {
    let user = user_folder(app)?;
    let command = current_command();
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
    let command = current_command().map_err(|reason| unavailable_error(reason).to_string())?;
    let (outcome, folder_created) = match location {
        CliLocation::User => install_into(&user_folder(&app)?, &command),
        CliLocation::System => {
            change_as_administrator(
                AdministratorChange::Install,
                Path::new(SYSTEM_BIN),
                &command,
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
    let command = current_command().map_err(|reason| unavailable_error(reason).to_string())?;
    let outcome = match location {
        CliLocation::User => uninstall_from(&user_folder(&app)?, &command),
        CliLocation::System => change_as_administrator(
            AdministratorChange::Uninstall,
            Path::new(SYSTEM_BIN),
            &command,
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

    /// A temporary home with an installed app at `Applications/<app>` whose
    /// bundle carries the command.
    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                root: tempfile::tempdir().unwrap(),
            }
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
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
                .root
                .path()
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
        let build = fixture.root.path().join("src/tidebreak/target/release");
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
        let system = fixture.root.path().join("usr/local/bin");
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
        assert_eq!(resolved, Some(display(&fixture.link())));

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

    #[test]
    fn a_build_without_the_sidecar_or_a_temporary_copy_cannot_install() {
        let fixture = Fixture::new();
        let bare = fixture.root.path().join("Bare.app/Contents/MacOS");
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
        for temporary in [
            "/Volumes/Tidebreak/Tidebreak.app/Contents/MacOS/tidebreak",
            "/private/var/folders/xy/T/AppTranslocation/1234/d/Tidebreak.app/Contents/MacOS/tidebreak",
        ] {
            assert!(is_temporary_location(Path::new(temporary)), "{temporary}");
        }
        assert!(!is_temporary_location(Path::new(
            "/Applications/Tidebreak.app/Contents/MacOS/tidebreak"
        )));
    }

    #[test]
    fn administrator_scripts_change_only_the_link_that_was_inspected() {
        let folder = Path::new("/usr/local/bin");
        let command = Path::new("/Applications/Tide break's.app/Contents/MacOS/tidebreak");

        let fresh = administrator_script(AdministratorChange::Install, folder, command, None);
        assert_eq!(
            fresh,
            "/bin/mkdir -p '/usr/local/bin' && if [ -e '/usr/local/bin/tidebreak' ] || [ -L '/usr/local/bin/tidebreak' ]; then exit 3; fi && /bin/ln -sfh '/Applications/Tide break'\\''s.app/Contents/MacOS/tidebreak' '/usr/local/bin/tidebreak'"
        );

        let old = Path::new("/Applications/Old.app/Contents/MacOS/tidebreak");
        let repair = administrator_script(AdministratorChange::Install, folder, command, Some(old));
        assert!(repair.contains(
            "[ \"$(/usr/bin/readlink '/usr/local/bin/tidebreak')\" != '/Applications/Old.app/Contents/MacOS/tidebreak' ]"
        ));
        assert!(repair.ends_with("/bin/ln -sfh '/Applications/Tide break'\\''s.app/Contents/MacOS/tidebreak' '/usr/local/bin/tidebreak'"));

        let remove =
            administrator_script(AdministratorChange::Uninstall, folder, command, Some(old));
        assert!(remove.starts_with("if [ ! -L '/usr/local/bin/tidebreak' ]"));
        assert!(remove.ends_with("&& /bin/rm -f '/usr/local/bin/tidebreak'"));

        assert_eq!(
            applescript_string(r#"say "hi" \ bye"#),
            r#""say \"hi\" \\ bye""#
        );
    }

    #[test]
    fn a_foreign_file_is_refused_before_any_administrator_prompt() {
        let fixture = Fixture::new();
        let command = fixture.app("Tidebreak.app");
        std::fs::create_dir_all(fixture.bin()).unwrap();
        std::fs::write(fixture.link(), b"someone else's tidebreak").unwrap();

        let refused = tauri::async_runtime::block_on(change_as_administrator(
            AdministratorChange::Install,
            &fixture.bin(),
            &command,
        ));

        // A prompt would have failed differently off macOS, or asked for a
        // password on it; the refusal comes first.
        assert_eq!(
            refused,
            Err(CliCommandError::Foreign(display(&fixture.link())))
        );
    }

    #[test]
    fn install_is_reachable_only_from_the_main_window() {
        assert!(require_main_webview("main").is_ok());
        for label in ["browser", "main-child", "", "Main"] {
            assert!(require_main_webview(label).is_err(), "{label}");
        }
    }
}

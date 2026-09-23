//! Background update checks and renderer-facing update state.
//!
//! Release builds check the signed feed after a short startup delay and then
//! periodically. By default a published update downloads and is
//! signature-verified in the background, then waits on disk in the app's
//! cache directory (`update_staging`), so memory does not hold the archive
//! while it waits. The app bundle is not replaced until an explicit user
//! restart so the running host and its sidecar always stay on the same version.
//!
//! Automatic downloads are a setting (`update_preferences`), on by default,
//! and an organization can turn them off with the `DownloadUpdatesAutomatically`
//! managed policy. With them off, checks still run and report an update as
//! available, and the download starts only when you choose Download.
//!
//! The feed only ever advertises the latest release, so a staged download can
//! go stale the moment a newer version ships. Every periodic check therefore
//! re-resolves the feed even while an update is staged and replaces the staged
//! artifact when the feed has moved on, and the restart path re-resolves once
//! more at click time so the app never installs an older artifact than the
//! newest published release it can reach.
//!
//! Installation is always an explicit user action. Native quiescence cannot
//! prove that renderer-only drafts, dialogs, or editor state have been saved,
//! so an unfocused window is not sufficient consent to replace and restart the
//! application.
//!
//! The restart itself does not interrupt session work. Before the bundle is
//! replaced the embedded server parks every code session at a turn boundary
//! (the idle-park path of decision 0064) and hands back chat turn leases, so
//! the relaunched process resumes sessions instead of fencing orphaned
//! engine children. A code turn still running at the quiesce deadline fails
//! the restart with a retryable message rather than being interrupted.

use std::future::Future;
#[cfg(any(test, target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::host_access::HostAccess;
use crate::update_preferences;
use crate::update_staging::{self, StagedArchive};

const UPDATE_STATE_EVENT: &str = "desktop-update-state";
/// Raised to the renderer when the native "Check for Updates…" menu item is
/// chosen; the UI opens a transient update card and runs an explicit check
/// without moving the reader away from the current screen.
pub(crate) const UPDATE_CHECK_REQUESTED_EVENT: &str = "desktop-update-check-requested";
const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(15);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const UPDATE_CHECK_ERROR: &str = "Could not check for updates. Try again later.";
pub(crate) const UPDATE_PREPARE_ERROR: &str = "Could not prepare the update. Try again later.";
const UPDATE_INSTALL_ERROR: &str = "Could not install the update. Try again later.";
const UPDATE_WITHDRAWN_ERROR: &str =
    "The downloaded update is no longer published. Tidebreak will keep checking.";
const UPDATE_PREFERENCE_MANAGED_ERROR: &str = "Your organization manages this setting.";
const UPDATE_PREFERENCE_SAVE_ERROR: &str = "Could not save the setting. Try again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DesktopUpdateStatus {
    Idle,
    Checking,
    /// A newer release is published and not downloaded yet, because automatic
    /// downloads are off.
    Available,
    Downloading,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUpdateState {
    status: DesktopUpdateStatus,
    version: Option<String>,
    error: Option<String>,
    enabled: bool,
}

impl DesktopUpdateState {
    fn idle() -> Self {
        Self {
            status: DesktopUpdateStatus::Idle,
            version: None,
            error: None,
            enabled: updates_enabled(),
        }
    }

    fn failed(message: &'static str) -> Self {
        Self {
            error: Some(message.to_owned()),
            ..Self::idle()
        }
    }

    fn available(version: String) -> Self {
        Self {
            status: DesktopUpdateStatus::Available,
            version: Some(version),
            ..Self::idle()
        }
    }
}

impl Default for DesktopUpdateState {
    fn default() -> Self {
        Self::idle()
    }
}

/// Whether Tidebreak downloads a published update without asking, and whether
/// that is yours to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUpdatePreferences {
    automatic_downloads: bool,
    /// Your organization's managed policy sets `automatic_downloads`, so
    /// Settings shows it as managed and refuses changes.
    managed: bool,
}

impl DesktopUpdatePreferences {
    /// An organization's policy outranks your own choice.
    fn resolve(policy: Option<bool>, chosen: impl FnOnce() -> bool) -> Self {
        match policy {
            Some(automatic_downloads) => Self {
                automatic_downloads,
                managed: true,
            },
            None => Self {
                automatic_downloads: chosen(),
                managed: false,
            },
        }
    }
}

/// Why a check runs, which decides whether it may start a download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckIntent {
    /// The periodic check, or Check for updates.
    Check,
    /// You chose Download.
    Download,
}

/// Whether a check that finds a release it has not staged downloads it now,
/// or only reports it as available. Download is always yours to ask for;
/// the setting and the policy only decide whether Tidebreak asks first.
fn downloads_now(intent: CheckIntent, preferences: DesktopUpdatePreferences) -> bool {
    intent == CheckIntent::Download || preferences.automatic_downloads
}

/// The update waiting for a restart: the feed entry that describes it and the
/// verified archive on disk. It holds the archive's path, never its bytes.
struct StagedUpdate {
    update: Update,
    archive: StagedArchive,
}

/// A staged update read back into memory for the install. It lives only from
/// the read to the bundle replacement.
struct LoadedUpdate {
    update: Update,
    bytes: Vec<u8>,
    archive: StagedArchive,
}

impl LoadedUpdate {
    /// Let go of the bytes and keep the file staged, for a restart that did
    /// not happen.
    fn unload(self) -> StagedUpdate {
        StagedUpdate {
            update: self.update,
            archive: self.archive,
        }
    }
}

#[derive(Default)]
pub(crate) struct UpdateManager {
    state: Mutex<DesktopUpdateState>,
    staged: Mutex<Option<StagedUpdate>>,
    busy: AtomicBool,
    /// You chose Download while another check held `busy`. That check runs
    /// the download before it lets go.
    download_requested: AtomicBool,
    /// Whether this process has created the staging folder and deleted what
    /// earlier runs left in it.
    staging_prepared: Mutex<bool>,
}

pub(crate) const fn updates_enabled() -> bool {
    cfg!(all(not(debug_assertions), target_os = "macos"))
        || cfg!(all(
            not(debug_assertions),
            any(target_os = "windows", target_os = "linux")
        ))
}

/// What a fresh look at the feed means for an already-staged artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StagedAction {
    /// The staged artifact is still the newest published release; keep it.
    Keep,
    /// The feed advertises something newer; download and stage that instead.
    Replace,
    /// The feed no longer advertises anything newer than the running app
    /// (the staged release was withdrawn); never install it.
    Discard,
}

fn reconcile_staged(feed_version: Option<&str>, staged_version: &str) -> StagedAction {
    match feed_version {
        None => StagedAction::Discard,
        Some(feed) if feed == staged_version => StagedAction::Keep,
        Some(feed) => match (
            semver::Version::parse(feed),
            semver::Version::parse(staged_version),
        ) {
            (Ok(feed), Ok(staged)) if feed > staged => StagedAction::Replace,
            // An older feed version is an intentional rollback/withdrawal as
            // far as this client can prove. Malformed unequal versions also
            // fail closed rather than preserving an unadvertised artifact.
            _ => StagedAction::Discard,
        },
    }
}

fn current_update_state(app: &AppHandle) -> DesktopUpdateState {
    app.state::<UpdateManager>()
        .state
        .lock()
        .expect("update state mutex poisoned")
        .clone()
}

fn set_update_state(app: &AppHandle, next: DesktopUpdateState) {
    *app.state::<UpdateManager>()
        .state
        .lock()
        .expect("update state mutex poisoned") = next.clone();
    if let Err(error) = app.emit(UPDATE_STATE_EVENT, next) {
        eprintln!("tidebreak-desktop: could not emit update state: {error}");
    }
}

/// Your automatic-download choice, unless your organization's policy sets it.
/// Both are read on every call, so a policy pushed while the app runs applies
/// at the next check.
fn update_preferences(app: &AppHandle) -> DesktopUpdatePreferences {
    let policy = tidebreak_server::desktop_update_downloads_policy(&app.config().identifier);
    DesktopUpdatePreferences::resolve(policy, || match crate::data_dir(app) {
        Ok(data_dir) => update_preferences::automatic_downloads(&data_dir),
        Err(error) => {
            eprintln!("tidebreak-desktop: could not read update preferences: {error}");
            true
        }
    })
}

fn staged_version(app: &AppHandle) -> Option<String> {
    app.state::<UpdateManager>()
        .staged
        .lock()
        .expect("staged update mutex poisoned")
        .as_ref()
        .map(|staged| staged.update.version.clone())
}

/// Put `next` in the staged slot, and delete the file of the update it
/// replaces: superseded by a newer release, or withdrawn.
fn store_staged(app: &AppHandle, next: Option<StagedUpdate>) {
    let previous = std::mem::replace(
        &mut *app
            .state::<UpdateManager>()
            .staged
            .lock()
            .expect("staged update mutex poisoned"),
        next,
    );
    if let Some(previous) = previous {
        discard_archive(&previous.archive);
    }
}

/// Delete a staged file that nothing will install.
fn discard_archive(archive: &StagedArchive) {
    if let Err(error) = archive.remove() {
        eprintln!(
            "tidebreak-desktop: could not delete staged update {}: {error}",
            archive.path().display()
        );
    }
}

/// The folder staged updates live in. The first call in a process creates it
/// and deletes whatever earlier runs left there, before anything is staged in
/// this one; later calls only return it. Blocking: call it off the async
/// runtime.
fn staging_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("app cache dir: {error}"))?
        .join(update_staging::STAGING_DIRECTORY);
    let manager = app.state::<UpdateManager>();
    let mut prepared = manager
        .staging_prepared
        .lock()
        .expect("update staging mutex poisoned");
    if !*prepared {
        let removed = update_staging::prepare_directory(&directory)
            .map_err(|error| format!("prepare {}: {error}", directory.display()))?;
        if removed > 0 {
            eprintln!("tidebreak-desktop: deleted {removed} stale staged update file(s)");
        }
        *prepared = true;
    }
    Ok(directory)
}

/// Download `update`, verify its signature, and write it to the staging
/// folder. The archive is in memory only from the download to the write.
async fn download_to_disk(app: &AppHandle, update: Update) -> Result<StagedUpdate, String> {
    // `download` checks the archive against the feed's signature before it
    // returns, so nothing unverified reaches the disk.
    let download = update.download(|_chunk, _total| {}, || {});
    let bytes = download
        .await
        .map_err(|error| format!("download failed: {error}"))?;
    let app = app.clone();
    let archive = tauri::async_runtime::spawn_blocking(move || {
        let directory = staging_directory(&app)?;
        StagedArchive::write(&directory, &bytes)
            .map_err(|error| format!("could not write the staged update: {error}"))
    })
    .await
    .map_err(|error| format!("staging task failed: {error}"))??;
    Ok(StagedUpdate { update, archive })
}

/// Read a staged update back into memory for the install. A file that is gone
/// or no longer matches what was verified is deleted rather than kept.
async fn load_staged(staged: StagedUpdate) -> Result<LoadedUpdate, String> {
    let StagedUpdate { update, archive } = staged;
    let (archive, bytes) = tauri::async_runtime::spawn_blocking(move || {
        let bytes = archive.read();
        (archive, bytes)
    })
    .await
    .map_err(|error| format!("read task failed: {error}"))?;
    match bytes {
        Ok(bytes) => Ok(LoadedUpdate {
            update,
            bytes,
            archive,
        }),
        Err(error) => {
            discard_archive(&archive);
            Err(error.to_string())
        }
    }
}

/// How a download shows itself, and what a failed one leaves on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DownloadMode {
    /// A check found the release: show the download and report a failure.
    Shown,
    /// You chose Download: show it, and on failure keep offering the release
    /// so you can try again.
    Requested,
    /// A newer release replaces the staged one: keep showing the staged
    /// update, and keep it if the download fails.
    Silent,
}

async fn download_and_stage(app: &AppHandle, update: Update, mode: DownloadMode) {
    let version = update.version.clone();
    if mode != DownloadMode::Silent {
        set_update_state(
            app,
            DesktopUpdateState {
                status: DesktopUpdateStatus::Downloading,
                version: Some(version.clone()),
                ..DesktopUpdateState::idle()
            },
        );
    }

    match download_to_disk(app, update).await {
        Ok(staged) => {
            store_staged(app, Some(staged));
            set_update_state(
                app,
                DesktopUpdateState {
                    status: DesktopUpdateStatus::Ready,
                    version: Some(version),
                    ..DesktopUpdateState::idle()
                },
            );
        }
        Err(error) => {
            eprintln!("tidebreak-desktop: could not stage update {version}: {error}");
            match mode {
                DownloadMode::Shown => {
                    set_update_state(app, DesktopUpdateState::failed(UPDATE_PREPARE_ERROR));
                }
                DownloadMode::Requested => set_update_state(
                    app,
                    DesktopUpdateState {
                        error: Some(UPDATE_PREPARE_ERROR.to_owned()),
                        ..DesktopUpdateState::available(version)
                    },
                ),
                // A silent refresh keeps the previously staged (older but
                // valid) update on download failure instead of surfacing an
                // error over a still-installable state.
                DownloadMode::Silent => {}
            }
        }
    }
}

/// What a failed check tells you: that the check failed, why, and what to do
/// next. It names the kind of failure and never repeats the updater's own
/// error text, which can carry URLs and internals.
fn check_failure_message(error: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;

    let reason = match error {
        Error::Reqwest(error) if error.is_timeout() => {
            "The update server did not answer in time. Try again later."
        }
        Error::Reqwest(error) if error.is_connect() => {
            "Tidebreak could not reach the update server. Check your internet connection and try again."
        }
        Error::Reqwest(_) => "The connection to the update server failed. Try again later.",
        Error::ReleaseNotFound => "The update server returned an error. Try again later.",
        Error::Serialization(_) | Error::Semver(_) | Error::TargetsNotFound(_) => {
            "The update server sent a release Tidebreak could not read. Try again later."
        }
        _ => "Try again later.",
    };
    format!("Could not check for updates. {reason}")
}

async fn perform_update_check(app: &AppHandle, intent: CheckIntent) {
    let staged = staged_version(app);
    let current = current_update_state(app);
    let offered = if current.status == DesktopUpdateStatus::Available {
        current.version
    } else {
        None
    };
    // A download you asked for shows itself at once. Otherwise a check with an
    // update already staged or offered runs silently: the visible state stays
    // put (the banner keeps showing that version) unless the feed has actually
    // moved on.
    let requested = intent == CheckIntent::Download && staged.is_none();
    let silent = !requested && (staged.is_some() || offered.is_some());
    // A check that cannot read the feed says why, unless it runs silently. A
    // download you asked for keeps offering the release so you can try again.
    let report_failure = |message: String| {
        if silent {
            return;
        }
        let failed = match offered.clone().filter(|_| requested) {
            Some(version) => DesktopUpdateState::available(version),
            None => DesktopUpdateState::idle(),
        };
        set_update_state(
            app,
            DesktopUpdateState {
                error: Some(message),
                ..failed
            },
        );
    };

    if requested {
        set_update_state(
            app,
            DesktopUpdateState {
                status: DesktopUpdateStatus::Downloading,
                version: offered.clone(),
                ..DesktopUpdateState::idle()
            },
        );
    } else if !silent {
        set_update_state(
            app,
            DesktopUpdateState {
                status: DesktopUpdateStatus::Checking,
                ..DesktopUpdateState::idle()
            },
        );
    }

    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            eprintln!("tidebreak-desktop: could not initialize updater: {error}");
            report_failure(UPDATE_CHECK_ERROR.to_owned());
            return;
        }
    };

    let update = match updater.check().await {
        Ok(update) => update,
        // The feed lists only the platforms a release ships. A platform with
        // no entry is up to date as far as it can be, not a failed check.
        Err(tauri_plugin_updater::Error::TargetNotFound(_)) => None,
        Err(error) => {
            eprintln!("tidebreak-desktop: update check failed: {error}");
            report_failure(check_failure_message(&error));
            return;
        }
    };

    let preferences = update_preferences(app);
    match staged {
        None => match update {
            Some(update) if downloads_now(intent, preferences) => {
                let mode = match intent {
                    CheckIntent::Download => DownloadMode::Requested,
                    CheckIntent::Check => DownloadMode::Shown,
                };
                download_and_stage(app, update, mode).await;
            }
            Some(update) => set_update_state(app, DesktopUpdateState::available(update.version)),
            None => set_update_state(app, DesktopUpdateState::idle()),
        },
        Some(staged) => {
            match reconcile_staged(update.as_ref().map(|u| u.version.as_str()), &staged) {
                StagedAction::Keep => {}
                // With automatic downloads off, the older staged update stays
                // installable. Restarting to update fetches the newest release
                // then, because that restart is you asking for the update.
                StagedAction::Replace if !downloads_now(intent, preferences) => {}
                StagedAction::Replace => {
                    let update = update.expect("replace implies an advertised update");
                    download_and_stage(app, update, DownloadMode::Silent).await;
                }
                StagedAction::Discard => {
                    store_staged(app, None);
                    set_update_state(app, DesktopUpdateState::idle());
                }
            }
        }
    }
}

async fn run_update_check(app: AppHandle, intent: CheckIntent) -> DesktopUpdateState {
    if !updates_enabled() {
        return current_update_state(&app);
    }

    let manager = app.state::<UpdateManager>();
    if intent == CheckIntent::Download {
        manager.download_requested.store(true, Ordering::SeqCst);
    }
    loop {
        if manager.busy.swap(true, Ordering::SeqCst) {
            // Whoever holds `busy` runs a requested download before it lets
            // go, so the request is not lost.
            return current_update_state(&app);
        }
        let intent = if manager.download_requested.swap(false, Ordering::SeqCst) {
            CheckIntent::Download
        } else {
            CheckIntent::Check
        };
        perform_update_check(&app, intent).await;
        manager.busy.store(false, Ordering::SeqCst);
        if !manager.download_requested.load(Ordering::SeqCst) {
            return current_update_state(&app);
        }
    }
}

pub(crate) fn spawn_update_loop(app: AppHandle) {
    if !updates_enabled() {
        return;
    }
    tauri::async_runtime::spawn({
        let app = app.clone();
        async move {
            // Clear out what earlier runs staged before this run stages
            // anything, whether or not a check ever downloads.
            let sweep = app.clone();
            let prepared = tauri::async_runtime::spawn_blocking(move || staging_directory(&sweep))
                .await
                .map_err(|error| error.to_string())
                .and_then(|prepared| prepared);
            if let Err(error) = prepared {
                eprintln!("tidebreak-desktop: could not prepare the update folder: {error}");
            }
            tokio::time::sleep(UPDATE_CHECK_STARTUP_DELAY).await;
            loop {
                run_update_check(app.clone(), CheckIntent::Check).await;
                tokio::time::sleep(UPDATE_CHECK_INTERVAL).await;
            }
        }
    });
}

#[tauri::command]
pub(crate) fn desktop_update_state(app: AppHandle) -> DesktopUpdateState {
    current_update_state(&app)
}

#[tauri::command]
pub(crate) async fn check_for_update(app: AppHandle) -> DesktopUpdateState {
    run_update_check(app, CheckIntent::Check).await
}

/// Download the published update now, whatever the automatic-download
/// setting says.
#[tauri::command]
pub(crate) async fn download_update(app: AppHandle) -> DesktopUpdateState {
    run_update_check(app, CheckIntent::Download).await
}

#[tauri::command]
pub(crate) async fn desktop_update_preferences(app: AppHandle) -> DesktopUpdatePreferences {
    update_preferences(&app)
}

#[tauri::command]
pub(crate) async fn set_automatic_update_downloads(
    app: AppHandle,
    enabled: bool,
) -> Result<DesktopUpdatePreferences, String> {
    if update_preferences(&app).managed {
        return Err(UPDATE_PREFERENCE_MANAGED_ERROR.to_owned());
    }
    let data_dir = crate::data_dir(&app)?;
    update_preferences::set_automatic_downloads(&data_dir, enabled).map_err(|error| {
        eprintln!("tidebreak-desktop: could not save update preferences: {error}");
        UPDATE_PREFERENCE_SAVE_ERROR.to_owned()
    })?;
    let preferences = update_preferences(&app);
    // Turning automatic downloads on while an update waits to be downloaded
    // starts that download now, not at the next check.
    if preferences.automatic_downloads
        && current_update_state(&app).status == DesktopUpdateStatus::Available
    {
        tauri::async_runtime::spawn(run_update_check(app.clone(), CheckIntent::Check));
    }
    Ok(preferences)
}

fn can_restart(state: &DesktopUpdateState, has_staged_update: bool) -> bool {
    state.enabled && state.status == DesktopUpdateStatus::Ready && has_staged_update
}

/// Re-resolves the feed at install time so a restart that raced a release
/// installs the newest published artifact, not the one that happened to be
/// staged when the button appeared. Falls back to the staged artifact when
/// the feed is unreachable — it is still an upgrade — but refuses to install
/// a release the feed has withdrawn.
async fn resolve_latest_for_install(
    app: &AppHandle,
    staged: StagedUpdate,
) -> Result<StagedUpdate, InstallResolutionError> {
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            eprintln!("tidebreak-desktop: could not initialize updater: {error}");
            return Ok(staged);
        }
    };

    let update = match updater.check().await {
        Ok(update) => update,
        // See the background check: a platform the feed omits has no update.
        Err(tauri_plugin_updater::Error::TargetNotFound(_)) => None,
        Err(error) => {
            eprintln!("tidebreak-desktop: install-time update check failed: {error}");
            return Ok(staged);
        }
    };

    match reconcile_staged(
        update.as_ref().map(|u| u.version.as_str()),
        &staged.update.version,
    ) {
        StagedAction::Keep => Ok(staged),
        StagedAction::Replace => {
            let update = update.expect("replace implies an advertised update");
            match download_to_disk(app, update).await {
                Ok(newer) => {
                    discard_archive(&staged.archive);
                    Ok(newer)
                }
                Err(error) => {
                    eprintln!("tidebreak-desktop: install-time update download failed: {error}");
                    Err(InstallResolutionError {
                        staged: Some(staged),
                        message: UPDATE_PREPARE_ERROR,
                    })
                }
            }
        }
        StagedAction::Discard => {
            discard_archive(&staged.archive);
            Err(InstallResolutionError {
                staged: None,
                message: UPDATE_WITHDRAWN_ERROR,
            })
        }
    }
}

struct InstallResolutionError {
    /// A still-published staged artifact that can be retried later. Withdrawn
    /// artifacts are deliberately omitted so no subsequent action can install
    /// them without downloading them from a newly authoritative feed.
    staged: Option<StagedUpdate>,
    message: &'static str,
}

fn retryable_update_state(version: String, message: impl Into<String>) -> DesktopUpdateState {
    DesktopUpdateState {
        status: DesktopUpdateStatus::Ready,
        version: Some(version),
        error: Some(message.into()),
        enabled: updates_enabled(),
    }
}

struct FailedInstall<E> {
    install_error: E,
    resume_error: Option<String>,
}

/// Run the synchronous bundle replacement only after the quiesce closure has
/// brought the process to a safe point — session work parked at turn
/// boundaries, then the broker's admission barrier drained. The closures keep
/// the ordering contract directly testable without constructing a packaged
/// Tauri updater in unit tests. The quiesce closure owns unwinding its own
/// partial progress: an error from it must leave everything resumed.
async fn install_behind_broker_barrier<E, Q, QF, I, R, RF, S, SF>(
    quiesce: Q,
    install: I,
    resume: R,
    shutdown: S,
) -> Result<Result<(), FailedInstall<E>>, String>
where
    Q: FnOnce() -> QF,
    QF: Future<Output = Result<(), String>>,
    I: FnOnce() -> Result<(), E>,
    R: FnOnce() -> RF,
    RF: Future<Output = Result<(), String>>,
    S: FnOnce() -> SF,
    SF: Future<Output = ()>,
{
    quiesce().await?;
    match install() {
        Ok(()) => {
            shutdown().await;
            Ok(Ok(()))
        }
        Err(install_error) => {
            let resume_error = resume().await.err();
            Ok(Err(FailedInstall {
                install_error,
                resume_error,
            }))
        }
    }
}

#[tauri::command]
pub(crate) async fn restart_for_update(app: AppHandle) -> Result<(), String> {
    take_staged_and_restart(app).await
}

/// Take the staged update, converge it on the newest published release, and
/// restart into it. This is reached only from the explicit restart command. On
/// success this never returns.
async fn take_staged_and_restart(app: AppHandle) -> Result<(), String> {
    if app
        .state::<UpdateManager>()
        .busy
        .swap(true, Ordering::AcqRel)
    {
        return Err("An update check is already in progress".to_owned());
    }

    let staged = {
        let manager = app.state::<UpdateManager>();
        let state = manager
            .state
            .lock()
            .expect("update state mutex poisoned")
            .clone();
        let mut staged = manager.staged.lock().expect("staged update mutex poisoned");
        if !can_restart(&state, staged.is_some()) {
            manager.busy.store(false, Ordering::Release);
            return Err("no update is ready to install".to_owned());
        }
        staged
            .take()
            .expect("ready update must have a staged archive")
    };

    let staged = match resolve_latest_for_install(&app, staged).await {
        Ok(staged) => staged,
        Err(error) => {
            if let Some(staged) = error.staged {
                let version = staged.update.version.clone();
                store_staged(&app, Some(staged));
                set_update_state(&app, retryable_update_state(version, error.message));
            } else {
                store_staged(&app, None);
                set_update_state(&app, DesktopUpdateState::failed(error.message));
            }
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            return Err(error.message.to_owned());
        }
    };

    // Read the archive back before anything quiesces, so a file that went
    // missing or changed since it was verified fails here with nothing to
    // unwind. The next check downloads the release again.
    let staged = match load_staged(staged).await {
        Ok(staged) => staged,
        Err(error) => {
            eprintln!("tidebreak-desktop: could not read the staged update: {error}");
            set_update_state(&app, DesktopUpdateState::failed(UPDATE_PREPARE_ERROR));
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            return Err(UPDATE_PREPARE_ERROR.to_owned());
        }
    };

    let version = staged.update.version.clone();
    let host_access = app.state::<HostAccess>();
    // The quiesce brings session work to a safe point before the broker
    // drains — code sessions park at a turn boundary and chat leases are
    // handed back (`HostAccess::quiesce_for_update`) — so the relaunch
    // resumes work instead of fencing orphans. A refusal, such as a code
    // turn still running at the deadline, arrives as a sentence the panel
    // shows as-is, with the partial quiesce already unwound.
    let install_result = install_behind_broker_barrier(
        || host_access.quiesce_for_update(),
        || staged.update.install(&staged.bytes),
        || host_access.resume_after_failed_update(),
        || host_access.shutdown(),
    )
    .await;

    match install_result {
        Err(error) => {
            eprintln!("tidebreak-desktop: could not quiesce for update: {error}");
            // Nothing was installed: the archive stays staged on disk for the
            // retry, and its bytes leave memory.
            store_staged(&app, Some(staged.unload()));
            set_update_state(&app, retryable_update_state(version, error.clone()));
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            Err(error)
        }
        Ok(Err(failure)) => {
            eprintln!(
                "tidebreak-desktop: update installation failed: {}",
                failure.install_error
            );
            if let Some(error) = failure.resume_error {
                eprintln!(
                    "tidebreak-desktop: old host broker could not resume after update failure: {error}"
                );
            }
            // An archive that failed to install is not kept for another try.
            // The next check downloads the release again.
            discard_archive(&staged.archive);
            set_update_state(&app, DesktopUpdateState::failed(UPDATE_INSTALL_ERROR));
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            Err(UPDATE_INSTALL_ERROR.to_owned())
        }
        Ok(Ok(())) => {
            // The new bundle is in place, so the archive has done its job.
            discard_archive(&staged.archive);
            relaunch_after_update(&app)
        }
    }
}

/// Relaunch into the just-installed bundle.
///
/// Tauri's [`AppHandle::restart`] spawns `Contents/MacOS/<exe>` directly. On
/// macOS that skips Launch Services, so the new process can draw a window
/// without appearing in the Command-Tab switcher or becoming the front app.
/// Packaged macOS builds therefore ask `open` to launch the `.app` after this
/// process has exited. Unpackaged binaries and other platforms keep Tauri's
/// restart.
fn relaunch_after_update(app: &AppHandle) -> ! {
    #[cfg(target_os = "macos")]
    if let Some(bundle) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(app_bundle_from_binary)
    {
        if schedule_bundle_relaunch(&bundle).is_ok() {
            app.exit(0);
            loop {
                std::thread::sleep(Duration::MAX);
            }
        }
    }
    app.restart();
}

/// Walk `…/Name.app/Contents/MacOS/<exe>` up to `Name.app`.
#[cfg(any(test, target_os = "macos"))]
fn app_bundle_from_binary(binary: &Path) -> Option<PathBuf> {
    let macos = binary.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    bundle
        .file_name()?
        .to_str()?
        .ends_with(".app")
        .then(|| bundle.to_path_buf())
}

/// Start a detached `open` of the bundle after this process exits.
///
/// Launch Services only registers the app if `open` runs against the `.app`,
/// and the single-instance plugin refuses a second Tidebreak until this
/// process is gone. The helper therefore waits on the captured parent pid,
/// then asks `open` to launch the bundle. Its own process group keeps a
/// SIGHUP from this exit from cancelling it.
#[cfg(target_os = "macos")]
fn schedule_bundle_relaunch(bundle: &Path) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    Command::new("/bin/sh")
        .args([
            "-c",
            r#"parent=$PPID
while /bin/kill -0 "$parent" 2>/dev/null; do
  /bin/sleep 0.1
done
exec /usr/bin/open "$1""#,
            "relaunch",
        ])
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[test]
    fn renderer_state_uses_stable_public_field_names() {
        let state = DesktopUpdateState {
            status: DesktopUpdateStatus::Ready,
            version: Some("1.2.3".to_owned()),
            error: None,
            enabled: true,
        };

        assert_eq!(
            serde_json::to_value(state).unwrap(),
            json!({
                "status": "ready",
                "version": "1.2.3",
                "error": null,
                "enabled": true,
            })
        );
        assert_eq!(
            serde_json::to_value(DesktopUpdateState::available("1.2.4".to_owned())).unwrap()
                ["status"],
            "available"
        );
        assert_eq!(
            serde_json::to_value(DesktopUpdatePreferences::resolve(Some(false), || true)).unwrap(),
            json!({ "automaticDownloads": false, "managed": true })
        );
    }

    /// With automatic downloads off, by your choice or your organization's
    /// policy, a check that finds a new release only reports it. The
    /// organization's policy outranks your own choice either way, and choosing
    /// Download always downloads.
    #[test]
    fn the_setting_and_the_policy_each_stop_an_automatic_download() {
        let default = DesktopUpdatePreferences::resolve(None, || true);
        assert!(!default.managed);
        assert!(downloads_now(CheckIntent::Check, default));

        let turned_off = DesktopUpdatePreferences::resolve(None, || false);
        assert!(!turned_off.managed);
        assert!(!downloads_now(CheckIntent::Check, turned_off));
        assert!(downloads_now(CheckIntent::Download, turned_off));

        let policy_off = DesktopUpdatePreferences::resolve(Some(false), || true);
        assert!(policy_off.managed);
        assert!(!downloads_now(CheckIntent::Check, policy_off));
        assert!(downloads_now(CheckIntent::Download, policy_off));

        let policy_on = DesktopUpdatePreferences::resolve(Some(true), || false);
        assert!(policy_on.managed);
        assert!(downloads_now(CheckIntent::Check, policy_on));
    }

    #[test]
    fn a_failed_check_names_its_reason_without_the_updaters_own_text() {
        assert_eq!(
            check_failure_message(&tauri_plugin_updater::Error::ReleaseNotFound),
            "Could not check for updates. The update server returned an error. Try again later."
        );
        let message = check_failure_message(&tauri_plugin_updater::Error::Network(
            "GET https://feed.example/latest.json failed".to_owned(),
        ));
        assert!(message.starts_with("Could not check for updates. "));
        assert!(!message.contains("feed.example"));
    }

    #[test]
    fn app_bundle_is_the_enclosing_dot_app() {
        let binary = Path::new("/Applications/Tidebreak.app/Contents/MacOS/tidebreak-desktop");
        assert_eq!(
            app_bundle_from_binary(binary).as_deref(),
            Some(Path::new("/Applications/Tidebreak.app"))
        );
        assert_eq!(
            app_bundle_from_binary(Path::new(
                "/Applications/Tidebreak Staging.app/Contents/MacOS/tidebreak-desktop"
            ))
            .as_deref(),
            Some(Path::new("/Applications/Tidebreak Staging.app"))
        );
    }

    #[test]
    fn unpackaged_binaries_have_no_app_bundle() {
        assert!(app_bundle_from_binary(Path::new("/tmp/tidebreak-desktop")).is_none());
        assert!(
            app_bundle_from_binary(Path::new("/Applications/Tidebreak.app/Contents/MacOS"))
                .is_none()
        );
    }

    #[test]
    fn relaunch_requires_an_enabled_staged_update() {
        let mut state = DesktopUpdateState {
            status: DesktopUpdateStatus::Ready,
            version: Some("1.2.3".to_owned()),
            error: None,
            enabled: true,
        };
        assert!(can_restart(&state, true));

        assert!(!can_restart(&state, false));

        state.status = DesktopUpdateStatus::Downloading;
        assert!(!can_restart(&state, true));

        state.status = DesktopUpdateStatus::Available;
        assert!(!can_restart(&state, true));

        state.status = DesktopUpdateStatus::Ready;
        state.enabled = false;
        assert!(!can_restart(&state, true));
    }

    #[test]
    fn a_newer_release_replaces_the_staged_artifact() {
        // The double-release window: v0.4.1 was staged, v0.4.2 shipped before
        // the user acted. The stale artifact must be replaced, never installed.
        assert_eq!(
            reconcile_staged(Some("0.4.2"), "0.4.1"),
            StagedAction::Replace
        );
        assert_eq!(
            reconcile_staged(Some("1.0.0"), "0.9.9"),
            StagedAction::Replace
        );
    }

    #[test]
    fn the_staged_artifact_is_kept_when_still_newest() {
        assert_eq!(reconcile_staged(Some("0.4.1"), "0.4.1"), StagedAction::Keep);
    }

    #[test]
    fn a_withdrawn_or_rolled_back_release_is_discarded_not_installed() {
        assert_eq!(reconcile_staged(None, "0.4.1"), StagedAction::Discard);
        assert_eq!(
            reconcile_staged(Some("0.4.0"), "0.4.1"),
            StagedAction::Discard
        );
        assert_eq!(
            reconcile_staged(Some("0.4.2-rc.1"), "0.4.2"),
            StagedAction::Discard
        );
    }

    #[test]
    fn unparseable_versions_fail_closed_unless_they_match_exactly() {
        assert_eq!(
            reconcile_staged(Some("build-124"), "build-123"),
            StagedAction::Discard
        );
        assert_eq!(
            reconcile_staged(Some("build-123"), "build-123"),
            StagedAction::Keep
        );
    }

    #[test]
    fn a_refused_restart_stays_retryable() {
        let refusal = "A code turn is still running. Try again when it finishes.";
        let mut state = retryable_update_state("1.2.3".to_owned(), refusal);

        assert_eq!(state.enabled, updates_enabled());
        state.enabled = true;
        assert!(can_restart(&state, true));
        assert_eq!(state.version.as_deref(), Some("1.2.3"));
        assert_eq!(state.error.as_deref(), Some(refusal));
    }

    #[tokio::test]
    async fn broker_barrier_drains_before_install_and_resumes_only_on_failure() {
        let events = std::sync::Arc::new(Mutex::new(Vec::new()));
        let quiesce_events = events.clone();
        let install_events = events.clone();
        let resume_events = events.clone();
        let shutdown_events = events.clone();

        let result = install_behind_broker_barrier(
            move || async move {
                let mut events = quiesce_events.lock().unwrap();
                events.push("in-flight finished");
                events.push("queued finished");
                events.push("admission closed");
                Ok(())
            },
            move || {
                install_events.lock().unwrap().push("install");
                Err("injected install failure")
            },
            move || async move {
                resume_events.lock().unwrap().push("resume pinned broker");
                Ok(())
            },
            move || async move {
                shutdown_events.lock().unwrap().push("shutdown");
            },
        )
        .await
        .unwrap()
        .unwrap_err();

        assert_eq!(result.install_error, "injected install failure");
        assert!(result.resume_error.is_none());
        assert_eq!(
            *events.lock().unwrap(),
            [
                "in-flight finished",
                "queued finished",
                "admission closed",
                "install",
                "resume pinned broker",
            ]
        );
    }

    #[tokio::test]
    async fn successful_install_permanently_shuts_down_before_restart() {
        let events = std::sync::Arc::new(Mutex::new(Vec::new()));
        let quiesce_events = events.clone();
        let install_events = events.clone();
        let resume_events = events.clone();
        let shutdown_events = events.clone();

        let result = install_behind_broker_barrier(
            move || async move {
                quiesce_events.lock().unwrap().push("quiesce");
                Ok(())
            },
            move || {
                install_events.lock().unwrap().push("install");
                Ok::<(), &'static str>(())
            },
            move || async move {
                resume_events.lock().unwrap().push("resume");
                Ok(())
            },
            move || async move {
                shutdown_events.lock().unwrap().push("shutdown");
            },
        )
        .await
        .unwrap();

        assert!(result.is_ok());
        assert_eq!(*events.lock().unwrap(), ["quiesce", "install", "shutdown"]);
    }
}

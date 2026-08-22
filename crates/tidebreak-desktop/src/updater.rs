//! Background update checks and renderer-facing update state.
//!
//! Release builds check the signed feed after a short startup delay and then
//! periodically. Updates are downloaded and signature-verified in the
//! background, but the app bundle is not replaced until an explicit user
//! restart so the running host and its sidecar always stay on the same version.
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

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::host_access::HostAccess;

const UPDATE_STATE_EVENT: &str = "desktop-update-state";
/// Raised to the renderer when the native "Check for Updates…" menu item is
/// chosen; the UI opens the Updates settings panel and runs an explicit check
/// there so the outcome (up to date, or an update staged) is visible.
pub(crate) const UPDATE_CHECK_REQUESTED_EVENT: &str = "desktop-update-check-requested";
const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(15);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const UPDATE_CHECK_ERROR: &str = "Could not check for updates. Try again later.";
const UPDATE_PREPARE_ERROR: &str = "Could not prepare the update. Try again later.";
const UPDATE_INSTALL_ERROR: &str = "Could not install the update. Try again later.";
const UPDATE_WITHDRAWN_ERROR: &str =
    "The downloaded update is no longer published. Tidebreak will keep checking.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DesktopUpdateStatus {
    Idle,
    Checking,
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
}

impl Default for DesktopUpdateState {
    fn default() -> Self {
        Self::idle()
    }
}

struct StagedUpdate {
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct UpdateManager {
    state: Mutex<DesktopUpdateState>,
    staged: Mutex<Option<StagedUpdate>>,
    busy: AtomicBool,
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

fn staged_version(app: &AppHandle) -> Option<String> {
    app.state::<UpdateManager>()
        .staged
        .lock()
        .expect("staged update mutex poisoned")
        .as_ref()
        .map(|staged| staged.update.version.clone())
}

fn store_staged(app: &AppHandle, staged: Option<StagedUpdate>) {
    *app.state::<UpdateManager>()
        .staged
        .lock()
        .expect("staged update mutex poisoned") = staged;
}

async fn download_and_stage(app: &AppHandle, update: Update, silent: bool) -> bool {
    let version = Some(update.version.clone());
    if !silent {
        set_update_state(
            app,
            DesktopUpdateState {
                status: DesktopUpdateStatus::Downloading,
                version: version.clone(),
                ..DesktopUpdateState::idle()
            },
        );
    }

    match update.download(|_chunk, _total| {}, || {}).await {
        Ok(bytes) => {
            store_staged(app, Some(StagedUpdate { update, bytes }));
            set_update_state(
                app,
                DesktopUpdateState {
                    status: DesktopUpdateStatus::Ready,
                    version,
                    ..DesktopUpdateState::idle()
                },
            );
            true
        }
        Err(error) => {
            eprintln!("tidebreak-desktop: update download failed: {error}");
            // A silent refresh keeps the previously staged (older but valid)
            // update on download failure instead of surfacing an error over a
            // still-installable state.
            if !silent {
                set_update_state(app, DesktopUpdateState::failed(UPDATE_PREPARE_ERROR));
            }
            false
        }
    }
}

async fn perform_update_check(app: &AppHandle) {
    // When an update is already staged the periodic re-check runs silently:
    // the visible state stays `Ready` (the banner keeps showing the staged
    // version) unless the feed has actually moved on.
    let staged = staged_version(app);
    let silent = staged.is_some();

    if !silent {
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
            if !silent {
                set_update_state(app, DesktopUpdateState::failed(UPDATE_CHECK_ERROR));
            }
            return;
        }
    };

    let update = match updater.check().await {
        Ok(update) => update,
        Err(error) => {
            eprintln!("tidebreak-desktop: update check failed: {error}");
            if !silent {
                set_update_state(app, DesktopUpdateState::failed(UPDATE_CHECK_ERROR));
            }
            return;
        }
    };

    match staged {
        None => match update {
            Some(update) => {
                download_and_stage(app, update, false).await;
            }
            None => set_update_state(app, DesktopUpdateState::idle()),
        },
        Some(staged) => {
            match reconcile_staged(update.as_ref().map(|u| u.version.as_str()), &staged) {
                StagedAction::Keep => {}
                StagedAction::Replace => {
                    let update = update.expect("replace implies an advertised update");
                    download_and_stage(app, update, true).await;
                }
                StagedAction::Discard => {
                    store_staged(app, None);
                    set_update_state(app, DesktopUpdateState::idle());
                }
            }
        }
    }
}

async fn run_update_check(app: AppHandle) -> DesktopUpdateState {
    if !updates_enabled() {
        return current_update_state(&app);
    }

    if app
        .state::<UpdateManager>()
        .busy
        .swap(true, Ordering::AcqRel)
    {
        return current_update_state(&app);
    }

    perform_update_check(&app).await;
    app.state::<UpdateManager>()
        .busy
        .store(false, Ordering::Release);
    current_update_state(&app)
}

pub(crate) fn spawn_update_loop(app: AppHandle) {
    if !updates_enabled() {
        return;
    }
    tauri::async_runtime::spawn({
        let app = app.clone();
        async move {
            tokio::time::sleep(UPDATE_CHECK_STARTUP_DELAY).await;
            loop {
                run_update_check(app.clone()).await;
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
    run_update_check(app).await
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
            match update.download(|_chunk, _total| {}, || {}).await {
                Ok(bytes) => Ok(StagedUpdate { update, bytes }),
                Err(error) => {
                    eprintln!("tidebreak-desktop: install-time update download failed: {error}");
                    Err(InstallResolutionError {
                        staged: Some(staged),
                        message: UPDATE_PREPARE_ERROR,
                    })
                }
            }
        }
        StagedAction::Discard => Err(InstallResolutionError {
            staged: None,
            message: UPDATE_WITHDRAWN_ERROR,
        }),
    }
}

struct InstallResolutionError {
    /// A still-published staged artifact that can be retried later. Withdrawn
    /// artifacts are deliberately omitted so no subsequent action can install
    /// them without downloading them from a newly authoritative feed.
    staged: Option<StagedUpdate>,
    message: &'static str,
}

fn retryable_update_state(version: String, message: &'static str) -> DesktopUpdateState {
    DesktopUpdateState {
        status: DesktopUpdateStatus::Ready,
        version: Some(version),
        error: Some(message.to_owned()),
        enabled: updates_enabled(),
    }
}

struct FailedInstall<E> {
    install_error: E,
    resume_error: Option<String>,
}

/// Run the synchronous bundle replacement only after the broker's admission
/// barrier has drained. The closures keep the ordering contract directly
/// testable without constructing a packaged Tauri updater in unit tests.
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
        staged.take().expect("ready update must have staged bytes")
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

    let version = staged.update.version.clone();
    let host_access = app.state::<HostAccess>();
    let install_result = install_behind_broker_barrier(
        || host_access.quiesce_for_update(),
        || staged.update.install(&staged.bytes),
        || host_access.resume_after_failed_update(),
        || host_access.shutdown(),
    )
    .await;

    match install_result {
        Err(error) => {
            eprintln!("tidebreak-desktop: could not quiesce host broker for update: {error}");
            store_staged(&app, Some(staged));
            set_update_state(&app, retryable_update_state(version, UPDATE_PREPARE_ERROR));
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            Err(UPDATE_PREPARE_ERROR.to_owned())
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
            store_staged(&app, Some(staged));
            set_update_state(&app, retryable_update_state(version, UPDATE_INSTALL_ERROR));
            app.state::<UpdateManager>()
                .busy
                .store(false, Ordering::Release);
            Err(UPDATE_INSTALL_ERROR.to_owned())
        }
        Ok(Ok(())) => {
            app.restart();
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn failed_install_state_remains_retryable() {
        let mut state = retryable_update_state("1.2.3".to_owned(), UPDATE_INSTALL_ERROR);

        assert_eq!(state.enabled, updates_enabled());
        state.enabled = true;
        assert!(can_restart(&state, true));
        assert_eq!(state.version.as_deref(), Some("1.2.3"));
        assert_eq!(state.error.as_deref(), Some(UPDATE_INSTALL_ERROR));
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

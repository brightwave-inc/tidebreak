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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::host_access::HostAccess;

const UPDATE_STATE_EVENT: &str = "desktop-update-state";
const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(15);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const UPDATE_CHECK_ERROR: &str = "Could not check for updates. Try again later.";
const UPDATE_PREPARE_ERROR: &str = "Could not prepare the update. Try again later.";
const UPDATE_WITHDRAWN_ERROR: &str =
    "The downloaded update is no longer published. OpenWave will keep checking.";

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

/// True when `candidate` should supersede the staged version.
///
/// Versions are published as semver tags; when either side fails to parse,
/// the feed is treated as authoritative and any *different* advertised
/// version replaces the staged one. Equal or older versions never do, so a
/// check that raced a release cannot downgrade what is already staged.
fn is_newer(candidate: &str, staged: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(staged),
    ) {
        (Ok(candidate), Ok(staged)) => candidate > staged,
        _ => candidate != staged,
    }
}

fn reconcile_staged(feed_version: Option<&str>, staged_version: &str) -> StagedAction {
    match feed_version {
        None => StagedAction::Discard,
        Some(feed) if is_newer(feed, staged_version) => StagedAction::Replace,
        Some(_) => StagedAction::Keep,
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
        eprintln!("openwave-desktop: could not emit update state: {error}");
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
            eprintln!("openwave-desktop: update download failed: {error}");
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
            eprintln!("openwave-desktop: could not initialize updater: {error}");
            if !silent {
                set_update_state(app, DesktopUpdateState::failed(UPDATE_CHECK_ERROR));
            }
            return;
        }
    };

    let update = match updater.check().await {
        Ok(update) => update,
        Err(error) => {
            eprintln!("openwave-desktop: update check failed: {error}");
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
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UPDATE_CHECK_STARTUP_DELAY).await;
        loop {
            run_update_check(app.clone()).await;
            tokio::time::sleep(UPDATE_CHECK_INTERVAL).await;
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
) -> Result<StagedUpdate, String> {
    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            eprintln!("openwave-desktop: could not initialize updater: {error}");
            return Ok(staged);
        }
    };

    let update = match updater.check().await {
        Ok(update) => update,
        Err(error) => {
            eprintln!("openwave-desktop: install-time update check failed: {error}");
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
                    eprintln!("openwave-desktop: install-time update download failed: {error}");
                    Ok(staged)
                }
            }
        }
        StagedAction::Discard => {
            set_update_state(app, DesktopUpdateState::idle());
            Err(UPDATE_WITHDRAWN_ERROR.to_owned())
        }
    }
}

#[tauri::command]
pub(crate) async fn restart_for_update(app: AppHandle) -> Result<(), String> {
    let staged = {
        let manager = app.state::<UpdateManager>();
        let state = manager
            .state
            .lock()
            .expect("update state mutex poisoned")
            .clone();
        let mut staged = manager.staged.lock().expect("staged update mutex poisoned");
        if !can_restart(&state, staged.is_some()) {
            return Err("no update is ready to install".to_owned());
        }
        staged.take().expect("ready update must have staged bytes")
    };

    // Block the background loop from starting a new check while the install
    // proceeds; on success the process restarts, and every error path below
    // returns through `release_busy`.
    let was_busy = app
        .state::<UpdateManager>()
        .busy
        .swap(true, Ordering::AcqRel);

    let staged = match resolve_latest_for_install(&app, staged).await {
        Ok(staged) => staged,
        Err(message) => {
            if !was_busy {
                app.state::<UpdateManager>()
                    .busy
                    .store(false, Ordering::Release);
            }
            return Err(message);
        }
    };

    // Once the bundle is replaced, every new sidecar process would come from
    // the new app. Close the old host's broker permanently before installation
    // so it cannot respawn a mismatched sidecar in the install/restart window.
    app.state::<HostAccess>().shutdown().await;
    if let Err(error) = staged.update.install(&staged.bytes) {
        eprintln!("openwave-desktop: update installation failed: {error}");
    }

    // Relaunch even if installation failed so the current app gets a fresh,
    // usable broker instead of remaining alive after its broker was closed.
    app.restart();
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
        // A check that raced a release and resolved the *older* feed must not
        // downgrade what is already staged.
        assert_eq!(reconcile_staged(Some("0.4.0"), "0.4.1"), StagedAction::Keep);
        assert_eq!(
            reconcile_staged(Some("0.4.2-rc.1"), "0.4.2"),
            StagedAction::Keep
        );
    }

    #[test]
    fn a_withdrawn_release_is_discarded_not_installed() {
        assert_eq!(reconcile_staged(None, "0.4.1"), StagedAction::Discard);
    }

    #[test]
    fn unparseable_versions_defer_to_the_feed() {
        // The feed is authoritative when tags are not semver: any different
        // advertised version replaces the staged one, an identical one stays.
        assert_eq!(
            reconcile_staged(Some("build-124"), "build-123"),
            StagedAction::Replace
        );
        assert_eq!(
            reconcile_staged(Some("build-123"), "build-123"),
            StagedAction::Keep
        );
    }
}

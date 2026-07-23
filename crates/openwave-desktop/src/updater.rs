//! Background update checks and renderer-facing update state.
//!
//! Release builds check the signed feed after a short startup delay and then
//! periodically. Updates are downloaded and installed in place, but relaunching
//! is always left to an explicit user action so native work is never
//! interrupted automatically.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

const UPDATE_STATE_EVENT: &str = "desktop-update-state";
const UPDATE_CHECK_STARTUP_DELAY: Duration = Duration::from_secs(15);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const UPDATE_CHECK_ERROR: &str = "Could not check for updates. Try again later.";
const UPDATE_INSTALL_ERROR: &str = "Could not prepare the update. Try again later.";

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

#[derive(Default)]
pub(crate) struct UpdateManager {
    state: Mutex<DesktopUpdateState>,
    busy: AtomicBool,
}

pub(crate) const fn updates_enabled() -> bool {
    cfg!(all(not(debug_assertions), target_os = "macos"))
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

async fn perform_update_check(app: &AppHandle) {
    set_update_state(
        app,
        DesktopUpdateState {
            status: DesktopUpdateStatus::Checking,
            ..DesktopUpdateState::idle()
        },
    );

    let updater = match app.updater() {
        Ok(updater) => updater,
        Err(error) => {
            eprintln!("openwave-desktop: could not initialize updater: {error}");
            return set_update_state(app, DesktopUpdateState::failed(UPDATE_CHECK_ERROR));
        }
    };

    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => return set_update_state(app, DesktopUpdateState::idle()),
        Err(error) => {
            eprintln!("openwave-desktop: update check failed: {error}");
            return set_update_state(app, DesktopUpdateState::failed(UPDATE_CHECK_ERROR));
        }
    };

    let version = Some(update.version.clone());
    set_update_state(
        app,
        DesktopUpdateState {
            status: DesktopUpdateStatus::Downloading,
            version: version.clone(),
            ..DesktopUpdateState::idle()
        },
    );

    match update
        .download_and_install(|_chunk, _total| {}, || {})
        .await
    {
        Ok(()) => set_update_state(
            app,
            DesktopUpdateState {
                status: DesktopUpdateStatus::Ready,
                version,
                ..DesktopUpdateState::idle()
            },
        ),
        Err(error) => {
            eprintln!("openwave-desktop: update installation failed: {error}");
            set_update_state(app, DesktopUpdateState::failed(UPDATE_INSTALL_ERROR));
        }
    }
}

async fn run_update_check(app: AppHandle) -> DesktopUpdateState {
    if !updates_enabled() {
        return current_update_state(&app);
    }

    {
        let manager = app.state::<UpdateManager>();
        if manager.busy.swap(true, Ordering::AcqRel) {
            return current_update_state(&app);
        }

        let update_is_staged = matches!(
            manager
                .state
                .lock()
                .expect("update state mutex poisoned")
                .status,
            DesktopUpdateStatus::Downloading | DesktopUpdateStatus::Ready
        );
        if update_is_staged {
            manager.busy.store(false, Ordering::Release);
            return current_update_state(&app);
        }
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

fn can_restart(state: &DesktopUpdateState) -> bool {
    state.enabled && state.status == DesktopUpdateStatus::Ready
}

#[tauri::command]
pub(crate) fn restart_for_update(app: AppHandle) -> Result<(), String> {
    if !can_restart(&current_update_state(&app)) {
        return Err("no update is ready to install".to_owned());
    }
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
        assert!(can_restart(&state));

        state.status = DesktopUpdateStatus::Downloading;
        assert!(!can_restart(&state));

        state.status = DesktopUpdateStatus::Ready;
        state.enabled = false;
        assert!(!can_restart(&state));
    }
}

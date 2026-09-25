//! Notice when the previous run ended without a clean exit.
//!
//! The shell writes a marker into the profile data directory at launch and
//! removes it in its exit handler. A marker that is still there at the next
//! launch means the process ended some other way: a crash, a force quit, or a
//! power loss. The renderer then shows a quiet notice, "Tidebreak quit
//! unexpectedly", with a way to save a diagnostics report: the same bundle
//! `GET /diagnostics/export` builds, written to a file the person picks.
//! Nothing leaves the machine unless the person shares that file.
//!
//! An update removes the marker just before it installs. On Windows the
//! install starts the installer and ends this process on the spot, so the
//! exit handler never runs. An install that fails writes the marker again.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

use crate::host_access::HostAccess;

/// The marker's file name in the profile data directory.
const MARKER_FILE: &str = "running.json";

/// What a run records about itself in its marker.
#[derive(Debug, Serialize, Deserialize)]
struct RunMarker {
    pid: u32,
    version: String,
    started_at: DateTime<Utc>,
}

/// The run before this one, which did not exit cleanly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UncleanExit {
    /// When that run started, when its marker could still be read.
    started_at: Option<DateTime<Utc>>,
    /// Which version that run was, when its marker could still be read.
    version: Option<String>,
}

/// This run's marker, and what the last run's said.
pub(crate) struct RunMarkerState {
    path: PathBuf,
    /// What this run writes, kept so a failed update can write it again.
    this_run: RunMarker,
    previous: Mutex<Option<UncleanExit>>,
}

impl RunMarkerState {
    /// Read the marker the previous run left, if any, then write this run's.
    pub(crate) fn open(data_dir: &Path, version: &str) -> Self {
        let path = data_dir.join(MARKER_FILE);
        let previous = read_unclean_exit(&path);
        let markers = Self {
            path,
            this_run: RunMarker {
                pid: std::process::id(),
                version: version.to_owned(),
                started_at: Utc::now(),
            },
            previous: Mutex::new(previous),
        };
        markers.write();
        markers
    }

    fn write(&self) {
        let written = serde_json::to_vec(&self.this_run)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&self.path, bytes));
        if let Err(error) = written {
            eprintln!("tidebreak-desktop: could not write the run marker: {error}");
        }
    }

    /// Run an update's quiesce, then remove this run's marker. The install
    /// comes next, and on Windows it ends the process without running the
    /// exit handler. A quiesce that fails leaves the marker in place.
    pub(crate) async fn clear_after_quiesce(
        &self,
        quiesce: impl Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        quiesce.await?;
        self.clear();
        Ok(())
    }

    /// This run is carrying on after all, because an update failed to
    /// install: mark it as running again.
    pub(crate) fn restore(&self) {
        self.write();
    }

    /// The exit is clean, or an update is about to install: remove this
    /// run's marker.
    pub(crate) fn clear(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("tidebreak-desktop: could not remove the run marker: {error}"),
        }
    }

    fn previous(&self) -> Option<UncleanExit> {
        self.previous
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn dismiss(&self) {
        *self.previous.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// What a leftover marker says about the run that left it. A marker that
/// cannot be read still means that run did not exit cleanly.
fn read_unclean_exit(path: &Path) -> Option<UncleanExit> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => Vec::new(),
    };
    Some(match serde_json::from_slice::<RunMarker>(&bytes) {
        Ok(marker) => UncleanExit {
            started_at: Some(marker.started_at),
            version: Some(marker.version),
        },
        Err(_) => UncleanExit {
            started_at: None,
            version: None,
        },
    })
}

/// The previous run's unclean exit, until the person dismisses the notice.
#[tauri::command]
pub(crate) fn unclean_exit_notice(markers: State<'_, Arc<RunMarkerState>>) -> Option<UncleanExit> {
    markers.previous()
}

#[tauri::command]
pub(crate) fn dismiss_unclean_exit_notice(markers: State<'_, Arc<RunMarkerState>>) {
    markers.dismiss();
}

/// What a diagnostics report that could not be built says.
const BUILD_FAILED: &str = "Could not build the diagnostics report";

/// Save the diagnostics bundle to a file the person picks. `Ok(false)` means
/// they cancelled the save dialog.
///
/// The running server builds it when there is one. A boot that failed, or a
/// server that stopped, leaves no server to ask, and that is exactly when a
/// report matters most, so the shell then builds the same bundle from the
/// same allowlist itself (`export_without_server`).
#[tauri::command]
pub(crate) async fn save_diagnostics_report(
    app: AppHandle,
    server: State<'_, Arc<crate::AppState>>,
    host_access: State<'_, HostAccess>,
) -> Result<bool, String> {
    let bytes = diagnostics_bundle(&app, server.inner()).await?;
    let filename = format!(
        "tidebreak-diagnostics-{}.zip",
        chrono::Local::now().format("%Y-%m-%d-%H%M")
    );
    let _picker = host_access.debug_exports.lock().await;
    let Some(destination) = pick_report_path(&app, &filename).await? else {
        return Ok(false);
    };
    crate::chat_debug::write_bundle(&destination, &bytes)
        .map_err(|_| "Could not save the diagnostics report".to_owned())?;
    Ok(true)
}

/// The diagnostics bundle, from the running server when there is one.
async fn diagnostics_bundle(
    app: &AppHandle,
    server: &Arc<crate::AppState>,
) -> Result<Vec<u8>, String> {
    let serving = server.info_rx.borrow().clone();
    if let Some(Ok(info)) = serving {
        match server_export(&info).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => eprintln!(
                "tidebreak-desktop: the server could not export diagnostics, building them here: {error}"
            ),
        }
    }
    let data_dir = crate::data_dir(app)?;
    tauri::async_runtime::spawn_blocking(move || {
        tidebreak_server::diagnostics_export::export_without_server(
            &data_dir,
            tidebreak_core::Profile::Desktop,
        )
    })
    .await
    .map_err(|_| BUILD_FAILED.to_owned())?
    .map_err(|error| {
        eprintln!("tidebreak-desktop: could not build diagnostics without a server: {error}");
        BUILD_FAILED.to_owned()
    })
}

/// `GET /diagnostics/export` from the running server.
async fn server_export(info: &crate::NativeServerInfo) -> Result<Vec<u8>, String> {
    let response = crate::documents::local_client()
        .get(format!("{}/diagnostics/export", info.base_url))
        .bearer_auth(&info.token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the export answered {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

async fn pick_report_path(app: &AppHandle, filename: &str) -> Result<Option<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt as _;

    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Save diagnostics report")
        .set_file_name(filename)
        .add_filter("ZIP archive", &["zip"]);
    if let Some(window) = app.get_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The save dialog closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The save dialog returned an invalid destination".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean exit leaves nothing for the next launch to report.
    #[test]
    fn a_clean_exit_leaves_no_notice() {
        let dir = tempfile::tempdir().unwrap();
        let first = RunMarkerState::open(dir.path(), "0.115.0");
        assert_eq!(first.previous(), None);
        first.clear();

        let second = RunMarkerState::open(dir.path(), "0.115.0");
        assert_eq!(second.previous(), None);
    }

    /// A run that ended without its exit handler leaves its marker, and the
    /// next launch reports it until the person dismisses the notice.
    #[test]
    fn an_unclean_exit_is_noticed_at_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        let crashed = RunMarkerState::open(dir.path(), "0.114.0");
        drop(crashed);

        let next = RunMarkerState::open(dir.path(), "0.115.0");
        let previous = next.previous().expect("the unclean exit is noticed");
        assert_eq!(previous.version.as_deref(), Some("0.114.0"));
        assert!(previous.started_at.is_some());

        next.dismiss();
        assert_eq!(next.previous(), None);
        // This run's marker replaced the old one, so a clean exit now clears
        // the slate for the launch after.
        next.clear();
        assert_eq!(RunMarkerState::open(dir.path(), "0.115.0").previous(), None);
    }

    /// On Windows an update's install ends the process without the exit
    /// handler. The marker is gone once the quiesce before it succeeds, so
    /// the relaunch into the new version reports nothing.
    #[tokio::test]
    async fn an_update_that_ends_the_process_in_its_install_leaves_no_notice() {
        let dir = tempfile::tempdir().unwrap();
        let updating = RunMarkerState::open(dir.path(), "0.115.0");
        updating
            .clear_after_quiesce(async { Ok(()) })
            .await
            .unwrap();
        // The installer ends the process: no exit handler runs.
        drop(updating);

        assert_eq!(RunMarkerState::open(dir.path(), "0.116.0").previous(), None);
    }

    /// An update that fails, before or during the install, leaves this run
    /// going, so a crash after it is still noticed at the next launch.
    #[tokio::test]
    async fn a_failed_update_leaves_the_run_marked_as_running() {
        let refused_dir = tempfile::tempdir().unwrap();
        let refused = RunMarkerState::open(refused_dir.path(), "0.115.0");
        let refusal = refused
            .clear_after_quiesce(async { Err("A code turn is still running.".to_owned()) })
            .await;
        assert!(refusal.is_err());
        drop(refused);
        assert!(RunMarkerState::open(refused_dir.path(), "0.115.0")
            .previous()
            .is_some());

        let failed_dir = tempfile::tempdir().unwrap();
        let failed = RunMarkerState::open(failed_dir.path(), "0.115.0");
        failed.clear_after_quiesce(async { Ok(()) }).await.unwrap();
        // The install failed: `HostAccess::resume_after_failed_update`.
        failed.restore();
        drop(failed);
        let next = RunMarkerState::open(failed_dir.path(), "0.115.0");
        assert_eq!(
            next.previous().and_then(|previous| previous.version),
            Some("0.115.0".to_owned())
        );
    }

    #[test]
    fn an_unreadable_marker_still_counts_as_an_unclean_exit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER_FILE), b"{ torn").unwrap();

        let next = RunMarkerState::open(dir.path(), "0.115.0");
        assert_eq!(
            next.previous(),
            Some(UncleanExit {
                started_at: None,
                version: None,
            })
        );
    }

    #[test]
    fn the_notice_serializes_for_the_renderer() {
        let notice = UncleanExit {
            started_at: None,
            version: Some("0.114.0".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(notice).unwrap(),
            serde_json::json!({"startedAt": null, "version": "0.114.0"})
        );
    }
}

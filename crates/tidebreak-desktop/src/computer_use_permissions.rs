//! Device permission setup, reachable only from Tidebreak's main webview.
//! Status reads never prompt. The explicit request goes through the same
//! broker and helper as agent operations, so macOS attributes both alike.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State, Webview};
#[cfg(target_os = "macos")]
use tidebreak_host_broker::ControlRequest;
#[cfg(any(target_os = "macos", test))]
use tidebreak_host_broker::ControlResult;

use crate::host_access::HostAccess;
use crate::host_authority::Authority;

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum PermissionSetupStatus {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Available {
        #[serde(rename = "appName")]
        app_name: String,
        #[serde(rename = "appIdentifier")]
        app_identifier: Option<String>,
        #[serde(rename = "screenRecording")]
        screen_recording: bool,
        accessibility: bool,
    },
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Unsupported,
}

/// Renderer input selects one fixed pane; it can never supply a URL.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionPane {
    Accessibility,
    ScreenRecording,
}

impl PermissionPane {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn url(self) -> &'static str {
        match self {
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Self::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
        }
    }
}

fn require_setup_access(label: &str, local_authority: Result<(), String>) -> Result<(), String> {
    if label != "main" {
        return Err("Computer-use setup is available only in Tidebreak Settings.".to_owned());
    }
    local_authority
}

async fn check_access(webview: &Webview, state: &HostAccess) -> Result<(), String> {
    require_setup_access(
        webview.label(),
        state.require_local(Authority::ComputerUse).await,
    )
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct AppIdentity {
    name: Option<String>,
    identifier: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
fn select_app_identity(
    registered_bundle: Option<AppIdentity>,
    main_bundle: AppIdentity,
    package_name: &str,
) -> (String, Option<String>) {
    let identity = registered_bundle.unwrap_or(main_bundle);
    (
        identity
            .name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| package_name.to_owned()),
        identity
            .identifier
            .filter(|identifier| !identifier.trim().is_empty()),
    )
}

/// A wrapped dev executable can embed metadata that differs from its app bundle.
/// Prefer the application registered with macOS and retain unbundled fallbacks.
#[cfg(target_os = "macos")]
fn app_identity(app: &AppHandle) -> (String, Option<String>) {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::{NSBundle, NSString};

    let running = NSRunningApplication::currentApplication();
    let registered_bundle = running.bundleURL().map(|_| AppIdentity {
        name: running.localizedName().map(|value| value.to_string()),
        identifier: running.bundleIdentifier().map(|value| value.to_string()),
    });
    let bundle = NSBundle::mainBundle();
    let name = ["CFBundleDisplayName", "CFBundleName"]
        .into_iter()
        .find_map(|key| {
            bundle
                .objectForInfoDictionaryKey(&NSString::from_str(key))?
                .downcast::<NSString>()
                .ok()
                .map(|name| name.to_string())
                .filter(|name| !name.trim().is_empty())
        });
    select_app_identity(
        registered_bundle,
        AppIdentity {
            name,
            identifier: bundle.bundleIdentifier().map(|value| value.to_string()),
        },
        &app.package_info().name,
    )
}

#[cfg(any(target_os = "macos", test))]
fn permission_status(
    result: ControlResult,
    identity: (String, Option<String>),
) -> Result<PermissionSetupStatus, String> {
    let permissions = match result {
        ControlResult::CuPermissionStatus(status) | ControlResult::CuRequestPermissions(status) => {
            status
        }
        _ => return Err("Tidebreak received an invalid macOS permission status.".to_owned()),
    };
    Ok(PermissionSetupStatus::Available {
        app_name: identity.0,
        app_identifier: identity.1,
        screen_recording: permissions.screen_recording,
        accessibility: permissions.accessibility,
    })
}

#[tauri::command]
pub(crate) async fn computer_use_permission_status(
    app: AppHandle,
    webview: Webview,
    state: State<'_, HostAccess>,
) -> Result<PermissionSetupStatus, String> {
    check_access(&webview, &state).await?;
    #[cfg(target_os = "macos")]
    {
        let result = state
            .broker
            .control(ControlRequest::CuPermissionStatus)
            .await
            .map_err(|_| "Tidebreak could not check macOS permissions. Refresh to try again. If the native helper stays unavailable, restart Tidebreak.".to_owned())?;
        permission_status(result, app_identity(&app))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(PermissionSetupStatus::Unsupported)
    }
}

#[tauri::command]
pub(crate) async fn request_computer_use_permissions(
    app: AppHandle,
    webview: Webview,
    state: State<'_, HostAccess>,
) -> Result<PermissionSetupStatus, String> {
    check_access(&webview, &state).await?;
    #[cfg(target_os = "macos")]
    {
        // A lost response must not replay a permission prompt.
        let result = state
            .broker
            .control_without_retry(
                ControlRequest::CuRequestPermissions,
                tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW,
            )
            .await
            .map_err(|_| "Tidebreak could not request macOS permissions. Open System Settings to enable them, then refresh.".to_owned())?;
        permission_status(result, app_identity(&app))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(PermissionSetupStatus::Unsupported)
    }
}

#[tauri::command]
pub(crate) async fn open_computer_use_permission_settings(
    webview: Webview,
    state: State<'_, HostAccess>,
    pane: PermissionPane,
) -> Result<(), String> {
    check_access(&webview, &state).await?;
    #[cfg(target_os = "macos")]
    {
        let opened = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new("/usr/bin/open")
                .arg(pane.url())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
        match opened {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => Err("System Settings could not be opened. Open Privacy & Security in System Settings and select the permission.".to_owned()),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pane;
        Err("Computer-use permission setup requires macOS.".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_host_broker::computer_use::PermissionStatus;

    #[test]
    fn registered_app_identity_wins_over_embedded_development_metadata() {
        let identity = select_app_identity(
            Some(AppIdentity {
                name: Some("WK Acceptance".into()),
                identifier: Some("io.brightwave.tidebreak.wkacceptance.test".into()),
            }),
            AppIdentity {
                name: Some("Tidebreak".into()),
                identifier: Some("io.brightwave.tidebreak".into()),
            },
            "Tidebreak",
        );
        assert_eq!(
            identity,
            (
                "WK Acceptance".into(),
                Some("io.brightwave.tidebreak.wkacceptance.test".into()),
            ),
        );
    }

    #[test]
    fn unbundled_process_keeps_main_bundle_and_package_fallbacks() {
        assert_eq!(
            select_app_identity(
                None,
                AppIdentity {
                    name: Some("Tidebreak Dev".into()),
                    identifier: Some("io.brightwave.tidebreak.dev".into()),
                },
                "Tidebreak",
            ),
            (
                "Tidebreak Dev".into(),
                Some("io.brightwave.tidebreak.dev".into())
            ),
        );
        for name in [None, Some(" ".into())] {
            assert_eq!(
                select_app_identity(
                    None,
                    AppIdentity {
                        name,
                        identifier: None
                    },
                    "Tidebreak",
                ),
                ("Tidebreak".into(), None),
            );
        }
    }

    #[test]
    fn incomplete_registered_identity_never_borrows_another_bundle_identifier() {
        for identifier in [None, Some(" ".into())] {
            assert_eq!(
                select_app_identity(
                    Some(AppIdentity {
                        name: None,
                        identifier
                    }),
                    AppIdentity {
                        name: Some("Embedded app".into()),
                        identifier: Some("io.brightwave.tidebreak".into()),
                    },
                    "Tidebreak",
                ),
                ("Tidebreak".into(), None),
            );
        }
    }

    #[test]
    fn setup_requires_main_webview_and_local_computer_authority() {
        assert!(require_setup_access("main", Ok(())).is_ok());
        for label in ["browser", "main-child", "", "Main"] {
            assert!(require_setup_access(label, Ok(())).is_err());
        }
        let denied = crate::host_authority::COMPUTER_USE_UNAVAILABLE.to_owned();
        assert_eq!(
            require_setup_access("main", Err(denied.clone())),
            Err(denied)
        );
    }

    #[test]
    fn settings_input_can_only_select_the_two_permission_panes() {
        assert_eq!(
            PermissionPane::Accessibility.url(),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        );
        assert_eq!(
            PermissionPane::ScreenRecording.url(),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        );
        for value in [
            "https://example.com",
            "file:///etc/passwd",
            "screen_recording?other",
            "all",
        ] {
            assert!(serde_json::from_value::<PermissionPane>(serde_json::json!(value)).is_err());
        }
    }

    #[test]
    fn denied_permissions_stay_distinct_from_an_invalid_status_response() {
        let result = permission_status(
            ControlResult::CuPermissionStatus(PermissionStatus {
                screen_recording: true,
                accessibility: false,
            }),
            (
                "WK Acceptance".to_owned(),
                Some("test.acceptance".to_owned()),
            ),
        )
        .unwrap();
        let result = serde_json::to_value(result).unwrap();
        assert_eq!(result["screenRecording"], true);
        assert_eq!(result["accessibility"], false);
        assert_eq!(result["appName"], "WK Acceptance");
        assert_eq!(result["appIdentifier"], "test.acceptance");
        assert!(permission_status(
            ControlResult::CuRevokeApp { revoked: false },
            ("Tidebreak".into(), None)
        )
        .is_err());
    }
}

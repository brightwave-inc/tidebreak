//! Native child-webview host for Code-mode browser tabs.
//!
//! Remote pages never receive Tidebreak IPC. The main app webview owns every
//! command and event, while this module validates the external URL again at
//! the native boundary and limits all handles to the `code-browser-` prefix.

use serde::{Deserialize, Serialize};
use tauri::{
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder},
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, Webview, WebviewUrl,
};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tidebreak_core::{
    BrowserGrantCapability, BrowserNavigateArgs, BrowserNavigateResult, BrowserOrigin,
    BrowserOriginScope, OwnerId,
};
use tokio::sync::oneshot;
use url::Url;
use uuid::Uuid;

use crate::browser_control::{
    BrowserAgentAccess, BrowserController, BrowserDispatchEffect, BrowserLoadState,
    BrowserNavigationDecision, BrowserRegistry, BrowserSnapshot, ManagedBrowserRegistration,
};
use crate::browser_downloads::{
    publish_completed_download, BrowserDownloadFinished, BrowserDownloadStore,
};
#[cfg(any(target_os = "macos", test))]
use crate::browser_profile::normalize_website_host;
use crate::browser_profile::{BrowserProfileStore, ManagedBrowserProfile};
use crate::browser_recovery::{
    LegacyBrowserImportResult, LegacyBrowserSession, RecoveredBrowserSession,
};
use crate::browser_semantics::{browser_inject_inspect_overlay, browser_remove_inspect_overlay};
#[cfg(target_os = "macos")]
use crate::browser_url_observer::{
    observe_browser_url, stop_observing_browser_url, BrowserUrlChangeHandler,
};

const BROWSER_LABEL_PREFIX: &str = "code-browser-";
const CODE_BROWSER_EVENT: &str = "code-browser:event";
const MAX_BROWSER_ID_CHARS: usize = 80;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const MAX_BROWSER_URL_CHARS: usize = 8_192;
const MAX_JS_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const AGENT_NAVIGATION_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const AGENT_NAVIGATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
/// How often the host reads the view's URL while the tab is showing, on the
/// platforms without a native URL observer. macOS pushes every change through
/// `browser_url_observer` and never polls.
#[cfg(not(target_os = "macos"))]
const SAME_DOCUMENT_NAVIGATION_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);
/// The cadence while the tab is hidden. Nothing shows the URL, so the read
/// waits; a hidden tab only checks that it is still the same document.
#[cfg(not(target_os = "macos"))]
const SAME_DOCUMENT_NAVIGATION_HIDDEN_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(2);
#[cfg(target_os = "macos")]
const PROFILE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(target_os = "macos")]
const PROFILE_CLOSE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// One URL change reported by the native view. `loading` is true while a
/// cross-document navigation is in flight, so the page-load events own that
/// URL and a same-document push must not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserUrlChange {
    pub(crate) url: String,
    pub(crate) loading: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeBrowserCommandRequest {
    workspace_id: String,
    browser_id: String,
    action: CodeBrowserAction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeBrowserLegacyStateRequest {
    workspace_id: String,
    browser_id: String,
    legacy_state: Option<CodeBrowserLegacyState>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodeBrowserLegacyState {
    version: u8,
    id: String,
    workspace_id: String,
    url: Option<String>,
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum CodeBrowserAction {
    Create {
        url: String,
        bounds: CodeBrowserBounds,
        visible: bool,
    },
    Navigate {
        url: String,
    },
    Reload,
    Stop,
    Back,
    Forward,
    SetBounds {
        bounds: CodeBrowserBounds,
    },
    SetVisible {
        visible: bool,
    },
    Snapshot,
    ShareWithAgent,
    RevokeAgentAccess,
    StopAgentControl,
    TakeHumanControl,
    SetInspect {
        enabled: bool,
    },
    RemoveInspect,
    ResetProfile {
        /// Correlates renderer recovery with native phase events. It never
        /// selects an owner, profile, engine store, or filesystem path.
        #[serde(rename = "resetId")]
        reset_id: u64,
    },
    Close,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct CodeBrowserBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeBrowserEvent {
    workspace_id: String,
    browser_id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    load_state: Option<BrowserLoadState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    controller: Option<BrowserController>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_access: Option<BrowserAgentAccess>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeBrowserProfileResetEvent {
    workspace_id: String,
    browser_id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    reset_id: u64,
}

#[tauri::command]
pub(crate) fn code_browser_import_legacy_state(
    registry: tauri::State<'_, BrowserRegistry>,
    request: CodeBrowserLegacyStateRequest,
) -> Result<LegacyBrowserImportResult, String> {
    validated_workspace_id(&request.workspace_id)?;
    browser_label(&request.browser_id)?;
    let legacy = request.legacy_state.map(|state| LegacyBrowserSession {
        version: state.version,
        browser_id: state.id,
        workspace_id: state.workspace_id,
        url: state.url,
        title: state.title,
    });
    registry.import_legacy_session(
        &OwnerId::local(),
        &request.browser_id,
        &request.workspace_id,
        legacy,
    )
}

#[tauri::command]
pub(crate) async fn code_browser_command(
    app: AppHandle,
    registry: tauri::State<'_, BrowserRegistry>,
    profiles: tauri::State<'_, BrowserProfileStore>,
    downloads: tauri::State<'_, BrowserDownloadStore>,
    request: CodeBrowserCommandRequest,
) -> Result<BrowserSnapshot, String> {
    validated_workspace_id(&request.workspace_id)?;
    let label = browser_label(&request.browser_id)?;
    let existing = app.get_webview(&label);
    let registry = registry.inner().clone();
    let profiles = profiles.inner().clone();
    let downloads = downloads.inner().clone();

    match request.action {
        CodeBrowserAction::Create {
            url,
            bounds,
            visible,
        } => {
            let _lifecycle = profiles.lock_lifecycle().await;
            let existing = app.get_webview(&label);
            if let Some(webview) = existing {
                registry.ensure_workspace(&request.browser_id, &request.workspace_id)?;
                set_bounds(&webview, bounds)?;
                set_visible(&webview, visible)?;
                registry.set_visible(&request.browser_id, &request.workspace_id, visible)?;
                return registry.snapshot(&request.browser_id, &request.workspace_id);
            }
            let owner_id = OwnerId::local();
            let recovered =
                registry.recover_session(&owner_id, &request.browser_id, &request.workspace_id)?;
            // A platform webview can disappear independently after a native
            // failure. Remove that same-workspace stale record before
            // allocating a fresh native instance; a cross-workspace record is
            // deliberately rejected rather than rebound.
            registry.remove(&request.browser_id, &request.workspace_id)?;
            let profile = profiles.get_or_create(&owner_id)?;
            let (url, title) = recovered
                .map(|session| (session.url, session.title))
                .unwrap_or((url, None));
            create_browser(
                &app,
                &registry,
                &profiles,
                &downloads,
                profile,
                &request.workspace_id,
                &request.browser_id,
                &label,
                &url,
                title,
                bounds,
                visible,
            )
        }
        CodeBrowserAction::ResetProfile { reset_id } => {
            validated_profile_reset_id(reset_id)?;
            downloads.cancel_browser(&request.browser_id)?;
            let _lifecycle = profiles.lock_lifecycle().await;
            reset_browser_profile(
                &app,
                &registry,
                &profiles,
                &request.browser_id,
                &request.workspace_id,
                reset_id,
            )
            .await?;
            Ok(BrowserSnapshot::missing(
                &request.browser_id,
                &request.workspace_id,
            ))
        }
        CodeBrowserAction::Snapshot => match existing {
            Some(_) => registry.snapshot(&request.browser_id, &request.workspace_id),
            None => {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                let recovered = registry.recover_session(
                    &OwnerId::local(),
                    &request.browser_id,
                    &request.workspace_id,
                )?;
                Ok(missing_browser_snapshot(
                    &request.browser_id,
                    &request.workspace_id,
                    recovered,
                ))
            }
        },
        CodeBrowserAction::SetInspect { enabled } => {
            if existing.is_none() {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            }
            if enabled {
                browser_inject_inspect_overlay(
                    &app,
                    &registry,
                    &request.browser_id,
                    &request.workspace_id,
                )
                .await?;
                registry.set_inspect(&request.browser_id, &request.workspace_id, true)?;
            } else {
                browser_remove_inspect_overlay(
                    &app,
                    &registry,
                    &request.browser_id,
                    &request.workspace_id,
                )
                .await?;
                registry.set_inspect(&request.browser_id, &request.workspace_id, false)?;
            }
            registry.snapshot(&request.browser_id, &request.workspace_id)
        }
        CodeBrowserAction::RemoveInspect => {
            if existing.is_none() {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            }
            browser_remove_inspect_overlay(
                &app,
                &registry,
                &request.browser_id,
                &request.workspace_id,
            )
            .await?;
            registry.clear_inspect(&request.browser_id, &request.workspace_id)?;
            registry.snapshot(&request.browser_id, &request.workspace_id)
        }
        CodeBrowserAction::Close => {
            downloads.cancel_browser(&request.browser_id)?;
            let owner_id = OwnerId::local();
            registry.ensure_recovery_binding(
                &owner_id,
                &request.browser_id,
                &request.workspace_id,
            )?;
            if let Some(webview) = existing {
                registry.ensure_workspace(&request.browser_id, &request.workspace_id)?;
                close_browser_webview(&webview)?;
            }
            registry.remove(&request.browser_id, &request.workspace_id)?;
            registry.forget_recovery(&owner_id, &request.browser_id, &request.workspace_id)?;
            Ok(BrowserSnapshot::missing(
                &request.browser_id,
                &request.workspace_id,
            ))
        }
        CodeBrowserAction::ShareWithAgent => {
            let Some(webview) = existing else {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            };
            let (snapshot, pending_navigation) = share_browser_with_agent(
                &app,
                &registry,
                &request.browser_id,
                &request.workspace_id,
            )
            .await?;
            if let Some(url) = pending_navigation {
                run_action(
                    &app,
                    &request.browser_id,
                    &webview,
                    CodeBrowserAction::Navigate { url },
                )?;
            }
            emit_access_event(&app, "agent_access_changed", &snapshot, None);
            Ok(snapshot)
        }
        CodeBrowserAction::RevokeAgentAccess => {
            if existing.is_none() {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            }
            let snapshot =
                registry.revoke_browser_access(&request.browser_id, &request.workspace_id)?;
            emit_access_event(&app, "agent_access_changed", &snapshot, None);
            Ok(snapshot)
        }
        CodeBrowserAction::StopAgentControl => {
            if existing.is_none() {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            }
            let snapshot = registry
                .stop_agent_control(&request.browser_id, &request.workspace_id)
                .await?;
            emit_controller_event(&app, &snapshot);
            Ok(snapshot)
        }
        CodeBrowserAction::TakeHumanControl => {
            if existing.is_none() {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            }
            let snapshot = registry
                .take_human_control(&request.browser_id, &request.workspace_id)
                .await?;
            emit_controller_event(&app, &snapshot);
            Ok(snapshot)
        }
        action => {
            registry.ensure_workspace(&request.browser_id, &request.workspace_id)?;
            let Some(webview) = existing else {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                return Err("browser session is not open".to_owned());
            };
            let visible = match &action {
                CodeBrowserAction::SetVisible { visible } => Some(*visible),
                _ => None,
            };
            if matches!(
                &action,
                CodeBrowserAction::Navigate { .. }
                    | CodeBrowserAction::Reload
                    | CodeBrowserAction::Stop
                    | CodeBrowserAction::Back
                    | CodeBrowserAction::Forward
            ) {
                let snapshot = registry
                    .take_human_control(&request.browser_id, &request.workspace_id)
                    .await?;
                emit_controller_event(&app, &snapshot);
            }
            run_action(&app, &request.browser_id, &webview, action)?;
            if let Some(visible) = visible {
                registry.set_visible(&request.browser_id, &request.workspace_id, visible)?;
            }
            registry.snapshot(&request.browser_id, &request.workspace_id)
        }
    }
}

/// Navigate one shared visible browser through the native agent-control gate.
///
/// The dispatch is authorized against the current origin. The child-webview's
/// `on_navigation` hook independently authorizes the destination origin and
/// pauses the navigation when the user's live grant does not cover it.
pub(crate) async fn navigate_browser_for_agent(
    app: &AppHandle,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    arguments: &BrowserNavigateArgs,
) -> Result<BrowserNavigateResult, String> {
    if !arguments.is_well_formed() {
        return Err("browser navigation request is not valid".to_owned());
    }

    let host_snapshot = registry.begin_agent_control(capability_id, &arguments.browser_id)?;
    let workspace_id = host_snapshot.workspace_id;
    let current_origin = host_snapshot
        .url
        .as_deref()
        .and_then(BrowserOrigin::from_url)
        .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
    let start_epoch = host_snapshot
        .document_epoch
        .ok_or_else(|| "browser document epoch is unavailable".to_owned())?;
    let renderer_url = app.get_webview("main").and_then(|main| main.url().ok());
    let destination = validated_url(&arguments.url, renderer_url.as_ref())?;
    let destination_origin = BrowserOrigin::from_url(destination.as_str())
        .ok_or_else(|| "browser destination has no HTTP origin".to_owned())?;
    let label = browser_label(&arguments.browser_id)?;
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| "browser session is not open".to_owned())?;
    let browser_id = arguments.browser_id.clone();
    let dispatch_browser_id = browser_id.clone();
    let fallback_url = destination.to_string();
    let dispatch_registry = registry.clone();

    registry
        .dispatch_agent(
            capability_id,
            &browser_id,
            &current_origin,
            BrowserGrantCapability::BrowserControlOrigin,
            "navigate",
            Some(destination_origin.as_str()),
            BrowserDispatchEffect::Mutate,
            None,
            move || async move {
                webview.navigate(destination).map_err(browser_error)?;
                let deadline = tokio::time::Instant::now() + AGENT_NAVIGATION_START_TIMEOUT;
                loop {
                    let snapshot =
                        dispatch_registry.snapshot(&dispatch_browser_id, &workspace_id)?;
                    if snapshot
                        .agent_access
                        .as_ref()
                        .is_some_and(|access| access.halted)
                    {
                        return Err("browser control was stopped by the user".to_owned());
                    }
                    if let Some(document_epoch) = snapshot
                        .document_epoch
                        .filter(|document_epoch| *document_epoch > start_epoch)
                    {
                        return Ok(BrowserNavigateResult {
                            browser_id: dispatch_browser_id,
                            url: snapshot.url.unwrap_or(fallback_url),
                            load_state: snapshot.load_state.unwrap_or(BrowserLoadState::Loading),
                            document_epoch,
                        });
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(
                            "browser navigation did not start before the deadline".to_owned()
                        );
                    }
                    tokio::time::sleep(AGENT_NAVIGATION_POLL_INTERVAL).await;
                }
            },
        )
        .await
}

fn missing_browser_snapshot(
    browser_id: &str,
    workspace_id: &str,
    recovered: Option<RecoveredBrowserSession>,
) -> BrowserSnapshot {
    let mut snapshot = BrowserSnapshot::missing(browser_id, workspace_id);
    if let Some(recovered) = recovered {
        snapshot.url = Some(recovered.url);
        snapshot.title = recovered.title;
    }
    snapshot
}

#[allow(
    clippy::too_many_arguments,
    reason = "the single native creation boundary keeps browser identity and initial view state explicit"
)]
fn create_browser(
    app: &AppHandle,
    registry: &BrowserRegistry,
    profiles: &BrowserProfileStore,
    downloads: &BrowserDownloadStore,
    profile: ManagedBrowserProfile,
    workspace_id: &str,
    browser_id: &str,
    label: &str,
    url: &str,
    title: Option<String>,
    bounds: CodeBrowserBounds,
    visible: bool,
) -> Result<BrowserSnapshot, String> {
    let main = app
        .get_webview("main")
        .ok_or_else(|| "main webview is not available".to_owned())?;
    let window = app
        .get_window("main")
        .ok_or_else(|| "main window is not available".to_owned())?;
    let renderer_url = main.url().ok();
    let target = validated_url(url, renderer_url.as_ref())?;
    let safe_bounds = validated_bounds(bounds)?;
    let owner_id = profile.owner_id().clone();
    let profile_id = profile.profile_id();
    profiles.record_url(&owner_id, &profile_id, &target)?;

    let instance_id = registry.register_managed_with_title(
        browser_id,
        workspace_id,
        ManagedBrowserRegistration {
            owner_id: owner_id.clone(),
            profile_id: profile_id.clone(),
            url: target.to_string(),
            title,
            visible,
        },
    )?;

    let navigation_main = main.clone();
    let navigation_browser = browser_id.to_owned();
    let navigation_workspace = workspace_id.to_owned();
    let navigation_renderer_url = renderer_url.clone();
    let navigation_registry = registry.clone();
    let navigation_profiles = profiles.clone();
    let navigation_owner = owner_id;
    let navigation_profile = profile_id;
    let popup_main = main.clone();
    let popup_browser = browser_id.to_owned();
    let popup_workspace = workspace_id.to_owned();
    let load_main = main.clone();
    let load_browser = browser_id.to_owned();
    let load_workspace = workspace_id.to_owned();
    let load_registry = registry.clone();
    #[cfg(not(target_os = "macos"))]
    let load_renderer_url = renderer_url.clone();
    let title_main = main.clone();
    let title_browser = browser_id.to_owned();
    let title_workspace = workspace_id.to_owned();
    let title_registry = registry.clone();
    let download_app = app.clone();
    let download_browser = browser_id.to_owned();
    let download_workspace = workspace_id.to_owned();
    let download_registry = registry.clone();
    let download_store = downloads.clone();

    let builder = WebviewBuilder::new(label, WebviewUrl::External(target));
    #[cfg(target_os = "macos")]
    let builder = builder.data_store_identifier(profile.data_store_identifier());
    let builder = builder
        .on_navigation(move |url| {
            let Ok(safe_url) = validated_url(url.as_str(), navigation_renderer_url.as_ref()) else {
                emit_event(
                    &navigation_main,
                    CodeBrowserEvent {
                        workspace_id: navigation_workspace.clone(),
                        browser_id: navigation_browser.clone(),
                        kind: "navigation_blocked",
                        url: Some(url.to_string()),
                        title: None,
                        message: Some(
                            "This address is not allowed in the in-app browser".to_owned(),
                        ),
                        load_state: None,
                        document_epoch: None,
                        controller: None,
                        agent_access: None,
                        origin: None,
                    },
                );
                return false;
            };
            let Some(origin) = BrowserOrigin::from_url(safe_url.as_str()) else {
                return false;
            };
            match navigation_registry.authorize_navigation(
                &navigation_browser,
                &navigation_workspace,
                instance_id,
                safe_url.as_str(),
                &origin,
            ) {
                BrowserNavigationDecision::Allow => {
                    if navigation_profiles
                        .record_url(&navigation_owner, &navigation_profile, &safe_url)
                        .is_err()
                    {
                        emit_event(
                            &navigation_main,
                            CodeBrowserEvent {
                                workspace_id: navigation_workspace.clone(),
                                browser_id: navigation_browser.clone(),
                                kind: "navigation_blocked",
                                url: Some(url.to_string()),
                                title: None,
                                message: Some(
                                    "This site could not be added to the managed browser profile"
                                        .to_owned(),
                                ),
                                load_state: None,
                                document_epoch: None,
                                controller: None,
                                agent_access: None,
                                origin: None,
                            },
                        );
                        false
                    } else {
                        true
                    }
                }
                BrowserNavigationDecision::Pause { origin, snapshot } => {
                    emit_event(
                        &navigation_main,
                        CodeBrowserEvent {
                            workspace_id: navigation_workspace.clone(),
                            browser_id: navigation_browser.clone(),
                            kind: "agent_navigation_paused",
                            url: None,
                            title: snapshot.title,
                            message: Some(format!(
                                "Agent navigation paused before opening {origin}"
                            )),
                            load_state: snapshot.load_state,
                            document_epoch: snapshot.document_epoch,
                            controller: snapshot.controller,
                            agent_access: snapshot.agent_access,
                            origin: Some(origin),
                        },
                    );
                    false
                }
                BrowserNavigationDecision::Deny => false,
            }
        })
        .on_new_window(move |url, _features| {
            emit_event(
                &popup_main,
                CodeBrowserEvent {
                    workspace_id: popup_workspace.clone(),
                    browser_id: popup_browser.clone(),
                    kind: "popup_blocked",
                    url: Some(url.to_string()),
                    title: None,
                    message: None,
                    load_state: None,
                    document_epoch: None,
                    controller: None,
                    agent_access: None,
                    origin: None,
                },
            );
            NewWindowResponse::Deny
        })
        .on_page_load(move |webview, payload| {
            let snapshot = match payload.event() {
                PageLoadEvent::Started => load_registry.page_started(
                    &load_browser,
                    &load_workspace,
                    instance_id,
                    payload.url().to_string(),
                ),
                PageLoadEvent::Finished => load_registry.page_finished(
                    &load_browser,
                    &load_workspace,
                    instance_id,
                    payload.url().to_string(),
                ),
            };
            let Some(snapshot) = snapshot else {
                return;
            };
            emit_event(
                &load_main,
                CodeBrowserEvent {
                    workspace_id: load_workspace.clone(),
                    browser_id: load_browser.clone(),
                    kind: match payload.event() {
                        PageLoadEvent::Started => "navigation_started",
                        PageLoadEvent::Finished => "navigation_finished",
                    },
                    url: snapshot.url,
                    title: snapshot.title,
                    message: None,
                    load_state: snapshot.load_state,
                    document_epoch: snapshot.document_epoch,
                    controller: snapshot.controller,
                    agent_access: snapshot.agent_access,
                    origin: None,
                },
            );
            // macOS observes the view's URL natively from creation; the
            // fallback below only reads the view on the other platforms.
            #[cfg(target_os = "macos")]
            let _ = &webview;
            #[cfg(not(target_os = "macos"))]
            if matches!(payload.event(), PageLoadEvent::Finished) {
                if let Some(document_epoch) = snapshot.document_epoch {
                    start_same_document_navigation_poll(
                        load_main.clone(),
                        load_registry.clone(),
                        load_browser.clone(),
                        load_workspace.clone(),
                        instance_id,
                        document_epoch,
                        load_renderer_url.clone(),
                        webview,
                    );
                }
            }
        })
        .on_document_title_changed(move |webview, title| {
            let url = webview.url().ok().map(|url| url.to_string());
            let title: String = title.chars().take(160).collect();
            let Some(snapshot) = title_registry.title_changed(
                &title_browser,
                &title_workspace,
                instance_id,
                url,
                title,
            ) else {
                return;
            };
            emit_event(
                &title_main,
                CodeBrowserEvent {
                    workspace_id: title_workspace.clone(),
                    browser_id: title_browser.clone(),
                    kind: "title_changed",
                    url: snapshot.url,
                    title: snapshot.title,
                    message: None,
                    load_state: snapshot.load_state,
                    document_epoch: snapshot.document_epoch,
                    controller: snapshot.controller,
                    agent_access: snapshot.agent_access,
                    origin: None,
                },
            );
        })
        .on_download(move |_webview, event| match event {
            DownloadEvent::Requested { url, destination } => {
                match download_store.begin(
                    &download_registry,
                    &download_browser,
                    &download_workspace,
                    &url,
                    destination,
                ) {
                    Ok(started) => {
                        *destination = started.destination;
                        emit_download_event(
                            &download_app,
                            &download_workspace,
                            &download_browser,
                            "download_started",
                            None,
                            format!("Saving {} to Outputs", started.filename),
                        );
                        true
                    }
                    Err(message) => {
                        emit_download_event(
                            &download_app,
                            &download_workspace,
                            &download_browser,
                            "download_blocked",
                            Some(url.to_string()),
                            message,
                        );
                        false
                    }
                }
            }
            DownloadEvent::Finished { url, path, success } => {
                match download_store.finish(&download_browser, &url, path.as_deref(), success) {
                    Ok(BrowserDownloadFinished::Publish(receipt)) => {
                        publish_completed_download(download_app.clone(), receipt, url.to_string());
                    }
                    Ok(BrowserDownloadFinished::Rejected { filename, message }) => {
                        emit_download_event(
                            &download_app,
                            &download_workspace,
                            &download_browser,
                            "download_failed",
                            Some(url.to_string()),
                            format!("{filename}: {message}"),
                        );
                    }
                    Ok(BrowserDownloadFinished::Ignored) => {}
                    Err(message) => emit_download_event(
                        &download_app,
                        &download_workspace,
                        &download_browser,
                        "download_failed",
                        Some(url.to_string()),
                        message,
                    ),
                }
                true
            }
            _ => false,
        });

    let webview = match window.add_child(
        builder,
        LogicalPosition::new(safe_bounds.x, safe_bounds.y),
        LogicalSize::new(safe_bounds.width, safe_bounds.height),
    ) {
        Ok(webview) => webview,
        Err(error) => {
            registry.remove_instance(browser_id, workspace_id, instance_id);
            return Err(browser_error(error));
        }
    };
    #[cfg(target_os = "macos")]
    if let Err(error) = observe_browser_url(
        &webview,
        same_document_navigation_handler(
            main.clone(),
            registry.clone(),
            browser_id.to_owned(),
            workspace_id.to_owned(),
            instance_id,
            renderer_url.clone(),
        ),
    ) {
        let _ = close_browser_webview(&webview);
        registry.remove_instance(browser_id, workspace_id, instance_id);
        return Err(error);
    }
    if !visible {
        if let Err(error) = webview.hide() {
            let _ = close_browser_webview(&webview);
            registry.remove_instance(browser_id, workspace_id, instance_id);
            return Err(browser_error(error));
        }
    }
    registry.snapshot(browser_id, workspace_id)
}

/// Close a managed browser view, detaching the native URL observer first so
/// nothing keeps watching a view that is going away.
fn close_browser_webview(webview: &Webview) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let _ = stop_observing_browser_url(webview);
    webview.close().map_err(browser_error)
}

/// Build the handler the native URL observer calls on the main thread. It
/// hands each change to one ordered task so the registry sees pushes in the
/// order the view reported them, and the main thread never waits on the
/// registry or the recovery store.
#[cfg(target_os = "macos")]
fn same_document_navigation_handler(
    main: Webview,
    registry: BrowserRegistry,
    browser_id: String,
    workspace_id: String,
    instance_id: u64,
    renderer_url: Option<Url>,
) -> BrowserUrlChangeHandler {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<BrowserUrlChange>();
    tauri::async_runtime::spawn(async move {
        while let Some(change) = receiver.recv().await {
            apply_browser_url_change(
                &main,
                &registry,
                &browser_id,
                &workspace_id,
                instance_id,
                renderer_url.as_ref(),
                &change,
            );
        }
    });
    Box::new(move |change| {
        let _ = sender.send(change);
    })
}

/// Record one reported URL as a same-document navigation when it qualifies,
/// and tell the renderer. Returns the snapshot it emitted, if any.
fn apply_browser_url_change(
    main: &Webview,
    registry: &BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
    instance_id: u64,
    renderer_url: Option<&Url>,
    change: &BrowserUrlChange,
) -> Option<BrowserSnapshot> {
    let snapshot = registry.snapshot(browser_id, workspace_id).ok()?;
    let (document_epoch, url) = same_document_navigation_target(&snapshot, change, renderer_url)?;
    let snapshot = registry.same_document_navigation(
        browser_id,
        workspace_id,
        instance_id,
        document_epoch,
        url.to_string(),
    )?;
    emit_event(
        main,
        CodeBrowserEvent {
            workspace_id: workspace_id.to_owned(),
            browser_id: browser_id.to_owned(),
            kind: "same_document_navigation",
            url: snapshot.url.clone(),
            title: snapshot.title.clone(),
            message: None,
            load_state: snapshot.load_state,
            document_epoch: snapshot.document_epoch,
            controller: snapshot.controller.clone(),
            agent_access: snapshot.agent_access.clone(),
            origin: None,
        },
    );
    Some(snapshot)
}

/// Decide whether a reported URL is a same-document navigation of the
/// document the registry currently holds. Cross-document loads are left to
/// the page-load events: the view is still loading, or the registry has not
/// seen the document finish, or the origin moved.
fn same_document_navigation_target(
    snapshot: &BrowserSnapshot,
    change: &BrowserUrlChange,
    renderer_url: Option<&Url>,
) -> Option<(u64, Url)> {
    if change.loading || snapshot.load_state != Some(BrowserLoadState::Ready) {
        return None;
    }
    let document_epoch = snapshot.document_epoch?;
    let current = snapshot.url.as_deref()?;
    if current == change.url {
        return None;
    }
    let expected_origin = BrowserOrigin::from_url(current)?;
    let url = validated_same_document_url(&change.url, renderer_url, &expected_origin).ok()?;
    Some((document_epoch, url))
}

/// Poll the view's URL for same-document navigation on the platforms that
/// have no native URL observer. The loop ends when the document changes.
#[cfg(not(target_os = "macos"))]
#[allow(
    clippy::too_many_arguments,
    reason = "the poll fence keeps native view, workspace, and document identity explicit"
)]
fn start_same_document_navigation_poll(
    main: Webview,
    registry: BrowserRegistry,
    browser_id: String,
    workspace_id: String,
    instance_id: u64,
    document_epoch: u64,
    renderer_url: Option<Url>,
    webview: Webview,
) {
    tauri::async_runtime::spawn(async move {
        let mut cadence = SAME_DOCUMENT_NAVIGATION_POLL_INTERVAL;
        loop {
            tokio::time::sleep(cadence).await;
            let Ok(snapshot) = registry.snapshot(&browser_id, &workspace_id) else {
                return;
            };
            if snapshot.document_epoch != Some(document_epoch) {
                return;
            }
            // A hidden tab shows no URL bar, so there is nothing to keep
            // current; the first visible read catches up on whatever moved.
            if snapshot.visible == Some(false) {
                cadence = SAME_DOCUMENT_NAVIGATION_HIDDEN_POLL_INTERVAL;
                continue;
            }
            cadence = SAME_DOCUMENT_NAVIGATION_POLL_INTERVAL;
            let Ok(url) = webview.url() else {
                continue;
            };
            let change = BrowserUrlChange {
                url: url.to_string(),
                loading: false,
            };
            apply_browser_url_change(
                &main,
                &registry,
                &browser_id,
                &workspace_id,
                instance_id,
                renderer_url.as_ref(),
                &change,
            );
        }
    });
}

async fn share_browser_with_agent(
    app: &AppHandle,
    registry: &BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
) -> Result<(BrowserSnapshot, Option<String>), String> {
    let origin = registry.share_target_origin(browser_id, workspace_id)?;
    let scope = if origin.is_loopback() {
        native_loopback_share_choice(app, &origin).await?
    } else if native_public_share_choice(app, &origin).await? {
        Some(BrowserOriginScope::Origin {
            origin: origin.clone(),
        })
    } else {
        None
    };
    let Some(scope) = scope else {
        return Ok((registry.snapshot(browser_id, workspace_id)?, None));
    };
    let snapshot = registry.grant_browser_access(
        browser_id,
        workspace_id,
        &origin,
        scope,
        &[
            BrowserGrantCapability::BrowserControlOrigin,
            BrowserGrantCapability::BrowserTransferFiles,
        ],
    )?;
    let pending_navigation = registry.take_pending_navigation(browser_id, workspace_id)?;
    Ok((snapshot, pending_navigation))
}

async fn native_public_share_choice(
    app: &AppHandle,
    origin: &BrowserOrigin,
) -> Result<bool, String> {
    let origin = crate::native_security_label(origin.as_str());
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow agents in this workspace to inspect and navigate {origin}?\n\nPage content is untrusted. Password and verification-code fields stay private and require human takeover. Every file upload requires another confirmation. Screenshots pause when the host cannot prove that visible fields are safe."
        ))
        .title("Share this site with agents?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Share this site".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = sender.send(approved);
    });
    receiver
        .await
        .map_err(|_| "the native browser permission prompt closed unexpectedly".to_owned())
}

async fn native_loopback_share_choice(
    app: &AppHandle,
    origin: &BrowserOrigin,
) -> Result<Option<BrowserOriginScope>, String> {
    let origin_label = crate::native_security_label(origin.as_str());
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(format!(
            "Allow agents in this workspace to inspect and navigate {origin_label}?\n\nPassword and verification-code fields stay private and require human takeover. Every file upload requires another confirmation. Screenshots pause when the host cannot prove that visible fields are safe. Choose only this origin, or all loopback sites in this workspace so development ports can change without another share prompt."
        ))
        .title("Share a local site with agents?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            "Only this origin".to_owned(),
            "All local sites".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show_with_result(move |answer| {
        let _ = sender.send(answer);
    });
    let answer = receiver
        .await
        .map_err(|_| "the native browser permission prompt closed unexpectedly".to_owned())?;
    match answer {
        MessageDialogResult::Yes => Ok(Some(BrowserOriginScope::Origin {
            origin: origin.clone(),
        })),
        MessageDialogResult::Custom(label) if label == "Only this origin" => {
            Ok(Some(BrowserOriginScope::Origin {
                origin: origin.clone(),
            }))
        }
        MessageDialogResult::No => Ok(Some(BrowserOriginScope::LoopbackWorkspace)),
        MessageDialogResult::Custom(label) if label == "All local sites" => {
            Ok(Some(BrowserOriginScope::LoopbackWorkspace))
        }
        _ => Ok(None),
    }
}

#[cfg(target_os = "macos")]
async fn reset_browser_profile(
    app: &AppHandle,
    registry: &BrowserRegistry,
    profiles: &BrowserProfileStore,
    browser_id: &str,
    workspace_id: &str,
    reset_id: u64,
) -> Result<(), String> {
    let reset = registry
        .begin_profile_reset(browser_id, workspace_id)
        .await?;
    let sessions = reset.sessions().to_vec();
    let profile = match profiles.resolve(reset.owner_id(), reset.profile_id()) {
        Ok(profile) => profile,
        Err(error) => {
            return recover_failed_profile_reset(reset, error, || {
                emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_reconstruct");
            });
        }
    };
    let owner_id = reset.owner_id().clone();
    let profile_id = reset.profile_id().to_owned();
    let result = close_before_delete(
        || emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_closing"),
        || close_profile_sessions(app, reset.sessions()),
        || emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_deleting_data"),
        || delete_managed_profile(app, &profile),
    )
    .await;
    match result {
        Ok(()) => match profiles.forget(&owner_id, &profile_id) {
            Ok(()) => {
                reset.finish();
                emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_reconstruct");
                Ok(())
            }
            Err(error) => recover_failed_profile_reset(reset, error, || {
                emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_reconstruct");
            }),
        },
        Err(error) => recover_failed_profile_reset(reset, error, || {
            emit_profile_reset_event(app, &sessions, reset_id, "profile_reset_reconstruct");
        }),
    }
}

#[cfg(not(target_os = "macos"))]
async fn reset_browser_profile(
    _app: &AppHandle,
    _registry: &BrowserRegistry,
    _profiles: &BrowserProfileStore,
    _browser_id: &str,
    _workspace_id: &str,
    _reset_id: u64,
) -> Result<(), String> {
    Err("browser profile reset is not available on this platform".to_owned())
}

#[cfg(any(target_os = "macos", test))]
async fn close_before_delete<S, C, CloseFuture, P, D, DeleteFuture>(
    started: S,
    close: C,
    closed: P,
    delete: D,
) -> Result<(), String>
where
    S: FnOnce(),
    C: FnOnce() -> CloseFuture,
    CloseFuture: std::future::Future<Output = Result<(), String>>,
    P: FnOnce(),
    D: FnOnce() -> DeleteFuture,
    DeleteFuture: std::future::Future<Output = Result<(), String>>,
{
    started();
    close().await?;
    closed();
    delete().await
}

#[cfg(any(target_os = "macos", test))]
fn recover_failed_profile_reset<R>(
    reset: crate::browser_control::BrowserProfileResetLease,
    error: String,
    reconstruct: R,
) -> Result<(), String>
where
    R: FnOnce(),
{
    drop(reset);
    reconstruct();
    Err(error)
}

#[cfg(target_os = "macos")]
fn emit_profile_reset_event(
    app: &AppHandle,
    sessions: &[crate::browser_control::BrowserProfileResetSession],
    reset_id: u64,
    kind: &'static str,
) {
    let Some(main) = app.get_webview("main") else {
        return;
    };
    for session in sessions {
        let _ = main.emit(
            CODE_BROWSER_EVENT,
            CodeBrowserProfileResetEvent {
                workspace_id: session.workspace_id.clone(),
                browser_id: session.browser_id.clone(),
                kind,
                reset_id,
            },
        );
    }
}

#[cfg(target_os = "macos")]
async fn close_profile_sessions(
    app: &AppHandle,
    sessions: &[crate::browser_control::BrowserProfileResetSession],
) -> Result<(), String> {
    let labels = sessions
        .iter()
        .map(|session| browser_label(&session.browser_id))
        .collect::<Result<Vec<_>, _>>()?;
    for label in &labels {
        if let Some(webview) = app.get_webview(label) {
            close_browser_webview(&webview)?;
        }
    }

    let deadline = tokio::time::Instant::now() + PROFILE_CLOSE_TIMEOUT;
    loop {
        if labels.iter().all(|label| app.get_webview(label).is_none()) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("browser sessions did not close before profile reset".to_owned());
        }
        tokio::time::sleep(PROFILE_CLOSE_POLL_INTERVAL).await;
    }
}

#[cfg(target_os = "macos")]
async fn delete_managed_profile(
    app: &AppHandle,
    profile: &ManagedBrowserProfile,
) -> Result<(), String> {
    if macos_major_version() >= 14 {
        let identifier = profile.data_store_identifier();
        let identifiers = app
            .fetch_data_store_identifiers()
            .await
            .map_err(browser_error)?;
        if identifiers.contains(&identifier) {
            app.remove_data_store(identifier)
                .await
                .map_err(browser_error)?;
        }
        Ok(())
    } else {
        remove_legacy_website_data(app, profile.website_hosts()).await
    }
}

#[cfg(target_os = "macos")]
fn macos_major_version() -> isize {
    objc2_foundation::NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
}

#[cfg(target_os = "macos")]
async fn remove_legacy_website_data(
    app: &AppHandle,
    website_hosts: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    use std::{
        ptr::NonNull,
        sync::{Arc, Mutex},
    };

    use block2::RcBlock;
    use objc2::{msg_send, rc::Retained, runtime::AnyObject, MainThreadMarker};
    use objc2_foundation::{NSArray, NSString};
    use objc2_web_kit::WKWebsiteDataStore;

    if website_hosts.is_empty() {
        return Ok(());
    }

    let website_hosts = Arc::new(website_hosts.clone());
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let callback_sender = Arc::clone(&sender);
    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            if let Some(sender) = sender.lock().ok().and_then(|mut sender| sender.take()) {
                let _ = sender.send(Err(
                    "browser profile reset requires the main thread".to_owned()
                ));
            }
            return;
        };
        unsafe {
            let store = WKWebsiteDataStore::defaultDataStore(mtm);
            let data_types = WKWebsiteDataStore::allWebsiteDataTypes(mtm);
            let removal_store = store.clone();
            let removal_types = data_types.clone();
            let fetch_sender = Arc::clone(&callback_sender);
            let fetch_hosts = Arc::clone(&website_hosts);
            let fetch = RcBlock::new(move |records: NonNull<NSArray<AnyObject>>| {
                let matching = records
                    .as_ref()
                    .to_vec()
                    .into_iter()
                    .filter(|record| {
                        let display_name: Retained<NSString> = msg_send![&**record, displayName];
                        website_record_matches(&display_name.to_string(), &fetch_hosts)
                    })
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    if let Some(sender) = fetch_sender
                        .lock()
                        .ok()
                        .and_then(|mut sender| sender.take())
                    {
                        let _ = sender.send(Ok(()));
                    }
                    return;
                }

                let records = NSArray::from_retained_slice(&matching);
                let retained_records = records.clone();
                let removal_sender = Arc::clone(&fetch_sender);
                let removed = RcBlock::new(move || {
                    let _keep_records_alive = &retained_records;
                    if let Some(sender) = removal_sender
                        .lock()
                        .ok()
                        .and_then(|mut sender| sender.take())
                    {
                        let _ = sender.send(Ok(()));
                    }
                });
                let _: () = msg_send![
                    &*removal_store,
                    removeDataOfTypes: &*removal_types,
                    forDataRecords: &*records,
                    completionHandler: &*removed
                ];
            });
            let _: () = msg_send![
                &*store,
                fetchDataRecordsOfTypes: &*data_types,
                completionHandler: &*fetch
            ];
        }
    })
    .map_err(browser_error)?;

    tokio::time::timeout(PROFILE_CLOSE_TIMEOUT, receiver)
        .await
        .map_err(|_| "browser website data reset timed out".to_owned())?
        .map_err(|_| "browser website data reset was interrupted".to_owned())?
}

#[cfg(any(target_os = "macos", test))]
fn website_record_matches(
    record_display_name: &str,
    website_hosts: &std::collections::BTreeSet<String>,
) -> bool {
    let Some(record) = normalize_website_host(record_display_name) else {
        return false;
    };
    website_hosts.iter().any(|host| {
        host == &record
            || ((record == "localhost" || record.contains('.'))
                && host
                    .strip_suffix(&record)
                    .is_some_and(|prefix| prefix.ends_with('.')))
    })
}

fn run_action(
    app: &AppHandle,
    browser_id: &str,
    webview: &Webview,
    action: CodeBrowserAction,
) -> Result<(), String> {
    match action {
        CodeBrowserAction::Navigate { url } => {
            let renderer_url = app.get_webview("main").and_then(|main| main.url().ok());
            webview
                .navigate(validated_url(&url, renderer_url.as_ref())?)
                .map_err(browser_error)
        }
        CodeBrowserAction::Reload => webview.reload().map_err(browser_error),
        CodeBrowserAction::Stop => webview.eval("window.stop()").map_err(browser_error),
        CodeBrowserAction::Back => webview.eval("window.history.back()").map_err(browser_error),
        CodeBrowserAction::Forward => webview
            .eval("window.history.forward()")
            .map_err(browser_error),
        CodeBrowserAction::SetBounds { bounds } => set_bounds(webview, bounds),
        CodeBrowserAction::SetVisible { visible } => set_visible(webview, visible),
        CodeBrowserAction::Create { .. }
        | CodeBrowserAction::Snapshot
        | CodeBrowserAction::ShareWithAgent
        | CodeBrowserAction::RevokeAgentAccess
        | CodeBrowserAction::StopAgentControl
        | CodeBrowserAction::TakeHumanControl
        | CodeBrowserAction::SetInspect { .. }
        | CodeBrowserAction::RemoveInspect
        | CodeBrowserAction::ResetProfile { .. }
        | CodeBrowserAction::Close => Err(format!(
            "browser action is not valid for the open session {browser_id}"
        )),
    }
}

fn set_bounds(webview: &Webview, bounds: CodeBrowserBounds) -> Result<(), String> {
    let bounds = validated_bounds(bounds)?;
    webview
        .set_bounds(Rect {
            position: LogicalPosition::new(bounds.x, bounds.y).into(),
            size: LogicalSize::new(bounds.width, bounds.height).into(),
        })
        .map_err(browser_error)
}

fn set_visible(webview: &Webview, visible: bool) -> Result<(), String> {
    if visible {
        webview.show().map_err(browser_error)
    } else {
        webview.hide().map_err(browser_error)
    }
}

fn validated_url(value: &str, renderer_url: Option<&Url>) -> Result<Url, String> {
    if value.len() > MAX_BROWSER_URL_CHARS {
        return Err("browser address is too long".to_owned());
    }
    let url = Url::parse(value).map_err(|_| "browser address is not valid".to_owned())?;
    if url.as_str().len() > MAX_BROWSER_URL_CHARS {
        return Err("browser address is too long".to_owned());
    }
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("only HTTP and HTTPS addresses can open in the browser".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("browser addresses cannot contain credentials".to_owned());
    }
    if renderer_url.is_some_and(|renderer| same_app_origin(&url, renderer)) {
        return Err("the Tidebreak application origin cannot open as a browser tab".to_owned());
    }
    Ok(url)
}

fn validated_same_document_url(
    value: &str,
    renderer_url: Option<&Url>,
    expected_origin: &BrowserOrigin,
) -> Result<Url, String> {
    let url = validated_url(value, renderer_url)?;
    let observed_origin = BrowserOrigin::from_url(url.as_str())
        .ok_or_else(|| "browser same-document address has no HTTP origin".to_owned())?;
    if &observed_origin != expected_origin {
        return Err("browser same-document address changed origin".to_owned());
    }
    Ok(url)
}

fn same_app_origin(left: &Url, right: &Url) -> bool {
    if left.scheme() != right.scheme()
        || left.port_or_known_default() != right.port_or_known_default()
    {
        return false;
    }
    match (left.host_str(), right.host_str()) {
        (Some(left), Some(right)) if left.eq_ignore_ascii_case(right) => true,
        (Some(left), Some(right)) => is_loopback_host(left) && is_loopback_host(right),
        _ => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn validated_bounds(bounds: CodeBrowserBounds) -> Result<CodeBrowserBounds, String> {
    if !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
        || bounds.x < 0.0
        || bounds.y < 0.0
        || bounds.width < 1.0
        || bounds.height < 1.0
    {
        return Err("browser bounds are not valid".to_owned());
    }
    Ok(bounds)
}

pub(crate) fn browser_label(session_id: &str) -> Result<String, String> {
    if session_id.is_empty()
        || session_id.chars().count() > MAX_BROWSER_ID_CHARS
        || !session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("browser session id is not valid".to_owned());
    }
    Ok(format!("{BROWSER_LABEL_PREFIX}{session_id}"))
}

pub(crate) fn validated_workspace_id(workspace_id: &str) -> Result<(), String> {
    if workspace_id.is_empty()
        || workspace_id.chars().count() > MAX_WORKSPACE_ID_CHARS
        || workspace_id.chars().any(char::is_control)
    {
        return Err("browser workspace id is not valid".to_owned());
    }
    Ok(())
}

fn validated_profile_reset_id(reset_id: u64) -> Result<(), String> {
    if reset_id == 0 || reset_id > MAX_JS_SAFE_INTEGER {
        return Err("browser profile reset id is not valid".to_owned());
    }
    Ok(())
}

fn emit_event(main: &Webview, event: CodeBrowserEvent) {
    let _ = main.emit(CODE_BROWSER_EVENT, event);
}

pub(crate) fn emit_download_event(
    app: &AppHandle,
    workspace_id: &str,
    browser_id: &str,
    kind: &'static str,
    url: Option<String>,
    message: String,
) {
    let Some(main) = app.get_webview("main") else {
        return;
    };
    emit_event(
        &main,
        CodeBrowserEvent {
            workspace_id: workspace_id.to_owned(),
            browser_id: browser_id.to_owned(),
            kind,
            url,
            title: None,
            message: Some(message),
            load_state: None,
            document_epoch: None,
            controller: None,
            agent_access: None,
            origin: None,
        },
    );
}

fn emit_controller_event(app: &AppHandle, snapshot: &BrowserSnapshot) {
    let Some(main) = app.get_webview("main") else {
        return;
    };
    emit_event(
        &main,
        CodeBrowserEvent {
            workspace_id: snapshot.workspace_id.clone(),
            browser_id: snapshot.browser_id.clone(),
            kind: "controller_changed",
            url: snapshot.url.clone(),
            title: snapshot.title.clone(),
            message: None,
            load_state: snapshot.load_state,
            document_epoch: snapshot.document_epoch,
            controller: snapshot.controller.clone(),
            agent_access: snapshot.agent_access.clone(),
            origin: snapshot
                .agent_access
                .as_ref()
                .and_then(|access| access.origin.clone()),
        },
    );
}

fn emit_access_event(
    app: &AppHandle,
    kind: &'static str,
    snapshot: &BrowserSnapshot,
    message: Option<String>,
) {
    let Some(main) = app.get_webview("main") else {
        return;
    };
    emit_event(
        &main,
        CodeBrowserEvent {
            workspace_id: snapshot.workspace_id.clone(),
            browser_id: snapshot.browser_id.clone(),
            kind,
            url: snapshot.url.clone(),
            title: snapshot.title.clone(),
            message,
            load_state: snapshot.load_state,
            document_epoch: snapshot.document_epoch,
            controller: snapshot.controller.clone(),
            agent_access: snapshot.agent_access.clone(),
            origin: snapshot
                .agent_access
                .as_ref()
                .and_then(|access| access.origin.clone()),
        },
    );
}

fn browser_error(error: impl std::fmt::Display) -> String {
    format!("browser host: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_ids_are_bounded_and_safe_for_tauri_labels() {
        assert_eq!(
            browser_label("browser_123").unwrap(),
            "code-browser-browser_123"
        );
        assert!(browser_label("").is_err());
        assert!(browser_label("has/slash").is_err());
        assert!(browser_label(&"a".repeat(MAX_BROWSER_ID_CHARS + 1)).is_err());
    }

    #[test]
    fn same_document_observer_accepts_only_valid_urls_on_the_loaded_origin() {
        let origin = BrowserOrigin::from_url("https://example.com/start").unwrap();
        assert_eq!(
            validated_same_document_url(
                "https://example.com/next?view=details#summary",
                None,
                &origin,
            )
            .unwrap()
            .as_str(),
            "https://example.com/next?view=details#summary"
        );
        assert!(validated_same_document_url("not a url", None, &origin).is_err());
        assert!(validated_same_document_url("javascript:alert(1)", None, &origin).is_err());
        assert!(validated_same_document_url("https://other.example/", None, &origin).is_err());
    }

    fn ready_snapshot(url: &str) -> BrowserSnapshot {
        let mut snapshot = BrowserSnapshot::missing("browser-1", "workspace-1");
        snapshot.exists = true;
        snapshot.url = Some(url.to_owned());
        snapshot.load_state = Some(BrowserLoadState::Ready);
        snapshot.document_epoch = Some(3);
        snapshot
    }

    fn change(url: &str, loading: bool) -> BrowserUrlChange {
        BrowserUrlChange {
            url: url.to_owned(),
            loading,
        }
    }

    #[test]
    fn pushed_url_changes_become_same_document_navigation_on_the_ready_document() {
        let snapshot = ready_snapshot("https://example.com/start");
        let (epoch, url) = same_document_navigation_target(
            &snapshot,
            &change("https://example.com/start?view=details#summary", false),
            None,
        )
        .unwrap();
        assert_eq!(epoch, 3);
        assert_eq!(
            url.as_str(),
            "https://example.com/start?view=details#summary"
        );
    }

    #[test]
    fn pushed_url_changes_leave_cross_document_loads_to_the_page_load_events() {
        let ready = ready_snapshot("https://example.com/start");
        // The view reports a provisional URL while it is still loading.
        assert!(same_document_navigation_target(
            &ready,
            &change("https://example.com/next", true),
            None
        )
        .is_none());
        // The registry has not seen the document finish yet.
        let mut loading = ready_snapshot("https://example.com/start");
        loading.load_state = Some(BrowserLoadState::Loading);
        assert!(same_document_navigation_target(
            &loading,
            &change("https://example.com/next", false),
            None
        )
        .is_none());
        let mut fresh = ready_snapshot("https://example.com/start");
        fresh.document_epoch = None;
        assert!(same_document_navigation_target(
            &fresh,
            &change("https://example.com/next", false),
            None
        )
        .is_none());
    }

    #[test]
    fn pushed_url_changes_ignore_repeats_and_origin_moves() {
        let snapshot = ready_snapshot("https://example.com/start");
        assert!(same_document_navigation_target(
            &snapshot,
            &change("https://example.com/start", false),
            None
        )
        .is_none());
        assert!(same_document_navigation_target(
            &snapshot,
            &change("https://other.example/start", false),
            None
        )
        .is_none());
        assert!(same_document_navigation_target(
            &snapshot,
            &change("javascript:alert(1)", false),
            None
        )
        .is_none());
    }

    #[test]
    fn workspace_ids_are_required_and_bounded() {
        assert!(validated_workspace_id("workspace-123").is_ok());
        assert!(validated_workspace_id("").is_err());
        assert!(validated_workspace_id("workspace\nother").is_err());
        assert!(validated_workspace_id(&"w".repeat(MAX_WORKSPACE_ID_CHARS + 1)).is_err());
    }

    #[test]
    fn missing_webview_snapshot_projects_only_durable_navigation_metadata() {
        let snapshot = missing_browser_snapshot(
            "browser-1",
            "workspace-1",
            Some(RecoveredBrowserSession {
                url: "https://example.com/restored".to_owned(),
                title: Some("Restored page".to_owned()),
            }),
        );

        assert!(!snapshot.exists);
        assert_eq!(
            snapshot.url.as_deref(),
            Some("https://example.com/restored")
        );
        assert_eq!(snapshot.title.as_deref(), Some("Restored page"));
        assert!(snapshot.profile_id.is_none());
        assert!(snapshot.document_epoch.is_none());
        assert!(snapshot.controller.is_none());
        assert!(snapshot.agent_access.is_none());
    }

    #[test]
    fn profile_reset_ids_stay_exact_across_renderer_events() {
        assert!(validated_profile_reset_id(1).is_ok());
        assert!(validated_profile_reset_id(MAX_JS_SAFE_INTEGER).is_ok());
        assert!(validated_profile_reset_id(0).is_err());
        assert!(validated_profile_reset_id(MAX_JS_SAFE_INTEGER + 1).is_err());
    }

    #[test]
    fn profile_reset_events_carry_the_target_session_and_cycle() {
        assert_eq!(
            serde_json::to_value(CodeBrowserProfileResetEvent {
                workspace_id: "workspace-1".to_owned(),
                browser_id: "browser-1".to_owned(),
                kind: "profile_reset_reconstruct",
                reset_id: 17,
            })
            .unwrap(),
            serde_json::json!({
                "workspaceId": "workspace-1",
                "browserId": "browser-1",
                "type": "profile_reset_reconstruct",
                "resetId": 17,
            })
        );
    }

    #[test]
    fn browser_urls_are_http_only_and_cannot_reenter_the_app_origin() {
        let renderer = Url::parse("http://localhost:1420/code/w/one").unwrap();
        assert!(validated_url("https://example.com/docs", Some(&renderer)).is_ok());
        assert!(validated_url("http://localhost:3000", Some(&renderer)).is_ok());
        assert!(validated_url("http://localhost:1420/other", Some(&renderer)).is_err());
        assert!(validated_url("http://127.0.0.1:1420/other", Some(&renderer)).is_err());
        assert!(validated_url("http://[::1]:1420/other", Some(&renderer)).is_err());
        assert!(validated_url("file:///tmp/secret", Some(&renderer)).is_err());
        assert!(validated_url("https://name:secret@example.com", Some(&renderer)).is_err());
        assert!(validated_url(
            &format!("https://example.com/{}", "a".repeat(MAX_BROWSER_URL_CHARS)),
            Some(&renderer)
        )
        .is_err());
    }

    #[test]
    fn browser_bounds_reject_offscreen_and_non_finite_values() {
        assert!(validated_bounds(CodeBrowserBounds {
            x: 0.0,
            y: 20.0,
            width: 800.0,
            height: 500.0,
        })
        .is_ok());
        assert!(validated_bounds(CodeBrowserBounds {
            x: -1.0,
            y: 0.0,
            width: 800.0,
            height: 500.0,
        })
        .is_err());
        assert!(validated_bounds(CodeBrowserBounds {
            x: 0.0,
            y: 0.0,
            width: f64::NAN,
            height: 500.0,
        })
        .is_err());
    }
    #[tokio::test]
    async fn profile_reset_closes_sessions_before_deleting_profile_data() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let close_events = std::sync::Arc::clone(&events);
        let delete_events = std::sync::Arc::clone(&events);

        close_before_delete(
            {
                let events = std::sync::Arc::clone(&events);
                move || events.lock().unwrap().push("closing")
            },
            move || async move {
                close_events.lock().unwrap().push("close");
                Ok(())
            },
            {
                let events = std::sync::Arc::clone(&events);
                move || events.lock().unwrap().push("deleting")
            },
            move || async move {
                delete_events.lock().unwrap().push("delete");
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *events.lock().unwrap(),
            ["closing", "close", "deleting", "delete"]
        );
    }

    #[tokio::test]
    async fn profile_reset_never_deletes_when_a_session_does_not_close() {
        let deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let delete_probe = std::sync::Arc::clone(&deleted);

        assert!(close_before_delete(
            || {},
            || async { Err("close failed".to_owned()) },
            || panic!("deletion phase must wait for every session to close"),
            move || async move {
                delete_probe.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .is_err());

        assert!(!deleted.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn profile_reset_preserves_the_deletion_error_after_the_phase_signal() {
        let phases = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let closing = std::sync::Arc::clone(&phases);
        let deleting = std::sync::Arc::clone(&phases);

        let error = close_before_delete(
            move || closing.lock().unwrap().push("closing"),
            || async { Ok(()) },
            move || deleting.lock().unwrap().push("deleting"),
            || async { Err("WebKit could not remove profile data".to_owned()) },
        )
        .await
        .unwrap_err();

        assert_eq!(*phases.lock().unwrap(), ["closing", "deleting"]);
        assert_eq!(error, "WebKit could not remove profile data");
    }

    #[tokio::test]
    async fn failed_profile_reset_restores_registry_before_reconstruction() {
        let registry = BrowserRegistry::default();
        registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com/app".to_owned(),
                true,
            )
            .unwrap();
        let reset = registry
            .begin_profile_reset("browser-1", "workspace-1")
            .await
            .unwrap();
        let recovered_registry = registry.clone();

        let error = recover_failed_profile_reset(
            reset,
            "WebKit could not remove profile data".to_owned(),
            move || {
                assert!(recovered_registry
                    .snapshot("browser-1", "workspace-1")
                    .is_ok());
            },
        )
        .unwrap_err();

        assert_eq!(error, "WebKit could not remove profile data");
        assert!(registry.snapshot("browser-1", "workspace-1").is_ok());
    }

    #[test]
    fn legacy_webkit_reset_selects_only_records_for_managed_browser_hosts() {
        let hosts = std::collections::BTreeSet::from([
            "docs.example.com".to_owned(),
            "localhost".to_owned(),
        ]);

        assert!(website_record_matches("example.com", &hosts));
        assert!(website_record_matches("docs.example.com", &hosts));
        assert!(website_record_matches("localhost", &hosts));
        assert!(!website_record_matches("ample.com", &hosts));
        assert!(!website_record_matches("evil-example.com", &hosts));
        assert!(!website_record_matches("com", &hosts));
        assert!(!website_record_matches("tidebreak.invalid", &hosts));
    }
}

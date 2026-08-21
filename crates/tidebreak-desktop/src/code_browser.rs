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
use tidebreak_core::{BrowserGrantCapability, BrowserOrigin, BrowserOriginScope};
use tokio::sync::oneshot;
use url::Url;

#[cfg(target_os = "macos")]
use crate::browser_control::BROWSER_DATA_STORE_IDENTIFIER;
use crate::browser_control::{
    BrowserAgentAccess, BrowserController, BrowserLoadState, BrowserNavigationDecision,
    BrowserRegistry, BrowserSnapshot,
};

const BROWSER_LABEL_PREFIX: &str = "code-browser-";
const CODE_BROWSER_EVENT: &str = "code-browser:event";
const MAX_BROWSER_ID_CHARS: usize = 80;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const MAX_BROWSER_URL_CHARS: usize = 8_192;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeBrowserCommandRequest {
    workspace_id: String,
    browser_id: String,
    action: CodeBrowserAction,
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

#[tauri::command]
pub(crate) async fn code_browser_command(
    app: AppHandle,
    registry: tauri::State<'_, BrowserRegistry>,
    request: CodeBrowserCommandRequest,
) -> Result<BrowserSnapshot, String> {
    validated_workspace_id(&request.workspace_id)?;
    let label = browser_label(&request.browser_id)?;
    let existing = app.get_webview(&label);
    let registry = registry.inner().clone();

    match request.action {
        CodeBrowserAction::Create {
            url,
            bounds,
            visible,
        } => {
            if let Some(webview) = existing {
                registry.ensure_workspace(&request.browser_id, &request.workspace_id)?;
                set_bounds(&webview, bounds)?;
                set_visible(&webview, visible)?;
                registry.set_visible(&request.browser_id, &request.workspace_id, visible)?;
                return registry.snapshot(&request.browser_id, &request.workspace_id);
            }
            // A platform webview can disappear independently after a native
            // failure. Remove that same-workspace stale record before
            // allocating a fresh native instance; a cross-workspace record is
            // deliberately rejected rather than rebound.
            registry.remove(&request.browser_id, &request.workspace_id)?;
            create_browser(
                &app,
                &registry,
                &request.workspace_id,
                &request.browser_id,
                &label,
                &url,
                bounds,
                visible,
            )
        }
        CodeBrowserAction::Snapshot => match existing {
            Some(_) => registry.snapshot(&request.browser_id, &request.workspace_id),
            None => {
                registry.remove(&request.browser_id, &request.workspace_id)?;
                Ok(BrowserSnapshot::missing(
                    &request.browser_id,
                    &request.workspace_id,
                ))
            }
        },
        CodeBrowserAction::Close => {
            if let Some(webview) = existing {
                registry.ensure_workspace(&request.browser_id, &request.workspace_id)?;
                webview.close().map_err(browser_error)?;
            }
            registry.remove(&request.browser_id, &request.workspace_id)?;
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

#[allow(
    clippy::too_many_arguments,
    reason = "the single native creation boundary keeps browser identity and initial view state explicit"
)]
fn create_browser(
    app: &AppHandle,
    registry: &BrowserRegistry,
    workspace_id: &str,
    browser_id: &str,
    label: &str,
    url: &str,
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

    let instance_id = registry.register(browser_id, workspace_id, target.to_string(), visible)?;

    let navigation_main = main.clone();
    let navigation_browser = browser_id.to_owned();
    let navigation_workspace = workspace_id.to_owned();
    let navigation_renderer_url = renderer_url.clone();
    let navigation_registry = registry.clone();
    let popup_main = main.clone();
    let popup_browser = browser_id.to_owned();
    let popup_workspace = workspace_id.to_owned();
    let load_main = main.clone();
    let load_browser = browser_id.to_owned();
    let load_workspace = workspace_id.to_owned();
    let load_registry = registry.clone();
    let title_main = main.clone();
    let title_browser = browser_id.to_owned();
    let title_workspace = workspace_id.to_owned();
    let title_registry = registry.clone();
    let download_main = main.clone();
    let download_browser = browser_id.to_owned();
    let download_workspace = workspace_id.to_owned();

    let builder = WebviewBuilder::new(label, WebviewUrl::External(target));
    #[cfg(target_os = "macos")]
    let builder = builder.data_store_identifier(BROWSER_DATA_STORE_IDENTIFIER);
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
                BrowserNavigationDecision::Allow => true,
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
        .on_page_load(move |_webview, payload| {
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
        .on_download(move |_webview, event| {
            if let DownloadEvent::Requested { url, .. } = event {
                emit_event(
                    &download_main,
                    CodeBrowserEvent {
                        workspace_id: download_workspace.clone(),
                        browser_id: download_browser.clone(),
                        kind: "download_blocked",
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
            }
            false
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
    if !visible {
        if let Err(error) = webview.hide() {
            let _ = webview.close();
            registry.remove_instance(browser_id, workspace_id, instance_id);
            return Err(browser_error(error));
        }
    }
    registry.snapshot(browser_id, workspace_id)
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
        &[BrowserGrantCapability::BrowserControlOrigin],
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
            "Allow agents in this workspace to inspect and navigate {origin}?\n\nPage content is untrusted. Password and verification-code values stay private and require human takeover."
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
            "Allow agents in this workspace to inspect and navigate {origin_label}?\n\nChoose only this origin, or all loopback sites in this workspace so development ports can change without another prompt."
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

fn emit_event(main: &Webview, event: CodeBrowserEvent) {
    let _ = main.emit(CODE_BROWSER_EVENT, event);
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
    fn workspace_ids_are_required_and_bounded() {
        assert!(validated_workspace_id("workspace-123").is_ok());
        assert!(validated_workspace_id("").is_err());
        assert!(validated_workspace_id("workspace\nother").is_err());
        assert!(validated_workspace_id(&"w".repeat(MAX_WORKSPACE_ID_CHARS + 1)).is_err());
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
}

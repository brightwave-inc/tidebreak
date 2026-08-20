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
use url::Url;

const BROWSER_LABEL_PREFIX: &str = "code-browser-";
const CODE_BROWSER_EVENT: &str = "code-browser:event";
const MAX_BROWSER_ID_CHARS: usize = 80;
const MAX_BROWSER_URL_CHARS: usize = 8_192;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeBrowserCommandRequest {
    session_id: String,
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
    Close,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct CodeBrowserBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeBrowserSnapshot {
    exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CodeBrowserEvent {
    session_id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[tauri::command]
pub(crate) async fn code_browser_command(
    app: AppHandle,
    request: CodeBrowserCommandRequest,
) -> Result<CodeBrowserSnapshot, String> {
    let label = browser_label(&request.session_id)?;
    let existing = app.get_webview(&label);

    match request.action {
        CodeBrowserAction::Create {
            url,
            bounds,
            visible,
        } => {
            if let Some(webview) = existing {
                set_bounds(&webview, bounds)?;
                set_visible(&webview, visible)?;
                return snapshot(&webview);
            }
            create_browser(&app, &request.session_id, &label, &url, bounds, visible)
        }
        CodeBrowserAction::Snapshot => match existing {
            Some(webview) => snapshot(&webview),
            None => Ok(CodeBrowserSnapshot {
                exists: false,
                url: None,
            }),
        },
        CodeBrowserAction::Close => {
            if let Some(webview) = existing {
                webview.close().map_err(browser_error)?;
            }
            Ok(CodeBrowserSnapshot {
                exists: false,
                url: None,
            })
        }
        action => {
            let webview = existing.ok_or_else(|| "browser session is not open".to_owned())?;
            run_action(&app, &request.session_id, &webview, action)?;
            snapshot(&webview)
        }
    }
}

fn create_browser(
    app: &AppHandle,
    session_id: &str,
    label: &str,
    url: &str,
    bounds: CodeBrowserBounds,
    visible: bool,
) -> Result<CodeBrowserSnapshot, String> {
    let main = app
        .get_webview("main")
        .ok_or_else(|| "main webview is not available".to_owned())?;
    let window = app
        .get_window("main")
        .ok_or_else(|| "main window is not available".to_owned())?;
    let renderer_url = main.url().ok();
    let target = validated_url(url, renderer_url.as_ref())?;
    let safe_bounds = validated_bounds(bounds)?;

    let navigation_main = main.clone();
    let navigation_session = session_id.to_owned();
    let navigation_renderer_url = renderer_url.clone();
    let popup_main = main.clone();
    let popup_session = session_id.to_owned();
    let load_main = main.clone();
    let load_session = session_id.to_owned();
    let title_main = main.clone();
    let title_session = session_id.to_owned();
    let download_main = main.clone();
    let download_session = session_id.to_owned();

    let builder = WebviewBuilder::new(label, WebviewUrl::External(target))
        .on_navigation(move |url| {
            if validated_url(url.as_str(), navigation_renderer_url.as_ref()).is_ok() {
                true
            } else {
                emit_event(
                    &navigation_main,
                    CodeBrowserEvent {
                        session_id: navigation_session.clone(),
                        kind: "navigation_blocked",
                        url: Some(url.to_string()),
                        title: None,
                        message: Some(
                            "This address is not allowed in the in-app browser".to_owned(),
                        ),
                    },
                );
                false
            }
        })
        .on_new_window(move |url, _features| {
            emit_event(
                &popup_main,
                CodeBrowserEvent {
                    session_id: popup_session.clone(),
                    kind: "popup_blocked",
                    url: Some(url.to_string()),
                    title: None,
                    message: None,
                },
            );
            NewWindowResponse::Deny
        })
        .on_page_load(move |_webview, payload| {
            emit_event(
                &load_main,
                CodeBrowserEvent {
                    session_id: load_session.clone(),
                    kind: match payload.event() {
                        PageLoadEvent::Started => "navigation_started",
                        PageLoadEvent::Finished => "navigation_finished",
                    },
                    url: Some(payload.url().to_string()),
                    title: None,
                    message: None,
                },
            );
        })
        .on_document_title_changed(move |webview, title| {
            emit_event(
                &title_main,
                CodeBrowserEvent {
                    session_id: title_session.clone(),
                    kind: "title_changed",
                    url: webview.url().ok().map(|url| url.to_string()),
                    title: Some(title.chars().take(160).collect()),
                    message: None,
                },
            );
        })
        .on_download(move |_webview, event| {
            if let DownloadEvent::Requested { url, .. } = event {
                emit_event(
                    &download_main,
                    CodeBrowserEvent {
                        session_id: download_session.clone(),
                        kind: "download_blocked",
                        url: Some(url.to_string()),
                        title: None,
                        message: None,
                    },
                );
            }
            false
        });

    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(safe_bounds.x, safe_bounds.y),
            LogicalSize::new(safe_bounds.width, safe_bounds.height),
        )
        .map_err(browser_error)?;
    if !visible {
        webview.hide().map_err(browser_error)?;
    }
    snapshot(&webview)
}

fn run_action(
    app: &AppHandle,
    session_id: &str,
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
        | CodeBrowserAction::Close => Err(format!(
            "browser action is not valid for the open session {session_id}"
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

fn snapshot(webview: &Webview) -> Result<CodeBrowserSnapshot, String> {
    Ok(CodeBrowserSnapshot {
        exists: true,
        url: Some(webview.url().map_err(browser_error)?.to_string()),
    })
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

fn browser_label(session_id: &str) -> Result<String, String> {
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

fn emit_event(main: &Webview, event: CodeBrowserEvent) {
    let _ = main.emit(CODE_BROWSER_EVENT, event);
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

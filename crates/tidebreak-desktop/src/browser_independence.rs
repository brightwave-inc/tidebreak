//! Confine agent writes to a native window that cannot acquire human focus.
//!
//! DOM input is still a write: a page's event handler can call focus(). Shared
//! tabs remain readable, but require an independent host before agent mutation.

use tauri::Webview;
use uuid::Uuid;

use crate::browser_control::{BrowserObservationFence, BrowserRegistry};

pub(crate) const SHARED_TAB: &str = "This tab has no independent agent host. Open the same URL with browser_open, then take a new snapshot and continue in that tab. No input was sent.";
pub(crate) const UNAVAILABLE: &str = "Independent in-app browser input is unavailable in this build. Use chrome_connect with mode managed for an independent browser. No input was sent.";

pub(crate) fn available() -> bool {
    cfg!(all(target_os = "macos", feature = "independent-wk-host"))
}

pub(crate) fn require_available() -> Result<(), String> {
    if available() {
        Ok(())
    } else {
        Err(UNAVAILABLE.to_owned())
    }
}

fn validate_host(enabled: bool, hosted: bool) -> Result<(), String> {
    if !enabled {
        return Err(UNAVAILABLE.to_owned());
    }
    if !hosted {
        return Err(SHARED_TAB.to_owned());
    }
    Ok(())
}

pub(crate) fn require_host(
    webview: &Webview,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    fence: BrowserObservationFence,
) -> Result<(), String> {
    #[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
    {
        validate_host(available(), crate::agent_browser_host::has_host(webview)?)?;
        crate::agent_browser_host::authorize_input(
            webview,
            registry,
            capability_id,
            workspace_id,
            fence.instance_id,
        )
    }
    #[cfg(not(all(target_os = "macos", feature = "independent-wk-host")))]
    {
        let _ = (webview, registry, capability_id, workspace_id, fence);
        validate_host(available(), false)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn require_native_host(
    webview: &Webview,
    view: &objc2_web_kit::WKWebView,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    fence: BrowserObservationFence,
) -> Result<(), String> {
    require_host(webview, registry, capability_id, workspace_id, fence)?;
    #[cfg(feature = "independent-wk-host")]
    {
        crate::agent_browser_host::verify_native_window(view)
    }
    #[cfg(not(feature = "independent-wk-host"))]
    {
        let _ = view;
        Err(UNAVAILABLE.to_owned())
    }
}

#[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
struct NavigationSubmission {
    sender: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    deadline: tokio::time::Instant,
}

#[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
struct NavigationCancellation(std::sync::Arc<std::sync::Mutex<NavigationSubmission>>);

#[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
impl Drop for NavigationCancellation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.sender.take();
        }
    }
}

#[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
fn submit_navigation(
    state: &std::sync::Mutex<NavigationSubmission>,
    submit: impl FnOnce() -> Result<(), String>,
) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    if state
        .sender
        .as_ref()
        .is_none_or(tokio::sync::oneshot::Sender::is_closed)
        || tokio::time::Instant::now() >= state.deadline
    {
        return;
    }
    let result = submit();
    if let Some(sender) = state.sender.take() {
        let _ = sender.send(result);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn navigate(
    webview: &Webview,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    browser_id: &str,
    origin: &tidebreak_core::BrowserOrigin,
    fence: BrowserObservationFence,
    destination: &url::Url,
    destination_origin: &tidebreak_core::BrowserOrigin,
) -> Result<(), String> {
    require_host(webview, registry, capability_id, workspace_id, fence)?;
    #[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
    {
        use objc2_foundation::{NSString, NSURLRequest, NSURL};
        use std::sync::{Arc, Mutex};
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        let state = Arc::new(Mutex::new(NavigationSubmission {
            sender: Some(sender),
            deadline,
        }));
        let _cancellation = NavigationCancellation(Arc::clone(&state));
        let webview_handle = webview.clone();
        let registry = registry.clone();
        let workspace_id = workspace_id.to_owned();
        let browser_id = browser_id.to_owned();
        let origin = origin.clone();
        let destination = destination.to_string();
        let destination_origin = destination_origin.clone();
        crate::browser_native_webview::with_browser_webview(webview, move |view| {
            submit_navigation(&state, || {
                registry.authorize_native_action_phase(
                    capability_id,
                    &browser_id,
                    &workspace_id,
                    &origin,
                    fence,
                )?;
                require_native_host(
                    &webview_handle,
                    view,
                    &registry,
                    capability_id,
                    &workspace_id,
                    fence,
                )?;
                if !matches!(
                    registry.authorize_agent_navigation(
                        capability_id,
                        &browser_id,
                        &origin,
                        fence,
                        &destination,
                        &destination_origin
                    )?,
                    crate::browser_control::BrowserNavigationDecision::Allow
                ) {
                    return Err("browser destination is not shared for navigation".to_owned());
                }
                let url = NSURL::URLWithString(&NSString::from_str(&destination))
                    .ok_or_else(|| "browser destination URL is not valid".to_owned())?;
                let request = NSURLRequest::requestWithURL(&url);
                // Verify and submit in one main-thread callback. Loading a page
                // can run autofocus before any subsequent agent action.
                unsafe { view.loadRequest(&request) }
                    .ok_or_else(|| "browser navigation could not start".to_owned())?;
                Ok(())
            });
        })?;
        tokio::time::timeout_at(deadline, receiver)
            .await
            .map_err(|_| "independent browser navigation timed out".to_owned())?
            .map_err(|_| "independent browser navigation was interrupted".to_owned())?
    }
    #[cfg(not(all(target_os = "macos", feature = "independent-wk-host")))]
    {
        let _ = (browser_id, origin, destination, destination_origin);
        Err(UNAVAILABLE.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_writes_require_both_a_host_and_the_enabled_executor() {
        let mut writes = Vec::new();
        for (enabled, hosted) in [(false, false), (false, true), (true, false), (true, true)] {
            let outcome = validate_host(enabled, hosted).map(|()| writes.push((enabled, hosted)));
            if !enabled {
                assert_eq!(outcome.unwrap_err(), UNAVAILABLE);
            } else if !hosted {
                assert_eq!(outcome.unwrap_err(), SHARED_TAB);
            } else {
                outcome.unwrap();
            }
        }
        assert_eq!(writes, [(true, true)]);
    }

    #[test]
    fn feature_disabled_build_cannot_authorize_browser_open() {
        assert_eq!(
            require_available().is_ok(),
            cfg!(all(target_os = "macos", feature = "independent-wk-host"))
        );
    }
    #[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
    #[test]
    fn canceled_navigation_never_loads_an_autofocusing_page() {
        use std::sync::{Arc, Mutex};
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        let state = Arc::new(Mutex::new(NavigationSubmission {
            sender: Some(sender),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        }));
        drop(NavigationCancellation(Arc::clone(&state)));
        submit_navigation(&state, || panic!("a canceled navigation loaded a page"));
    }

    #[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
    #[test]
    fn queued_navigation_checks_deadline_and_receiver_before_loading() {
        use std::sync::Mutex;
        for expired in [false, true] {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let deadline = if expired {
                tokio::time::Instant::now()
            } else {
                tokio::time::Instant::now() + std::time::Duration::from_secs(1)
            };
            let state = Mutex::new(NavigationSubmission {
                sender: Some(sender),
                deadline,
            });
            if !expired {
                drop(receiver);
            }
            submit_navigation(&state, || {
                panic!("an expired or abandoned navigation loaded a page")
            });
        }
    }

    #[cfg(all(target_os = "macos", feature = "independent-wk-host"))]
    #[tokio::test]
    async fn active_navigation_returns_the_guard_result_once() {
        use std::sync::Mutex;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let state = Mutex::new(NavigationSubmission {
            sender: Some(sender),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        });
        submit_navigation(&state, || Err(SHARED_TAB.to_owned()));
        assert_eq!(receiver.await.unwrap().unwrap_err(), SHARED_TAB);
        submit_navigation(&state, || panic!("navigation submitted twice"));
    }
}

//! Keep agent-owned WK views in a window that cannot acquire keyboard focus.
//!
//! The browser panel still displays the original WK view. Only its native
//! window differs. The agent-open lease selects this host; renderer ids do not.
//! Only text insertion uses native input. Pointer actions remain DOM operations
//! because WK can start a system drag session after a page handles mouse input.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, PhysicalPosition, Rect, Webview, Window,
    WindowBuilder,
};
use uuid::Uuid;

use crate::{browser_control::BrowserRegistry, code_browser::CodeBrowserBounds};

const OPEN_TICKET_TTL: Duration = Duration::from_secs(15);
const HOST_PREFIX: &str = "agent-browser-host-";

#[derive(Clone)]
struct Opening {
    id: Uuid,
    capability_id: Uuid,
    workspace_id: String,
    deadline: Instant,
}

impl Opening {
    fn authorize(&self, workspace_id: &str, now: Instant) -> Result<(), String> {
        if self.workspace_id != workspace_id {
            return Err("agent browser opening belongs to another workspace".to_owned());
        }
        if now >= self.deadline {
            return Err("agent browser opening expired".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Host {
    opening_id: Uuid,
    instance_id: u64,
    workspace_id: String,
    window: Window,
    main: Window,
    bounds: CodeBrowserBounds,
    human_takeover_started: bool,
}

#[derive(Default)]
struct HostState {
    opening: HashMap<String, Opening>,
    live: HashMap<String, Host>,
    // Retain canceled ids for the app session. A renderer can deliver Create
    // after any delay; expiration would turn a canceled opening into a user tab.
    canceled: HashSet<String>,
}

#[derive(Clone, Default)]
struct Hosts(Arc<Mutex<HostState>>);

fn hosts(app: &AppHandle) -> Hosts {
    if app.try_state::<Hosts>().is_none() {
        app.manage(Hosts::default());
    }
    app.state::<Hosts>().inner().clone()
}

fn release_opening(opening: &mut HashMap<String, Opening>, browser_id: &str, opening_id: Uuid) {
    if opening
        .get(browser_id)
        .is_some_and(|current| current.id == opening_id)
    {
        opening.remove(browser_id);
    }
}

/// Native-only reservation held across the renderer's tab creation and load.
/// Dropping an unfinished request closes only that request's native instance.
pub(crate) struct OpenLease {
    app: AppHandle,
    registry: BrowserRegistry,
    browser_id: String,
    opening_id: Uuid,
    finished: bool,
}

impl OpenLease {
    pub(crate) fn finish(mut self) {
        self.finished = true;
        let store = hosts(&self.app);
        let mut state = store
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        release_opening(&mut state.opening, &self.browser_id, self.opening_id);
    }
}

impl Drop for OpenLease {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let store = hosts(&self.app);
        let removed = {
            let mut state = store
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let owns_opening = state
                .opening
                .get(&self.browser_id)
                .is_some_and(|opening| opening.id == self.opening_id);
            release_opening(&mut state.opening, &self.browser_id, self.opening_id);
            if owns_opening
                && !state
                    .live
                    .get(&self.browser_id)
                    .is_some_and(|host| host.human_takeover_started)
            {
                state.canceled.insert(self.browser_id.clone());
            }
            if state.live.get(&self.browser_id).is_some_and(|host| {
                host.opening_id == self.opening_id && !host.human_takeover_started
            }) {
                state.live.remove(&self.browser_id)
            } else {
                None
            }
        };
        if let Some(host) = removed {
            for view in host.window.webviews() {
                let _ = crate::code_browser::close_browser_webview(&view);
            }
            let _ = host.window.destroy();
            self.registry
                .remove_instance(&self.browser_id, &host.workspace_id, host.instance_id);
            crate::code_browser::emit_agent_lifecycle_event(
                &self.app,
                &host.workspace_id,
                &self.browser_id,
                "agent_closed_tab",
                None,
            );
        }
    }
}

pub(crate) fn reserve_open(
    app: &AppHandle,
    registry: &BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
    capability_id: Uuid,
) -> Result<OpenLease, String> {
    let store = hosts(app);
    let mut state = store
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if state.opening.contains_key(browser_id)
        || state.live.contains_key(browser_id)
        || state.canceled.contains(browser_id)
    {
        return Err("agent browser already has a native host".to_owned());
    }
    let opening_id = Uuid::new_v4();
    state.opening.insert(
        browser_id.to_owned(),
        Opening {
            id: opening_id,
            capability_id,
            workspace_id: workspace_id.to_owned(),
            deadline: Instant::now() + OPEN_TICKET_TTL,
        },
    );
    Ok(OpenLease {
        app: app.clone(),
        registry: registry.clone(),
        browser_id: browser_id.to_owned(),
        opening_id,
        finished: false,
    })
}

fn opening_for_create(state: &HostState, browser_id: &str) -> Result<Option<Opening>, String> {
    if state.canceled.contains(browser_id) {
        return Err("agent browser opening was canceled".to_owned());
    }
    Ok(state.opening.get(browser_id).cloned())
}

/// Create only for a live native reservation and recheck the capability after
/// the renderer round trip. Unreserved user tabs stay in the main window.
#[allow(
    clippy::too_many_arguments,
    reason = "native host creation keeps the opening authority and initial view placement explicit"
)]
pub(crate) fn create_if_reserved(
    app: &AppHandle,
    registry: &BrowserRegistry,
    browser_id: &str,
    workspace_id: &str,
    instance_id: u64,
    main: &Window,
    bounds: CodeBrowserBounds,
    origin: &tidebreak_core::BrowserOrigin,
) -> Result<Option<Window>, String> {
    let store = hosts(app);
    let opening = opening_for_create(
        &store
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        browser_id,
    )?;
    let Some(opening) = opening else {
        return Ok(None);
    };
    opening.authorize(workspace_id, Instant::now())?;
    let authorized_workspace = registry.authorize_agent_open(
        opening.capability_id,
        &tidebreak_core::OwnerId::local(),
        origin,
    )?;
    if authorized_workspace != workspace_id {
        return Err("agent browser opening no longer belongs to this workspace".to_owned());
    }
    registry.bind_independent_open(opening.capability_id, browser_id, instance_id)?;
    let window = WindowBuilder::new(app, format!("{HOST_PREFIX}{}", opening.id.simple()))
        .title("Tidebreak agent browser")
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .focusable(false)
        .focused(false)
        .visible(false)
        .inner_size(bounds.width, bounds.height)
        .parent(main)
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    let prepare = (|| {
        window
            .set_ignore_cursor_events(true)
            .map_err(|error| error.to_string())?;
        place_host(&window, main, bounds)?;
        if window.is_focused().map_err(|error| error.to_string())? {
            return Err("agent browser host acquired keyboard focus".to_owned());
        }
        let mut state = store
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.opening.get(browser_id).is_some_and(|current| {
            current.id == opening.id && current.authorize(workspace_id, Instant::now()).is_ok()
        }) {
            return Err("agent browser opening was canceled".to_owned());
        }
        state.live.insert(
            browser_id.to_owned(),
            Host {
                opening_id: opening.id,
                instance_id,
                workspace_id: workspace_id.to_owned(),
                window: window.clone(),
                main: main.clone(),
                bounds,
                human_takeover_started: false,
            },
        );
        Ok(())
    })();
    if let Err(error) = prepare {
        let _ = window.destroy();
        return Err(error);
    }
    Ok(Some(window))
}

fn physical_origin(
    main_x: i32,
    main_y: i32,
    scale: f64,
    bounds: CodeBrowserBounds,
) -> Result<PhysicalPosition<i32>, String> {
    let x = f64::from(main_x) + bounds.x * scale;
    let y = f64::from(main_y) + bounds.y * scale;
    if !scale.is_finite()
        || scale <= 0.0
        || !x.is_finite()
        || !y.is_finite()
        || x < f64::from(i32::MIN)
        || x > f64::from(i32::MAX)
        || y < f64::from(i32::MIN)
        || y > f64::from(i32::MAX)
    {
        return Err("agent browser host position is not valid".to_owned());
    }
    Ok(PhysicalPosition::new(x.round() as i32, y.round() as i32))
}

fn place_host(window: &Window, main: &Window, bounds: CodeBrowserBounds) -> Result<(), String> {
    let origin = main.inner_position().map_err(|error| error.to_string())?;
    let scale = main.scale_factor().map_err(|error| error.to_string())?;
    window
        .set_position(physical_origin(origin.x, origin.y, scale, bounds)?)
        .map_err(|error| error.to_string())?;
    window
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|error| error.to_string())
}

fn host_for_view(webview: &Webview) -> Result<Option<(String, Host)>, String> {
    let store = hosts(webview.app_handle());
    let state = store
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current_window = webview.window();
    let found = state
        .live
        .iter()
        .find(|(_, host)| host.window.label() == current_window.label());
    if let Some((browser_id, host)) = found {
        if crate::code_browser::browser_label(browser_id)? != webview.label() {
            return Err("agent browser view does not match its native host".to_owned());
        }
        Ok(Some((browser_id.clone(), host.clone())))
    } else if current_window.label().starts_with(HOST_PREFIX) {
        Err("agent browser native host is no longer registered".to_owned())
    } else {
        Ok(None)
    }
}

pub(crate) fn has_host(webview: &Webview) -> Result<bool, String> {
    Ok(host_for_view(webview)?.is_some())
}

/// Check native ownership again immediately before each page or input phase.
pub(crate) fn authorize_input(
    webview: &Webview,
    registry: &BrowserRegistry,
    capability_id: Uuid,
    workspace_id: &str,
    instance_id: u64,
) -> Result<(), String> {
    let Some((browser_id, host)) = host_for_view(webview)? else {
        return Err("trusted input requires an agent-owned browser host".to_owned());
    };
    if host.workspace_id != workspace_id
        || host.instance_id != instance_id
        || host.human_takeover_started
    {
        return Err("agent browser host ownership changed".to_owned());
    }
    let authorized_workspace = registry.authorize_agent_close(capability_id, &browser_id)?;
    if authorized_workspace != workspace_id {
        return Err("agent browser host belongs to another workspace".to_owned());
    }
    Ok(())
}

/// Recheck the still-live opening in the main-thread callback that starts
/// external content. A canceled or replaced opening must stay inert.
pub(crate) fn authorize_initial_load(
    webview: &Webview,
    registry: &BrowserRegistry,
    target: &url::Url,
) -> Result<(), String> {
    let Some((browser_id, host)) = host_for_view(webview)? else {
        return Err("agent browser host is unavailable before navigation".to_owned());
    };
    let opening = hosts(webview.app_handle())
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .opening
        .get(&browser_id)
        .cloned()
        .ok_or_else(|| "agent browser opening was canceled".to_owned())?;
    if opening.id != host.opening_id || host.human_takeover_started {
        return Err("agent browser opening changed before navigation".to_owned());
    }
    opening.authorize(&host.workspace_id, Instant::now())?;
    let origin = tidebreak_core::BrowserOrigin::from_url(target.as_str())
        .ok_or_else(|| "browser destination has no HTTP origin".to_owned())?;
    let workspace = registry.authorize_agent_open(
        opening.capability_id,
        &tidebreak_core::OwnerId::local(),
        &origin,
    )?;
    if workspace != host.workspace_id {
        return Err("agent browser opening changed workspace".to_owned());
    }
    Ok(())
}

pub(crate) fn verify_native_window(view: &objc2_web_kit::WKWebView) -> Result<(), String> {
    let native_view: &objc2_app_kit::NSView =
        unsafe { &*(view as *const _ as *const objc2_app_kit::NSView) };
    let window = native_view
        .window()
        .ok_or_else(|| "agent browser host has no native window".to_owned())?;
    if window.canBecomeKeyWindow()
        || window.canBecomeMainWindow()
        || window.isKeyWindow()
        || window.isMainWindow()
        || !window.ignoresMouseEvents()
    {
        return Err("agent browser host cannot preserve independent input".to_owned());
    }
    Ok(())
}

pub(crate) fn set_bounds(webview: &Webview, bounds: CodeBrowserBounds) -> Result<bool, String> {
    let Some((browser_id, host)) = host_for_view(webview)? else {
        return Ok(false);
    };
    place_host(&host.window, &host.main, bounds)?;
    webview
        .set_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(bounds.width, bounds.height).into(),
        })
        .map_err(|error| error.to_string())?;
    if let Some(current) = hosts(webview.app_handle())
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live
        .get_mut(&browser_id)
    {
        if current.opening_id != host.opening_id || current.instance_id != host.instance_id {
            return Err("agent browser host changed during placement".to_owned());
        }
        current.bounds = bounds;
    }
    Ok(true)
}

pub(crate) fn set_visible(webview: &Webview, visible: bool) -> Result<bool, String> {
    let Some((_, host)) = host_for_view(webview)? else {
        return Ok(false);
    };
    if visible {
        place_host(&host.window, &host.main, host.bounds)?;
    }
    // Window::show uses makeKeyAndOrderFront in Tao. Use public orderFront
    // directly so this path never asks AppKit for keyboard ownership.
    let on_main_thread = objc2::MainThreadMarker::new().is_some();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let canceled = Arc::new(AtomicBool::new(false));
    let callback_canceled = Arc::clone(&canceled);
    let deadline = Instant::now() + Duration::from_secs(3);
    crate::browser_native_webview::with_browser_webview(webview, move |view| {
        if callback_canceled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return;
        }
        let result = (|| {
            let native_view: &objc2_app_kit::NSView =
                unsafe { &*(view as *const _ as *const objc2_app_kit::NSView) };
            let window = native_view
                .window()
                .ok_or_else(|| "agent browser host has no native window".to_owned())?;
            if window.canBecomeKeyWindow()
                || window.canBecomeMainWindow()
                || window.isKeyWindow()
                || window.isMainWindow()
                || !window.ignoresMouseEvents()
            {
                return Err("agent browser host cannot preserve independent input".to_owned());
            }
            // Keep the view renderable while its preview window is ordered out.
            native_view.setHidden(false);
            if visible {
                window.orderFront(None)
            } else {
                window.orderOut(None)
            }
            Ok(())
        })();
        let _ = sender.send(result);
    })?;
    let result = visibility_reply(&receiver, on_main_thread);
    canceled.store(true, Ordering::Release);
    result?;
    Ok(true)
}

fn visibility_reply(
    receiver: &std::sync::mpsc::Receiver<Result<(), String>>,
    on_main_thread: bool,
) -> Result<(), String> {
    if on_main_thread {
        // The pinned runtime executes WithWebview inline on its event thread.
        // Refuse if that changes; never wait for this thread to service itself.
        receiver.try_recv().map_err(|_| {
            "agent browser visibility was not dispatched on the main thread".to_owned()
        })?
    } else {
        receiver
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "agent browser visibility update timed out".to_owned())?
    }
}

/// Mark explicit takeover before awaiting the broker gate. A canceled
/// browser_open waiter must not close a window while the user takes it over.
pub(crate) fn begin_human_takeover(webview: &Webview, workspace_id: &str) -> Result<(), String> {
    let Some((browser_id, host)) = host_for_view(webview)? else {
        return Ok(());
    };
    if host.workspace_id != workspace_id {
        return Err("agent browser host belongs to another workspace".to_owned());
    }
    if let Some(current) = hosts(webview.app_handle())
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .live
        .get_mut(&browser_id)
    {
        if current.opening_id != host.opening_id || current.instance_id != host.instance_id {
            return Err("agent browser host changed before takeover".to_owned());
        }
        current.human_takeover_started = true;
    }
    Ok(())
}

/// The caller first drains agent dispatch with `take_human_control`.
/// Only this explicit transition moves a WK view into the interactive window.
pub(crate) fn take_human_control(webview: &Webview) -> Result<(), String> {
    let Some((browser_id, host)) = host_for_view(webview)? else {
        return Ok(());
    };
    if let Err(error) = webview.reparent(&host.main) {
        // Tauri updates its handle before its native reparent completes.
        // Restore that handle if the native operation fails.
        let _ = webview.reparent(&host.window);
        return Err(format!(
            "could not move the browser into human control: {error}"
        ));
    }
    if let Err(error) = crate::agent_browser_dialogs::with_guarded_view(webview, |view| {
        crate::agent_browser_dialogs::restore(view)
    }) {
        let _ = webview.reparent(&host.window);
        return Err(error);
    }
    {
        let store = hosts(webview.app_handle());
        let mut state = store
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.live.get(&browser_id).is_some_and(|current| {
            current.opening_id == host.opening_id && current.instance_id == host.instance_id
        }) {
            state.live.remove(&browser_id);
        }
    }
    let placed = webview
        .set_bounds(Rect {
            position: LogicalPosition::new(host.bounds.x, host.bounds.y).into(),
            size: LogicalSize::new(host.bounds.width, host.bounds.height).into(),
        })
        .map_err(|error| error.to_string());
    let _ = host.window.destroy();
    placed
}

/// Call after the managed WK view and its observers have closed.
pub(crate) fn close_empty_host(app: &AppHandle, window: &Window) {
    let store = hosts(app);
    let mut state = store
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let browser_id = state
        .live
        .iter()
        .find(|(_, host)| host.window.label() == window.label())
        .map(|(browser_id, _)| browser_id.clone());
    if let Some(browser_id) = browser_id {
        state.live.remove(&browser_id);
        drop(state);
        let _ = window.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_round_trip_requires_same_workspace_and_live_opening() {
        let now = Instant::now();
        let opening = Opening {
            id: Uuid::new_v4(),
            capability_id: Uuid::new_v4(),
            workspace_id: "workspace-a".to_owned(),
            deadline: now + Duration::from_secs(1),
        };
        assert!(opening.authorize("workspace-a", now).is_ok());
        assert!(opening.authorize("workspace-b", now).is_err());
        assert!(opening.authorize("workspace-a", opening.deadline).is_err());
    }

    #[test]
    fn canceled_opening_cannot_remove_a_replacement_ticket() {
        let original = Uuid::new_v4();
        let replacement = Uuid::new_v4();
        let mut opening = HashMap::from([(
            "agent-browser".to_owned(),
            Opening {
                id: replacement,
                capability_id: Uuid::new_v4(),
                workspace_id: "workspace".to_owned(),
                deadline: Instant::now() + OPEN_TICKET_TTL,
            },
        )]);
        release_opening(&mut opening, "agent-browser", original);
        assert_eq!(opening.get("agent-browser").unwrap().id, replacement);
        release_opening(&mut opening, "agent-browser", replacement);
        assert!(opening.is_empty());
    }

    #[test]
    fn late_renderer_create_cannot_reclassify_a_canceled_agent_tab() {
        let mut state = HostState::default();
        state.canceled.insert("native-issued-id".to_owned());
        assert!(opening_for_create(&state, "native-issued-id").is_err());
        assert!(opening_for_create(&state, "user-tab").unwrap().is_none());
    }

    #[test]
    fn main_thread_visibility_never_waits_for_a_queued_callback() {
        let (_sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert!(visibility_reply(&receiver, true)
            .unwrap_err()
            .contains("main thread"));
    }

    #[test]
    fn visibility_preserves_the_native_result_for_inline_and_background_dispatch() {
        for main_thread in [true, false] {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            sender
                .send(Err("native ownership changed".to_owned()))
                .unwrap();
            assert_eq!(
                visibility_reply(&receiver, main_thread).unwrap_err(),
                "native ownership changed"
            );
        }
    }

    #[test]
    fn viewport_placement_uses_main_content_origin_and_display_scale() {
        let bounds = CodeBrowserBounds {
            x: 80.0,
            y: 100.0,
            width: 400.0,
            height: 300.0,
        };
        assert_eq!(
            physical_origin(-1920, -160, 2.0, bounds).unwrap(),
            PhysicalPosition::new(-1760, 40)
        );
        assert!(physical_origin(0, 0, f64::NAN, bounds).is_err());
        assert!(physical_origin(i32::MAX, 0, 2.0, bounds).is_err());
    }
}

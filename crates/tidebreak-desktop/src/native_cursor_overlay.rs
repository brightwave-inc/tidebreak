//! Passive native cursor decoration. Only trusted executor events enter here.
//! The panel never receives input or grants authority to perform an action.

use crate::computer_use_action::ComputerUseActionEvent;
use tauri::AppHandle;

pub(crate) fn update(app: &AppHandle, event: &ComputerUseActionEvent) {
    #[cfg(target_os = "macos")]
    native::update(app, event);
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}

/// Clear from the trusted Stop/revoke path, never from a renderer event.
pub(crate) fn clear_all(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    native::clear(app, None);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

pub(crate) fn clear_session(app: &AppHandle, session_id: &str) {
    #[cfg(target_os = "macos")]
    native::clear(app, Some(session_id.to_owned()));
    #[cfg(not(target_os = "macos"))]
    let _ = (app, session_id);
}

#[cfg(any(target_os = "macos", test))]
mod policy {
    use crate::computer_use_action::{
        ComputerUseActionBounds, ComputerUseActionEvent, ComputerUseActionPhase,
        ComputerUseActionPoint, ComputerUseActionSource, ComputerUseCoordinateFrame,
    };

    use std::collections::HashSet;

    /// Match completion to a running action that Stop/revoke has not invalidated.
    /// The cap bounds abandoned actions even if their executor never settles.
    #[derive(Default)]
    pub(super) struct ActionGate {
        revision: u64,
        pending: HashSet<(String, String)>,
    }

    impl ActionGate {
        pub(super) fn register(&mut self, event: &ComputerUseActionEvent) -> Option<u64> {
            if event.session_id.is_empty() || event.action_id.is_empty() {
                return None;
            }
            let key = (event.session_id.clone(), event.action_id.clone());
            if matches!(event.phase, ComputerUseActionPhase::Running) {
                if self.pending.len() >= 128 {
                    self.clear(None);
                }
                self.pending.insert(key);
                Some(self.revision)
            } else {
                self.pending.remove(&key).then_some(self.revision)
            }
        }

        pub(super) fn clear(&mut self, session_id: Option<&str>) {
            self.revision = self.revision.saturating_add(1);
            if let Some(session_id) = session_id {
                self.pending.retain(|(session, _)| session != session_id);
            } else {
                self.pending.clear();
            }
        }

        pub(super) fn is_current(&self, revision: u64) -> bool {
            self.revision == revision
        }
    }

    pub(super) const VISIBLE_MILLIS: i64 = 1_500;
    pub(super) const SIDE: f64 = 40.0;
    pub(super) const TIP_X: f64 = 12.0;
    pub(super) const TIP_Y: f64 = 11.0;

    #[derive(Clone)]
    pub(super) struct Position {
        pub action_id: String,
        pub session_id: String,
        pub bundle_id: String,
        pub window_id: u32,
        pub point: ComputerUseActionPoint,
        pub bounds: ComputerUseActionBounds,
        pub started_at: i64,
        pub expires_at: i64,
    }

    pub(super) fn position(event: &ComputerUseActionEvent, now: i64) -> Option<Position> {
        if !matches!(event.source, ComputerUseActionSource::Native)
            || !matches!(event.phase, ComputerUseActionPhase::Completed)
            || !matches!(event.coordinate_frame, ComputerUseCoordinateFrame::Screen)
            || event.visible_until_millis <= now
        {
            return None;
        }
        let point = event.point?;
        let bounds = event.target_bounds?;
        let window_id = event.window_id.filter(|id| *id != 0)?;
        let bundle_id = event.bundle_id.as_ref().filter(|id| !id.is_empty())?;
        if ![
            point.x,
            point.y,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
        ]
        .into_iter()
        .all(f64::is_finite)
            || bounds.width <= 0.0
            || bounds.height <= 0.0
            || point.x < bounds.x
            || point.y < bounds.y
            || point.x >= bounds.x + bounds.width
            || point.y >= bounds.y + bounds.height
        {
            return None;
        }
        Some(Position {
            action_id: event.action_id.clone(),
            session_id: event.session_id.clone(),
            bundle_id: bundle_id.clone(),
            window_id,
            point,
            bounds,
            started_at: event.started_at_millis,
            expires_at: event
                .visible_until_millis
                .min(now.saturating_add(VISIBLE_MILLIS)),
        })
    }

    /// Quartz uses a top-left origin; AppKit screen coordinates use bottom-left.
    pub(super) fn panel_origin(point: ComputerUseActionPoint, primary_height: f64) -> (f64, f64) {
        (point.x - TIP_X, primary_height - point.y - (SIDE - TIP_Y))
    }

    pub(super) fn same_bounds(a: ComputerUseActionBounds, b: ComputerUseActionBounds) -> bool {
        a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height
    }

    pub(super) fn can_replace(current: &Position, event: &ComputerUseActionEvent) -> bool {
        event.started_at_millis >= current.started_at
            || (event.action_id == current.action_id && event.session_id == current.session_id)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::policy::{self, Position};
    use super::*;
    use crate::computer_use_action::{ComputerUseActionBounds, ComputerUseActionSource};
    use objc2::rc::Retained;
    use objc2::runtime::NSObjectProtocol;
    use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSBackingStoreType, NSBezierPath, NSColor, NSFloatingWindowLevel, NSLineJoinStyle, NSPanel,
        NSRunningApplication, NSScreen, NSView, NSWindow, NSWindowAnimationBehavior,
        NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};
    use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::{Duration, Instant};

    static ACTION_GATE: LazyLock<Mutex<policy::ActionGate>> =
        LazyLock::new(|| Mutex::new(policy::ActionGate::default()));

    fn action_gate() -> std::sync::MutexGuard<'static, policy::ActionGate> {
        ACTION_GATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    define_class!(
        // SAFETY: NSPanel has no additional subclassing requirements. All
        // access stays on the main thread, and no Rust Drop is implemented.
        #[unsafe(super(NSPanel, NSWindow, objc2_app_kit::NSResponder))]
        #[thread_kind = MainThreadOnly]
        #[name = "TidebreakPassiveCursorPanel"]
        struct CursorPanel;

        unsafe impl NSObjectProtocol for CursorPanel {}

        impl CursorPanel {
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key_window(&self) -> bool { false }
            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main_window(&self) -> bool { false }
        }
    );

    define_class!(
        // SAFETY: NSView has no additional subclassing requirements. Drawing
        // runs in AppKit's current graphics context on the main thread.
        #[unsafe(super(NSView, objc2_app_kit::NSResponder))]
        #[thread_kind = MainThreadOnly]
        #[name = "TidebreakPassiveCursorView"]
        struct CursorView;

        unsafe impl NSObjectProtocol for CursorView {}

        impl CursorView {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool { true }
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool { false }
            #[unsafe(method(isAccessibilityElement))]
            fn is_accessibility_element(&self) -> bool { false }
            #[unsafe(method(drawRect:))]
            fn draw(&self, _dirty: NSRect) {
                // Same Lucide mouse-pointer-2 path, ring, and colors as the
                // browser ghost. sRGB values convert its two OKLCH colors.
                let teal = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.589226, 0.592989, 1.0);
                let edge = NSColor::colorWithSRGBRed_green_blue_alpha(0.975890, 0.981233, 0.985275, 1.0);
                let ring_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.589226, 0.592989, 0.55);
                let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                    NSPoint::new(policy::TIP_X - 8.0, policy::TIP_Y - 8.0), NSSize::new(16.0, 16.0)));
                ring.setLineWidth(1.5);
                ring_color.setStroke();
                ring.stroke();
                let arrow = NSBezierPath::bezierPath();
                arrow.moveToPoint(NSPoint::new(12.0, 11.0));
                arrow.lineToPoint(NSPoint::new(19.07, 28.0));
                arrow.lineToPoint(NSPoint::new(21.58, 20.61));
                arrow.lineToPoint(NSPoint::new(29.0, 18.07));
                arrow.closePath();
                teal.setFill();
                arrow.fill();
                edge.setStroke();
                arrow.setLineWidth(2.0);
                arrow.setLineJoinStyle(NSLineJoinStyle::Round);
                arrow.stroke();
            }
        }
    );

    struct Overlay {
        panel: Retained<CursorPanel>,
        position: Position,
        alive: Arc<AtomicBool>,
    }

    thread_local! {
        static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    }

    fn hide(slot: &mut Option<Overlay>) {
        if let Some(overlay) = slot.take() {
            overlay.alive.store(false, Ordering::Release);
            overlay.panel.orderOut(None);
        }
    }

    fn number(dict: &NSDictionary, key: &str) -> Option<Retained<NSNumber>> {
        dict.objectForKey(&NSString::from_str(key))?
            .downcast::<NSNumber>()
            .ok()
    }

    /// Query WindowServer before display and on each expiry tick. The cursor
    /// disappears if the window moves, hides, closes, or changes ownership.
    fn target_is_visible(
        position: &Position,
        overlay_window: Option<isize>,
        mtm: MainThreadMarker,
    ) -> Option<f64> {
        let windows = CGWindowListCopyWindowInfo(
            CGWindowListOption::OptionIncludingWindow,
            position.window_id,
        )?;
        // SAFETY: CGWindowListCopyWindowInfo returns a retained CFArray of
        // dictionaries. CFArray/CFDictionary are toll-free bridged to the
        // corresponding Foundation collections. The borrow ends before release.
        let rows = unsafe { &*std::ptr::from_ref(&*windows).cast::<NSArray<NSDictionary>>() };
        let row = rows.firstObject()?;
        if number(&row, "kCGWindowNumber")?.unsignedIntValue() != position.window_id
            || !number(&row, "kCGWindowIsOnscreen")?.boolValue()
        {
            return None;
        }
        let pid = number(&row, "kCGWindowOwnerPID")?.intValue();
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        if app.isTerminated() || app.bundleIdentifier()?.to_string() != position.bundle_id {
            return None;
        }
        let bounds = row
            .objectForKey(&NSString::from_str("kCGWindowBounds"))?
            .downcast::<NSDictionary>()
            .ok()?;
        let current = ComputerUseActionBounds {
            x: number(&bounds, "X")?.doubleValue(),
            y: number(&bounds, "Y")?.doubleValue(),
            width: number(&bounds, "Width")?.doubleValue(),
            height: number(&bounds, "Height")?.doubleValue(),
        };
        if !policy::same_bounds(current, position.bounds) {
            return None;
        }
        let primary_height = NSScreen::screens(mtm).firstObject()?.frame().size.height;
        let point = NSPoint::new(position.point.x, primary_height - position.point.y);
        let mut hit = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, mtm);
        // If AppKit includes our click-through panel in this query, exclude
        // only that frontmost panel. Never skip another window above it.
        if overlay_window == Some(hit) {
            hit = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, hit, mtm);
        }
        if hit != position.window_id as isize {
            return None;
        }
        Some(primary_height)
    }

    fn make_panel(
        position: &Position,
        primary_height: f64,
        mtm: MainThreadMarker,
    ) -> Retained<CursorPanel> {
        let (x, y) = policy::panel_origin(position.point, primary_height);
        let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(policy::SIDE, policy::SIDE));
        // SAFETY: standard superclass initializers on freshly allocated,
        // main-thread-only subclasses; both return the same class.
        let panel: Retained<CursorPanel> = unsafe {
            msg_send![super(CursorPanel::alloc(mtm).set_ivars(())), initWithContentRect: rect,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered, defer: false]
        };
        let view: Retained<CursorView> = unsafe {
            msg_send![super(CursorView::alloc(mtm).set_ivars(())), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), rect.size)]
        };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setExcludedFromWindowsMenu(true);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::Transient
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        // SAFETY: Rust owns the panel until it is ordered out and dropped.
        unsafe {
            panel.setReleasedWhenClosed(false);
        }
        panel.setContentView(Some(&view));
        panel
    }

    pub(super) fn update(app: &AppHandle, event: &ComputerUseActionEvent) {
        if !matches!(event.source, ComputerUseActionSource::Native) {
            return;
        }
        let Some(revision) = action_gate().register(event) else {
            return;
        };
        let event = event.clone();
        let timer_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            if !action_gate().is_current(revision) {
                return;
            }
            let position = policy::position(&event, chrono::Utc::now().timestamp_millis());
            OVERLAY.with(|state| {
                let mut slot = state.borrow_mut();
                if slot
                    .as_ref()
                    .is_some_and(|old| !policy::can_replace(&old.position, &event))
                {
                    return;
                }
                let Some(position) = position else {
                    if slot
                        .as_ref()
                        .is_some_and(|old| old.position.session_id == event.session_id)
                    {
                        hide(&mut slot);
                    }
                    return;
                };
                hide(&mut slot);
                let Some(primary_height) = target_is_visible(&position, None, mtm) else {
                    return;
                };
                let panel = make_panel(&position, primary_height, mtm);
                let alive = Arc::new(AtomicBool::new(true));
                panel.orderFrontRegardless();
                *slot = Some(Overlay {
                    panel,
                    position,
                    alive: alive.clone(),
                });
                schedule_checks(timer_app, alive);
            });
        });
    }

    fn schedule_checks(app: AppHandle, alive: Arc<AtomicBool>) {
        tauri::async_runtime::spawn(async move {
            let deadline = Instant::now() + Duration::from_millis(policy::VISIBLE_MILLIS as u64);
            while alive.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let current = alive.clone();
                let expired = Instant::now() >= deadline;
                let result = app.run_on_main_thread(move || {
                    let Some(mtm) = MainThreadMarker::new() else {
                        return;
                    };
                    OVERLAY.with(|state| {
                        let mut slot = state.borrow_mut();
                        let Some(overlay) = slot.as_ref() else {
                            return;
                        };
                        if !Arc::ptr_eq(&overlay.alive, &current) {
                            return;
                        }
                        if expired
                            || chrono::Utc::now().timestamp_millis() >= overlay.position.expires_at
                            || target_is_visible(
                                &overlay.position,
                                Some(overlay.panel.windowNumber()),
                                mtm,
                            )
                            .is_none()
                        {
                            hide(&mut slot);
                        }
                    });
                });
                if result.is_err() || expired {
                    break;
                }
            }
        });
    }

    pub(super) fn clear(app: &AppHandle, session_id: Option<String>) {
        // Invalidate queued display work and completions from existing actions.
        action_gate().clear(session_id.as_deref());
        let _ = app.run_on_main_thread(move || {
            OVERLAY.with(|state| {
                let mut slot = state.borrow_mut();
                if session_id.as_ref().is_none_or(|session| {
                    slot.as_ref()
                        .is_some_and(|overlay| &overlay.position.session_id == session)
                }) {
                    hide(&mut slot);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::policy::*;
    use crate::computer_use_action::*;

    fn event() -> ComputerUseActionEvent {
        ComputerUseActionEvent {
            action_id: "action".into(),
            session_id: "session".into(),
            source: ComputerUseActionSource::Native,
            action: ComputerUseActionKind::Click,
            phase: ComputerUseActionPhase::Completed,
            execution_mode: ComputerUseExecutionMode::Background,
            coordinate_frame: ComputerUseCoordinateFrame::Screen,
            started_at_millis: 100,
            visible_until_millis: 2_000,
            point: Some(ComputerUseActionPoint { x: -100.0, y: 50.0 }),
            target_bounds: Some(ComputerUseActionBounds {
                x: -300.0,
                y: -100.0,
                width: 400.0,
                height: 300.0,
            }),
            viewport: None,
            browser_id: None,
            workspace_id: None,
            instance_id: None,
            document_epoch: None,
            bundle_id: Some("dev.fixture".into()),
            window_id: Some(42),
            capture_id: None,
        }
    }

    #[test]
    fn stop_and_revoke_reject_late_completions_and_allow_fresh_actions() {
        for session_only in [false, true] {
            let mut gate = ActionGate::default();
            let mut running = event();
            running.phase = ComputerUseActionPhase::Running;
            let queued = gate.register(&running).unwrap();
            gate.clear(session_only.then_some("session"));
            assert!(!gate.is_current(queued));
            assert_eq!(gate.register(&event()), None);

            running.action_id = "after-resume".into();
            let fresh = gate.register(&running).unwrap();
            running.phase = ComputerUseActionPhase::Completed;
            assert_eq!(gate.register(&running), Some(fresh));
            assert!(gate.is_current(fresh));
        }
    }

    #[test]
    fn revoke_preserves_another_sessions_pending_completion() {
        let mut gate = ActionGate::default();
        let mut first = event();
        first.phase = ComputerUseActionPhase::Running;
        let mut second = first.clone();
        second.session_id = "another-session".into();
        gate.register(&first).unwrap();
        gate.register(&second).unwrap();
        gate.clear(Some("session"));
        first.phase = ComputerUseActionPhase::Completed;
        second.phase = ComputerUseActionPhase::Completed;
        assert_eq!(gate.register(&first), None);
        let revision = gate.register(&second).unwrap();
        assert!(gate.is_current(revision));
    }

    #[test]
    fn completed_markers_require_one_live_action_and_queued_markers_obey_stop() {
        let mut gate = ActionGate::default();
        assert_eq!(gate.register(&event()), None);
        let mut running = event();
        running.phase = ComputerUseActionPhase::Running;
        gate.register(&running).unwrap();
        let queued = gate.register(&event()).unwrap();
        assert_eq!(gate.register(&event()), None);
        gate.clear(None);
        assert!(!gate.is_current(queued));
    }

    #[test]
    fn only_acknowledged_fresh_native_screen_geometry_can_show() {
        let mut event = event();
        let shown = position(&event, 200).unwrap();
        assert_eq!(shown.bundle_id, "dev.fixture");
        assert_eq!(shown.window_id, 42);
        let point = event.point.unwrap();
        assert_eq!((shown.point.x, shown.point.y), (point.x, point.y));
        let bounds = event.target_bounds.unwrap();
        assert_eq!(
            (
                shown.bounds.x,
                shown.bounds.y,
                shown.bounds.width,
                shown.bounds.height
            ),
            (bounds.x, bounds.y, bounds.width, bounds.height)
        );
        event.phase = ComputerUseActionPhase::Running;
        assert!(position(&event, 200).is_none());
        event.phase = ComputerUseActionPhase::Completed;
        event.point = None;
        assert!(position(&event, 200).is_none());
        event = self::event();
        event.source = ComputerUseActionSource::Chrome;
        assert!(position(&event, 200).is_none());
        event = self::event();
        event.coordinate_frame = ComputerUseCoordinateFrame::Viewport;
        assert!(position(&event, 200).is_none());
        assert!(position(&self::event(), 2_000).is_none());
    }

    #[test]
    fn invalid_or_outside_geometry_is_hidden() {
        for point in [(f64::NAN, 50.0), (100.0, 50.0), (-100.0, 200.0)] {
            let mut event = event();
            event.point = Some(ComputerUseActionPoint {
                x: point.0,
                y: point.1,
            });
            assert!(position(&event, 200).is_none());
        }
        let mut event = event();
        event.target_bounds.as_mut().unwrap().width = f64::INFINITY;
        assert!(position(&event, 200).is_none());
    }

    #[test]
    fn expiry_is_bounded_and_old_actions_cannot_replace_newer_positions() {
        let event = event();
        let shown = position(&event, 200).unwrap();
        assert_eq!(shown.expires_at, 1_700);
        let mut older = event.clone();
        older.action_id = "older".into();
        older.started_at_millis = 99;
        assert!(!can_replace(&shown, &older));
        assert!(can_replace(&shown, &event));
    }

    #[test]
    fn screen_conversion_preserves_negative_display_coordinates_and_tip() {
        let point = ComputerUseActionPoint {
            x: -100.0,
            y: -50.0,
        };
        let (x, y) = panel_origin(point, 900.0);
        assert_eq!(x + TIP_X, -100.0);
        assert_eq!(y + SIDE - TIP_Y, 950.0);
        let bounds = event().target_bounds.unwrap();
        assert!(same_bounds(bounds, bounds));
        assert!(!same_bounds(
            bounds,
            ComputerUseActionBounds {
                x: bounds.x + 1.0,
                ..bounds
            }
        ));
    }
}

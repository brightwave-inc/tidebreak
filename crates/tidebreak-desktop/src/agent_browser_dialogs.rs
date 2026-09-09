//! Keep page dialogs inside an independent agent browser from opening native UI.
//!
//! WebKit holds its UI delegate weakly. The view owns this guard through an
//! associated object, and the guard retains Wry's delegate for human takeover.

use std::cell::RefCell;
use std::ffi::c_void;

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_web_kit::{
    WKFrameInfo, WKMediaCaptureType, WKNavigationAction, WKOpenPanelParameters,
    WKPermissionDecision, WKSecurityOrigin, WKUIDelegate, WKWebView, WKWebViewConfiguration,
    WKWindowFeatures,
};

static GUARD_KEY: u8 = 0;

const FILE_PANEL: &str = "The page requested a file picker. The agent browser canceled it. Use browser_upload or take human control; the requested file selection did not complete.";
const ALERT: &str = "The page requested an alert. The agent browser dismissed it. Inspect the page before continuing.";
const CONFIRM: &str = "The page requested confirmation. The agent browser canceled it. The requested confirmation did not complete; inspect the page before continuing.";
const PROMPT: &str = "The page requested text in a dialog. The agent browser canceled it. The requested dialog did not complete; inspect the page before continuing.";
const MEDIA: &str = "The page requested camera or microphone access. The agent browser denied it. Take human control to grant access.";
const MOTION: &str = "The page requested device motion access. The agent browser denied it. Take human control to grant access.";
const POPUP: &str = "The page requested a new window. The agent browser blocked it. Open the destination with browser_open; the requested window did not open.";

#[derive(Default)]
struct BlockedDialog(Option<&'static str>);

impl BlockedDialog {
    fn record(&mut self, message: &'static str) {
        // Retain the first reason until an action acknowledges it. Repeated
        // page requests cannot replace it or grow an unbounded queue.
        self.0.get_or_insert(message);
    }

    fn take(&mut self) -> Result<(), String> {
        self.0
            .take()
            .map_or(Ok(()), |message| Err(message.to_owned()))
    }
}

struct Ivars {
    original: Option<Retained<ProtocolObject<dyn WKUIDelegate>>>,
    blocked: RefCell<BlockedDialog>,
    notify: Box<dyn Fn(&'static str)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class and all
    // delegate callbacks remain on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TidebreakAgentBrowserDialogGuard"]
    #[ivars = Ivars]
    struct AgentBrowserDialogGuard;

    unsafe impl NSObjectProtocol for AgentBrowserDialogGuard {}

    unsafe impl WKUIDelegate for AgentBrowserDialogGuard {
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn file_panel(
            &self,
            _view: &WKWebView,
            _parameters: &WKOpenPanelParameters,
            _frame: &WKFrameInfo,
            complete: &DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            self.block(FILE_PANEL);
            complete.call((std::ptr::null_mut(),));
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(
            &self,
            _view: &WKWebView,
            _message: &NSString,
            _frame: &WKFrameInfo,
            complete: &DynBlock<dyn Fn()>,
        ) {
            self.block(ALERT);
            complete.call(());
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            _view: &WKWebView,
            _message: &NSString,
            _frame: &WKFrameInfo,
            complete: &DynBlock<dyn Fn(Bool)>,
        ) {
            self.block(CONFIRM);
            complete.call((Bool::NO,));
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn prompt(
            &self,
            _view: &WKWebView,
            _prompt: &NSString,
            _default: Option<&NSString>,
            _frame: &WKFrameInfo,
            complete: &DynBlock<dyn Fn(*mut NSString)>,
        ) {
            self.block(PROMPT);
            complete.call((std::ptr::null_mut(),));
        }

        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media(
            &self,
            _view: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            _capture: WKMediaCaptureType,
            complete: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            self.block(MEDIA);
            complete.call((WKPermissionDecision::Deny,));
        }

        #[unsafe(method(webView:requestDeviceOrientationAndMotionPermissionForOrigin:initiatedByFrame:decisionHandler:))]
        fn motion(
            &self,
            _view: &WKWebView,
            _origin: &WKSecurityOrigin,
            _frame: &WKFrameInfo,
            complete: &DynBlock<dyn Fn(WKPermissionDecision)>,
        ) {
            self.block(MOTION);
            complete.call((WKPermissionDecision::Deny,));
        }

        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn popup(
            &self,
            _view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            self.block(POPUP);
            None
        }
    }
);

impl AgentBrowserDialogGuard {
    fn new(
        mtm: MainThreadMarker,
        original: Option<Retained<ProtocolObject<dyn WKUIDelegate>>>,
        notify: Box<dyn Fn(&'static str)>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            original,
            blocked: RefCell::new(BlockedDialog::default()),
            notify,
        });
        // SAFETY: Initialize the freshly allocated NSObject subclass.
        unsafe { msg_send![super(this), init] }
    }

    fn block(&self, message: &'static str) {
        let first = self.ivars().blocked.borrow().0.is_none();
        self.ivars().blocked.borrow_mut().record(message);
        if first {
            (self.ivars().notify)(message);
        }
    }
}

fn guard(view: &WKWebView) -> Option<&AgentBrowserDialogGuard> {
    // SAFETY: GUARD_KEY is private to this module. Only install writes this
    // association, with a retained AgentBrowserDialogGuard. The view owns its
    // lifetime, and both the view and guard are confined to the main thread.
    unsafe {
        objc2::ffi::objc_getAssociatedObject(
            view as *const _ as *const AnyObject,
            &GUARD_KEY as *const _ as *const c_void,
        )
        .cast::<AgentBrowserDialogGuard>()
        .as_ref()
    }
}

/// Install before loading any page in an independent agent host.
pub(crate) fn install(
    view: &WKWebView,
    notify: impl Fn(&'static str) + 'static,
) -> Result<(), String> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "browser dialog guard requires the main thread".to_owned())?;
    if guard(view).is_some() {
        return verify(view);
    }
    // SAFETY: WebKit's UI delegate access and the association are main-thread
    // operations. The association retains our otherwise weak UI delegate.
    unsafe {
        let guard = AgentBrowserDialogGuard::new(mtm, view.UIDelegate(), Box::new(notify));
        objc2::ffi::objc_setAssociatedObject(
            view as *const _ as *mut AnyObject,
            &GUARD_KEY as *const _ as *const c_void,
            Retained::as_ptr(&guard) as *mut AnyObject,
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        view.setUIDelegate(Some(ProtocolObject::from_ref(&*guard)));
    }
    verify(view)
}

/// Fail closed if another component replaces the per-view guard.
pub(crate) fn verify(view: &WKWebView) -> Result<(), String> {
    let guard = guard(view).ok_or_else(|| {
        "The agent browser dialog guard is unavailable. Inspect the page before continuing."
            .to_owned()
    })?;
    // SAFETY: WKWebView and its delegate are confined to the main thread.
    let installed = unsafe { view.UIDelegate() };
    if !installed.is_some_and(|delegate| {
        std::ptr::eq(
            Retained::as_ptr(&delegate).cast::<AnyObject>(),
            guard as *const _ as *const AnyObject,
        )
    }) {
        return Err(
            "The agent browser dialog guard changed. Inspect the page before continuing."
                .to_owned(),
        );
    }
    Ok(())
}

/// Return a canceled dialog as an action failure. A late dialog remains pending
/// until the next action, so it cannot silently disappear between callbacks.
pub(crate) fn take_blocked(view: &WKWebView) -> Result<(), String> {
    guard(view).map_or(Ok(()), |guard| guard.ivars().blocked.borrow_mut().take())
}

/// Run one synchronous main-thread operation before returning to the caller.
/// The pinned runtime invokes callbacks inline when the caller is already on
/// the main thread. A timed-out queued callback cannot later load a page.
pub(crate) fn with_guarded_view(
    webview: &tauri::Webview,
    operation: impl FnOnce(&WKWebView) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    use std::sync::{Arc, Mutex};
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let pending = Arc::new(Mutex::new(Some(sender)));
    let callback_pending = Arc::clone(&pending);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    crate::browser_native_webview::with_browser_webview(webview, move |view| {
        let mut pending = callback_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sender) = pending.take() else {
            return;
        };
        if std::time::Instant::now() >= deadline {
            let _ = sender.send(Err("browser dialog guard operation timed out".to_owned()));
            return;
        }
        let _ = sender.send(operation(view));
    })?;
    match receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
        Ok(result) => result,
        Err(_) => resolve_guard_timeout(&pending, &receiver),
    }
}

type GuardResultSender = std::sync::mpsc::SyncSender<Result<(), String>>;

fn resolve_guard_timeout(
    pending: &std::sync::Mutex<Option<GuardResultSender>>,
    receiver: &std::sync::mpsc::Receiver<Result<(), String>>,
) -> Result<(), String> {
    let mut pending = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.take().is_some() {
        // The callback has not started. Disarm it before returning.
        Err("browser dialog guard operation timed out".to_owned())
    } else {
        // Acquiring the same lock drains a callback already in progress.
        // Return its real result: a completed delegate restore must not
        // be mistaken for failure and rolled back into an agent host.
        drop(pending);
        receiver
            .try_recv()
            .map_err(|_| "browser dialog guard operation was interrupted".to_owned())?
    }
}

/// Restore normal page UI only after the agent dispatch gate is drained.
pub(crate) fn restore(view: &WKWebView) -> Result<(), String> {
    let Some(guard) = guard(view) else {
        return Ok(());
    };
    verify(view)?;
    // SAFETY: Wry's original delegate is retained by the guard until the weak
    // delegate slot is restored. Clearing the association releases only our
    // guard; Wry continues to own its original delegate.
    unsafe {
        view.setUIDelegate(guard.ivars().original.as_deref());
        objc2::ffi::objc_setAssociatedObject(
            view as *const _ as *mut AnyObject,
            &GUARD_KEY as *const _ as *const c_void,
            std::ptr::null_mut(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_disarms_a_guard_operation_that_has_not_started() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let pending = std::sync::Mutex::new(Some(sender));
        assert!(resolve_guard_timeout(&pending, &receiver).is_err());
        assert!(pending.lock().unwrap().is_none());
    }

    #[test]
    fn timeout_returns_the_real_result_after_a_guard_operation_started() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let pending = std::sync::Mutex::new(Some(sender));
        pending
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(Ok(()))
            .unwrap();
        resolve_guard_timeout(&pending, &receiver).unwrap();
    }

    #[test]
    fn blocked_dialog_prevents_success_until_reported() {
        let mut blocked = BlockedDialog::default();
        blocked.take().unwrap();
        blocked.record(CONFIRM);
        assert_eq!(blocked.take().unwrap_err(), CONFIRM);
        blocked.take().unwrap();
    }

    #[test]
    fn repeated_dialogs_keep_the_first_cancellation_bounded() {
        let mut blocked = BlockedDialog::default();
        blocked.record(FILE_PANEL);
        for _ in 0..10_000 {
            blocked.record(ALERT);
        }
        assert_eq!(blocked.take().unwrap_err(), FILE_PANEL);
        blocked.record(MEDIA);
        assert_eq!(blocked.take().unwrap_err(), MEDIA);
    }
}

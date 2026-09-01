//! Push URL changes out of a managed browser view.
//!
//! `WKWebView.URL` is key-value observable, and WebKit updates it for
//! same-document navigations (`pushState`, `replaceState`, `popstate`,
//! `hashchange`) as well as for cross-document loads. Observing it gives the
//! host a native push for route changes, so the URL bar follows a
//! single-page app without a timer and without any page-side script.
//!
//! The observer object lives on the main thread and is retained here, keyed by
//! webview label, until [`stop_observing_browser_url`] removes it. Remove it
//! before the view closes so no observer outlives the view it watches.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{
    NSKeyValueObservingOptions, NSObjectNSKeyValueObserverRegistration, NSString,
};
use objc2_web_kit::WKWebView;
use tauri::Webview;

use crate::code_browser::BrowserUrlChange;

/// Runs on the main thread each time the observed view's URL changes.
pub(crate) type BrowserUrlChangeHandler = Box<dyn Fn(BrowserUrlChange) + Send + 'static>;

const URL_KEY_PATH: &str = "URL";

struct Ivars {
    handler: BrowserUrlChangeHandler,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `BrowserUrlObserver` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TidebreakBrowserUrlObserver"]
    #[ivars = Ivars]
    struct BrowserUrlObserver;

    unsafe impl NSObjectProtocol for BrowserUrlObserver {}

    impl BrowserUrlObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            if !key_path.is_some_and(|key_path| key_path.to_string() == URL_KEY_PATH) {
                return;
            }
            let Some(object) = object else {
                return;
            };
            // SAFETY: the observer is only ever registered on a `WKWebView`,
            // and KVO hands back the observed object.
            let view: &WKWebView = unsafe { &*(object as *const AnyObject).cast() };
            let url = unsafe { view.URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            let Some(url) = url else {
                return;
            };
            let loading = unsafe { view.isLoading() };
            (self.ivars().handler)(BrowserUrlChange { url, loading });
        }
    }
);

impl BrowserUrlObserver {
    fn new(mtm: MainThreadMarker, handler: BrowserUrlChangeHandler) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { handler });
        // SAFETY: `NSObject::init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    static URL_OBSERVERS: RefCell<HashMap<String, Retained<BrowserUrlObserver>>> =
        RefCell::new(HashMap::new());
}

/// Observe the view's URL and call `handler` on every change. Replaces any
/// observer already registered for the same webview label.
pub(crate) fn observe_browser_url(
    webview: &Webview,
    handler: BrowserUrlChangeHandler,
) -> Result<(), String> {
    let label = webview.label().to_owned();
    webview
        .with_webview(move |platform| {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            // SAFETY: tauri hands the native `WKWebView` pointer to this
            // closure on the main thread.
            let view: &WKWebView = unsafe { &*platform.inner().cast() };
            let observer = BrowserUrlObserver::new(mtm, handler);
            let key_path = NSString::from_str(URL_KEY_PATH);
            let previous = URL_OBSERVERS
                .with(|observers| observers.borrow_mut().insert(label, observer.clone()));
            if let Some(previous) = previous {
                // SAFETY: `previous` was registered on this key path below.
                unsafe {
                    view.removeObserver_forKeyPath_context(
                        &previous,
                        &key_path,
                        std::ptr::null_mut(),
                    );
                }
            }
            // SAFETY: the observer implements
            // `observeValueForKeyPath:ofObject:change:context:` and stays
            // retained in `URL_OBSERVERS` until it is removed.
            unsafe {
                view.addObserver_forKeyPath_options_context(
                    &observer,
                    &key_path,
                    NSKeyValueObservingOptions::New,
                    std::ptr::null_mut(),
                );
            }
        })
        .map_err(|error| format!("browser host: {error}"))
}

/// Remove the URL observer registered for this webview, if any. Call before
/// closing the view.
pub(crate) fn stop_observing_browser_url(webview: &Webview) -> Result<(), String> {
    let label = webview.label().to_owned();
    webview
        .with_webview(move |platform| {
            let Some(observer) =
                URL_OBSERVERS.with(|observers| observers.borrow_mut().remove(&label))
            else {
                return;
            };
            // SAFETY: tauri hands the native `WKWebView` pointer to this
            // closure on the main thread, and `observer` was registered on it.
            let view: &WKWebView = unsafe { &*platform.inner().cast() };
            let key_path = NSString::from_str(URL_KEY_PATH);
            unsafe {
                view.removeObserver_forKeyPath_context(&observer, &key_path, std::ptr::null_mut());
            }
        })
        .map_err(|error| format!("browser host: {error}"))
}

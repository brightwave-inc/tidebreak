//! Push URL changes out of a managed browser view.
//!
//! `WKWebView.URL` is key-value observable, and WebKit updates it for
//! same-document navigations (`pushState`, `replaceState`, `popstate`,
//! `hashchange`) as well as for cross-document loads. Observing it gives the
//! host a native push for route changes, so the URL bar follows a
//! single-page app without a timer and without any page-side script.
//!
//! The observer object lives on the main thread and is retained here, keyed by
//! webview label, until [`stop_observing_browser_url`] removes it. The
//! observer holds the view it watches, so the view cannot be deallocated
//! while an observation is registered: a view that goes away outside the
//! close path (the window tearing down, the host dropping it) stays alive
//! until its observer is detached, and never trips KVO's deallocation check.
//! [`detach_all_browser_url_observers`] runs at exit so nothing lingers.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
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
    /// The observed view. Held strongly so it outlives the observation.
    view: Retained<WKWebView>,
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
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            if !key_path.is_some_and(|key_path| key_path.to_string() == URL_KEY_PATH) {
                return;
            }
            let view = &self.ivars().view;
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
    fn new(
        mtm: MainThreadMarker,
        view: Retained<WKWebView>,
        handler: BrowserUrlChangeHandler,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { handler, view });
        // SAFETY: `NSObject::init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Register on the held view's `URL` key path.
    fn attach(&self) {
        let key_path = NSString::from_str(URL_KEY_PATH);
        // SAFETY: the observer implements
        // `observeValueForKeyPath:ofObject:change:context:` and holds the
        // view, so both sides stay alive until `detach` runs.
        unsafe {
            self.ivars().view.addObserver_forKeyPath_options_context(
                self,
                &key_path,
                NSKeyValueObservingOptions::New,
                std::ptr::null_mut(),
            );
        }
    }

    /// Remove the registration made by `attach`. Dropping the observer
    /// afterwards releases the view.
    fn detach(&self) {
        let key_path = NSString::from_str(URL_KEY_PATH);
        // SAFETY: `attach` registered this observer on this key path.
        unsafe {
            self.ivars().view.removeObserver_forKeyPath_context(
                self,
                &key_path,
                std::ptr::null_mut(),
            );
        }
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
            let observer = BrowserUrlObserver::new(mtm, view.retain(), handler);
            observer.attach();
            let previous =
                URL_OBSERVERS.with(|observers| observers.borrow_mut().insert(label, observer));
            if let Some(previous) = previous {
                previous.detach();
            }
        })
        .map_err(|error| format!("browser host: {error}"))
}

/// Remove the URL observer registered for this webview, if any. Call before
/// closing the view so the observer releases it.
pub(crate) fn stop_observing_browser_url(webview: &Webview) -> Result<(), String> {
    let label = webview.label().to_owned();
    webview
        .with_webview(move |_platform| {
            if let Some(observer) =
                URL_OBSERVERS.with(|observers| observers.borrow_mut().remove(&label))
            {
                observer.detach();
            }
        })
        .map_err(|error| format!("browser host: {error}"))
}

/// Detach every registered observer and release the views they held. Runs on
/// the main thread at exit, after which the host may drop the views freely.
pub(crate) fn detach_all_browser_url_observers() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let observers: Vec<_> = URL_OBSERVERS.with(|observers| {
        observers
            .borrow_mut()
            .drain()
            .map(|(_, observer)| observer)
            .collect()
    });
    for observer in observers {
        observer.detach();
    }
}

//! Balance the native references transferred by the pinned Tauri runtime.

use std::sync::OnceLock;

use objc2::{rc::Retained, runtime::AnyObject, MainThreadMarker};
use objc2_web_kit::WKWebView;
use tauri::{webview::PlatformWebview, Webview};

const LOCKFILE: &str = include_str!("../../../Cargo.lock");
const REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const AUDITED_PACKAGES: [(&str, &str, &str); 3] = [
    (
        "tauri",
        "2.11.5",
        "667b20e2726d572dea2de7370da16e188eb06008faf9a92fab7cdc46791190b5",
    ),
    (
        "tauri-runtime-wry",
        "2.11.4",
        "4e6fac707727b7a2f48e4ded90976324267371073edbb415ffb73bb0458d203f",
    ),
    (
        "wry",
        "0.55.1",
        "186f9871daa55fd9c016578b810d149de58367113db7fb72b462d2323ce19514",
    ),
];

/// Stop before calling Tauri if an update changes the audited ownership contract.
fn validate_native_webview_ownership(lockfile: &str) -> Result<(), String> {
    for (name, version, checksum) in AUDITED_PACKAGES {
        let name_line = format!("name = {name:?}");
        let mut packages = lockfile
            .split("[[package]]")
            .filter(|package| package.lines().any(|line| line == name_line));
        let package = packages.next();
        let duplicate = packages.next().is_some();
        let fields = [
            name_line,
            format!("version = {version:?}"),
            format!("source = {REGISTRY_SOURCE:?}"),
            format!("checksum = {checksum:?}"),
        ];
        if duplicate
            || !package.is_some_and(|package| {
                fields
                    .iter()
                    .all(|field| package.lines().filter(|line| *line == field).count() == 1)
            })
        {
            return Err(format!(
                "browser host: native webview ownership requires an audit of {name}; \
                 review browser_native_webview.rs before enabling browser access"
            ));
        }
    }
    if std::mem::needs_drop::<PlatformWebview>() {
        return Err("browser host: PlatformWebview now owns cleanup; review browser_native_webview.rs before enabling browser access".to_owned());
    }
    Ok(())
}

/// Own all three +1 references produced by Tauri's WithWebview message.
struct NativeBrowserHandles {
    view: Retained<AnyObject>,
    _controller: Retained<AnyObject>,
    _window: Retained<AnyObject>,
}

impl NativeBrowserHandles {
    /// The pointers must each transfer one owned Objective-C reference.
    unsafe fn from_owned_pointers(
        view: *mut AnyObject,
        controller: *mut AnyObject,
        window: *mut AnyObject,
    ) -> Self {
        // Adopt every reference before checking for a null pointer, so unwinding
        // releases the other handles too. The audited runtime never sends null.
        let view = unsafe { Retained::from_raw(view) };
        let controller = unsafe { Retained::from_raw(controller) };
        let window = unsafe { Retained::from_raw(window) };
        Self {
            view: view.expect("Tauri transferred a null WKWebView"),
            _controller: controller.expect("Tauri transferred a null controller"),
            _window: window.expect("Tauri transferred a null NSWindow"),
        }
    }
}

/// Borrow the native view on the main thread and release Tauri's transferred handles.
/// Retain the view separately when an asynchronous callback needs to keep it alive.
pub(crate) fn with_browser_webview(
    webview: &Webview,
    callback: impl FnOnce(&WKWebView) + Send + 'static,
) -> Result<(), String> {
    static OWNERSHIP_CONTRACT: OnceLock<Result<(), String>> = OnceLock::new();
    OWNERSHIP_CONTRACT
        .get_or_init(|| validate_native_webview_ownership(LOCKFILE))
        .clone()?;
    webview
        .with_webview(move |platform| {
            let _main_thread = MainThreadMarker::new()
                .expect("Tauri must deliver native browser handles on the main thread");
            // SAFETY: tauri-runtime-wry 2.11.4's WithWebview handler calls
            // Retained::into_raw for the WKWebView, WKUserContentController,
            // and NSWindow. Neither its raw Webview nor Tauri's PlatformWebview
            // has Drop. The lockfile guard rejects changes to that contract.
            let handles = unsafe {
                NativeBrowserHandles::from_owned_pointers(
                    platform.inner().cast(),
                    platform.controller().cast(),
                    platform.ns_window().cast(),
                )
            };
            // SAFETY: inner() is the audited WKWebView pointer. The guard owns
            // its reference until this callback returns or unwinds.
            let view = unsafe { &*Retained::as_ptr(&handles.view).cast::<WKWebView>() };
            callback(view);
        })
        .map_err(|error| format!("browser host: {error}"))
}

#[cfg(test)]
mod tests {
    use objc2::{rc::Weak, runtime::NSObject};

    use super::*;

    fn owned_handles() -> (NativeBrowserHandles, [Weak<NSObject>; 3]) {
        let objects = [NSObject::new(), NSObject::new(), NSObject::new()];
        let observers = objects.each_ref().map(Weak::from);
        let [view, controller, window] = objects.map(Retained::into_raw);
        // SAFETY: each pointer transfers the only owned reference to that object.
        let handles = unsafe {
            NativeBrowserHandles::from_owned_pointers(view.cast(), controller.cast(), window.cast())
        };
        (handles, observers)
    }

    #[test]
    fn native_webview_releases_all_transferred_references() {
        let (handles, observers) = owned_handles();
        assert!(observers.iter().all(|observer| observer.load().is_some()));
        drop(handles);
        assert!(observers.iter().all(|observer| observer.load().is_none()));
    }

    #[test]
    fn native_webview_releases_references_when_callback_unwinds() {
        let (handles, observers) = owned_handles();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _handles = handles;
            panic!("callback failed");
        }));
        assert!(result.is_err());
        assert!(observers.iter().all(|observer| observer.load().is_none()));
    }

    #[test]
    fn native_webview_preserves_separate_async_reference() {
        let (handles, observers) = owned_handles();
        let retained_for_callback = handles.view.clone();
        drop(handles);
        assert!(observers[0].load().is_some());
        assert!(observers[1..]
            .iter()
            .all(|observer| observer.load().is_none()));
        drop(retained_for_callback);
        assert!(observers[0].load().is_none());
    }

    #[test]
    fn native_webview_accepts_the_audited_lockfile() {
        assert_eq!(validate_native_webview_ownership(LOCKFILE), Ok(()));
    }

    #[test]
    fn native_webview_rejects_dependency_ownership_changes() {
        for (name, version, checksum) in AUDITED_PACKAGES {
            let name_line = format!("name = {name:?}");
            let package = LOCKFILE
                .split("[[package]]")
                .find(|package| package.lines().any(|line| line == name_line))
                .expect("audited package exists");
            for (old, replacement) in [
                (
                    format!("version = {version:?}"),
                    "version = \"99.0.0\"".to_owned(),
                ),
                (
                    format!("source = {REGISTRY_SOURCE:?}"),
                    "source = \"git+https://example.com/fork\"".to_owned(),
                ),
                (
                    format!("checksum = {checksum:?}"),
                    "checksum = \"changed\"".to_owned(),
                ),
            ] {
                let changed = LOCKFILE.replacen(package, &package.replace(&old, &replacement), 1);
                assert!(
                    validate_native_webview_ownership(&changed).is_err(),
                    "accepted {name} change to {replacement}"
                );
            }
            let missing = LOCKFILE.replacen(package, "", 1);
            assert!(
                validate_native_webview_ownership(&missing).is_err(),
                "accepted missing {name}"
            );
            let duplicated = format!("{LOCKFILE}\n[[package]]{package}");
            assert!(
                validate_native_webview_ownership(&duplicated).is_err(),
                "accepted duplicate {name}"
            );
        }
    }

    #[test]
    fn native_webview_callers_use_the_ownership_wrapper() {
        for source in [
            include_str!("browser_semantics.rs"),
            include_str!("browser_url_observer.rs"),
        ] {
            assert!(!source.contains(".with_webview("));
            assert!(!source.contains("platform.inner()"));
        }
    }
}

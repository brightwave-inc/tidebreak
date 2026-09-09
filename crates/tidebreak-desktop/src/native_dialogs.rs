//! Keep native dialog commands without replacing browser JavaScript globals.
//!
//! The upstream plugin injects `window.confirm = async ...` into every webview.
//! A website can treat that Promise as consent, and its dialogs bypass WebKit's
//! agent guard. Tidebreak uses explicit dialog commands, so it needs no overrides.

use tauri::ipc::Invoke;
use tauri::plugin::{Plugin, TauriPlugin};
use tauri::webview::PageLoadPayload;
use tauri::{AppHandle, RunEvent, Runtime, Webview, Window};

pub(crate) fn init<R: Runtime>() -> impl Plugin<R> {
    NativeDialogs(tauri_plugin_dialog::init())
}

struct NativeDialogs<R: Runtime>(TauriPlugin<R>);

impl<R: Runtime> Plugin<R> for NativeDialogs<R> {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    fn initialize(
        &mut self,
        app: &AppHandle<R>,
        config: serde_json::Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.0.initialize(app, config)
    }

    // Leave both initialization-script methods at their default of None. The
    // native browser keeps its synchronous alert, confirm, and prompt behavior.

    fn window_created(&mut self, window: Window<R>) {
        self.0.window_created(window);
    }

    fn webview_created(&mut self, webview: Webview<R>) {
        self.0.webview_created(webview);
    }

    fn on_navigation(&mut self, webview: &Webview<R>, url: &url::Url) -> bool {
        self.0.on_navigation(webview, url)
    }

    fn on_page_load(&mut self, webview: &Webview<R>, payload: &PageLoadPayload<'_>) {
        self.0.on_page_load(webview, payload);
    }

    fn on_event(&mut self, app: &AppHandle<R>, event: &RunEvent) {
        self.0.on_event(app, event);
    }

    fn extend_api(&mut self, invoke: Invoke<R>) -> bool {
        self.0.extend_api(invoke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_plugin_preserves_browser_globals() {
        let plugin = init::<tauri::Wry>();
        assert_eq!(plugin.name(), "dialog");
        assert!(
            plugin.initialization_script().is_none()
                && plugin.initialization_script_2().is_none(),
            "Browser pages must keep native synchronous dialogs instead of the plugin's Promise-returning confirm override"
        );
    }
}

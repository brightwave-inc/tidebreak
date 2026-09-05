//! The app's native menu bar, and what its items do.
//!
//! Tauri's default menu is close to right, but its Close item takes Cmd+W and
//! closes the window. Tidebreak has one window, so closing it ends the app —
//! and on macOS a menu accelerator is claimed before the key ever reaches the
//! webview, which is why the shell's own "close the tab" binding never ran.
//! The menu is therefore built here rather than patched onto the default:
//! Cmd+W is a Close Tab item the renderer answers, and closing the window is
//! an item with no accelerator behind it.
//!
//! Only macOS gets a menu bar; the app ships no menu on Windows or Linux, so
//! `install_app_menu` is macOS-only and the event handler simply never fires
//! elsewhere.

use tauri::{AppHandle, Emitter, Manager};

use crate::updater::UPDATE_CHECK_REQUESTED_EVENT;

/// Raised when the reader presses Cmd+W. The renderer closes whichever tab it
/// has in front of them, and does nothing when there is none — the window
/// survives either way, because the key must not be able to end the app.
const CLOSE_TAB_REQUESTED_EVENT: &str = "desktop-close-tab-requested";

const MENU_CHECK_FOR_UPDATES_ID: &str = "check-for-updates";
const MENU_RELOAD_ID: &str = "reload-app";
const MENU_CLOSE_TAB_ID: &str = "close-tab";
const MENU_CLOSE_WINDOW_ID: &str = "close-window";

/// Build and install the menu bar.
///
/// Everything Tauri's default menu carries is here, so the items macOS readers
/// reach for — Services, Hide, the Edit menu's clipboard commands, full
/// screen, Minimize — behave as they always did.
#[cfg(target_os = "macos")]
pub(crate) fn install_app_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let handle = app.handle();
    let pkg = app.package_info();
    let bundle = &app.config().bundle;
    let about = AboutMetadata {
        name: Some(pkg.name.clone()),
        version: Some(pkg.version.to_string()),
        copyright: bundle.copyright.clone(),
        authors: bundle.publisher.clone().map(|publisher| vec![publisher]),
        ..Default::default()
    };

    let app_menu = Submenu::with_items(
        handle,
        pkg.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(handle, None, Some(about))?,
            &MenuItem::with_id(
                handle,
                MENU_CHECK_FOR_UPDATES_ID,
                "Check for Updates…",
                true,
                None::<&str>,
            )?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::services(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::hide(handle, None)?,
            &PredefinedMenuItem::hide_others(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::quit(handle, None)?,
        ],
    )?;

    // Cmd+W closes a tab, the way it does in a browser. Closing the window is
    // the item under it and carries no accelerator: Cmd+Shift+W already starts
    // a pull-request watch in code mode, and a menu accelerator there would
    // take that chord before the webview saw it.
    let file_menu = Submenu::with_items(
        handle,
        "File",
        true,
        &[
            &MenuItem::with_id(
                handle,
                MENU_CLOSE_TAB_ID,
                "Close Tab",
                true,
                Some("CmdOrCtrl+W"),
            )?,
            &MenuItem::with_id(
                handle,
                MENU_CLOSE_WINDOW_ID,
                "Close Window",
                true,
                None::<&str>,
            )?,
        ],
    )?;

    let edit_menu = Submenu::with_items(
        handle,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(handle, None)?,
            &PredefinedMenuItem::redo(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::cut(handle, None)?,
            &PredefinedMenuItem::copy(handle, None)?,
            &PredefinedMenuItem::paste(handle, None)?,
            &PredefinedMenuItem::select_all(handle, None)?,
        ],
    )?;

    let view_menu = Submenu::with_items(
        handle,
        "View",
        true,
        &[
            &MenuItem::with_id(handle, MENU_RELOAD_ID, "Reload", true, Some("CmdOrCtrl+R"))?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::fullscreen(handle, None)?,
        ],
    )?;

    let window_menu = Submenu::with_items(
        handle,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(handle, None)?,
            &PredefinedMenuItem::maximize(handle, None)?,
        ],
    )?;

    // Empty, as in Tauri's default: macOS fills the Help menu with its own
    // search field and the app has nothing to add above it.
    let help_menu = Submenu::new(handle, "Help", true)?;

    let menu = Menu::with_items(
        handle,
        &[
            &app_menu,
            &file_menu,
            &edit_menu,
            &view_menu,
            &window_menu,
            &help_menu,
        ],
    )?;
    app.set_menu(menu)?;
    Ok(())
}

pub(crate) fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_CHECK_FOR_UPDATES_ID => {
            if let Err(error) = app.emit(UPDATE_CHECK_REQUESTED_EVENT, ()) {
                eprintln!("tidebreak-desktop: could not raise the update-check request: {error}");
            }
        }
        MENU_CLOSE_TAB_ID => {
            if let Err(error) = app.emit(CLOSE_TAB_REQUESTED_EVENT, ()) {
                eprintln!("tidebreak-desktop: could not raise the close-tab request: {error}");
            }
        }
        MENU_CLOSE_WINDOW_ID => {
            if let Some(window) = app.get_window("main") {
                if let Err(error) = window.close() {
                    eprintln!("tidebreak-desktop: could not close the window: {error}");
                }
            }
        }
        MENU_RELOAD_ID => {
            if let Some(window) = app.get_webview("main") {
                if let Err(error) = window.reload() {
                    eprintln!("tidebreak-desktop: could not reload the app: {error}");
                }
            }
        }
        _ => {}
    }
}

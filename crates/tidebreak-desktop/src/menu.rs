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
//! The same rule shapes every other item that carries a shortcut. The
//! renderer already binds Cmd+N, Cmd+K, Cmd+B, the zoom keys, and Cmd+/ in
//! its shortcut table, and the menu items that show those chords take them
//! first. So each such item raises a [`MENU_COMMAND_EVENT`] naming what it
//! wants, and the renderer runs the handler the keyboard would have run. The
//! menu makes a command discoverable; the renderer decides what it does.
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

/// Raised when a menu item asks the renderer to carry out a command. The
/// payload is the item's id, one of [`RENDERER_COMMANDS`].
const MENU_COMMAND_EVENT: &str = "desktop-menu-command";

/// The menu items the renderer carries out. Each id doubles as the command
/// name the renderer receives, so the two sides share one vocabulary;
/// `ui/src/nativeMenu.ts` lists the same names and a test there reads this
/// list to keep them in step.
const RENDERER_COMMANDS: [&str; 9] = [
    MENU_NEW_ID,
    MENU_COMMAND_PALETTE_ID,
    MENU_SETTINGS_ID,
    MENU_TOGGLE_SIDEBAR_ID,
    MENU_ZOOM_IN_ID,
    MENU_ZOOM_OUT_ID,
    MENU_ZOOM_RESET_ID,
    MENU_KEYBOARD_SHORTCUTS_ID,
    MENU_DOCUMENTATION_ID,
];

const MENU_CHECK_FOR_UPDATES_ID: &str = "check-for-updates";
const MENU_SETTINGS_ID: &str = "settings";
const MENU_NEW_ID: &str = "new";
const MENU_COMMAND_PALETTE_ID: &str = "command-palette";
const MENU_CLOSE_TAB_ID: &str = "close-tab";
const MENU_CLOSE_WINDOW_ID: &str = "close-window";
const MENU_TOGGLE_SIDEBAR_ID: &str = "toggle-sidebar";
const MENU_ZOOM_RESET_ID: &str = "zoom-reset";
const MENU_ZOOM_IN_ID: &str = "zoom-in";
const MENU_ZOOM_OUT_ID: &str = "zoom-out";
const MENU_RELOAD_ID: &str = "reload-app";
const MENU_KEYBOARD_SHORTCUTS_ID: &str = "keyboard-shortcuts";
const MENU_DOCUMENTATION_ID: &str = "documentation";

/// Build and install the menu bar.
///
/// Everything Tauri's default menu carries is here, so the items macOS readers
/// reach for — Services, Hide, the Edit menu's clipboard commands, full
/// screen, Minimize — behave as they always did.
#[cfg(target_os = "macos")]
pub(crate) fn install_app_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{
        AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu, HELP_SUBMENU_ID,
        WINDOW_SUBMENU_ID,
    };

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
    let item = |id: &str, text: &str, accelerator: Option<&str>| {
        MenuItem::with_id(handle, id, text, true, accelerator)
    };

    let app_menu = Submenu::with_items(
        handle,
        pkg.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(handle, None, Some(about))?,
            &item(MENU_CHECK_FOR_UPDATES_ID, "Check for Updates…", None)?,
            &PredefinedMenuItem::separator(handle)?,
            &item(MENU_SETTINGS_ID, "Settings…", Some("CmdOrCtrl+,"))?,
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
    //
    // New is whatever Cmd+N makes where the reader is: work in chat mode, a
    // workspace in code mode. The renderer knows which mode is up; the menu
    // does not.
    let file_menu = Submenu::with_items(
        handle,
        "File",
        true,
        &[
            &item(MENU_NEW_ID, "New", Some("CmdOrCtrl+N"))?,
            &item(
                MENU_COMMAND_PALETTE_ID,
                "Command Palette…",
                Some("CmdOrCtrl+K"),
            )?,
            &PredefinedMenuItem::separator(handle)?,
            &item(MENU_CLOSE_TAB_ID, "Close Tab", Some("CmdOrCtrl+W"))?,
            &item(MENU_CLOSE_WINDOW_ID, "Close Window", None)?,
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

    // Reload keeps its item but gives up Cmd+R: Cmd+Shift+R rebases in code
    // mode, and a missed Shift used to throw away the whole window's state.
    let view_menu = Submenu::with_items(
        handle,
        "View",
        true,
        &[
            &item(
                MENU_TOGGLE_SIDEBAR_ID,
                "Toggle Sidebar",
                Some("CmdOrCtrl+B"),
            )?,
            &PredefinedMenuItem::separator(handle)?,
            &item(MENU_ZOOM_RESET_ID, "Actual Size", Some("CmdOrCtrl+0"))?,
            &item(MENU_ZOOM_IN_ID, "Zoom In", Some("CmdOrCtrl+="))?,
            &item(MENU_ZOOM_OUT_ID, "Zoom Out", Some("CmdOrCtrl+-"))?,
            &PredefinedMenuItem::separator(handle)?,
            &item(MENU_RELOAD_ID, "Reload", None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::fullscreen(handle, None)?,
        ],
    )?;

    // Tauri hands the submenus with these two ids to macOS, which lists the
    // open windows under Window and puts its search field at the top of Help.
    let window_menu = Submenu::with_id_and_items(
        handle,
        WINDOW_SUBMENU_ID,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(handle, None)?,
            &PredefinedMenuItem::maximize(handle, None)?,
            &PredefinedMenuItem::separator(handle)?,
            &PredefinedMenuItem::bring_all_to_front(handle, None)?,
        ],
    )?;

    let help_menu = Submenu::with_id_and_items(
        handle,
        HELP_SUBMENU_ID,
        "Help",
        true,
        &[
            &item(
                MENU_KEYBOARD_SHORTCUTS_ID,
                "Keyboard Shortcuts",
                Some("CmdOrCtrl+/"),
            )?,
            &item(MENU_DOCUMENTATION_ID, "Documentation", None)?,
        ],
    )?;

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
    let id = event.id().as_ref();
    if let Some(command) = renderer_command(id) {
        if let Err(error) = app.emit(MENU_COMMAND_EVENT, command) {
            eprintln!("tidebreak-desktop: could not raise the {command} menu command: {error}");
        }
        return;
    }
    match id {
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

/// The command a menu item hands the renderer, or `None` when the shell
/// carries the item out itself.
fn renderer_command(id: &str) -> Option<&'static str> {
    RENDERER_COMMANDS
        .iter()
        .copied()
        .find(|command| *command == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_the_renderer_only_the_commands_it_carries_out() {
        for command in RENDERER_COMMANDS {
            assert_eq!(renderer_command(command), Some(command));
        }
        // The shell answers these itself. Handing one to the renderer as well
        // would run it twice, or close the window from a renderer that has no
        // way to stop it.
        for native in [
            MENU_CHECK_FOR_UPDATES_ID,
            MENU_CLOSE_TAB_ID,
            MENU_CLOSE_WINDOW_ID,
            MENU_RELOAD_ID,
        ] {
            assert_eq!(renderer_command(native), None, "{native}");
        }
    }
}

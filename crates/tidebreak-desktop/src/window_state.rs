//! Where the window was, and how big, when the reader last quit.
//!
//! `tauri-plugin-window-state` saves the main window's size, position, and
//! maximized state when the app exits and puts them back at the next launch.
//! The first launch, or a launch after the display the window sat on has gone,
//! still opens centered at the size `tauri.conf.json` gives it.

use tauri::{LogicalSize, Manager, Runtime, WindowEvent};
use tauri_plugin_window_state::StateFlags;

const MAIN_WINDOW: &str = "main";

/// The plugin, restoring size, position, and whether the window was
/// maximized.
///
/// The plugin's other flags stay off. A window saved while hidden would open
/// hidden and leave the reader with no window at all; the overlay titlebar
/// fixes the decorations; and a relaunch that lands in a full-screen space is
/// a surprise rather than a return to where the reader was.
pub(crate) fn plugin<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_window_state::Builder::new()
        .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
        .build()
}

/// Hold the main window to the minimum size its configuration names.
///
/// The plugin saves the size in physical pixels. Saved on one display and
/// restored on another with a different scale, that size can land below the
/// minimum, and macOS holds only a drag to the minimum, not a programmatic
/// resize. So any resize that ends below the minimum is put back up to it.
pub(crate) fn keep_minimum_size<R: Runtime>(app: &tauri::App<R>) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW) else {
        return;
    };
    let configured = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == MAIN_WINDOW);
    let Some((Some(width), Some(height))) =
        configured.map(|config| (config.min_width, config.min_height))
    else {
        return;
    };
    let minimum = LogicalSize::new(width, height);
    let resized = window.clone();
    window.on_window_event(move |event| {
        let WindowEvent::Resized(size) = event else {
            return;
        };
        let Ok(scale) = resized.scale_factor() else {
            return;
        };
        if let Some(size) = raised_to_minimum(size.to_logical(scale), minimum) {
            if let Err(error) = resized.set_size(size) {
                eprintln!(
                    "tidebreak-desktop: could not hold the window to its minimum size: {error}"
                );
            }
        }
    });
}

/// The size a window resized to `size` should take instead, or `None` when it
/// already meets `minimum`.
fn raised_to_minimum(
    size: LogicalSize<f64>,
    minimum: LogicalSize<f64>,
) -> Option<LogicalSize<f64>> {
    // A minimized window can report no size at all, which is not a size to
    // correct.
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    // Converting between physical and logical pixels can shave a fraction off
    // a window that sits exactly at the minimum.
    const SLACK: f64 = 0.5;
    let short = size.width + SLACK < minimum.width || size.height + SLACK < minimum.height;
    short.then(|| {
        LogicalSize::new(
            size.width.max(minimum.width),
            size.height.max(minimum.height),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMUM: LogicalSize<f64> = LogicalSize {
        width: 720.0,
        height: 480.0,
    };

    #[test]
    fn raises_only_the_side_that_is_short() {
        // A 1300×800 window saved on a 1x display and restored on a 2x one
        // comes back as 650×400.
        assert_eq!(
            raised_to_minimum(LogicalSize::new(650.0, 400.0), MINIMUM),
            Some(MINIMUM)
        );
        assert_eq!(
            raised_to_minimum(LogicalSize::new(1_000.0, 400.0), MINIMUM),
            Some(LogicalSize::new(1_000.0, 480.0))
        );
    }

    #[test]
    fn leaves_a_window_at_or_above_the_minimum_alone() {
        assert_eq!(raised_to_minimum(MINIMUM, MINIMUM), None);
        assert_eq!(
            raised_to_minimum(LogicalSize::new(719.75, 479.5), MINIMUM),
            None
        );
        assert_eq!(
            raised_to_minimum(LogicalSize::new(1_280.0, 800.0), MINIMUM),
            None
        );
    }

    #[test]
    fn ignores_a_window_with_no_size() {
        assert_eq!(raised_to_minimum(LogicalSize::new(0.0, 0.0), MINIMUM), None);
    }
}

//! Tidebreak desktop — the Tauri application shell.
//!
//! Boots the in-process HTTP/WebSocket surface and hosts the chat UI webview.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// A debug build of this binary emits ~18MB of `__eh_frame`, past the 16MB the
// Mach-O compact unwind table can address, so `ld` warns on every dev link.
// Only unwinding a panic pays for that, and nothing here is on a hot path, so
// the warning is noise. Scoped tight: release builds and every other platform
// still surface whatever the linker has to say.
#![cfg_attr(all(target_os = "macos", debug_assertions), allow(linker_messages))]

fn main() {
    tidebreak_desktop_lib::run();
}

//! OpenWave desktop — the Tauri application shell.
//!
//! Boots the in-process HTTP/WebSocket surface and hosts the chat UI webview.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    openwave_desktop_lib::run();
}

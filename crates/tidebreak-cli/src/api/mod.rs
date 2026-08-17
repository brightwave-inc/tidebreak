//! The loopback client for the in-process server, shared by every CLI surface
//! that drives a chat: non-interactive print mode and CLI setup commands.
//!
//! [`client`] speaks the same HTTP+WebSocket contract the desktop webview
//! consumes; [`wire`] decodes the event socket's frames forward-compatibly.

pub mod client;
pub mod code;
pub mod wire;

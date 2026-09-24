//! Runtime configuration and supervision for external MCP servers.
//!
//! A definition is either a local stdio child process or a remote Streamable
//! HTTP endpoint. Definitions are typed data, never shell fragments. Every
//! child starts with a cleared environment plus HOME and the host search
//! PATH, and receives only the other values its definition names: literal
//! values held in the secret store, plus values selected by *name* from the
//! parent environment. An HTTP server's bearer token is either selected by
//! name or held in the secret store, and so are its custom header values.
//! **No credential value of any kind lives in a definition**: the
//! connected-app record and every API projection carry names only, and
//! values are resolved at the connection boundary.

mod oauth;
mod runtime;
mod stdio;
mod types;
mod validation;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use runtime::*;
pub use stdio::{resolve_stdio_executable, resolve_stdio_executable_on, FORWARDED_BY_DEFAULT};
pub use types::*;

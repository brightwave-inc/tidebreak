//! Runtime configuration and supervision for external MCP servers.
//!
//! A definition is either a local stdio child process or a remote Streamable
//! HTTP endpoint. Definitions are typed data, never shell fragments. Every
//! child starts with a cleared environment and receives only values named by
//! the definition: literal values held in the secret store, plus values
//! selected by *name* from the parent environment. An HTTP server's bearer
//! token is likewise selected by name. **No environment value of any kind
//! lives in a definition**: the connected-app record and every API projection
//! carry names only, and values are resolved at the connection boundary.

mod runtime;
mod types;
mod validation;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub(crate) use runtime::*;
pub(crate) use types::*;

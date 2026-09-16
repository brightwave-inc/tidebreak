//! Renderer-safe OAuth connection status for remote MCP servers.
//!
//! This module owns the wire vocabulary the desktop uses to render the Connect
//! action and its states for an HTTP MCP server that authenticates with OAuth.
//! The connector mechanics — discovery, registration, PKCE sign-in, refresh —
//! live in [`crate::connectors`] (`mcp_oauth`); this module is the projection
//! the UI reads and the state machine that drives it.
//!
//! The status is projected beside each server's [`McpHealth`], never merged
//! into it: health describes whether a *connected* session is live, while this
//! describes whether the user has authorized one at all. A server can be
//! `NotConnected` here yet absent from the active tool set, without that being
//! a health failure.
//!
//! [`McpHealth`]: crate::mcp_config::McpHealth

use serde::Serialize;

/// Connection state for a remote MCP server that authenticates with OAuth.
///
/// The states are exhaustive and ordered by the user's path through them:
/// a server is `Unsupported` or, once OAuth is detected, moves
/// `NotConnected` → `Authorizing` → `Connected`, and from `Connected` can fall
/// to `Expired` (refresh rejected) or `AccessDenied` (the authorization server
/// refused this user).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthState {
    /// The endpoint offers no OAuth authorization path this client can drive
    /// (no protected-resource metadata, or no dynamic-registration endpoint).
    Unsupported,
    /// OAuth is required and no session is stored: the user must Connect.
    NotConnected,
    /// A browser authorization is in flight.
    Authorizing,
    /// A usable session is stored and presented as the per-call bearer.
    Connected,
    /// A session was stored but its refresh was rejected: reconnect required.
    Expired,
    /// The authorization server refused access for this user.
    AccessDenied,
}

/// Renderer-safe OAuth status for one server. Carries no token material: the
/// only URL it ever holds is the system-browser authorization URL shown while
/// `Authorizing`, and that URL never contains a token.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct McpOAuthStatus {
    pub state: McpOAuthState,
    /// The system-browser URL to open while `Authorizing`. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pending_authorization_url: Option<String>,
    /// A bounded, secret-free reason shown for `Expired`, `AccessDenied`, or
    /// `Unsupported`. Never echoes a URL, token, or upstream body.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

impl McpOAuthStatus {
    #[must_use]
    pub fn of(state: McpOAuthState) -> Self {
        Self {
            state,
            pending_authorization_url: None,
            error: None,
        }
    }

    #[must_use]
    pub fn not_connected() -> Self {
        Self::of(McpOAuthState::NotConnected)
    }

    #[must_use]
    pub fn connected() -> Self {
        Self::of(McpOAuthState::Connected)
    }

    #[must_use]
    pub fn authorizing(authorization_url: String) -> Self {
        Self {
            state: McpOAuthState::Authorizing,
            pending_authorization_url: Some(authorization_url),
            error: None,
        }
    }

    #[must_use]
    pub fn failed(state: McpOAuthState, reason: impl Into<String>) -> Self {
        Self {
            state,
            pending_authorization_url: None,
            error: Some(reason.into()),
        }
    }
}

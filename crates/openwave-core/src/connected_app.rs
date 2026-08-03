//! Closed contract for profile-scoped connected apps.
//!
//! A connected app is the durable record of an outside integration the
//! profile can reach: an opaque [`ConnectedAppId`], a display name, a
//! [`ConnectedAppKind`], and a kind-specific definition. MCP server
//! definitions are one kind of connected app; a plain REST API with a
//! credential reference is the other. App manifests and grants bind
//! capabilities by this record's id, never by a raw server namespace.
//!
//! The definition is carried here as bounded JSON: each kind's typed shape,
//! validation, and fingerprint canonicalization live with the host layer that
//! owns the kind (the MCP runtime, the REST executor). The store enforces
//! only what is kind-independent — identity, naming, and bounds.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::ConnectedAppId;

/// Largest number of connected apps one profile retains, across kinds. Twice
/// the MCP server bound (32), leaving the same headroom for REST entries.
pub const MAX_CONNECTED_APPS: usize = 64;
/// Largest connected-app display name. For an `mcp_server` app the name is
/// also the mount namespace, which enforces its own tighter contract.
pub const MAX_CONNECTED_APP_NAME_CHARS: usize = 120;
/// Largest serialized kind-specific definition one record may carry. Sized
/// for a `rest_api` definition holding an ingested operation catalog, well
/// above any MCP transport config.
pub const MAX_CONNECTED_APP_DEFINITION_BYTES: usize = 2 * 1024 * 1024;

/// The transport class of a connected app. Closed vocabulary, persisted as
/// the snake_case string; deliberately the same words the model gateway uses
/// for its `connected_apps`, so promotion is a translation, not a reframing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectedAppKind {
    /// An MCP server definition (stdio command, HTTP url, or gateway
    /// endpoint), absorbed from the pre-record configuration.
    McpServer,
    /// A plain REST API: base URL, ingested OpenAPI operation catalog, and an
    /// optional credential *reference* into the profile secret store. Refused
    /// entirely on gateway-managed profiles by every write surface.
    RestApi,
}

impl ConnectedAppKind {
    /// The persisted string form of the kind.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::McpServer => "mcp_server",
            Self::RestApi => "rest_api",
        }
    }

    /// Parse the persisted string form back into the closed vocabulary.
    #[must_use]
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "mcp_server" => Some(Self::McpServer),
            "rest_api" => Some(Self::RestApi),
            _ => None,
        }
    }
}

impl std::fmt::Display for ConnectedAppKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One profile-scoped connected app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedApp {
    /// Durable opaque identity — what app-keyed bindings and grants name.
    pub id: ConnectedAppId,
    /// Display name. For an `mcp_server` app this is also the namespace its
    /// tools mount under (`mcp__{name}__…`).
    pub name: String,
    /// Which kind of integration the definition describes.
    pub kind: ConnectedAppKind,
    /// Kind-specific definition, validated by the layer that owns the kind
    /// before it reaches the store. Bounded here so no write path can persist
    /// an unbounded blob.
    pub definition: serde_json::Value,
    /// Creation time of the record.
    pub created_at: DateTime<Utc>,
    /// Last time the definition or name changed.
    pub updated_at: DateTime<Utc>,
}

/// Validate the kind-independent contract of a connected app record.
///
/// Kind-specific validation (transport invariants, catalog shape) happens in
/// the owning layer before the store is reached; this is the storage door's
/// backstop on identity, naming, and bounds.
pub fn validate_connected_app(app: &ConnectedApp) -> Result<(), String> {
    if app.name.is_empty() || app.name.chars().count() > MAX_CONNECTED_APP_NAME_CHARS {
        return Err(format!(
            "connected app name must contain between 1 and {MAX_CONNECTED_APP_NAME_CHARS} characters"
        ));
    }
    if app.name.trim() != app.name {
        return Err("connected app name may not have surrounding whitespace".into());
    }
    if app.name.chars().any(char::is_control) {
        return Err("connected app name may not contain control characters".into());
    }
    if !app.definition.is_object() {
        return Err("connected app definition must be a JSON object".into());
    }
    let encoded_len = app.definition.to_string().len();
    if encoded_len > MAX_CONNECTED_APP_DEFINITION_BYTES {
        return Err(format!(
            "connected app definition is too large ({encoded_len} bytes, \
             maximum {MAX_CONNECTED_APP_DEFINITION_BYTES})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_round_trip_and_stay_closed() {
        for kind in [ConnectedAppKind::McpServer, ConnectedAppKind::RestApi] {
            assert_eq!(ConnectedAppKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ConnectedAppKind::parse("oauth_connector"), None);
    }

    #[test]
    fn records_enforce_naming_and_definition_bounds() {
        let app = |name: &str, definition: serde_json::Value| ConnectedApp {
            id: ConnectedAppId::new(),
            name: name.into(),
            kind: ConnectedAppKind::McpServer,
            definition,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(validate_connected_app(&app("sentry", serde_json::json!({}))).is_ok());
        for name in ["", " padded ", "line\nbreak", &"x".repeat(121)] {
            assert!(validate_connected_app(&app(name, serde_json::json!({}))).is_err());
        }
        assert!(
            validate_connected_app(&app("sentry", serde_json::json!([]))).is_err(),
            "a definition must be an object"
        );
        assert!(
            validate_connected_app(&app(
                "sentry",
                serde_json::json!({ "blob": "x".repeat(MAX_CONNECTED_APP_DEFINITION_BYTES) })
            ))
            .is_err(),
            "an oversized definition is refused at the storage door"
        );
    }
}

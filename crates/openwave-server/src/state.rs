//! Shared application state handed to every request handler.

use std::sync::Arc;

use openwave_core::{Config, Store};
use uuid::Uuid;

/// The state cloned into each handler: the boot config, the durable store, and
/// the per-launch bearer token that guards the API.
///
/// Cheap to clone — everything shared is behind an `Arc`.
#[derive(Clone)]
pub struct AppState {
    /// Boot configuration for this launch.
    pub config: Arc<Config>,
    /// Durable metadata and conversation state.
    pub store: Arc<dyn Store>,
    /// The secret every request must present as `Authorization: Bearer <token>`.
    pub token: Arc<str>,
}

impl AppState {
    /// Assemble state, minting a fresh random bearer token for this launch.
    ///
    /// The token is generated per launch (not persisted): the server binds to a
    /// loopback port, and the token is handed to the local client that spawned
    /// it, so a fresh secret each run is exactly what we want.
    pub fn new(config: Config, store: Arc<dyn Store>) -> Self {
        Self {
            config: Arc::new(config),
            store,
            token: Uuid::new_v4().to_string().into(),
        }
    }
}

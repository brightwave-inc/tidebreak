//! The adapter half: probe, capabilities, launch.

use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_core::db::DbStore;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind, ReasoningEffort};
use tidebreak_harness::{
    HarnessAdapter, HarnessError, HarnessProbe, HarnessSession, HostEnv, ListedHarnessModel,
    SessionSpec,
};

use super::session::InternalSession;
use crate::code::bus::CodeEventBus;
use crate::state::AppState;

/// The in-process engine, registered under [`HarnessKind::Internal`].
///
/// Holds the application state the chat turn lane runs on: the store, the
/// chat event bus, the approval broker, and the turn wake handles. It is
/// installed after the state exists, like the recap and rewrite hooks, and
/// the copy it keeps carries no code runtime, so nothing here can reach back
/// into the runtime that owns it. What it does keep from the runtime is the
/// journal store and the session bus, because the lane journals straight
/// into the session's code journal and the engine follows it there.
pub(crate) struct InternalAdapter {
    state: AppState,
    db: Arc<DbStore>,
    bus: Arc<CodeEventBus>,
}

impl InternalAdapter {
    pub(crate) fn new(state: AppState, db: Arc<DbStore>, bus: Arc<CodeEventBus>) -> Self {
        Self { state, db, bus }
    }
}

#[async_trait]
impl HarnessAdapter for InternalAdapter {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Internal
    }

    async fn probe(&self, _host: &HostEnv) -> HarnessProbe {
        HarnessProbe {
            found: true,
            // In-process: no binary to resolve, nothing to pin or download.
            binary_path: None,
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            // The engine has no sign-in of its own. The provider that serves
            // it is configured in Tidebreak's settings, and a turn with no
            // usable provider fails the same way a chat turn does.
            authenticated: Some(true),
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn capabilities(&self, _probe: &HarnessProbe) -> HarnessCaps {
        HarnessCaps {
            resume: CapLevel::Supported,
            streaming_deltas: CapLevel::Supported,
            structured_approvals: CapLevel::Supported,
            mid_turn_steering: CapLevel::Supported,
            plan_mode: CapLevel::Supported,
            auto_mode: CapLevel::Supported,
            allow_mode: CapLevel::Supported,
            reasoning_levels: CapLevel::Supported,
            native_file_change_events: CapLevel::Unsupported,
            native_interrupt: CapLevel::Supported,
            // Chat's attachment model is the image path for the internal
            // engine; hydrated bytes on the turn input are not wired yet.
            image_input: CapLevel::Unsupported,
            slash_commands: CapLevel::Unsupported,
            durable_parks: CapLevel::Supported,
            user_questions: CapLevel::Supported,
            standing_grants: CapLevel::Supported,
        }
    }

    fn reasoning_efforts(&self, _probe: &HarnessProbe) -> Vec<ReasoningEffort> {
        ReasoningEffort::ALL.to_vec()
    }

    async fn list_models(&self, _probe: &HarnessProbe) -> Vec<ListedHarnessModel> {
        // The chat model catalog already serves this engine's models through
        // the models routes; it lists nothing engine-specific.
        Vec::new()
    }

    async fn launch(&self, spec: SessionSpec) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let session =
            InternalSession::launch(self.state.clone(), self.db.clone(), self.bus.clone(), spec)
                .await?;
        Ok(Box::new(session))
    }
}

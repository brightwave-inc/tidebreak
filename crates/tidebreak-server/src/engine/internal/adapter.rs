//! The adapter half: probe, capabilities, launch.

use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_core::db::DbStore;
use tidebreak_core::{CapLevel, HarnessCaps, HarnessKind, ReasoningEffort};
use tidebreak_harness::{
    HarnessAdapter, HarnessError, HarnessProbe, HarnessSession, HostEnv, ListedHarnessModel,
    SessionSpec,
};
use tokio::sync::Semaphore;

use super::leg::{LegDriver, LegDriverConfig};
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
    /// Process-wide ceiling on concurrent internal-engine model calls.
    /// Sized from the same config as [`LegDriverConfig`].
    concurrency: Arc<Semaphore>,
    driver: LegDriver,
}

impl InternalAdapter {
    pub(crate) fn new(state: AppState, db: Arc<DbStore>, bus: Arc<CodeEventBus>) -> Self {
        let driver = LegDriver::new(
            state.store.clone(),
            state.resolver.clone(),
            state.secrets.clone(),
            state.provisioned_policy.clone(),
            state.os_policy.clone(),
            state.tools.clone(),
            state.approvals.clone(),
            state.events.clone(),
            state.active_turns.clone(),
            state.turn_job_wake.clone(),
            state.agent_run_wake.clone(),
            state.queued_turn_wake.clone(),
            state.agent_config.clone(),
            Some(state.config.data_dir.join("scratch")),
            LegDriverConfig::default(),
        )
        .with_blobs(state.blobs.clone())
        .with_blob_write_locks(state.blob_writes.clone())
        .with_mcp_runtime(state.mcp.clone());
        Self {
            state,
            db,
            bus,
            concurrency: Arc::new(Semaphore::new(LegDriverConfig::default().max_concurrency)),
            driver,
        }
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
            // Hydrated bytes on the turn input publish through chat's
            // attachment model and reach the model request as image blocks.
            image_input: CapLevel::Supported,
            slash_commands: CapLevel::Unsupported,
            durable_parks: CapLevel::Supported,
            user_questions: CapLevel::Supported,
            standing_grants: CapLevel::Supported,
            mid_turn_resume: CapLevel::Supported,
            transcript: CapLevel::Supported,
            memory_loopback: CapLevel::Unsupported,
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
        let session = InternalSession::launch(
            self.state.clone(),
            self.db.clone(),
            self.bus.clone(),
            self.concurrency.clone(),
            self.driver.clone(),
            spec,
        )
        .await?;
        Ok(Box::new(session))
    }
}

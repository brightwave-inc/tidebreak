//! Shared application state handed to every request handler.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use openwave_core::{
    AgentConfig, AgentError, AgentRunId, BlobStore, CallId, CancelToken, ChatId, Config,
    FsBlobStore, Profile, Result, SecretProvider, SteerInbox, Store, ToolRegistry, TurnId,
};
use tokio::sync::{mpsc, Notify, Semaphore};
use uuid::Uuid;

use crate::approvals::ApprovalBroker;
use crate::bus::EventBus;
use crate::mcp_config::McpRuntime;
use crate::resolver::ProviderResolver;
use crate::view_frames::ViewFrameTokens;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVoiceStatus {
    pub state: LocalVoiceState,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalVoiceState {
    NotInstalled,
    Downloading,
    Ready,
    Failed,
    Unavailable,
}

/// Why a local transcription attempt failed.
///
/// The distinction is the caller's, not the runner's: a recording the decoder
/// rejects is the upload's problem and answers `4xx`, while a model or worker
/// fault is ours and answers `500`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalVoiceError {
    /// The recording's container or codec is not one this runner accepts.
    UnsupportedMedia(String),
    /// The recording is in an accepted format but could not be decoded.
    Undecodable(String),
    /// The runner itself failed — model install, inference, or its worker.
    Runner(String),
}

impl LocalVoiceError {
    pub fn message(&self) -> &str {
        match self {
            Self::UnsupportedMedia(message)
            | Self::Undecodable(message)
            | Self::Runner(message) => message,
        }
    }
}

/// The trusted half of local voice input. Every call names a catalog model by
/// id; the runner owns where its bytes live and how they are verified.
#[async_trait::async_trait]
pub trait LocalVoiceRunner: Send + Sync {
    async fn status(&self, model: &str) -> LocalVoiceStatus;
    async fn install(&self, model: &str) -> std::result::Result<LocalVoiceStatus, String>;
    async fn transcribe(
        &self,
        model: &str,
        content_type: &str,
        audio: Vec<u8>,
    ) -> std::result::Result<String, LocalVoiceError>;
}

struct UnavailableLocalVoiceRunner;

#[async_trait::async_trait]
impl LocalVoiceRunner for UnavailableLocalVoiceRunner {
    async fn status(&self, _model: &str) -> LocalVoiceStatus {
        LocalVoiceStatus {
            state: LocalVoiceState::Unavailable,
            downloaded_bytes: None,
            total_bytes: None,
            error: Some("local voice input is available only in the desktop app".into()),
        }
    }

    async fn install(&self, _model: &str) -> std::result::Result<LocalVoiceStatus, String> {
        Err("local voice input is available only in the desktop app".into())
    }

    async fn transcribe(
        &self,
        _model: &str,
        _content_type: &str,
        _audio: Vec<u8>,
    ) -> std::result::Result<String, LocalVoiceError> {
        Err(LocalVoiceError::Runner(
            "local voice input is available only in the desktop app".into(),
        ))
    }
}

/// The state cloned into each handler: the boot config, the durable store, the
/// agent's dependencies (provider + tools + tuning), the per-launch bearer token,
/// and the guard that keeps a chat to one running turn at a time.
///
/// Cheap to clone — everything shared is behind an `Arc`.
#[derive(Clone)]
pub struct AppState {
    /// Boot configuration for this launch.
    pub config: Arc<Config>,
    /// Durable metadata, conversation state, and the event journal.
    pub store: Arc<dyn Store>,
    /// Durable raw bytes and generated artifacts under the configured data directory.
    pub blobs: Arc<dyn BlobStore>,
    /// Builds the model provider for each turn from the configured credentials.
    pub resolver: Arc<dyn ProviderResolver>,
    /// Credential custody — where the model API key is read from and written to.
    pub secrets: Arc<dyn SecretProvider>,
    /// The tools available to the agent.
    pub tools: Arc<ToolRegistry>,
    /// Runtime-managed MCP connections and immutable per-turn tool snapshots.
    pub(crate) mcp: Arc<McpRuntime>,
    /// Executes one governed REST operation for the app-invoke route. The
    /// production assembly is the real executor over the real transport and
    /// resolver ([`crate::connected_apps::governed_rest_dispatcher`]); tests
    /// substitute a fake transport behind the same governed validation.
    pub(crate) rest_dispatch: Arc<dyn crate::connected_apps::RestOperationDispatcher>,
    /// Outstanding single-use tokens redeemed by the sandboxed view frames —
    /// prefetched MCP views and stored local-app revisions alike.
    pub(crate) view_frames: Arc<ViewFrameTokens>,
    /// The signed-in model-gateway session handle (sign-in, model sync,
    /// per-request tokens). Always the same instance `mcp` dispatches
    /// through — see [`Self::with_gateway_runtime`] for why the process
    /// must hold exactly one.
    pub(crate) gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
    /// ChatGPT subscription OAuth for the OpenAI provider (sign-in, sign-out,
    /// per-request tokens). Shared with the resolver for the reason given in
    /// [`Self::with_gateway_runtime`].
    pub(crate) chatgpt: Arc<crate::chatgpt_runtime::ChatGptRuntime>,
    /// The OS-managed policy reader for managed-mode resolution. Directly
    /// assembled state (tests, custom hosts) gets the source that asserts
    /// nothing, so it reads nothing from the host OS; the production
    /// assembly in `bind_inner` injects the platform's reader via
    /// `managed_policy::platform_source`.
    pub(crate) os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    /// Wakes the durable turn worker after acceptance or cancellation commits.
    pub(crate) turn_job_wake: Arc<Notify>,
    /// Wakes the bounded sandbox-run worker after delegated work commits.
    ///
    /// This is only a latency hint; the worker always uses its durable claim
    /// scan as the correctness source after a missed or coalesced wake-up.
    pub(crate) agent_run_wake: Arc<Notify>,
    /// Wakes the source-blob retirement worker after a reference drop commits.
    pub(crate) blob_retirement_wake: Arc<Notify>,
    /// Coordinates source publication and retirement across server processes.
    pub(crate) blob_writes: Arc<BlobWriteGuard>,
    /// Bounds expensive document renderers used by post-turn binary previews.
    pub(crate) file_preview_permits: Arc<Semaphore>,
    /// Per-turn agent tuning (model, limits, …).
    pub agent_config: AgentConfig,
    /// The secret every request must present as `Authorization: Bearer <token>`.
    /// Authenticates the local owner on the desktop profile; on self-host it
    /// names nobody and admits nobody.
    pub token: Arc<str>,
    /// The self-host profile's named bearer tokens (credential → user).
    /// Loaded from `Config::auth_tokens_file` at assembly; empty on desktop,
    /// where `require_token` never consults it.
    pub(crate) principal_tokens: Arc<crate::auth::TokenMap>,
    /// A second per-launch secret required for native-only operations.
    pub(crate) client_executor_token: Arc<str>,
    /// Stable private identity owning native attachment reconciliation work.
    pub(crate) client_executor_id: Uuid,
    /// Whether this embedding supplied a restart-stable attachment executor.
    pub(crate) root_attachment_routes_enabled: bool,
    /// Process-local cancel/steer handles for exact durably claimed attempts.
    pub active_turns: Arc<TurnGuard>,
    /// Process-local, exact-attempt cancellation signals for sandbox work.
    ///
    /// Durable run and tool-call state remains authoritative. These handles
    /// only let an authenticated cancellation request promptly drop provider
    /// futures owned by this server process.
    pub(crate) sandbox_attempts: Arc<SandboxAttemptGuard>,
    /// Process-local steering sinks for sandbox-resident runs attached here.
    ///
    /// Mid-run steering is delivered over the connection the container driver
    /// holds, so a run this process is not attached to cannot be steered and
    /// the request is refused rather than queued.
    pub(crate) sandbox_steering: Arc<SandboxSteerGuard>,
    /// Live fan-out of turn events to connected WebSocket clients.
    pub events: Arc<EventBus>,
    /// Coordinates durable Sensitive-tool decisions and low-latency wakeups.
    pub approvals: Arc<ApprovalBroker>,
    pub(crate) local_voice: Arc<dyn LocalVoiceRunner>,
    /// The configured code-execution provider, when this embedding has one.
    ///
    /// Installed after assembly, like the blob store: the provider is built
    /// before the state that carries its event bus. Only the plugin
    /// management routes read it — they list what is installed, which is the
    /// provider's own load — so an embedding without one (tests, headless)
    /// simply has nothing to manage.
    pub(crate) code_execution: Option<Arc<crate::code_execution::ConfiguredCodeExecutionProvider>>,
    /// The host-folder surface for local-app folder bindings, when this
    /// embedding has one (docs/folder-bindings.md).
    ///
    /// Installed after assembly by the desktop over its broker client;
    /// absent everywhere else, where folder bindings read as not connected
    /// and refuse to grant instead of parking.
    pub(crate) host_folders: Option<Arc<dyn crate::host_folders::HostFolders>>,
}

impl AppState {
    /// Assemble state, minting a fresh random bearer token for this launch.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        tools: Arc<ToolRegistry>,
        agent_config: AgentConfig,
    ) -> Self {
        let mut state = Self::new_with_client_executor_id(
            config,
            store,
            resolver,
            secrets,
            tools,
            agent_config,
            Uuid::new_v4(),
        )
        .expect("state assembly with a generated executor id only fails on a self-host config whose token file is missing or invalid");
        state.root_attachment_routes_enabled = false;
        state
    }

    /// Assemble state with a stable native executor identity supplied by an
    /// app-private embedding boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_client_executor_id(
        config: Config,
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        tools: Arc<ToolRegistry>,
        agent_config: AgentConfig,
        client_executor_id: Uuid,
    ) -> Result<Self> {
        // Hermetic default wiring: the policy source that asserts nothing and
        // one gateway runtime built over these same inputs. The sharing
        // invariant lives in `with_gateway_runtime`; this only picks inputs.
        let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
            Arc::new(crate::managed_policy::NoOsPolicy);
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            os_policy.clone(),
        );
        let chatgpt = Arc::new(crate::chatgpt_runtime::ChatGptRuntime::new(
            store.clone(),
            secrets.clone(),
        )?);
        Self::with_gateway_runtime(
            config,
            store,
            resolver,
            secrets,
            tools,
            agent_config,
            client_executor_id,
            gateway,
            chatgpt,
            os_policy,
        )
    }

    /// Assemble state around the one process-wide gateway runtime and the OS
    /// policy source it was built from.
    ///
    /// Every holder that reaches the gateway — the provider resolver, the
    /// `/gateway` routes (`state.gateway`), and MCP dispatch (`state.mcp`) —
    /// must share this single instance. Attestation contexts live in a
    /// per-instance in-memory registry, so a second instance mints a chat's
    /// inference tokens and its MCP call bearers in *different* attestation
    /// contexts and the gateway refuses every attested `tools/call` (#1441).
    /// Refresh rotation is likewise serialized per instance; two instances
    /// over the same keychain entry can race a stale refresh token into the
    /// gateway's reuse detection (a spurious full sign-out).
    ///
    /// `chatgpt` is shared for the same reason: the resolver's per-request
    /// token source and the `/providers/openai/chatgpt` routes refresh and
    /// revoke the one stored subscription session, and only a shared instance
    /// serializes them.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_gateway_runtime(
        config: Config,
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        tools: Arc<ToolRegistry>,
        agent_config: AgentConfig,
        client_executor_id: Uuid,
        gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
        chatgpt: Arc<crate::chatgpt_runtime::ChatGptRuntime>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Result<Self> {
        if client_executor_id.is_nil() {
            return Err(AgentError::config("client executor id must not be nil"));
        }
        // Resolve the profile's principal-naming credentials up front, so a
        // self-host server that could authenticate nobody fails assembly
        // instead of starting and rejecting every request.
        let principal_tokens = match config.profile {
            Profile::SelfHost => {
                let path = config.auth_tokens_file.as_deref().ok_or_else(|| {
                    AgentError::config(
                        "self-host requires OPENWAVE_AUTH_TOKENS_FILE (named bearer tokens \
                         mapping token to user; see the auth module docs)",
                    )
                })?;
                Arc::new(crate::auth::TokenMap::load(path)?)
            }
            // Desktop (and any future single-user embedding) authenticates
            // with the per-launch bearer only.
            _ => Arc::default(),
        };
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(config.data_dir.join("blobs")));
        let blob_writes = Arc::new(BlobWriteGuard::new(config.data_dir.join("blob-locks")));
        let mcp = Arc::new(McpRuntime::new(
            tools.clone(),
            store.clone(),
            secrets.clone(),
            gateway.clone(),
            os_policy.clone(),
        ));
        let rest_dispatch = crate::connected_apps::governed_rest_dispatcher(secrets.clone());
        Ok(Self {
            config: Arc::new(config),
            store: store.clone(),
            blobs,
            resolver,
            secrets,
            tools,
            mcp,
            rest_dispatch,
            view_frames: Arc::default(),
            gateway,
            chatgpt,
            os_policy,
            turn_job_wake: Arc::new(Notify::new()),
            agent_run_wake: Arc::new(Notify::new()),
            blob_retirement_wake: Arc::new(Notify::new()),
            blob_writes,
            file_preview_permits: Arc::new(Semaphore::new(2)),
            agent_config,
            token: Uuid::new_v4().to_string().into(),
            principal_tokens,
            client_executor_token: mint_client_executor_token(),
            client_executor_id,
            root_attachment_routes_enabled: true,
            active_turns: Arc::new(TurnGuard::default()),
            sandbox_attempts: Arc::new(SandboxAttemptGuard::default()),
            sandbox_steering: Arc::new(SandboxSteerGuard::default()),
            events: Arc::new(EventBus::default()),
            approvals: Arc::new(ApprovalBroker::new(store)),
            local_voice: Arc::new(UnavailableLocalVoiceRunner),
            code_execution: None,
            host_folders: None,
        })
    }

    pub fn set_local_voice_runner(&mut self, runner: Arc<dyn LocalVoiceRunner>) {
        self.local_voice = runner;
    }
}

/// Exact local cancellation handles for sandbox model and checkpoint attempts.
///
/// Every executor lane shares the checkpoint map — search, delegated file read,
/// command execution — because a cancellation reaches them all the same way:
/// by the exact durable call, run, and executor lease identity, never by tool
/// name. Registration is RAII: stale permits cannot remove a newer lease for
/// the same durable identity. The maps are intentionally process-local and
/// never participate in admission or terminal decisions.
#[derive(Default)]
pub(crate) struct SandboxAttemptGuard {
    models: Mutex<HashMap<(AgentRunId, Uuid), CancelToken>>,
    checkpoints: Mutex<HashMap<(CallId, AgentRunId, Uuid), CancelToken>>,
}

impl SandboxAttemptGuard {
    pub(crate) fn register_model(
        self: &Arc<Self>,
        agent_run_id: AgentRunId,
        lease_token: Uuid,
    ) -> Option<ActiveSandboxModelAttempt> {
        let mut models = self.models.lock().unwrap();
        let identity = (agent_run_id, lease_token);
        if models.contains_key(&identity) {
            return None;
        }
        let cancel = CancelToken::new();
        models.insert(identity, cancel.clone());
        Some(ActiveSandboxModelAttempt {
            guard: Arc::clone(self),
            agent_run_id,
            lease_token,
            cancel,
        })
    }

    pub(crate) fn register_checkpoint(
        self: &Arc<Self>,
        call_id: CallId,
        agent_run_id: AgentRunId,
        executor_lease_token: Uuid,
    ) -> Option<ActiveSandboxCheckpointAttempt> {
        let mut checkpoints = self.checkpoints.lock().unwrap();
        let identity = (call_id, agent_run_id, executor_lease_token);
        if checkpoints.contains_key(&identity) {
            return None;
        }
        let cancel = CancelToken::new();
        checkpoints.insert(identity, cancel.clone());
        Some(ActiveSandboxCheckpointAttempt {
            guard: Arc::clone(self),
            call_id,
            agent_run_id,
            executor_lease_token,
            cancel,
        })
    }

    /// Signal only a model handle owned by the exact durable run lease.
    pub(crate) fn cancel_model(&self, agent_run_id: AgentRunId, lease_token: Uuid) -> bool {
        if let Some(cancel) = self
            .models
            .lock()
            .unwrap()
            .get(&(agent_run_id, lease_token))
        {
            cancel.cancel();
            return true;
        }
        false
    }

    /// Signal only the exact sandbox call, run, and executor lease identity.
    pub(crate) fn cancel_checkpoint(
        &self,
        call_id: CallId,
        agent_run_id: AgentRunId,
        executor_lease_token: Uuid,
    ) -> bool {
        if let Some(cancel) =
            self.checkpoints
                .lock()
                .unwrap()
                .get(&(call_id, agent_run_id, executor_lease_token))
        {
            cancel.cancel();
            return true;
        }
        false
    }
}

/// Steering handles for sandbox-resident runs this process currently holds a
/// live connection to.
///
/// Steering a background run is attached-only: the instruction rides the
/// connection the container driver is holding right now, and there is no durable
/// queue behind it. Registration is therefore exactly as long as one attached
/// connection — the driver registers after its attach handshake is accepted and
/// the RAII handle deregisters when that connection ends — so an absent entry
/// means "nobody can carry this instruction", which is what the API reports.
/// Like [`SandboxAttemptGuard`], this map is process-local and never
/// participates in admission or terminal decisions.
#[derive(Default)]
pub(crate) struct SandboxSteerGuard {
    attached: Mutex<HashMap<(AgentRunId, Uuid), mpsc::Sender<String>>>,
}

/// Why one steering instruction could not be handed to a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxSteerRefusal {
    /// No connection to the run is live in this process, so nothing can carry
    /// the instruction. Nothing was queued.
    NotAttached,
    /// The run is attached but has not consumed the instructions already sent;
    /// the caller retries rather than growing an unbounded backlog.
    Backlogged,
}

impl SandboxSteerGuard {
    /// Register the live connection's steering sink for one exact run and lease.
    ///
    /// Returns `None` when that identity is already registered, so a superseded
    /// registration can never displace the live one.
    pub(crate) fn register(
        self: &Arc<Self>,
        agent_run_id: AgentRunId,
        lease_token: Uuid,
        sink: mpsc::Sender<String>,
    ) -> Option<AttachedSandboxRun> {
        let mut attached = self.attached.lock().unwrap();
        let identity = (agent_run_id, lease_token);
        if attached.contains_key(&identity) {
            return None;
        }
        attached.insert(identity, sink);
        Some(AttachedSandboxRun {
            guard: Arc::clone(self),
            agent_run_id,
            lease_token,
        })
    }

    /// Hand `text` to the run's live connection.
    ///
    /// Acceptance means a live connection took the instruction, not that the
    /// agent has read it; the run's own event stream reports when it is applied.
    ///
    /// # Errors
    /// [`SandboxSteerRefusal::NotAttached`] when no connection to the run is
    /// live here, or [`SandboxSteerRefusal::Backlogged`] when the live one has
    /// not drained what it was already given.
    pub(crate) fn steer(
        &self,
        agent_run_id: AgentRunId,
        text: String,
    ) -> std::result::Result<(), SandboxSteerRefusal> {
        let attached = self.attached.lock().unwrap();
        // At most one connection per run is live, but it is keyed by the exact
        // lease that owns it, which the caller does not (and should not) name.
        let sink = attached
            .iter()
            .find(|((run, _lease), _sink)| *run == agent_run_id)
            .map(|(_identity, sink)| sink.clone())
            .ok_or(SandboxSteerRefusal::NotAttached)?;
        drop(attached);
        sink.try_send(text).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SandboxSteerRefusal::Backlogged,
            mpsc::error::TrySendError::Closed(_) => SandboxSteerRefusal::NotAttached,
        })
    }
}

/// The registration of one attached sandbox-resident run, deregistered on drop.
pub(crate) struct AttachedSandboxRun {
    guard: Arc<SandboxSteerGuard>,
    agent_run_id: AgentRunId,
    lease_token: Uuid,
}

impl Drop for AttachedSandboxRun {
    fn drop(&mut self) {
        self.guard
            .attached
            .lock()
            .unwrap()
            .remove(&(self.agent_run_id, self.lease_token));
    }
}

pub(crate) struct ActiveSandboxModelAttempt {
    guard: Arc<SandboxAttemptGuard>,
    agent_run_id: AgentRunId,
    lease_token: Uuid,
    cancel: CancelToken,
}

impl ActiveSandboxModelAttempt {
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for ActiveSandboxModelAttempt {
    fn drop(&mut self) {
        self.guard
            .models
            .lock()
            .unwrap()
            .remove(&(self.agent_run_id, self.lease_token));
    }
}

pub(crate) struct ActiveSandboxCheckpointAttempt {
    guard: Arc<SandboxAttemptGuard>,
    call_id: CallId,
    agent_run_id: AgentRunId,
    executor_lease_token: Uuid,
    cancel: CancelToken,
}

impl ActiveSandboxCheckpointAttempt {
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for ActiveSandboxCheckpointAttempt {
    fn drop(&mut self) {
        self.guard.checkpoints.lock().unwrap().remove(&(
            self.call_id,
            self.agent_run_id,
            self.executor_lease_token,
        ));
    }
}

#[cfg(test)]
pub(crate) const TEST_CLIENT_EXECUTOR_TOKEN: &str = "test-native-client-executor";

fn mint_client_executor_token() -> Arc<str> {
    #[cfg(test)]
    return TEST_CLIENT_EXECUTOR_TOKEN.into();

    #[cfg(not(test))]
    return Uuid::new_v4().to_string().into();
}

/// Cross-process exclusion for one content-addressed blob lifecycle.
///
/// Lock files are permanent stable rendezvous points. Removing one could split
/// waiters across different inodes, so the grace-period auditor must ignore this
/// dedicated directory.
pub(crate) struct BlobWriteGuard {
    root: Arc<PathBuf>,
}

pub(crate) struct BlobWritePermit {
    _file: File,
}

impl BlobWriteGuard {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
        }
    }

    pub(crate) async fn acquire(&self, blob_id: Uuid) -> Result<BlobWritePermit> {
        let root = Arc::clone(&self.root);
        tokio::task::spawn_blocking(move || {
            fs::create_dir_all(&*root).map_err(blob_lock_error)?;
            let path = root.join(format!("{blob_id}.lock"));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true);
            #[cfg(unix)]
            options.mode(0o600);
            let file = options.open(path).map_err(blob_lock_error)?;
            file.lock().map_err(blob_lock_error)?;
            Ok(BlobWritePermit { _file: file })
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob lock task failed: {error}")))?
    }
}

fn blob_lock_error(error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to acquire blob lifecycle lock: {error}"))
}

/// Exact local handles for one durably claimed turn attempt.
#[derive(Clone)]
struct TurnHandles {
    turn_id: TurnId,
    lease_token: Uuid,
    cancel: CancelToken,
    steer: SteerInbox,
}

/// Locates the process-local signals for an exact durably claimed attempt.
///
/// The database owns admission and single-writer fencing. This registry only
/// lets cancel/steer ingress reach the worker currently executing that exact
/// `(chat, turn, lease)` identity in this process.
#[derive(Default)]
pub struct TurnGuard {
    active: Mutex<HashMap<ChatId, TurnHandles>>,
    released: tokio::sync::Notify,
}

impl TurnGuard {
    /// Register one exact local attempt, or refuse a conflicting local worker.
    pub fn register(
        self: &Arc<Self>,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: Uuid,
    ) -> Option<ActiveTurn> {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(&chat_id) {
            return None;
        }
        let cancel = CancelToken::new();
        let steer = SteerInbox::new();
        active.insert(
            chat_id,
            TurnHandles {
                turn_id,
                lease_token,
                cancel: cancel.clone(),
                steer: steer.clone(),
            },
        );
        Some(ActiveTurn {
            guard: Arc::clone(self),
            chat_id,
            turn_id,
            lease_token,
            cancel,
            steer,
        })
    }

    /// Wait until no local worker owns this chat.
    pub async fn wait_until_vacant(&self, chat_id: ChatId) {
        loop {
            let released = self.released.notified();
            if !self.active.lock().unwrap().contains_key(&chat_id) {
                return;
            }
            released.await;
        }
    }

    /// Trip cancellation only for the exact turn currently executing locally.
    pub fn cancel(&self, chat_id: ChatId, turn_id: TurnId) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.turn_id == turn_id => {
                handles.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Wake the exact local worker after durable steering admission commits.
    ///
    /// The instruction remains in the store; this process-local signal only
    /// reduces delivery latency and optionally preempts the provider stream.
    pub fn signal_steer(&self, chat_id: ChatId, turn_id: TurnId, interrupt: bool) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.turn_id == turn_id => handles.steer.signal_durable(interrupt),
            _ => false,
        }
    }
}

/// A held turn slot; releases the chat on drop.
pub struct ActiveTurn {
    guard: Arc<TurnGuard>,
    chat_id: ChatId,
    turn_id: TurnId,
    lease_token: Uuid,
    cancel: CancelToken,
    steer: SteerInbox,
}

impl ActiveTurn {
    /// The cancel token for this turn — handed to the agent so a `POST .../cancel`
    /// routed through [`TurnGuard::cancel`] stops it.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The steer inbox for this turn — handed to the agent so a durable steer
    /// notification injects mid-turn.
    pub fn steer_inbox(&self) -> SteerInbox {
        self.steer.clone()
    }
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        let mut active = self.guard.active.lock().unwrap();
        if active.get(&self.chat_id).is_some_and(|handles| {
            handles.turn_id == self.turn_id && handles.lease_token == self.lease_token
        }) {
            active.remove(&self.chat_id);
            self.guard.released.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_local_attempt_per_chat_then_released_on_drop() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("first register succeeds");
        assert!(
            guard
                .register(chat, TurnId::new(), Uuid::new_v4())
                .is_none(),
            "a conflicting local attempt is refused"
        );
        assert!(guard
            .register(ChatId::new(), TurnId::new(), Uuid::new_v4())
            .is_some());

        drop(held);
        assert!(
            guard
                .register(chat, TurnId::new(), Uuid::new_v4())
                .is_some(),
            "dropping the permit frees the local registry"
        );
    }

    #[test]
    fn cancel_trips_the_held_token() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        assert!(!guard.cancel(chat, turn), "nothing to cancel yet");
        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("register");
        assert!(!held.cancel_token().is_cancelled());
        assert!(!guard.cancel(chat, TurnId::new()));
        assert!(guard.cancel(chat, turn));
        assert!(held.cancel_token().is_cancelled());
        assert!(guard.cancel(chat, turn));
    }

    #[test]
    fn stale_permit_cannot_remove_a_newer_attempt() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let old_turn = TurnId::new();
        let old_token = Uuid::new_v4();
        let held = guard.register(chat, old_turn, old_token).expect("register");
        let new_turn = TurnId::new();
        let new_token = Uuid::new_v4();
        guard.active.lock().unwrap().insert(
            chat,
            TurnHandles {
                turn_id: new_turn,
                lease_token: new_token,
                cancel: CancelToken::new(),
                steer: SteerInbox::new(),
            },
        );
        drop(held);
        assert!(guard.cancel(chat, new_turn));
    }

    #[test]
    fn steer_signal_wakes_the_held_inbox() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        assert!(!guard.signal_steer(chat, turn, false));
        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("register");
        assert!(!guard.signal_steer(chat, TurnId::new(), true));
        assert!(guard.signal_steer(chat, turn, true));
        assert!(held.steer_inbox().interrupt_requested());
        assert!(held.steer_inbox().drain().is_empty());
    }

    #[test]
    fn sandbox_model_signals_require_the_exact_run_lease() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let lease = Uuid::new_v4();
        let held = guard.register_model(run, lease).expect("register model");

        assert!(!guard.cancel_model(run, Uuid::new_v4()));
        assert!(!held.cancel_token().is_cancelled());
        assert!(!guard.cancel_model(AgentRunId::sandbox_for_spawn_call(CallId::new()), lease));
        assert!(guard.cancel_model(run, lease));
        assert!(held.cancel_token().is_cancelled());
        assert!(
            guard.cancel_model(run, lease),
            "exact retry stays signalled"
        );
    }

    #[test]
    fn sandbox_search_signals_require_the_exact_call_run_and_lease() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let call = CallId::new();
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let lease = Uuid::new_v4();
        let held = guard
            .register_checkpoint(call, run, lease)
            .expect("register search");

        assert!(!guard.cancel_checkpoint(CallId::new(), run, lease));
        assert!(!guard.cancel_checkpoint(
            call,
            AgentRunId::sandbox_for_spawn_call(CallId::new()),
            lease
        ));
        assert!(!guard.cancel_checkpoint(call, run, Uuid::new_v4()));
        assert!(!held.cancel_token().is_cancelled());
        assert!(guard.cancel_checkpoint(call, run, lease));
        assert!(held.cancel_token().is_cancelled());
    }

    #[test]
    fn stale_and_current_sandbox_attempts_are_exactly_isolated() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let old_lease = Uuid::new_v4();
        let stale_model = guard
            .register_model(run, old_lease)
            .expect("register old model");
        let new_lease = Uuid::new_v4();
        let current_model = guard
            .register_model(run, new_lease)
            .expect("a distinct model lease may coexist until its heartbeat fences it");
        assert!(!stale_model.cancel_token().is_cancelled());
        assert!(!current_model.cancel_token().is_cancelled());
        assert!(guard.register_model(run, new_lease).is_none());
        drop(stale_model);
        assert!(!current_model.cancel_token().is_cancelled());
        assert!(guard.cancel_model(run, new_lease));
        assert!(current_model.cancel_token().is_cancelled());

        let call = CallId::new();
        let old_checkpoint_lease = Uuid::new_v4();
        let stale_search = guard
            .register_checkpoint(call, run, old_checkpoint_lease)
            .expect("register old search");
        let new_checkpoint_lease = Uuid::new_v4();
        let current_search = guard
            .register_checkpoint(call, run, new_checkpoint_lease)
            .expect("a distinct search lease may coexist until its heartbeat fences it");
        assert!(!stale_search.cancel_token().is_cancelled());
        assert!(!current_search.cancel_token().is_cancelled());
        assert!(guard
            .register_checkpoint(call, run, new_checkpoint_lease)
            .is_none());
        drop(stale_search);
        assert!(!current_search.cancel_token().is_cancelled());
        assert!(guard.cancel_checkpoint(call, run, new_checkpoint_lease));
        assert!(current_search.cancel_token().is_cancelled());
    }
}

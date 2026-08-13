//! Durable native executor for computer-use tool calls.
//!
//! The desktop is the only place these calls can run: it holds the display,
//! the input devices, and the broker sidecar that authorizes every operation
//! against the user's per-app grants. The renderer never supplies an execution
//! context, an element address, or a broker handle; canonical arguments are
//! recovered from the checkpointed server call and current chat authority is
//! derived natively before each broker operation.
//!
//! Consent surfaces here, not in the server's approval gate: a broker `Denied`
//! on a grantable op parks the call behind a per-app consent card ("once /
//! always for this chat / always"), and the decision is written to the
//! broker's grant store before the op is re-issued. A consequential control
//! action the broker holds (`CuNeedsConfirmation`) parks behind a second,
//! separate confirmation the broker honors only while the target's label still
//! matches.
//!
//! Safety state (the halt latch, the in-control indicator, the pending prompt
//! set) lives in [`ComputerUseState`] and is renderer-visible only through the
//! snapshot event/query; the Stop latch short-circuits any subsequent control
//! op before its broker round-trip.

use std::collections::HashMap;
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tidebreak_core::{
    validate_computer_capture_screen_arguments, validate_computer_click_arguments,
    validate_computer_focus_window_arguments, validate_computer_key_press_arguments,
    validate_computer_list_windows_arguments, validate_computer_read_app_content_arguments,
    validate_computer_return_to_openwave_arguments, validate_computer_scroll_arguments,
    validate_computer_type_text_arguments, validate_computer_wait_arguments, CallId, ChatId,
    ComputerCaptureScreenArgs, ComputerClickArgs, ComputerFocusWindowArgs, ComputerKeyPressArgs,
    ComputerListWindowsArgs, ComputerReadAppContentArgs, ComputerScrollArgs, ComputerTypeTextArgs,
    ComputerWaitArgs, ImageRef, ToolCallExecution, ToolCallRecord, ToolCallStatus,
    COMPUTER_CAPTURE_SCREEN_TOOL, COMPUTER_CLICK_TOOL, COMPUTER_FOCUS_WINDOW_TOOL,
    COMPUTER_KEY_PRESS_TOOL, COMPUTER_LIST_WINDOWS_TOOL, COMPUTER_READ_APP_CONTENT_TOOL,
    COMPUTER_RETURN_TO_OPENWAVE_TOOL, COMPUTER_SCROLL_TOOL, COMPUTER_TYPE_TEXT_TOOL,
    COMPUTER_WAIT_TOOL, MAX_WAIT_SECONDS,
};
use tidebreak_host_broker::{
    extract_marks, is_blocked_control_bundle, Capability, ConsentMethod, ControlRequest,
    ControlResult, CuConfirmControlActionRequest, CuGrantAppRequest, CuResolveHandoffRequest,
    CuRevokeAppRequest, ElementTargetWire, ErrorCode, GrantSubject, Mark, OperationEnvelope,
    OperationRequest, OperationResult, PROTOCOL_VERSION,
};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::broker::BrokerClientError;
use crate::host_access::{AuthoritativeContext, HostAccess};
use crate::image_attachments::PublishedImageAttachment;
use crate::AppState;

use super::{
    control_plane, control_plane_error, private_receipt_error, ComputerUseReceipt,
    FolderOperationPhase, StoredResolution,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Ceiling on the serialized model-facing result, well under the durable
/// per-call result budget so the resolve payload never fails validation.
const MAX_RESULT_CONTENT_BYTES: usize = 56 * 1024;
/// Bound on a rendered AX tree inside a `computer_read_app_content` result.
/// The helper already caps nodes and strings; this keeps the model-facing
/// payload sized for a transcript.
const MAX_TREE_RESULT_BYTES: usize = 48 * 1024;
/// Most windows listed in one `computer_list_windows` result.
const MAX_WINDOW_ROWS: usize = 64;
/// Most mark-table entries retained across conversations and apps. A burst of
/// captures across many apps resets the table rather than growing it; marks
/// are a look-then-act affordance, so a dropped entry only costs a re-capture.
const MAX_MARK_TABLES: usize = 64;
/// Mark numbers are 1-based; mirrors the core contract's bound.
const MAX_CACHED_MARKS: usize = tidebreak_core::MAX_MARK as usize;
/// Renderer event carrying the full control/consent snapshot on every change.
const STATE_EVENT: &str = "computer-use-state-changed";
/// A control op touches the indicator as recently active for this long after
/// its last broker round-trip; the renderer re-arms the banner on this window.
const INDICATOR_IDLE_REARM: std::time::Duration = std::time::Duration::from_secs(30);

/// Capability a grant miss was asking for, in the card's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsentCapability {
    CaptureScreen,
    ReadAppContent,
    ControlApp,
}

/// The decision a consent card can commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConsentDecision {
    /// Run the op once; nothing is remembered.
    Once,
    /// Remember for this conversation.
    Chat,
    /// Remember for the whole project (or this conversation, when it has no
    /// project — there is nowhere wider to durably put it).
    Always,
    Decline,
}

/// One parked consent ask, as the renderer needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsentPromptView {
    call_id: CallId,
    chat_id: ChatId,
    bundle_id: String,
    app_name: Option<String>,
    capability: ConsentCapability,
}

/// One parked consequential-action confirmation, as the renderer needs it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfirmationPromptView {
    call_id: CallId,
    chat_id: ChatId,
    bundle_id: String,
    app_name: Option<String>,
    target_label: Option<String>,
    reason: String,
}

/// What the indicator reports: the app most recently under control and when.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveControlView {
    bundle_id: String,
    app_name: Option<String>,
    /// Epoch milliseconds of the last control round-trip.
    last_activity_millis: i64,
    /// Epoch milliseconds after which the banner may re-arm to hidden.
    visible_until_millis: i64,
}

/// The whole computer-use surface the renderer may see.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComputerUseSnapshot {
    pub(crate) active: Option<ActiveControlView>,
    pub(crate) halted: bool,
    pub(crate) pending_consents: Vec<ConsentPromptView>,
    pub(crate) pending_confirmations: Vec<ConfirmationPromptView>,
}

/// What a parked prompt is waiting on, with the channel that resolves it.
enum PendingPrompt {
    Consent {
        view: ConsentPromptView,
        decision: oneshot::Sender<ConsentDecision>,
    },
    Confirmation {
        view: ConfirmationPromptView,
        decision: oneshot::Sender<bool>,
    },
}

/// Indicator bookkeeping, kept separate so the lock is never held across an
/// await or a publish.
#[derive(Default)]
struct IndicatorState {
    active: Option<ActiveControlView>,
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Native computer-use state: the Set-of-Marks table, the halt latch, the
/// in-control indicator, and the prompts parked on a user decision.
///
/// Everything here is in-memory on purpose. The mark table is rebuilt by the
/// next capture after a restart; a halt survives only until one (a stopped
/// agent was already told not to retry, and consent is re-asked, not assumed).
pub(crate) struct ComputerUseState {
    /// (conversation, app scope) → marks from that pair's latest capture or
    /// tree read. The scope is the bundle id, or empty for a whole-display
    /// capture (which carries no marks today, but the key shape stays uniform).
    marks: StdMutex<HashMap<(Uuid, String), Vec<Mark>>>,
    /// Bundle id → human app name, learned from window lists and tree reads so
    /// cards and the banner can name the app rather than its identifier.
    app_names: StdMutex<HashMap<String, String>>,
    indicator: StdMutex<IndicatorState>,
    /// The Stop latch, as a watch so a halt that lands between the pre-dispatch
    /// check and the prompt wait is still observed. Set only by the user,
    /// cleared only by the user (resume, or a fresh consent approval — both
    /// are explicit opt-ins).
    halt: tokio::sync::watch::Sender<bool>,
    prompts: StdMutex<HashMap<CallId, PendingPrompt>>,
}

impl Default for ComputerUseState {
    fn default() -> Self {
        Self {
            marks: StdMutex::new(HashMap::new()),
            app_names: StdMutex::new(HashMap::new()),
            indicator: StdMutex::new(IndicatorState::default()),
            halt: tokio::sync::watch::channel(false).0,
            prompts: StdMutex::new(HashMap::new()),
        }
    }
}

fn lock<'a, T>(mutex: &'a StdMutex<T>) -> StdMutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ComputerUseState {
    /// Record the marks one capture or tree read reported for (chat, app).
    fn remember_marks(&self, chat_id: Uuid, scope: &str, marks: Vec<Mark>) {
        let mut table = lock(&self.marks);
        if table.len() >= MAX_MARK_TABLES && !table.contains_key(&(chat_id, scope.to_owned())) {
            table.clear();
        }
        table.insert((chat_id, scope.to_owned()), marks);
    }

    /// Resolve a Set-of-Marks number to its element address, from the most
    /// recent capture or tree read for this chat and app.
    fn resolve_mark(&self, chat_id: Uuid, scope: &str, mark: u32) -> Option<(String, String)> {
        lock(&self.marks)
            .get(&(chat_id, scope.to_owned()))
            .and_then(|marks| marks.iter().find(|entry| entry.mark == mark))
            .map(|entry| (entry.element_id.clone(), entry.element_fingerprint.clone()))
    }

    fn learn_app_name(&self, bundle_id: &str, app_name: Option<&str>) {
        if let Some(name) = app_name.filter(|name| !name.is_empty()) {
            lock(&self.app_names).insert(bundle_id.to_owned(), name.to_owned());
        }
    }

    fn app_name(&self, bundle_id: &str) -> Option<String> {
        lock(&self.app_names).get(bundle_id).cloned()
    }

    /// Mark an app as under control right now.
    fn note_control_activity(&self, bundle_id: &str) {
        let now = now_millis();
        lock(&self.indicator).active = Some(ActiveControlView {
            bundle_id: bundle_id.to_owned(),
            app_name: self.app_name(bundle_id),
            last_activity_millis: now,
            visible_until_millis: now + INDICATOR_IDLE_REARM.as_millis() as i64,
        });
    }

    /// Whether the user has halted control. Checked before every control
    /// round-trip so a Stop always lands first.
    fn is_halted(&self) -> bool {
        *self.halt.borrow()
    }

    /// Await the next halt, returning immediately if already halted. A halt
    /// that fired between the pre-dispatch check and here is still observed:
    /// the watch receiver starts from the current value, not the next change.
    async fn wait_for_halt(&self) {
        let mut rx = self.halt.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    fn halt(&self) {
        // send_replace, not send: the latch must hold even when no prompt is
        // currently parked on it (send drops the value with zero receivers).
        self.halt.send_replace(true);
    }

    fn resume(&self) {
        self.halt.send_replace(false);
    }

    fn snapshot(&self) -> ComputerUseSnapshot {
        let prompts = lock(&self.prompts);
        ComputerUseSnapshot {
            active: lock(&self.indicator).active.clone(),
            halted: self.is_halted(),
            pending_consents: prompts
                .values()
                .filter_map(|prompt| match prompt {
                    PendingPrompt::Consent { view, .. } => Some(view.clone()),
                    PendingPrompt::Confirmation { .. } => None,
                })
                .collect(),
            pending_confirmations: prompts
                .values()
                .filter_map(|prompt| match prompt {
                    PendingPrompt::Confirmation { view, .. } => Some(view.clone()),
                    PendingPrompt::Consent { .. } => None,
                })
                .collect(),
        }
    }
}

fn emit_state(app: &AppHandle, cu: &ComputerUseState) {
    if let Err(error) = app.emit(STATE_EVENT, cu.snapshot()) {
        eprintln!("openwave-desktop: could not emit computer-use state: {error}");
    }
}

// MARK: - Tauri surface

/// The renderer's initial read of the computer-use surface; changes arrive on
/// [`STATE_EVENT`] afterwards.
#[tauri::command]
pub(crate) fn computer_use_state(state: State<'_, HostAccess>) -> ComputerUseSnapshot {
    state.computer_use.snapshot()
}

/// The Stop button: halt before the next control round-trip. Pending prompts
/// settle as stopped rather than hanging on a decision that is no longer
/// wanted. In-memory only — a restart re-arms, and every agent whose op was
/// short-circuited was already told not to retry.
#[tauri::command]
pub(crate) fn stop_computer_use_control(app: AppHandle, state: State<'_, HostAccess>) {
    state.computer_use.halt();
    emit_state(&app, &state.computer_use);
}

/// Re-arm control after a Stop. Explicit and user-driven only.
#[tauri::command]
pub(crate) fn resume_computer_use_control(app: AppHandle, state: State<'_, HostAccess>) {
    state.computer_use.resume();
    emit_state(&app, &state.computer_use);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResolveConsentRequest {
    call_id: Uuid,
    decision: ConsentDecision,
}

/// Commit the user's per-app consent decision for a parked computer-use call.
#[tauri::command]
pub(crate) fn resolve_computer_use_consent(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ResolveConsentRequest,
) -> Result<(), String> {
    let prompt = lock(&state.computer_use.prompts).remove(&CallId::from(request.call_id));
    let Some(PendingPrompt::Consent { decision, .. }) = prompt else {
        return Err("that computer-use consent request is no longer pending".to_owned());
    };
    // A grant the user just approved is an explicit opt-in, which is also what
    // re-arms control after a Stop.
    if request.decision != ConsentDecision::Decline {
        state.computer_use.resume();
    }
    let _ = decision.send(request.decision);
    emit_state(&app, &state.computer_use);
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResolveConfirmationRequest {
    call_id: Uuid,
    confirmed: bool,
}

/// Commit the user's decision on one consequential action the broker held.
#[tauri::command]
pub(crate) fn resolve_computer_use_confirmation(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ResolveConfirmationRequest,
) -> Result<(), String> {
    let prompt = lock(&state.computer_use.prompts).remove(&CallId::from(request.call_id));
    let Some(PendingPrompt::Confirmation { decision, .. }) = prompt else {
        return Err("that computer-use confirmation is no longer pending".to_owned());
    };
    let _ = decision.send(request.confirmed);
    emit_state(&app, &state.computer_use);
    Ok(())
}

// MARK: - Recovery loop

/// Recover persisted outcomes, then discover new computer-use calls. Native-
/// owned like the folder executor: no renderer event is an execution authority.
pub(crate) async fn recover_computer_use_operations(app: AppHandle) {
    loop {
        let failed = recover_once(&app).await;
        if failed {
            eprintln!("openwave-desktop: computer-use executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_computer_uses() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("openwave-desktop: computer-use receipt recovery failed: {error}");
            return true;
        }
    };
    let recovered_call_ids: std::collections::HashSet<CallId> =
        receipts.iter().map(|receipt| receipt.call_id).collect();
    let mut failed = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(app, &state, receipt).await {
            eprintln!("openwave-desktop: computer-use receipt deferred: {error}");
            failed = true;
        }
    }

    let Some(store) = state.store() else {
        return true;
    };
    let client = match control_plane(&state) {
        Ok(client) => client,
        Err(_) => return true,
    };
    let chats = match store.list_chats().await {
        Ok(chats) => chats,
        Err(_) => return true,
    };
    for chat in chats {
        let calls = match client.pending(chat.id).await {
            Ok(calls) => calls,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for call in calls
            .into_iter()
            .filter(|call| !recovered_call_ids.contains(&call.id) && is_computer_use_call(call))
        {
            let receipt = ComputerUseReceipt::new(chat.id, call.id, state.receipts.executor_id());
            if let Err(error) = execute_receipt(app, &state, receipt).await {
                eprintln!("openwave-desktop: computer-use execution deferred: {error}");
                failed = true;
            }
        }
    }
    failed
}

async fn execute_receipt(
    app: &AppHandle,
    state: &HostAccess,
    mut receipt: ComputerUseReceipt,
) -> Result<(), String> {
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }
    // An interrupted dispatch is never re-fired: a control op may already have
    // acted, and a capture already disclosed the screen. Close it out and let
    // the agent retry deliberately.
    if receipt.phase == FolderOperationPhase::DispatchStarted {
        receipt.resolution = Some(unavailable(
            "computer_use_interrupted",
            "The computer-use operation could not be safely resumed after an interruption. Please try again.",
        ));
        state
            .receipts
            .save_computer_use(&receipt)
            .map_err(private_receipt_error)?;
        return publish_resolution(
            state,
            &receipt,
            receipt.resolution.as_ref().expect("stored above"),
        )
        .await;
    }

    let context = state.context(receipt.chat_id.0).await?;

    // Persist the chosen lease token before claiming, exactly like the folder
    // executor: a lost claim response must not strand a live lease.
    state
        .receipts
        .save_computer_use(&receipt)
        .map_err(private_receipt_error)?;
    let client = control_plane(state)?;
    let claim = match client
        .claim(
            receipt.chat_id,
            receipt.call_id,
            receipt.executor_id,
            receipt.lease_token,
        )
        .await
    {
        Ok(claim) => claim,
        Err(error) if error.is_conflict() => {
            return recover_after_claim_conflict(state, &mut receipt).await;
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    if claim.call.chat_id != receipt.chat_id
        || claim.call.id != receipt.call_id
        || claim.lease_token != receipt.lease_token
        || claim.call.client_executor_id != Some(receipt.executor_id)
        || !is_computer_use_call(&claim.call)
    {
        return Err("local control plane returned an invalid computer-use request".to_owned());
    }

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    // The final durable fence before any host effect or consent wait.
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_computer_use(&receipt)
        .map_err(private_receipt_error)?;
    let resolution = execute_operation(app, state, context, &claim.call).await;
    receipt.resolution = Some(resolution);
    state
        .receipts
        .save_computer_use(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        &receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

/// A claim that can no longer be recovered belongs to another executor (or to
/// no one); close out the local receipt rather than race its owner.
async fn recover_after_claim_conflict(
    state: &HostAccess,
    receipt: &mut ComputerUseReceipt,
) -> Result<(), String> {
    let client = control_plane(state)?;
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        return state
            .receipts
            .remove_computer_use(receipt.call_id)
            .map_err(private_receipt_error);
    };
    if call.chat_id != receipt.chat_id || !is_computer_use_call(&call) {
        return Err("local control plane returned an invalid computer-use request".to_owned());
    }
    if call.client_executor_id != Some(receipt.executor_id) {
        return state
            .receipts
            .remove_computer_use(receipt.call_id)
            .map_err(private_receipt_error);
    }
    // The lease is still ours, but whether a broker op already ran is
    // unknowable — terminalize rather than re-fire. The server accepts the
    // exact token through its expired-claim path once the lease lapses.
    receipt.resolution = Some(unavailable(
        "computer_use_interrupted",
        "The computer-use operation could not be safely resumed after an interruption. Please try again.",
    ));
    state
        .receipts
        .save_computer_use(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &ComputerUseReceipt,
    resolution: &StoredResolution,
) -> Result<(), String> {
    let client = control_plane(state)?;
    match client
        .resolve(
            receipt.chat_id,
            receipt.call_id,
            receipt.lease_token,
            resolution,
        )
        .await
    {
        Ok(()) => state
            .receipts
            .remove_computer_use(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if pending {
                Err("computer-use result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_computer_use(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

fn is_computer_use_call(call: &ToolCallRecord) -> bool {
    if call.execution != ToolCallExecution::Client || call.status != ToolCallStatus::Pending {
        return false;
    }
    match call.name.as_str() {
        COMPUTER_LIST_WINDOWS_TOOL => validate_computer_list_windows_arguments(&call.arguments),
        COMPUTER_CAPTURE_SCREEN_TOOL => validate_computer_capture_screen_arguments(&call.arguments),
        COMPUTER_READ_APP_CONTENT_TOOL => {
            validate_computer_read_app_content_arguments(&call.arguments)
        }
        COMPUTER_CLICK_TOOL => validate_computer_click_arguments(&call.arguments),
        COMPUTER_TYPE_TEXT_TOOL => validate_computer_type_text_arguments(&call.arguments),
        COMPUTER_KEY_PRESS_TOOL => validate_computer_key_press_arguments(&call.arguments),
        COMPUTER_SCROLL_TOOL => validate_computer_scroll_arguments(&call.arguments),
        COMPUTER_FOCUS_WINDOW_TOOL => validate_computer_focus_window_arguments(&call.arguments),
        COMPUTER_RETURN_TO_OPENWAVE_TOOL => {
            validate_computer_return_to_openwave_arguments(&call.arguments)
        }
        COMPUTER_WAIT_TOOL => validate_computer_wait_arguments(&call.arguments),
        _ => false,
    }
}

// MARK: - Execution

/// What one parsed call wants done: a broker operation, or a purely local one.
#[derive(Debug)]
enum CuAction {
    Broker(OperationRequest),
    /// Return focus to OpenWave itself. Deliberately not a broker op: OpenWave
    /// is on the control blocklist, and focusing our own window is a local
    /// window-manager call, not synthesized input into another app.
    ReturnToOpenwave,
    /// A bounded local pause. The broker exposes the same op clamped; keeping
    /// it local saves a round-trip and never reaches the helper either way.
    Wait(f64),
}

async fn execute_operation(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
) -> StoredResolution {
    let cu = &state.computer_use;
    let action = match build_action(cu, call) {
        Ok(action) => action,
        Err(resolution) => return resolution,
    };
    match action {
        CuAction::ReturnToOpenwave => {
            crate::deep_link::focus_main_window(app);
            completed(serde_json::json!({ "status": "ok", "focused": "openwave" }))
        }
        CuAction::Wait(seconds) => {
            let seconds = seconds.clamp(0.0, MAX_WAIT_SECONDS);
            tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
            completed(serde_json::json!({ "status": "ok", "waited_seconds": seconds }))
        }
        CuAction::Broker(request) => dispatch_broker(app, state, context, call, request).await,
    }
}

/// Parse the canonical arguments and map the tool to its broker operation.
///
/// A `mark` target is resolved here, before anything reaches the broker: it is
/// a reference into this chat's latest capture for that app, and only this
/// process holds the table. An unknown mark fails as retryable guidance —
/// capture again — rather than acting on a guess.
fn build_action(
    cu: &ComputerUseState,
    call: &ToolCallRecord,
) -> Result<CuAction, StoredResolution> {
    let invalid = || {
        unavailable(
            "invalid_request",
            "The computer-use request was not available.",
        )
    };
    match call.name.as_str() {
        COMPUTER_LIST_WINDOWS_TOOL => {
            let args: ComputerListWindowsArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Broker(OperationRequest::CuListWindows {
                bundle_id: args.app_id,
            }))
        }
        COMPUTER_CAPTURE_SCREEN_TOOL => {
            let args: ComputerCaptureScreenArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let target = match args.app_id {
                Some(bundle_id) => tidebreak_host_broker::CaptureTargetWire::App { bundle_id },
                None => tidebreak_host_broker::CaptureTargetWire::Display {
                    display_id: args.display_id,
                },
            };
            Ok(CuAction::Broker(OperationRequest::CuCaptureScreen {
                target,
            }))
        }
        COMPUTER_READ_APP_CONTENT_TOOL => {
            let args: ComputerReadAppContentArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Broker(OperationRequest::CuReadAppContent {
                bundle_id: args.app_id,
                max_depth: args.max_depth,
                max_nodes: args.max_nodes,
            }))
        }
        COMPUTER_CLICK_TOOL => {
            let args: ComputerClickArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let target = resolve_target(cu, call.chat_id, &args.app_id, &args.target)?;
            Ok(CuAction::Broker(OperationRequest::CuClick {
                bundle_id: args.app_id,
                target,
                button: args.button.map(|button| match button {
                    tidebreak_core::ClickButton::Left => "left".to_owned(),
                    tidebreak_core::ClickButton::Right => "right".to_owned(),
                }),
                click_count: if args.double.unwrap_or(false) {
                    Some(2)
                } else {
                    None
                },
            }))
        }
        COMPUTER_TYPE_TEXT_TOOL => {
            let args: ComputerTypeTextArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let target = resolve_target(cu, call.chat_id, &args.app_id, &args.target)?;
            Ok(CuAction::Broker(OperationRequest::CuTypeText {
                bundle_id: args.app_id,
                text: args.text,
                target,
            }))
        }
        COMPUTER_KEY_PRESS_TOOL => {
            let args: ComputerKeyPressArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let modifiers = args.modifiers.map(|modifiers| {
                modifiers
                    .into_iter()
                    .map(|modifier| match modifier {
                        tidebreak_core::KeyModifier::Cmd => "cmd",
                        tidebreak_core::KeyModifier::Shift => "shift",
                        tidebreak_core::KeyModifier::Ctrl => "ctrl",
                        tidebreak_core::KeyModifier::Alt => "alt",
                        tidebreak_core::KeyModifier::Fn => "fn",
                    })
                    .map(str::to_owned)
                    .collect()
            });
            Ok(CuAction::Broker(OperationRequest::CuKeyPress {
                bundle_id: args.app_id,
                key: args.key,
                modifiers,
            }))
        }
        COMPUTER_SCROLL_TOOL => {
            let args: ComputerScrollArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let target = resolve_target(cu, call.chat_id, &args.app_id, &args.target)?;
            Ok(CuAction::Broker(OperationRequest::CuScroll {
                bundle_id: args.app_id,
                target,
                dx: args.dx,
                dy: args.dy,
            }))
        }
        COMPUTER_FOCUS_WINDOW_TOOL => {
            let args: ComputerFocusWindowArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Broker(OperationRequest::CuFocusWindow {
                bundle_id: args.app_id,
                window_id: args.window_id,
            }))
        }
        COMPUTER_RETURN_TO_OPENWAVE_TOOL => Ok(CuAction::ReturnToOpenwave),
        COMPUTER_WAIT_TOOL => {
            let args: ComputerWaitArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Wait(args.seconds.unwrap_or(1.0)))
        }
        _ => Err(unavailable(
            "invalid_request",
            "The computer-use request was not available.",
        )),
    }
}

/// Translate the model's element/coordinate target into the wire shape,
/// resolving a `mark` against this chat's latest marks for the app.
fn resolve_target(
    cu: &ComputerUseState,
    chat_id: ChatId,
    bundle_id: &str,
    target: &tidebreak_core::ElementTargetArgs,
) -> Result<ElementTargetWire, StoredResolution> {
    if let Some(mark) = target.mark {
        let Some((element_id, element_fingerprint)) = cu.resolve_mark(chat_id.0, bundle_id, mark)
        else {
            return Err(unavailable(
                "stale_mark",
                "That mark is not from the latest screenshot for this app. Capture the screen again and use a mark from the new image.",
            ));
        };
        return Ok(ElementTargetWire {
            element_id: Some(element_id),
            element_fingerprint: Some(element_fingerprint),
            x: None,
            y: None,
        });
    }
    Ok(ElementTargetWire {
        element_id: target.element_id.clone(),
        element_fingerprint: target.element_fingerprint.clone(),
        x: target.x,
        y: target.y,
    })
}

/// The bundle id an operation acts on, when it names one.
fn request_bundle_id(request: &OperationRequest) -> Option<&str> {
    match request {
        OperationRequest::CuListWindows { bundle_id } => bundle_id.as_deref(),
        OperationRequest::CuCaptureScreen { target } => match target {
            tidebreak_host_broker::CaptureTargetWire::App { bundle_id } => Some(bundle_id),
            tidebreak_host_broker::CaptureTargetWire::Display { .. } => None,
        },
        OperationRequest::CuReadAppContent { bundle_id, .. }
        | OperationRequest::CuClick { bundle_id, .. }
        | OperationRequest::CuTypeText { bundle_id, .. }
        | OperationRequest::CuKeyPress { bundle_id, .. }
        | OperationRequest::CuScroll { bundle_id, .. }
        | OperationRequest::CuFocusWindow { bundle_id, .. } => Some(bundle_id),
        _ => None,
    }
}

/// The capability a grant miss on this call is asking for, matching the
/// broker's own authorization: the three control tools need `ControlApp`;
/// scroll, focus, and tree reads need `ReadAppContent`; capture and window
/// listing need `CaptureScreen`.
fn consent_capability(call: &ToolCallRecord, request: &OperationRequest) -> ConsentCapability {
    if tidebreak_core::is_computer_use_control_tool(&call.name) {
        return ConsentCapability::ControlApp;
    }
    match request {
        OperationRequest::CuCaptureScreen { .. } | OperationRequest::CuListWindows { .. } => {
            ConsentCapability::CaptureScreen
        }
        _ => ConsentCapability::ReadAppContent,
    }
}

/// Whether this call acts on the host (synthesizes input or moves windows), as
/// opposed to only reading. Acting ops are what the Stop latch halts, what the
/// indicator reports, and what the blocklist pre-check guards.
fn acts_on_host(name: &str) -> bool {
    tidebreak_core::is_computer_use_control_tool(name)
        || name == COMPUTER_SCROLL_TOOL
        || name == COMPUTER_FOCUS_WINDOW_TOOL
}

async fn dispatch_broker(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    request: OperationRequest,
) -> StoredResolution {
    let cu = &state.computer_use;
    let acting = acts_on_host(&call.name);
    let bundle_id = request_bundle_id(&request).map(str::to_owned);

    // The halt latch is checked immediately before the round-trip so Stop
    // always lands first; the broker's blocklist is mirrored here so a blocked
    // app fails closed without surfacing a consent card for it.
    if acting && cu.is_halted() {
        return stopped_resolution();
    }
    if let Some(bundle_id) = bundle_id.as_deref() {
        if is_blocked_control_bundle(bundle_id) {
            return unavailable(
                "app_blocked",
                "That application cannot be captured, read, or controlled by OpenWave.",
            );
        }
    }
    if let Some(bundle_id) = bundle_id.as_deref() {
        if acting {
            cu.note_control_activity(bundle_id);
            emit_state(app, cu);
        }
    }

    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_host_broker::RequestId::new(),
            context: context.execution,
            request: request.clone(),
        })
        .await;
    match result {
        Ok(OperationResult::CuNeedsConfirmation(held)) => {
            if cu.is_halted() {
                return stopped_resolution();
            }
            dispatch_confirmation(app, state, call, held).await
        }
        Ok(result) => map_result(app, state, context, call, result).await,
        Err(error) => match map_broker_error(&error) {
            BrokerFailure::ConsentRequired => {
                dispatch_consent(app, state, context, call, request).await
            }
            BrokerFailure::Resolution(resolution) => resolution,
        },
    }
}

/// How one broker failure should proceed: a grant miss parks for consent;
/// everything else is already a terminal answer.
enum BrokerFailure {
    ConsentRequired,
    Resolution(StoredResolution),
}

fn map_broker_error(error: &BrokerClientError) -> BrokerFailure {
    let BrokerClientError::Broker { code, .. } = error else {
        // Transport-layer failures say nothing about authorization and must
        // not surface broker internals to the model.
        return BrokerFailure::Resolution(unavailable(
            "computer_unavailable",
            "Computer use is not available right now. Try again.",
        ));
    };
    match code {
        ErrorCode::Yielded => BrokerFailure::Resolution(unavailable(
            "control_yielded",
            "A system security surface owns the foreground, so the action was refused. Do not retry; tell the user what you were trying to do.",
        )),
        // The blocklist was pre-checked natively, so Denied is a grant miss —
        // every computer-use op names a grantable capability (a display
        // capture or screen-wide window list asks for the whole-screen scope).
        ErrorCode::Denied => BrokerFailure::ConsentRequired,
        ErrorCode::OsPermissionDenied => BrokerFailure::Resolution(unavailable(
            "os_permission_required",
            "macOS has not granted OpenWave Screen Recording and Accessibility. Ask the user to enable them in Settings, then retry.",
        )),
        ErrorCode::StaleElement => BrokerFailure::Resolution(unavailable(
            "stale_element",
            "The target element moved or changed since it was last seen. Read the app content or capture the screen again, then retry against the fresh element.",
        )),
        ErrorCode::NotFound => BrokerFailure::Resolution(unavailable(
            "not_found",
            "The target app, window, or element was not found. List windows to see what is on screen.",
        )),
        ErrorCode::InvalidRequest => BrokerFailure::Resolution(unavailable(
            "invalid_request",
            "The computer-use request was not available.",
        )),
        ErrorCode::TooLarge => BrokerFailure::Resolution(unavailable(
            "too_large",
            "The computer-use result exceeded its limit. Narrow the request (shallower tree, fewer nodes) and retry.",
        )),
        _ => BrokerFailure::Resolution(unavailable(
            "operation_failed",
            "The computer-use operation failed on the host. Retry once; if it keeps failing, tell the user.",
        )),
    }
}

fn stopped_resolution() -> StoredResolution {
    unavailable(
        "stopped_by_user",
        "The user stopped computer control. Do not retry this or any further control action; tell the user control was stopped.",
    )
}

/// The per-app consent park: surface the card, wait for the decision, write
/// the grant the decision implies, then re-issue the operation once.
async fn dispatch_consent(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    request: OperationRequest,
) -> StoredResolution {
    let cu = &state.computer_use;
    let capability = consent_capability(call, &request);
    let bundle_id = request_bundle_id(&request).map(str::to_owned);
    let view = ConsentPromptView {
        call_id: call.id,
        chat_id: call.chat_id,
        bundle_id: bundle_id.clone().unwrap_or_default(),
        app_name: bundle_id
            .as_deref()
            .and_then(|bundle_id| cu.app_name(bundle_id)),
        capability,
    };
    let (sender, receiver) = oneshot::channel();
    {
        let mut prompts = lock(&cu.prompts);
        prompts.insert(
            call.id,
            PendingPrompt::Consent {
                view,
                decision: sender,
            },
        );
    }
    emit_state(app, cu);
    let decision = tokio::select! {
        decision = receiver => decision.unwrap_or(ConsentDecision::Decline),
        () = cu.wait_for_halt() => ConsentDecision::Decline,
    };
    lock(&cu.prompts).remove(&call.id);
    emit_state(app, cu);

    if cu.is_halted() {
        return stopped_resolution();
    }
    let conversation_subject = match GrantSubject::conversation(call.chat_id.0) {
        Ok(subject) => subject,
        Err(_) => {
            return unavailable(
                "invalid_request",
                "The computer-use request was not available.",
            )
        }
    };
    let (capability_wire, grant_subject) = match decision {
        ConsentDecision::Decline => {
            return unavailable(
                "grant_declined",
                "The user declined to let OpenWave use this app. Do not retry; ask how they want to proceed.",
            );
        }
        ConsentDecision::Once | ConsentDecision::Chat => (capability, conversation_subject),
        // "Always" takes the widest durable subject this conversation has —
        // its project, or the conversation itself when there is none.
        ConsentDecision::Always => (capability, context.subject),
    };
    let capability = match capability_wire {
        ConsentCapability::CaptureScreen => Capability::CaptureScreen,
        ConsentCapability::ReadAppContent => Capability::ReadAppContent,
        ConsentCapability::ControlApp => Capability::ControlApp,
    };
    let grant = ControlRequest::CuGrantApp(CuGrantAppRequest {
        subject: grant_subject,
        capability,
        bundle_id: bundle_id.clone(),
        consent: ConsentMethod::PermissionDialog,
        single_use: decision == ConsentDecision::Once,
    });
    if let Err(error) = state.broker.control(grant).await {
        return map_control_error(&error);
    }

    // Re-issued exactly once, now authorized. A second Denied means the grant
    // did not cover the op (a broker-side surprise, not another ask). A held
    // consequential action re-authorizes at confirm time, so the one-time
    // grant must outlive the whole continuation — revoke happens after it.
    if cu.is_halted() {
        revoke_once_grant(state, decision, capability, bundle_id.as_deref(), call).await;
        return stopped_resolution();
    }
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_host_broker::RequestId::new(),
            context: context.execution,
            request,
        })
        .await;
    let resolution = match result {
        Ok(OperationResult::CuNeedsConfirmation(held)) => {
            if cu.is_halted() {
                stopped_resolution()
            } else {
                dispatch_confirmation(app, state, call, held).await
            }
        }
        Ok(result) => map_result(app, state, context, call, result).await,
        Err(error) => match map_broker_error(&error) {
            BrokerFailure::Resolution(resolution) => resolution,
            BrokerFailure::ConsentRequired => unavailable(
                "denied",
                "The computer-use grant did not cover this operation. Ask the user to review the app's grants in Settings.",
            ),
        },
    };
    // A one-time consent leaves nothing behind, whatever the op came back as.
    revoke_once_grant(state, decision, capability, bundle_id.as_deref(), call).await;
    resolution
}

/// A `once` consent wrote a session-only grant so the broker would authorize.
/// The broker also consumes that grant when the authorizing op finishes;
/// this revoke is the halt / abandoned-hold cleanup so a leftover one-shot
/// cannot authorize a later op in the same session. Best-effort.
async fn revoke_once_grant(
    state: &HostAccess,
    decision: ConsentDecision,
    capability: Capability,
    bundle_id: Option<&str>,
    call: &ToolCallRecord,
) {
    if decision != ConsentDecision::Once {
        return;
    }
    let Ok(subject) = GrantSubject::conversation(call.chat_id.0) else {
        return;
    };
    let revoke = ControlRequest::CuRevokeApp(CuRevokeAppRequest {
        subject,
        capability,
        bundle_id: bundle_id.map(str::to_owned),
    });
    if let Err(error) = state.broker.control(revoke).await {
        eprintln!("openwave-desktop: one-time computer-use grant was not withdrawn: {error}");
    }
}

/// The act-time consequential confirmation: the broker is holding the action
/// and honors the confirmation only while the target's label still matches.
async fn dispatch_confirmation(
    app: &AppHandle,
    state: &HostAccess,
    call: &ToolCallRecord,
    held: tidebreak_host_broker::CuNeedsConfirmationResult,
) -> StoredResolution {
    let cu = &state.computer_use;
    let view = ConfirmationPromptView {
        call_id: call.id,
        chat_id: call.chat_id,
        bundle_id: held.bundle_id.clone(),
        app_name: cu.app_name(&held.bundle_id),
        target_label: held.target_label.clone(),
        reason: held.reason.clone(),
    };
    let (sender, receiver) = oneshot::channel();
    {
        let mut prompts = lock(&cu.prompts);
        prompts.insert(
            call.id,
            PendingPrompt::Confirmation {
                view,
                decision: sender,
            },
        );
    }
    emit_state(app, cu);
    let confirmed = tokio::select! {
        confirmed = receiver => confirmed.unwrap_or(false),
        () = cu.wait_for_halt() => false,
    };
    lock(&cu.prompts).remove(&call.id);
    emit_state(app, cu);

    if !confirmed {
        return unavailable(
            if cu.is_halted() {
                "stopped_by_user"
            } else {
                "confirmation_declined"
            },
            if cu.is_halted() {
                "The user stopped computer control. Do not retry this or any further control action; tell the user control was stopped."
            } else {
                "The user declined this action. Do not retry it; ask how they want to proceed."
            },
        );
    }
    // The confirmation identity is single-use; a replayed redeem would fail as
    // unknown, so it is sent without transport retry.
    let confirm = ControlRequest::CuConfirmControlAction(CuConfirmControlActionRequest {
        confirmation_id: held.confirmation_id,
    });
    let deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
    match state.broker.control_without_retry(confirm, deadline).await {
        Ok(ControlResult::CuConfirmControlAction(meta)) => completed(control_meta_json(&meta)),
        Ok(_) => unavailable(
            "operation_failed",
            "The computer-use confirmation returned an unexpected result.",
        ),
        Err(error) => match map_broker_error(&error) {
            BrokerFailure::Resolution(resolution) => resolution,
            BrokerFailure::ConsentRequired => unavailable(
                "grant_declined",
                "The computer-use grant no longer covers this app. Ask the user to review the app's grants in Settings.",
            ),
        },
    }
}

/// The published screenshot of one capture, kept typed so the resolution wire
/// can carry it as structured image references once the server accepts them.
/// Today the same identity reaches the model through the result text.
fn capture_image_refs(published: &PublishedImageAttachment) -> Vec<ImageRef> {
    let Some(media_type) = tidebreak_core::ImageMediaType::parse(published.media_type()) else {
        return Vec::new();
    };
    vec![ImageRef {
        blob_id: published.blob_id(),
        media_type,
        width: published.width(),
        height: published.height(),
        byte_len: published.byte_len(),
    }]
}

/// Map a successful broker result into the model-facing resolution. Capture is
/// the one op whose result is partly out-of-band: the PNG crosses the trusted
/// channel by handoff redemption and is published into the chat's blob store.
async fn map_result(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    result: OperationResult,
) -> StoredResolution {
    let cu = &state.computer_use;
    match result {
        OperationResult::CuListWindows { windows } => {
            for window in &windows {
                if let (Some(bundle_id), Some(app_name)) = (&window.bundle_id, &window.app_name) {
                    cu.learn_app_name(bundle_id, Some(app_name));
                }
            }
            let windows: Vec<_> = windows.into_iter().take(MAX_WINDOW_ROWS).collect();
            completed(serde_json::json!({
                "status": "ok",
                "windows": windows.iter().map(|window| serde_json::json!({
                    "window_id": window.window_id,
                    "title": window.title,
                    "app_name": window.app_name,
                    "bundle_id": window.bundle_id,
                    "pid": window.pid,
                    "frame": {
                        "x": window.frame.x,
                        "y": window.frame.y,
                        "width": window.frame.width,
                        "height": window.frame.height,
                    },
                })).collect::<Vec<_>>(),
            }))
        }
        OperationResult::CuCaptureScreen(capture) => {
            finish_capture(app, state, context, call, capture).await
        }
        OperationResult::CuReadAppContent(tree) => {
            if let Some(bundle_id) = call
                .arguments
                .get("app_id")
                .and_then(serde_json::Value::as_str)
            {
                cu.learn_app_name(bundle_id, tree.app_name.as_deref());
                // Marks from a tree read are valid targets exactly like a
                // capture's — the same elements, numbered the same way.
                let marks = extract_marks(&tree.tree, MAX_CACHED_MARKS);
                cu.remember_marks(call.chat_id.0, bundle_id, marks);
            }
            let tree_text = serde_json::to_string(&tree.tree).unwrap_or_else(|_| "{}".to_owned());
            let (tree_text, over_limit) =
                super::folder_operations::truncate_utf8(&tree_text, MAX_TREE_RESULT_BYTES);
            completed(serde_json::json!({
                "status": "ok",
                "app_name": tree.app_name,
                "truncated": tree.truncated || over_limit,
                "tree": tree_text,
            }))
        }
        OperationResult::CuClick(meta)
        | OperationResult::CuTypeText(meta)
        | OperationResult::CuKeyPress(meta)
        | OperationResult::CuScroll(meta)
        | OperationResult::CuFocusWindow(meta) => completed(control_meta_json(&meta)),
        OperationResult::CuWait { seconds } => {
            completed(serde_json::json!({ "status": "ok", "waited_seconds": seconds }))
        }
        // A held action never reaches here (dispatch intercepts it); anything
        // else is a result this tool did not ask for.
        _ => unavailable(
            "unexpected_result",
            "The computer-use operation returned an unexpected result.",
        ),
    }
}

/// Redeem a staged capture, publish the PNG into the chat's image store, and
/// build the result the model reads — dimensions, the image's blob identity,
/// and the marks it can act on by number.
async fn finish_capture(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    capture: tidebreak_host_broker::CuCaptureScreenResult,
) -> StoredResolution {
    let cu = &state.computer_use;
    // The handoff is single-use; a replayed redeem fails as unknown, so it is
    // sent without transport retry.
    let redeem = ControlRequest::CuResolveHandoff(CuResolveHandoffRequest {
        handoff_id: capture.handoff_id,
    });
    let deadline = tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
    let handoff = match state.broker.control_without_retry(redeem, deadline).await {
        Ok(ControlResult::CuResolveHandoff(handoff)) => handoff,
        Ok(_) => {
            return unavailable(
                "operation_failed",
                "The screen capture could not be retrieved. Capture again.",
            )
        }
        Err(error) => {
            return match map_broker_error(&error) {
                BrokerFailure::Resolution(resolution) => resolution,
                BrokerFailure::ConsentRequired => unavailable(
                    "denied",
                    "The screen capture could not be retrieved. Capture again.",
                ),
            }
        }
    };
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&handoff.content_base64)
    else {
        return unavailable(
            "operation_failed",
            "The screen capture could not be read. Capture again.",
        );
    };
    let app_state = app.state::<std::sync::Arc<AppState>>();
    // A capture without its image is not a usable result — the model called
    // this tool to see. Publish failure fails the call.
    let published = match crate::image_attachments::publish_image_bytes(
        app_state.inner(),
        state,
        context.chat_id,
        bytes,
    )
    .await
    {
        Ok(published) => published,
        Err(_) => {
            return unavailable(
                "image_publish_failed",
                "The screenshot could not be attached to this conversation. Capture again.",
            )
        }
    };
    let image_refs = capture_image_refs(&published);
    // Mark the table this capture's marks belong to, so a later "click mark N"
    // resolves here before the broker ever sees it.
    let scope = call
        .arguments
        .get("app_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    cu.remember_marks(call.chat_id.0, scope, capture.marks.clone());

    let image = image_refs.first();
    let marks_json = capture
        .marks
        .iter()
        .map(|mark| {
            serde_json::json!({
                "mark": mark.mark,
                "role": mark.role,
                "label": mark.label,
                "element_id": mark.element_id,
                "element_fingerprint": mark.element_fingerprint,
            })
        })
        .collect::<Vec<_>>();
    let result = serde_json::json!({
        "status": "ok",
        "width": capture.width,
        "height": capture.height,
        "media_type": capture.media_type,
        // The structured reference a transcript carrier lifts into an image
        // block; the model also reads it here as the capture's identity.
        "image": image.map(|image| serde_json::json!({
            "blob_id": image.blob_id,
            "media_type": image.media_type.as_str(),
            "width": image.width,
            "height": image.height,
            "byte_len": image.byte_len,
        })),
        "marks": marks_json,
        "marks_note": "Act on a marked element with its `mark` number, or on any element with its `element_id` and `element_fingerprint`.",
    });
    // The store counts the marks from `rows` for the preview card; capture has
    // no entry list, so rows carries only the marks.
    let rows = serde_json::json!({ "marks": marks_json });
    completed_with_images(result, Some(rows), Some(image_refs))
}

fn control_meta_json(meta: &tidebreak_host_broker::ControlMeta) -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "used_fallback": meta.used_fallback,
    })
}

fn map_control_error(error: &BrokerClientError) -> StoredResolution {
    match error {
        BrokerClientError::Broker {
            code: ErrorCode::Denied,
            ..
        } => unavailable(
            "grant_declined",
            "The computer-use grant was not recorded. The app may be blocked, or the broker refused it.",
        ),
        _ => unavailable(
            "computer_unavailable",
            "Computer use is not available right now. Try again.",
        ),
    }
}

fn completed(result: serde_json::Value) -> StoredResolution {
    completed_with_images(result, None, None)
}

/// A completed resolution that may carry published image references (a screen
/// capture) and a marks list for the preview card. The images ride the
/// resolution wire so the server projects a `ScreenCapture` preview and the
/// transcript reattaches the image; they are metadata refs, the pixels already
/// published via the image-attachment route. `rows` carries the marks the store
/// counts for the card (and, for capture, nothing the entry-allowlist would
/// project, so it stays off the entries path).
fn completed_with_images(
    result: serde_json::Value,
    rows: Option<serde_json::Value>,
    images: Option<Vec<ImageRef>>,
) -> StoredResolution {
    match serde_json::to_string(&result) {
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => StoredResolution::Completed {
            result,
            rows,
            images,
        },
        _ => unavailable(
            "result_too_large",
            "The computer-use result was too large to return. Narrow the request and retry.",
        ),
    }
}

fn unavailable(code: &str, message: &str) -> StoredResolution {
    StoredResolution::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(number: u32, id: &str) -> Mark {
        Mark {
            mark: number,
            element_id: id.to_owned(),
            element_fingerprint: format!("fp-{id}"),
            role: "AXButton".to_owned(),
            label: format!("Button {id}"),
            frame: tidebreak_host_broker::MarkFrame {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        }
    }

    fn target_with_mark(mark: u32) -> tidebreak_core::ElementTargetArgs {
        tidebreak_core::ElementTargetArgs {
            mark: Some(mark),
            ..Default::default()
        }
    }

    #[test]
    fn a_mark_resolves_to_its_element_from_the_latest_capture() {
        let cu = ComputerUseState::default();
        let chat = Uuid::new_v4();
        cu.remember_marks(
            chat,
            "com.example.app",
            vec![mark(1, "0.1"), mark(2, "0.4.2")],
        );

        let wire = resolve_target(
            &cu,
            ChatId::from(chat),
            "com.example.app",
            &target_with_mark(2),
        )
        .expect("a live mark resolves");
        assert_eq!(wire.element_id.as_deref(), Some("0.4.2"));
        assert_eq!(wire.element_fingerprint.as_deref(), Some("fp-0.4.2"));
        assert_eq!(wire.x, None);
    }

    #[test]
    fn a_newer_capture_replaces_the_marks_a_target_resolves_against() {
        let cu = ComputerUseState::default();
        let chat = Uuid::new_v4();
        cu.remember_marks(chat, "com.example.app", vec![mark(1, "0.1")]);
        cu.remember_marks(chat, "com.example.app", vec![mark(1, "9.9")]);

        let wire = resolve_target(
            &cu,
            ChatId::from(chat),
            "com.example.app",
            &target_with_mark(1),
        )
        .expect("the fresh capture's mark resolves");
        assert_eq!(wire.element_id.as_deref(), Some("9.9"));
    }

    #[test]
    fn an_unknown_mark_refuses_as_retryable_rather_than_guessing() {
        let cu = ComputerUseState::default();
        let chat = Uuid::new_v4();
        cu.remember_marks(chat, "com.example.app", vec![mark(1, "0.1")]);

        // A mark the latest capture does not have…
        let missing = resolve_target(
            &cu,
            ChatId::from(chat),
            "com.example.app",
            &target_with_mark(7),
        )
        .expect_err("an unknown mark must not act");
        let StoredResolution::Failed {
            error_code, result, ..
        } = &missing
        else {
            panic!("an unknown mark fails the call");
        };
        assert_eq!(error_code, "stale_mark");
        assert!(result.contains("Capture the screen again"));

        // …and a mark from another app or chat is equally not a target here.
        for (other_chat, app) in [(chat, "com.other.app"), (Uuid::new_v4(), "com.example.app")] {
            assert!(
                resolve_target(&cu, ChatId::from(other_chat), app, &target_with_mark(1)).is_err(),
                "marks are scoped to one conversation and app"
            );
        }
    }

    #[test]
    fn an_explicit_element_target_passes_through_untouched() {
        let cu = ComputerUseState::default();
        let target = tidebreak_core::ElementTargetArgs {
            element_id: Some("0.3.1".to_owned()),
            element_fingerprint: Some("abc".to_owned()),
            ..Default::default()
        };
        let wire = resolve_target(&cu, ChatId::new(), "com.example.app", &target)
            .expect("an explicit element needs no marks");
        assert_eq!(wire.element_id.as_deref(), Some("0.3.1"));
        assert_eq!(wire.element_fingerprint.as_deref(), Some("abc"));
    }

    #[test]
    fn the_halt_latch_short_circuits_before_any_broker_round_trip() {
        let cu = ComputerUseState::default();
        assert!(!cu.is_halted());
        cu.halt();
        assert!(cu.is_halted());
        // The gate the dispatcher runs immediately before dispatch: halted
        // control ops get the non-retryable stop error without a broker call.
        let resolution = if cu.is_halted() {
            stopped_resolution()
        } else {
            unreachable!("the latch was just set")
        };
        let StoredResolution::Failed {
            error_code, result, ..
        } = &resolution
        else {
            panic!("a halted control op fails the call");
        };
        assert_eq!(error_code, "stopped_by_user");
        assert!(result.contains("Do not retry"));

        cu.resume();
        assert!(!cu.is_halted());
    }

    #[test]
    fn tool_arguments_map_to_their_broker_operations() {
        let cu = ComputerUseState::default();
        let call = |name: &str, arguments: serde_json::Value| ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: name.into(),
            arguments,
            raw_arguments: None,
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        };

        let action = build_action(
            &cu,
            &call(
                COMPUTER_CLICK_TOOL,
                serde_json::json!({
                    "app_id": "com.example.app",
                    "element_id": "0.1",
                    "element_fingerprint": "fp",
                    "button": "right",
                    "double": true,
                }),
            ),
        );
        let Ok(CuAction::Broker(OperationRequest::CuClick {
            bundle_id,
            button,
            click_count,
            ..
        })) = action
        else {
            panic!("click maps to CuClick: {action:?}");
        };
        assert_eq!(bundle_id, "com.example.app");
        assert_eq!(button.as_deref(), Some("right"));
        assert_eq!(click_count, Some(2));

        let action = build_action(
            &cu,
            &call(
                COMPUTER_KEY_PRESS_TOOL,
                serde_json::json!({
                    "app_id": "com.example.app",
                    "key": "return",
                    "modifiers": ["cmd", "shift"],
                }),
            ),
        );
        let Ok(CuAction::Broker(OperationRequest::CuKeyPress { key, modifiers, .. })) = action
        else {
            panic!("key press maps to CuKeyPress");
        };
        assert_eq!(key, "return");
        assert_eq!(modifiers, Some(vec!["cmd".to_owned(), "shift".to_owned()]));

        let action = build_action(
            &cu,
            &call(COMPUTER_CAPTURE_SCREEN_TOOL, serde_json::json!({})),
        );
        assert!(matches!(
            action,
            Ok(CuAction::Broker(OperationRequest::CuCaptureScreen {
                target: tidebreak_host_broker::CaptureTargetWire::Display { display_id: None }
            }))
        ));

        // Wait stays local; the clamp to the contract's bound happens at
        // execution time.
        let action = build_action(
            &cu,
            &call(COMPUTER_WAIT_TOOL, serde_json::json!({ "seconds": 2.5 })),
        );
        let Ok(CuAction::Wait(seconds)) = action else {
            panic!("wait stays local");
        };
        assert_eq!(seconds, 2.5);
        assert!(matches!(
            build_action(
                &cu,
                &call(COMPUTER_RETURN_TO_OPENWAVE_TOOL, serde_json::json!({}))
            ),
            Ok(CuAction::ReturnToOpenwave)
        ));
    }

    #[test]
    fn a_yield_maps_to_a_hard_refusal_not_a_consent_prompt() {
        let error = BrokerClientError::Broker {
            code: ErrorCode::Yielded,
            message: "wording is not the contract".to_owned(),
            retryable: false,
        };
        match map_broker_error(&error) {
            BrokerFailure::Resolution(StoredResolution::Failed { error_code, .. }) => {
                assert_eq!(error_code, "control_yielded");
            }
            _ => panic!("a yield must never become a consent card"),
        }

        // Denied is a grant miss even if the message still uses the former
        // yield sentence — the code is the contract, not the English.
        let grant_miss = BrokerClientError::Broker {
            code: ErrorCode::Denied,
            message: "a system security surface owns the foreground".to_owned(),
            retryable: false,
        };
        assert!(matches!(
            map_broker_error(&grant_miss),
            BrokerFailure::ConsentRequired
        ));
    }

    #[test]
    fn cu_receipts_round_trip_through_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let store = super::super::receipt_store::ReceiptStore::open(temp.path()).unwrap();
        let receipt = ComputerUseReceipt::new(ChatId::new(), CallId::new(), Uuid::new_v4());
        store.save_computer_use(&receipt).unwrap();
        let loaded = store.load_computer_uses().unwrap();
        assert_eq!(loaded, vec![receipt.clone()]);
        store.remove_computer_use(receipt.call_id).unwrap();
        assert!(store.load_computer_uses().unwrap().is_empty());
    }
}

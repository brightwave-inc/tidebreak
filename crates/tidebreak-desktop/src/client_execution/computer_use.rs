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

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tidebreak_core::{
    validate_computer_use_arguments, CallId, ComputerCaptureScreenArgs, ComputerClickArgs,
    ComputerDragArgs, ComputerFocusWindowArgs, ComputerHoverArgs, ComputerKeyPressArgs,
    ComputerLaunchAppArgs, ComputerListWindowsArgs, ComputerReadAppContentArgs,
    ComputerResizeWindowArgs, ComputerReturnToTidebreakArgs, ComputerScrollArgs,
    ComputerTypeTextArgs, ComputerWaitArgs, ComputerWaitConditionArgs, ImageRef, SessionId,
    ToolCallExecution, ToolCallRecord, ToolCallStatus, COMPUTER_CAPTURE_SCREEN_TOOL,
    COMPUTER_CLICK_TOOL, COMPUTER_DRAG_TOOL, COMPUTER_FOCUS_WINDOW_TOOL, COMPUTER_HOVER_TOOL,
    COMPUTER_KEY_PRESS_TOOL, COMPUTER_LAUNCH_APP_TOOL, COMPUTER_LIST_WINDOWS_TOOL,
    COMPUTER_READ_APP_CONTENT_TOOL, COMPUTER_RESIZE_WINDOW_TOOL, COMPUTER_RETURN_TO_TIDEBREAK_TOOL,
    COMPUTER_SCROLL_TOOL, COMPUTER_TYPE_TEXT_TOOL, COMPUTER_WAIT_TOOL, MAX_WAIT_SECONDS,
};
use tidebreak_host_broker::{
    is_blocked_control_bundle, Capability, ConditionWire, ConsentMethod, ControlRequest,
    ControlResult, CuConfirmControlActionRequest, CuGrantAppRequest, CuResolveHandoffRequest,
    CuRevokeAppRequest, ElementTargetWire, ErrorCode, ExecutionMode, GrantSubject, Mark,
    OperationEnvelope, OperationRequest, OperationResult, SubjectKind, PROTOCOL_VERSION,
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
/// Renderer event carrying the full control/consent snapshot on every change.
const STATE_EVENT: &str = "computer-use-state-changed";
/// A control op touches the indicator as recently active for this long after
/// its last broker round-trip; the renderer re-arms the banner on this window.
const INDICATOR_IDLE_REARM: std::time::Duration = std::time::Duration::from_secs(30);
/// Foreground-approval scope key for `computer_return_to_tidebreak`. It is
/// Tidebreak's own (control-blocked) bundle id, so it can never collide with
/// an app the broker could actually grant.
const TIDEBREAK_FOCUS_SCOPE: &str = "io.brightwave.tidebreak";

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

/// One native consent ask. This stays host-side: the renderer never receives
/// the call id or an authority that can resolve the decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConsentPromptView {
    call_id: CallId,
    chat_id: SessionId,
    bundle_id: String,
    app_name: Option<String>,
    capability: ConsentCapability,
    grant_scope: ConsentGrantScope,
}

/// The widest durable scope the consent prompt may offer truthfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConsentGrantScope {
    Chat,
    Project,
}

/// One native consequential-action confirmation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfirmationPromptView {
    call_id: CallId,
    chat_id: SessionId,
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
    /// Sessions stopped while another session owns input. They remain paused
    /// until trusted Resume, without describing the live owner as stopped.
    pub(crate) stopped_sessions: usize,
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
    /// (conversation, app scope) → marks from that pair's latest capture.
    /// The scope is the bundle id, or empty for a whole-display
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
    /// Linearizes Stop with every broker dispatch that can act on the host.
    /// The gate stays held through the broker round-trip so Stop either lands
    /// before dispatch (and prevents it) or after that dispatch has already
    /// completed. Read-only operations do not take this gate.
    acting_dispatch: tokio::sync::Mutex<()>,
    /// (conversation, app) pairs the user has separately approved for
    /// foreground takeover through the trusted native dialog. Host-side and
    /// session-only, exactly like the halt latch: nothing the renderer or the
    /// model produces can insert into this set, so an app-control grant can
    /// never silently become a takeover. Cleared by Stop — after a halt, a
    /// takeover must be re-approved.
    foreground_takeovers: StdMutex<HashSet<(Uuid, String)>>,
    /// The broker dispatch that actually owns input, plus session stops that
    /// remain set until the user resumes. Cancellation and owner changes use
    /// this synchronous lock so a helper is stopped before its future drops.
    dispatch_state: StdMutex<DispatchState>,
}

#[derive(Default)]
struct DispatchState {
    owner: Option<SessionId>,
    cancelled: HashSet<SessionId>,
    revoked: HashSet<SessionId>,
    stop_revision: u64,
}

struct ActingOwner<'a> {
    state: &'a StdMutex<DispatchState>,
}

impl Drop for ActingOwner<'_> {
    fn drop(&mut self) {
        lock(self.state).owner = None;
    }
}

impl Default for ComputerUseState {
    fn default() -> Self {
        Self {
            marks: StdMutex::new(HashMap::new()),
            app_names: StdMutex::new(HashMap::new()),
            indicator: StdMutex::new(IndicatorState::default()),
            halt: tokio::sync::watch::channel(false).0,
            acting_dispatch: tokio::sync::Mutex::new(()),
            foreground_takeovers: StdMutex::new(HashSet::new()),
            dispatch_state: StdMutex::new(DispatchState::default()),
        }
    }
}

fn lock<'a, T>(mutex: &'a StdMutex<T>) -> StdMutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ComputerUseState {
    /// Record the marks one capture reported for (chat, app).
    fn remember_marks(&self, chat_id: Uuid, scope: &str, marks: Vec<Mark>) {
        let mut table = lock(&self.marks);
        if table.len() >= MAX_MARK_TABLES && !table.contains_key(&(chat_id, scope.to_owned())) {
            table.clear();
        }
        table.insert((chat_id, scope.to_owned()), marks);
    }

    /// Resolve a Set-of-Marks number to its element address, from the most
    /// recent capture for this chat and app.
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
    pub(crate) fn is_halted(&self) -> bool {
        *self.halt.borrow()
    }

    /// Whether this chat already holds the user's separate takeover approval
    /// for this scope (an app bundle id, or [`TIDEBREAK_FOCUS_SCOPE`]).
    fn has_foreground_approval(&self, chat_id: Uuid, scope: &str) -> bool {
        lock(&self.foreground_takeovers).contains(&(chat_id, scope.to_owned()))
    }

    /// Record a trusted-dialog takeover approval for (chat, scope).
    fn remember_foreground_approval(&self, chat_id: Uuid, scope: &str) {
        lock(&self.foreground_takeovers).insert((chat_id, scope.to_owned()));
    }

    /// Await the next halt, returning immediately if already halted. A halt
    /// that fired between the pre-dispatch check and here is still observed:
    /// the watch receiver starts from the current value, not the next change.
    pub(crate) async fn wait_for_halt(&self) {
        let mut rx = self.halt.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Stop only this session. Only its live owner can cancel the shared
    /// helper; cancelling a queued session leaves another owner running.
    pub(crate) fn cancel_session<E>(
        &self,
        session: SessionId,
        cancel_helper: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        self.stop_session(session, false, cancel_helper)
    }

    fn revoke_session<E>(
        &self,
        session: SessionId,
        cancel_helper: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        self.stop_session(session, true, cancel_helper)
    }

    fn stop_session<E>(
        &self,
        session: SessionId,
        revoked: bool,
        cancel_helper: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut state = lock(&self.dispatch_state);
        lock(&self.foreground_takeovers).retain(|(id, _)| *id != session.0);
        state.stop_revision = state.stop_revision.saturating_add(1);
        if revoked {
            state.cancelled.remove(&session);
            state.revoked.insert(session);
        } else if !state.revoked.contains(&session) {
            state.cancelled.insert(session);
        }
        if state.owner != Some(session) {
            return Ok(false);
        }
        self.halt.send_replace(true);
        lock(&self.foreground_takeovers).clear();
        cancel_helper()?;
        Ok(true)
    }

    pub(crate) fn owns_dispatch(&self, session: SessionId) -> bool {
        lock(&self.dispatch_state).owner == Some(session)
    }

    pub(crate) fn stop_all<E>(
        &self,
        cancel_helper: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let mut state = lock(&self.dispatch_state);
        state.stop_revision = state.stop_revision.saturating_add(1);
        self.halt.send_replace(true);
        lock(&self.foreground_takeovers).clear();
        cancel_helper()
    }

    pub(crate) async fn drain_acting(&self) {
        let _dispatch = self.acting_dispatch.lock().await;
    }

    #[cfg(test)]
    async fn halt(&self) {
        self.stop_all(|| Ok::<_, ()>(())).unwrap();
        self.drain_acting().await;
    }

    async fn dispatch_acting<T, F, Fut>(
        &self,
        session: SessionId,
        dispatch: F,
    ) -> Result<T, StoredResolution>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let _dispatch = self.acting_dispatch.lock().await;
        let _owner = {
            let mut state = lock(&self.dispatch_state);
            if self.is_halted()
                || state.cancelled.contains(&session)
                || state.revoked.contains(&session)
            {
                return Err(stopped_resolution());
            }
            state.owner = Some(session);
            ActingOwner {
                state: &self.dispatch_state,
            }
        };
        Ok(dispatch().await)
    }

    /// Foreground WK input shares the native app input owner. Dropping its
    /// operation disarms the queued native callback and waits for an executing
    /// callback's lock before this method releases the shared dispatch gate.
    pub(crate) async fn dispatch_foreground_browser<T, F, Fut>(
        &self,
        session: SessionId,
        dispatch: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        self.dispatch_acting(session, || async {
            tokio::select! {
                biased;
                _ = self.wait_for_halt() => Err("browser control was stopped by the user".to_owned()),
                result = dispatch() => result,
            }
        })
        .await
        .map_err(|_| "browser control was stopped by the user".to_owned())?
    }

    #[cfg(test)]
    pub(crate) async fn test_dispatch_acting<T>(
        &self,
        session: SessionId,
        operation: impl std::future::Future<Output = T>,
    ) -> Result<T, ()> {
        self.dispatch_acting(session, || operation)
            .await
            .map_err(|_| ())
    }

    fn stop_revision(&self) -> u64 {
        lock(&self.dispatch_state).stop_revision
    }

    fn resume_with<E>(
        &self,
        approved_revision: u64,
        resume_helper: impl FnOnce() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut state = lock(&self.dispatch_state);
        if state.stop_revision != approved_revision {
            return Ok(false);
        }
        resume_helper()?;
        state.cancelled.clear();
        self.halt.send_replace(false);
        Ok(true)
    }

    #[cfg(test)]
    fn resume(&self) {
        self.resume_with(self.stop_revision(), || Ok::<_, ()>(()))
            .unwrap();
    }

    fn snapshot(&self) -> ComputerUseSnapshot {
        ComputerUseSnapshot {
            active: lock(&self.indicator).active.clone(),
            halted: self.is_halted(),
            stopped_sessions: lock(&self.dispatch_state).cancelled.len(),
        }
    }
}

fn emit_state(app: &AppHandle, cu: &ComputerUseState) {
    if let Err(error) = app.emit(STATE_EVENT, cu.snapshot()) {
        eprintln!("tidebreak-desktop: could not emit computer-use state: {error}");
    }
}

async fn native_binary_choice(
    app: &AppHandle,
    title: &str,
    message: &str,
    allow_label: &str,
) -> Result<bool, String> {
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            allow_label.to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |approved| {
        let _ = sender.send(approved);
    });
    receiver
        .await
        .map_err(|_| "the native computer-use prompt closed unexpectedly".to_owned())
}

async fn native_three_way_choice(
    app: &AppHandle,
    title: &str,
    message: &str,
    first: &str,
    second: &str,
    cancel: &str,
) -> Result<MessageDialogResult, String> {
    let (sender, receiver) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            first.to_owned(),
            second.to_owned(),
            cancel.to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show_with_result(move |answer| {
        let _ = sender.send(answer);
    });
    receiver
        .await
        .map_err(|_| "the native computer-use prompt closed unexpectedly".to_owned())
}

fn computer_use_consent_message(view: &ConsentPromptView) -> String {
    let app_label = if view.bundle_id.is_empty() {
        "your entire screen".to_owned()
    } else {
        crate::native_security_label(view.app_name.as_deref().unwrap_or(&view.bundle_id))
    };
    let action = match view.capability {
        ConsentCapability::CaptureScreen => "capture",
        ConsentCapability::ReadAppContent => "read on-screen content from",
        ConsentCapability::ControlApp => "control",
    };
    let mut message = format!(
        "Allow Tidebreak to {action} {app_label}? Screenshots and on-screen content within this permission can be sent to your selected model and provider. You can stop control at any time."
    );
    if view.capability == ConsentCapability::ControlApp {
        if tidebreak_host_broker::blocklist::is_development_control_bundle(&view.bundle_id) {
            message.push_str(" This app can run commands on your Mac. Allowing control lets Tidebreak use those commands with your account's permissions, including access outside the coding sandbox.");
        }
        if matches!(
            view.bundle_id.as_str(),
            "com.google.Chrome"
                | "com.google.Chrome.beta"
                | "com.google.Chrome.dev"
                | "com.google.Chrome.canary"
                | "com.apple.Safari"
                | "com.microsoft.edgemac"
                | "org.mozilla.firefox"
        ) {
            message.push_str(" This permission covers the browser app and its visible tabs. It is broader than sharing one website.");
        }
    }
    message
}

async fn native_consent_choice(
    app: &AppHandle,
    view: &ConsentPromptView,
) -> Result<ConsentDecision, String> {
    let first = native_three_way_choice(
        app,
        "Allow computer use?",
        &computer_use_consent_message(view),
        "Allow once",
        "Remember permission…",
        "Don't allow",
    )
    .await?;
    match first {
        MessageDialogResult::Yes => Ok(ConsentDecision::Once),
        MessageDialogResult::Custom(ref value) if value == "Allow once" => {
            Ok(ConsentDecision::Once)
        }
        MessageDialogResult::No => {
            if view.grant_scope == ConsentGrantScope::Chat {
                return Ok(ConsentDecision::Chat);
            }
            let scope = native_three_way_choice(
                app,
                "Remember computer-use permission?",
                "Choose how widely Tidebreak may remember this native permission.",
                "This chat",
                "This project",
                "Cancel",
            )
            .await?;
            remembered_scope(scope)
        }
        MessageDialogResult::Custom(ref value) if value == "Remember permission…" => {
            if view.grant_scope == ConsentGrantScope::Chat {
                return Ok(ConsentDecision::Chat);
            }
            let scope = native_three_way_choice(
                app,
                "Remember computer-use permission?",
                "Choose how widely Tidebreak may remember this native permission.",
                "This chat",
                "This project",
                "Cancel",
            )
            .await?;
            remembered_scope(scope)
        }
        _ => Ok(ConsentDecision::Decline),
    }
}

fn remembered_scope(answer: MessageDialogResult) -> Result<ConsentDecision, String> {
    match answer {
        MessageDialogResult::Yes => Ok(ConsentDecision::Chat),
        MessageDialogResult::No => Ok(ConsentDecision::Always),
        MessageDialogResult::Custom(value) if value == "This chat" => Ok(ConsentDecision::Chat),
        MessageDialogResult::Custom(value) if value == "This project" => {
            Ok(ConsentDecision::Always)
        }
        _ => Ok(ConsentDecision::Decline),
    }
}

async fn native_confirmation_choice(
    app: &AppHandle,
    view: &ConfirmationPromptView,
) -> Result<bool, String> {
    let app_label =
        crate::native_security_label(view.app_name.as_deref().unwrap_or(&view.bundle_id));
    let target = view
        .target_label
        .as_deref()
        .map(crate::native_security_label)
        .unwrap_or_else(|| "an unlabeled control".to_owned());
    let reason = crate::native_security_label(&view.reason);
    native_binary_choice(
        app,
        "Confirm consequential action",
        &format!("Allow Tidebreak to {reason}?\n\nTarget: {target}\nApplication: {app_label}"),
        "Allow action",
    )
    .await
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
pub(crate) async fn stop_computer_use_control(
    app: AppHandle,
    state: State<'_, HostAccess>,
) -> Result<(), String> {
    state
        .require_local(crate::host_authority::Authority::ComputerUse)
        .await?;
    let cancelled = state
        .computer_use
        .stop_all(|| state.broker.cancel_native_actions());
    emit_state(&app, &state.computer_use);
    if let Some(runtime) =
        app.try_state::<std::sync::Arc<crate::computer_runtime_adapter::DesktopComputerRuntime>>()
    {
        runtime.stop_all_chrome();
    }
    state.computer_use.drain_acting().await;
    cancelled.map_err(|error| error.to_string())
}

/// Re-arm control after a Stop. A renderer request may open the native prompt,
/// but cannot silently authorize the state transition.
#[tauri::command]
pub(crate) async fn resume_computer_use_control(
    app: AppHandle,
    state: State<'_, HostAccess>,
) -> Result<(), String> {
    state
        .require_local(crate::host_authority::Authority::ComputerUse)
        .await?;
    let approved_revision = state.computer_use.stop_revision();
    if native_binary_choice(
        &app,
        "Resume computer control?",
        "Resume only if you want Tidebreak to continue controlling applications on this Mac.",
        "Resume control",
    )
    .await?
    {
        // A new helper generation must not revive input that is still draining.
        let _dispatch = state.computer_use.acting_dispatch.lock().await;
        let resumed = state
            .computer_use
            .resume_with(approved_revision, || state.broker.resume_native_actions())
            .map_err(|error| error.to_string())?;
        if !resumed {
            return Err("Computer control was stopped again while Resume waited. Choose Resume again if you want to continue.".to_owned());
        }
        emit_state(&app, &state.computer_use);
    }
    Ok(())
}

// MARK: - Recovery loop

/// Recover persisted outcomes, then discover new computer-use calls. Native-
/// owned like the folder executor: no renderer event is an execution authority.
pub(crate) async fn recover_computer_use_operations(app: AppHandle) {
    loop {
        let failed = recover_once(&app).await;
        if failed {
            eprintln!("tidebreak-desktop: computer-use executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_computer_uses() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("tidebreak-desktop: computer-use receipt recovery failed: {error}");
            return true;
        }
    };
    let recovered_call_ids: std::collections::HashSet<CallId> =
        receipts.iter().map(|receipt| receipt.call_id).collect();
    let mut failed = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(app, &state, receipt).await {
            eprintln!("tidebreak-desktop: computer-use receipt deferred: {error}");
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
                eprintln!("tidebreak-desktop: computer-use execution deferred: {error}");
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
    let resolution = execute_operation(
        app,
        state,
        context,
        &claim.call,
        CaptureDelivery::PublishToChat,
    )
    .await;
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
    validate_computer_use_arguments(&call.name, &call.arguments)
}

// MARK: - Execution

/// How a capture's PNG leaves the executor: published into the chat's image
/// store (the renderer / transcript path), or handed back inline for the
/// session-native transport, which carries pixels in its own result frame
/// and has no chat blob store to publish into.
pub(crate) enum CaptureDelivery<'a> {
    PublishToChat,
    Inline(&'a mut Vec<SessionCaptureImage>),
}

/// One capture image returned inline to the session-native transport.
pub(crate) struct SessionCaptureImage {
    pub(crate) media_type: String,
    pub(crate) bytes: Vec<u8>,
}

/// What one parsed call wants done: a broker operation, or a purely local one.
#[derive(Debug)]
enum CuAction {
    Broker(OperationRequest),
    /// Return focus to Tidebreak itself. Deliberately not a broker op: Tidebreak
    /// is on the control blocklist, and focusing our own window is a local
    /// window-manager call, not synthesized input into another app. Reached
    /// only in foreground mode — build_action refuses the background default —
    /// and still gated on the user's separate takeover approval at execution.
    ReturnToTidebreak,
    /// A bounded local pause. The broker exposes the same op clamped; keeping
    /// it local saves a round-trip and never reaches the helper either way.
    Wait(f64),
}

/// The broker-wire mode for the model's requested mode. Absent means the
/// canonical background default — never an implicit takeover.
fn broker_mode(mode: Option<tidebreak_core::ExecutionMode>) -> ExecutionMode {
    match mode.unwrap_or_default() {
        tidebreak_core::ExecutionMode::Background => ExecutionMode::Background,
        tidebreak_core::ExecutionMode::Foreground => ExecutionMode::Foreground,
    }
}

/// The refusal every focus-taking call gets in background mode, and every
/// broker `requires_foreground` maps to. Nothing acted; the agent must not
/// blindly retry — a foreground re-issue is a deliberate escalation that asks
/// the user first.
fn requires_foreground_resolution() -> StoredResolution {
    unavailable(
        "requires_foreground",
        "This action needs the user's real focus or pointer, so it cannot run in the default background mode and was not performed. Do not retry it automatically. If taking over the screen is truly necessary, re-issue the action with execution_mode set to \"foreground\", which asks the user for permission first.",
    )
}

async fn execute_operation(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    delivery: CaptureDelivery<'_>,
) -> StoredResolution {
    let cu = &state.computer_use;
    let action = match build_action(cu, call) {
        Ok(action) => action,
        Err(resolution) => return resolution,
    };
    let event_call = tidebreak_core::computer_session::ComputerUseCall {
        request_id: call.id.0,
        name: call.name.clone(),
        arguments: call.arguments.clone(),
    };
    let activity = crate::computer_use_action::activity_for_call(
        call.chat_id,
        &event_call,
        crate::computer_use_action::ComputerUseActionSource::Native,
    )
    .map(|event| crate::computer_use_action::CallActivity::start(app, event));
    let resolution = async {
        match action {
            CuAction::ReturnToTidebreak => {
                // Foreground mode was already required by build_action; the
                // takeover approval is still asked separately per chat.
                if let Err(resolution) =
                    ensure_foreground_takeover(app, state, call, TIDEBREAK_FOCUS_SCOPE).await
                {
                    return resolution;
                }
                match cu
                    .dispatch_acting(call.chat_id, || async {
                        crate::deep_link::focus_main_window(app);
                        completed(serde_json::json!({
                            "status": "ok",
                            "focused": "tidebreak",
                            "execution_mode": "foreground",
                        }))
                    })
                    .await
                {
                    Ok(resolution) | Err(resolution) => resolution,
                }
            }
            CuAction::Wait(seconds) => {
                let seconds = seconds.clamp(0.0, MAX_WAIT_SECONDS);
                tokio::select! {
                    () = cu.wait_for_halt() => stopped_resolution(),
                    () = tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)) =>
                        completed(serde_json::json!({ "status": "ok", "waited_seconds": seconds })),
                }
            }
            CuAction::Broker(request) => {
                dispatch_broker(app, state, context, call, request, delivery).await
            }
        }
    }
    .await;
    if let Some(activity) = activity {
        let (success, error_code) = match &resolution {
            StoredResolution::Completed { .. } => (true, None),
            StoredResolution::Failed { error_code, .. } => (false, Some(error_code.as_str())),
            StoredResolution::Cancelled { .. } => (false, Some("cancelled")),
        };
        activity.finish(success, error_code);
    }
    resolution
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
    if !validate_computer_use_arguments(&call.name, &call.arguments) {
        return Err(invalid());
    }
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
            Ok(CuAction::Broker(
                OperationRequest::CuCaptureScreenDetailed {
                    target,
                    annotate: args.annotate,
                    window_id: args.window_id,
                    max_dimension: args.max_dimension,
                },
            ))
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
                execution_mode: broker_mode(args.execution_mode),
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
                execution_mode: broker_mode(args.execution_mode),
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
                execution_mode: broker_mode(args.execution_mode),
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
                execution_mode: broker_mode(args.execution_mode),
            }))
        }
        COMPUTER_FOCUS_WINDOW_TOOL => {
            let args: ComputerFocusWindowArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            // Focusing always changes what the user is looking at, so the
            // background default refuses here — before any broker round-trip —
            // rather than leaving a path that steals focus without the
            // explicit foreground escalation.
            if broker_mode(args.execution_mode) != ExecutionMode::Foreground {
                return Err(requires_foreground_resolution());
            }
            Ok(CuAction::Broker(OperationRequest::CuFocusWindow {
                bundle_id: args.app_id,
                window_id: args.window_id,
                execution_mode: ExecutionMode::Foreground,
            }))
        }
        COMPUTER_LAUNCH_APP_TOOL => {
            let args: ComputerLaunchAppArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Broker(OperationRequest::CuLaunchApp {
                bundle_id: args.app_id,
                execution_mode: broker_mode(args.execution_mode),
            }))
        }
        COMPUTER_HOVER_TOOL => {
            let args: ComputerHoverArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let target = resolve_target(cu, call.chat_id, &args.app_id, &args.target)?;
            Ok(CuAction::Broker(OperationRequest::CuHover {
                bundle_id: args.app_id,
                target,
                execution_mode: broker_mode(args.execution_mode),
            }))
        }
        COMPUTER_DRAG_TOOL => {
            let args: ComputerDragArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            let from = resolve_target(cu, call.chat_id, &args.app_id, &args.from)?;
            let to = resolve_target(cu, call.chat_id, &args.app_id, &args.to)?;
            Ok(CuAction::Broker(OperationRequest::CuDrag {
                bundle_id: args.app_id,
                from,
                to,
                duration_ms: args.duration_ms,
                execution_mode: broker_mode(args.execution_mode),
            }))
        }
        COMPUTER_RESIZE_WINDOW_TOOL => {
            let args: ComputerResizeWindowArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            Ok(CuAction::Broker(OperationRequest::CuResizeWindow {
                bundle_id: args.app_id,
                window_id: args.window_id,
                width: args.width,
                height: args.height,
                execution_mode: broker_mode(args.execution_mode),
            }))
        }
        COMPUTER_RETURN_TO_TIDEBREAK_TOOL => {
            let args: ComputerReturnToTidebreakArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            // Raising Tidebreak steals the user's focus like any other focus
            // move; the background default refuses instead of bypassing the
            // foreground escalation just because the target is our own window.
            if broker_mode(args.execution_mode) != ExecutionMode::Foreground {
                return Err(requires_foreground_resolution());
            }
            Ok(CuAction::ReturnToTidebreak)
        }
        COMPUTER_WAIT_TOOL => {
            let args: ComputerWaitArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| invalid())?;
            if let Some(condition) = args.condition {
                let condition = match condition {
                    ComputerWaitConditionArgs::AppRunning => ConditionWire::AppRunning,
                    ComputerWaitConditionArgs::WindowVisible => ConditionWire::WindowVisible,
                    ComputerWaitConditionArgs::TextPresent { text } => {
                        ConditionWire::TextPresent { text }
                    }
                    ComputerWaitConditionArgs::TextAbsent { text } => {
                        ConditionWire::TextAbsent { text }
                    }
                };
                Ok(CuAction::Broker(OperationRequest::CuWaitCondition {
                    bundle_id: args.app_id.ok_or_else(invalid)?,
                    condition,
                    timeout_seconds: args.condition_timeout_seconds,
                }))
            } else {
                Ok(CuAction::Wait(args.seconds.unwrap_or(1.0)))
            }
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
    chat_id: SessionId,
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
        OperationRequest::CuCaptureScreen { target }
        | OperationRequest::CuCaptureScreenDetailed { target, .. } => match target {
            tidebreak_host_broker::CaptureTargetWire::App { bundle_id } => Some(bundle_id),
            tidebreak_host_broker::CaptureTargetWire::Display { .. } => None,
        },
        OperationRequest::CuReadAppContent { bundle_id, .. }
        | OperationRequest::CuClick { bundle_id, .. }
        | OperationRequest::CuTypeText { bundle_id, .. }
        | OperationRequest::CuKeyPress { bundle_id, .. }
        | OperationRequest::CuScroll { bundle_id, .. }
        | OperationRequest::CuFocusWindow { bundle_id, .. }
        | OperationRequest::CuLaunchApp { bundle_id, .. }
        | OperationRequest::CuHover { bundle_id, .. }
        | OperationRequest::CuDrag { bundle_id, .. }
        | OperationRequest::CuResizeWindow { bundle_id, .. }
        | OperationRequest::CuWaitCondition { bundle_id, .. } => Some(bundle_id),
        _ => None,
    }
}

/// The mode a broker control operation will act in, when it carries one.
/// Read requests have no mode: observation never touches focus.
fn request_execution_mode(request: &OperationRequest) -> Option<ExecutionMode> {
    match request {
        OperationRequest::CuClick { execution_mode, .. }
        | OperationRequest::CuTypeText { execution_mode, .. }
        | OperationRequest::CuKeyPress { execution_mode, .. }
        | OperationRequest::CuScroll { execution_mode, .. }
        | OperationRequest::CuFocusWindow { execution_mode, .. }
        | OperationRequest::CuLaunchApp { execution_mode, .. }
        | OperationRequest::CuHover { execution_mode, .. }
        | OperationRequest::CuDrag { execution_mode, .. }
        | OperationRequest::CuResizeWindow { execution_mode, .. } => Some(*execution_mode),
        _ => None,
    }
}

/// The separate per-app, per-chat takeover approval every foreground action
/// requires. The existing app-control grant never implies it: the decision is
/// made through the trusted native dialog and remembered host-side in
/// [`ComputerUseState`], where neither renderer events nor model output can
/// forge it. Raced against the Stop latch like every other prompt.
async fn ensure_foreground_takeover(
    app: &AppHandle,
    state: &HostAccess,
    call: &ToolCallRecord,
    scope: &str,
) -> Result<(), StoredResolution> {
    let cu = &state.computer_use;
    if cu.is_halted() {
        return Err(stopped_resolution());
    }
    if cu.has_foreground_approval(call.chat_id.0, scope) {
        return Ok(());
    }
    let target_label = if scope == TIDEBREAK_FOCUS_SCOPE {
        "the Tidebreak window".to_owned()
    } else {
        crate::native_security_label(cu.app_name(scope).as_deref().unwrap_or(scope))
    };
    let message = format!(
        "Allow Tidebreak to take over your screen for {target_label}? Foreground control moves your pointer, keyboard focus, and active window while it acts, instead of working in the background. You can stop control at any time."
    );
    let approved = tokio::select! {
        approved = native_binary_choice(app, "Allow foreground control?", &message, "Take over") =>
            approved.unwrap_or(false),
        () = cu.wait_for_halt() => false,
    };
    if cu.is_halted() {
        return Err(stopped_resolution());
    }
    if !approved {
        return Err(unavailable(
            "foreground_declined",
            "The user declined to let Tidebreak take over the screen for this action. Do not retry in foreground mode; continue in the background or ask how they want to proceed.",
        ));
    }
    cu.remember_foreground_approval(call.chat_id.0, scope);
    Ok(())
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
        OperationRequest::CuCaptureScreen { .. }
        | OperationRequest::CuCaptureScreenDetailed { .. }
        | OperationRequest::CuListWindows { bundle_id: None } => ConsentCapability::CaptureScreen,
        _ => ConsentCapability::ReadAppContent,
    }
}

/// Whether this call acts on the host (synthesizes input or moves windows), as
/// opposed to only reading. Acting ops are what the Stop latch halts, what the
/// indicator reports, and what the blocklist pre-check guards.
pub(crate) fn acts_on_host(name: &str) -> bool {
    tidebreak_core::is_computer_use_control_tool(name)
        || name == COMPUTER_SCROLL_TOOL
        || name == COMPUTER_FOCUS_WINDOW_TOOL
        || name == COMPUTER_RETURN_TO_TIDEBREAK_TOOL
}

async fn dispatch_broker(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    request: OperationRequest,
    delivery: CaptureDelivery<'_>,
) -> StoredResolution {
    let cu = &state.computer_use;
    let acting = acts_on_host(&call.name);
    let bundle_id = request_bundle_id(&request).map(str::to_owned);

    // The broker's blocklist is mirrored here so a blocked app fails closed
    // without surfacing a consent card for it. Acting dispatches take the Stop
    // gate below; that gate owns the authoritative final halt check.
    if acting && cu.is_halted() {
        return stopped_resolution();
    }
    if let Some(bundle_id) = bundle_id.as_deref() {
        if is_blocked_control_bundle(bundle_id) {
            return unavailable(
                "app_blocked",
                "That application cannot be captured, read, or controlled by Tidebreak.",
            );
        }
    }
    // Foreground is an escalation on top of the app-control grant: it needs
    // its own per-app, per-chat trusted approval before any broker dispatch.
    // The approval outlives this call for the session (until Stop), so the
    // post-consent re-issue and follow-up actions in the same chat/app do not
    // re-prompt.
    if request_execution_mode(&request) == Some(ExecutionMode::Foreground) {
        let scope = bundle_id.clone().unwrap_or_default();
        if let Err(resolution) = ensure_foreground_takeover(app, state, call, &scope).await {
            return resolution;
        }
    }
    if let Some(bundle_id) = bundle_id.as_deref() {
        if acting {
            cu.note_control_activity(bundle_id);
            emit_state(app, cu);
        }
    }

    let envelope = OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: tidebreak_host_broker::RequestId::new(),
        context: context.execution,
        request: request.clone(),
    };
    let result = if acting {
        match cu
            .dispatch_acting(call.chat_id, || state.broker.operation(envelope))
            .await
        {
            Ok(result) => result,
            Err(resolution) => return resolution,
        }
    } else {
        state.broker.operation(envelope).await
    };
    match result {
        Ok(OperationResult::CuNeedsConfirmation(held)) => {
            if cu.is_halted() {
                return stopped_resolution();
            }
            dispatch_confirmation(app, state, call, held).await
        }
        Ok(result) => map_result(app, state, context, call, result, delivery).await,
        Err(error) => match map_broker_error(&error) {
            BrokerFailure::ConsentRequired => {
                dispatch_consent(app, state, context, call, request, delivery).await
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
            "macOS has not granted Tidebreak Screen Recording and Accessibility. Ask the user to enable them in Settings, then retry.",
        )),
        // The helper could not act without taking over the user's focus or
        // pointer, and did nothing. Surfaced verbatim as requires_foreground —
        // never a consent card, never an automatic foreground retry.
        ErrorCode::RequiresForeground => BrokerFailure::Resolution(requires_foreground_resolution()),
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

/// The per-app consent park: surface a native prompt, wait for the decision,
/// write the grant the decision implies, then re-issue the operation once.
async fn dispatch_consent(
    app: &AppHandle,
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
    request: OperationRequest,
    delivery: CaptureDelivery<'_>,
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
        grant_scope: match context.subject.kind() {
            SubjectKind::Project => ConsentGrantScope::Project,
            SubjectKind::Conversation => ConsentGrantScope::Chat,
        },
    };
    let decision = tokio::select! {
        decision = native_consent_choice(app, &view) => decision.unwrap_or(ConsentDecision::Decline),
        () = cu.wait_for_halt() => ConsentDecision::Decline,
    };

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
                "The user declined to let Tidebreak use this app. Do not retry; ask how they want to proceed.",
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

    if cu.is_halted() {
        revoke_once_grant(state, decision, capability, bundle_id.as_deref(), call).await;
        return stopped_resolution();
    }

    // Re-issued exactly once, now authorized. A second Denied means the grant
    // did not cover the op (a broker-side surprise, not another ask). A held
    // consequential action re-authorizes at confirm time, so the one-time
    // grant must outlive the whole continuation — revoke happens after it.
    let envelope = OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: tidebreak_host_broker::RequestId::new(),
        context: context.execution,
        request,
    };
    let acting = acts_on_host(&call.name);
    let result = if acting {
        match cu
            .dispatch_acting(call.chat_id, || state.broker.operation(envelope))
            .await
        {
            Ok(result) => result,
            Err(resolution) => {
                revoke_once_grant(state, decision, capability, bundle_id.as_deref(), call).await;
                return resolution;
            }
        }
    } else {
        state.broker.operation(envelope).await
    };
    let resolution = match result {
        Ok(OperationResult::CuNeedsConfirmation(held)) => {
            if cu.is_halted() {
                stopped_resolution()
            } else {
                dispatch_confirmation(app, state, call, held).await
            }
        }
        Ok(result) => map_result(app, state, context, call, result, delivery).await,
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
        eprintln!("tidebreak-desktop: one-time computer-use grant was not withdrawn: {error}");
    }
}

/// The act-time consequential confirmation: the broker is holding the action
/// and honors the native confirmation only while the target's label still
/// matches.
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
    let confirmed = tokio::select! {
        confirmed = native_confirmation_choice(app, &view) => confirmed.unwrap_or(false),
        () = cu.wait_for_halt() => false,
    };

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
    let result = match cu
        .dispatch_acting(call.chat_id, || {
            state.broker.control_without_retry(confirm, deadline)
        })
        .await
    {
        Ok(result) => result,
        Err(resolution) => return resolution,
    };
    match result {
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

/// Preserve screenshot badge numbers when a tree read returns a smaller or
/// changed set of elements. Tree targets use their explicit element identities.
fn finish_tree_read(
    cu: &ComputerUseState,
    call: &ToolCallRecord,
    tree: tidebreak_host_broker::computer_use::AxTree,
) -> StoredResolution {
    if let Some(bundle_id) = call
        .arguments
        .get("app_id")
        .and_then(serde_json::Value::as_str)
    {
        cu.learn_app_name(bundle_id, tree.app_name.as_deref());
    }
    let tree_text = serde_json::to_string(&tree.tree).unwrap_or_else(|_| "{}".to_owned());
    let (tree_text, over_limit) = tidebreak_core::truncate_utf8(&tree_text, MAX_TREE_RESULT_BYTES);
    completed(serde_json::json!({
        "status": "ok",
        "app_name": tree.app_name,
        "truncated": tree.truncated || over_limit,
        "tree": tree_text,
    }))
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
    delivery: CaptureDelivery<'_>,
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
            finish_capture(app, state, context, call, capture, delivery).await
        }
        OperationResult::CuReadAppContent(tree) => finish_tree_read(cu, call, tree),
        OperationResult::CuClick(meta)
        | OperationResult::CuTypeText(meta)
        | OperationResult::CuKeyPress(meta)
        | OperationResult::CuScroll(meta)
        | OperationResult::CuFocusWindow(meta)
        | OperationResult::CuLaunchApp(meta)
        | OperationResult::CuHover(meta)
        | OperationResult::CuDrag(meta)
        | OperationResult::CuResizeWindow(meta) => {
            if meta.success {
                completed(control_meta_json(&meta))
            } else {
                unavailable(
                    "operation_failed",
                    meta.detail
                        .as_deref()
                        .unwrap_or("The native action did not complete."),
                )
            }
        }
        OperationResult::CuWaitCondition(observation) => completed(serde_json::json!({
            "status": if observation.met { "ok" } else { "timed_out" },
            "met": observation.met,
            "timed_out": observation.timed_out,
        })),
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
    delivery: CaptureDelivery<'_>,
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
    let (image_refs, inline_images) = match delivery {
        CaptureDelivery::PublishToChat => {
            let app_state = app.state::<std::sync::Arc<AppState>>();
            // A capture without its image is not a usable result — the model
            // called this tool to see. Publish failure fails the call.
            let published =
                match crate::image_attachments::publish_image_bytes(
                    app_state.inner(),
                    state,
                    context.chat_id,
                    bytes,
                )
                .await
                {
                    Ok(published) => published,
                    Err(_) => return unavailable(
                        "image_publish_failed",
                        "The screenshot could not be attached to this conversation. Capture again.",
                    ),
                };
            (capture_image_refs(&published), None)
        }
        CaptureDelivery::Inline(sink) => {
            // The session-native transport carries the pixels in its own
            // result frame; identity is content-addressed the same way a
            // published attachment's would be.
            let media_type = tidebreak_core::ImageMediaType::parse(&capture.media_type)
                .unwrap_or(tidebreak_core::ImageMediaType::Png);
            let blob = tidebreak_core::DocumentBlob::from_bytes(&bytes);
            let image_ref = ImageRef {
                blob_id: blob.id,
                media_type,
                width: capture.width,
                height: capture.height,
                byte_len: bytes.len() as u64,
            };
            (vec![image_ref], Some((sink, media_type, bytes)))
        }
    };
    if let Some((sink, media_type, bytes)) = inline_images {
        sink.push(SessionCaptureImage {
            media_type: media_type.as_str().to_owned(),
            bytes,
        });
    }
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
        "coordinate_frame": capture.coordinate_frame,
        "coordinate_note": "Input coordinates are global logical points. Map screenshot pixels with x = coordinate_frame.x + pixel_x * coordinate_frame.width / width and y = coordinate_frame.y + pixel_y * coordinate_frame.height / height.",
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
        "status": if meta.success { "ok" } else { "failed" },
        "success": meta.success,
        "used_fallback": meta.used_fallback,
        "detail": meta.detail,
        // The mode the helper actually acted in — truthful result metadata,
        // absent when the helper predates the mode contract.
        "execution_mode": meta.execution_mode,
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

// MARK: - Session-native transport entry point

/// The terminal shape one session-native operation resolves to, decoupled
/// from the durable chat receipt format.
pub(crate) enum SessionNativeResolution {
    Completed {
        result: serde_json::Value,
    },
    Failed {
        result: serde_json::Value,
        error_code: String,
    },
}

/// One session-native operation's outcome: the resolution plus any capture
/// images returned inline.
pub(crate) struct SessionNativeOutput {
    pub(crate) resolution: SessionNativeResolution,
    pub(crate) images: Vec<SessionCaptureImage>,
    /// Whether the operation synthesizes input or moves focus — the class
    /// whose interrupted or lost outcome must be treated as unknown rather
    /// than safely absent.
    pub(crate) acts_on_host: bool,
}

/// Latch this session before waking its operation. A live owner also stops
/// the helper synchronously; the caller then drains that dispatch. A queued
/// session cannot cancel another session's helper.
pub(crate) fn cancel_session_native_input(
    app: &AppHandle,
    state: &HostAccess,
    session: SessionId,
) -> Result<bool, String> {
    let result = state
        .computer_use
        .cancel_session(session, || state.broker.cancel_native_actions())
        .map_err(|error| error.to_string());
    emit_state(app, &state.computer_use);
    result
}

pub(crate) fn revoke_session_native_input(
    app: &AppHandle,
    state: &HostAccess,
    session: SessionId,
) -> Result<bool, String> {
    let result = state
        .computer_use
        .revoke_session(session, || state.broker.cancel_native_actions())
        .map_err(|error| error.to_string());
    emit_state(app, &state.computer_use);
    result
}

pub(crate) fn stop_all_native_input(app: &AppHandle, state: &HostAccess) -> Result<(), String> {
    let result = state
        .computer_use
        .stop_all(|| state.broker.cancel_native_actions())
        .map_err(|error| error.to_string());
    emit_state(app, &state.computer_use);
    result
}

/// Execute one native computer-use operation for a code session.
///
/// Same executor, same authority: the broker authorizes against per-session
/// grants (the session id is the conversation-scoped grant subject), a grant
/// miss parks behind the same native consent card, the acting-dispatch gate
/// gives the operation exclusive desktop input ownership, and the user's
/// Stop latch short-circuits control exactly as it does for chat calls. The
/// only difference is transport: capture pixels return inline instead of
/// publishing into a chat blob store, because the session-native channel
/// carries its own bounded image frames.
pub(crate) async fn execute_session_native_operation(
    app: &AppHandle,
    state: &HostAccess,
    session_id: SessionId,
    call_id: CallId,
    name: &str,
    arguments: serde_json::Value,
) -> Result<SessionNativeOutput, String> {
    let context = crate::host_access::session_native_context(session_id.0)?;
    let call = session_call_record(session_id, call_id, name, arguments);
    let mut images = Vec::new();
    let resolution = execute_operation(
        app,
        state,
        context,
        &call,
        CaptureDelivery::Inline(&mut images),
    )
    .await;
    let resolution = match resolution {
        StoredResolution::Completed { result, .. } => SessionNativeResolution::Completed {
            result: serde_json::from_str(&result).unwrap_or(serde_json::Value::Null),
        },
        StoredResolution::Failed {
            result, error_code, ..
        } => SessionNativeResolution::Failed {
            result: serde_json::from_str(&result).unwrap_or(serde_json::Value::Null),
            error_code,
        },
        StoredResolution::Cancelled { result } => SessionNativeResolution::Failed {
            result: serde_json::from_str(&result).unwrap_or(serde_json::Value::Null),
            error_code: "computer_use_cancelled".to_owned(),
        },
    };
    Ok(SessionNativeOutput {
        resolution,
        images,
        acts_on_host: acts_on_host(name),
    })
}

/// A synthetic call record for the session-native path. The executor reads
/// only identity, subject, name, and arguments from it; every durable-lease
/// field stays empty because the session channel owns its own request-id
/// recovery instead of the chat control plane's leases.
fn session_call_record(
    session_id: SessionId,
    call_id: CallId,
    name: &str,
    arguments: serde_json::Value,
) -> ToolCallRecord {
    ToolCallRecord {
        id: call_id,
        chat_id: session_id,
        turn_id: tidebreak_core::TurnId::new(),
        provider_id: String::new(),
        name: name.to_owned(),
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
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_app_consent_discloses_pixels_and_broader_control() {
        let mut view = ConsentPromptView {
            call_id: tidebreak_core::CallId::new(),
            chat_id: tidebreak_core::SessionId::new(),
            bundle_id: "com.microsoft.VSCode".into(),
            app_name: Some("Visual Studio Code".into()),
            capability: ConsentCapability::ControlApp,
            grant_scope: ConsentGrantScope::Chat,
        };
        let message = computer_use_consent_message(&view);
        assert!(message.contains("selected model and provider"));
        assert!(message.contains("outside the coding sandbox"));
        view.bundle_id = "com.google.Chrome".into();
        assert!(computer_use_consent_message(&view).contains("broader than sharing one website"));
        view.capability = ConsentCapability::ReadAppContent;
        assert!(!computer_use_consent_message(&view).contains("can run commands"));
        view.bundle_id.clear();
        assert!(computer_use_consent_message(&view).contains("your entire screen"));
    }

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
            SessionId::from(chat),
            "com.example.app",
            &target_with_mark(2),
        )
        .expect("a live mark resolves");
        assert_eq!(wire.element_id.as_deref(), Some("0.4.2"));
        assert_eq!(wire.element_fingerprint.as_deref(), Some("fp-0.4.2"));
        assert_eq!(wire.x, None);
    }

    #[test]
    fn a_narrow_tree_read_preserves_the_last_screenshot_mark_mapping() {
        let cu = ComputerUseState::default();
        let chat = SessionId::new();
        cu.remember_marks(
            chat.0,
            "com.example.app",
            vec![mark(1, "0.1"), mark(2, "0.4.2")],
        );
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat,
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tree-read".into(),
            name: COMPUTER_READ_APP_CONTENT_TOOL.into(),
            arguments: serde_json::json!({ "app_id": "com.example.app", "max_nodes": 1 }),
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
        let tree = tidebreak_host_broker::computer_use::AxTree {
            app_name: Some("Example".into()),
            tree: serde_json::json!({
                "role": "AXButton",
                "id": "0.4.2",
                "fingerprint": "fp-0.4.2",
                "title": "Second button",
                "frame": { "x": 0, "y": 0, "width": 10, "height": 10 },
            }),
            truncated: true,
        };
        let narrow_marks = tidebreak_host_broker::extract_marks(&tree.tree, 80);
        assert_eq!(narrow_marks[0].element_id, "0.4.2");
        assert_eq!(
            narrow_marks[0].mark, 1,
            "the narrower tree would renumber this element"
        );

        let resolution = finish_tree_read(&cu, &call, tree);
        assert!(matches!(resolution, StoredResolution::Completed { .. }));
        assert_eq!(cu.app_name("com.example.app").as_deref(), Some("Example"));
        for (number, element_id) in [(1, "0.1"), (2, "0.4.2")] {
            let target = resolve_target(&cu, chat, "com.example.app", &target_with_mark(number))
                .expect("the last screenshot still owns its badge numbers");
            assert_eq!(target.element_id.as_deref(), Some(element_id));
            assert_eq!(target.element_fingerprint, Some(format!("fp-{element_id}")));
        }
    }

    #[test]
    fn a_newer_capture_replaces_the_marks_a_target_resolves_against() {
        let cu = ComputerUseState::default();
        let chat = Uuid::new_v4();
        cu.remember_marks(chat, "com.example.app", vec![mark(1, "0.1")]);
        cu.remember_marks(chat, "com.example.app", vec![mark(1, "9.9")]);

        let wire = resolve_target(
            &cu,
            SessionId::from(chat),
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
            SessionId::from(chat),
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
                resolve_target(&cu, SessionId::from(other_chat), app, &target_with_mark(1))
                    .is_err(),
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
        let wire = resolve_target(&cu, SessionId::new(), "com.example.app", &target)
            .expect("an explicit element needs no marks");
        assert_eq!(wire.element_id.as_deref(), Some("0.3.1"));
        assert_eq!(wire.element_fingerprint.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn the_halt_latch_short_circuits_before_any_broker_round_trip() {
        let cu = ComputerUseState::default();
        assert!(!cu.is_halted());
        cu.halt().await;
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

    #[tokio::test]
    async fn stop_sets_the_latch_before_waiting_for_an_in_flight_action() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let (dispatch_started_tx, dispatch_started_rx) = oneshot::channel::<()>();
        let (release_dispatch_tx, release_dispatch_rx) = oneshot::channel::<()>();

        let dispatch_cu = std::sync::Arc::clone(&cu);
        let dispatch = tokio::spawn(async move {
            dispatch_cu
                .dispatch_acting(SessionId::new(), || async move {
                    dispatch_started_tx
                        .send(())
                        .expect("the test observes the in-flight action");
                    release_dispatch_rx
                        .await
                        .expect("the test releases the in-flight action");
                })
                .await
        });
        dispatch_started_rx
            .await
            .expect("the acting request holds the dispatch gate");

        let halt_cu = std::sync::Arc::clone(&cu);
        let halt = tokio::spawn(async move { halt_cu.halt().await });
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            while !cu.is_halted() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Stop must publish its latch without waiting for the broker");
        assert!(
            !halt.is_finished(),
            "Stop still drains the in-flight action"
        );

        release_dispatch_tx
            .send(())
            .expect("release the in-flight action");
        dispatch.await.expect("the acting task completes").unwrap();
        halt.await.expect("the Stop task completes after the drain");
    }

    #[tokio::test]
    async fn stop_prevents_an_ordinary_acting_request_from_starting_after_it_completes() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let (ready_to_dispatch, dispatch_ready) = oneshot::channel::<()>();
        let (continue_dispatch, dispatch_continues) = oneshot::channel::<()>();
        let (broker_request, mut broker_requests) = tokio::sync::mpsc::unbounded_channel();

        let dispatch_cu = std::sync::Arc::clone(&cu);
        let dispatch = tokio::spawn(async move {
            ready_to_dispatch
                .send(())
                .expect("the test observes the acting request before dispatch");
            dispatch_continues
                .await
                .expect("the acting request may continue");
            dispatch_cu
                .dispatch_acting(SessionId::new(), || async move {
                    broker_request
                        .send("ordinary_acting_request")
                        .expect("the broker observer remains open");
                })
                .await
        });

        dispatch_ready
            .await
            .expect("the acting request reaches the pre-dispatch pause");
        cu.halt().await;
        continue_dispatch
            .send(())
            .expect("release the acting request after Stop completes");

        let resolution = dispatch
            .await
            .expect("the acting dispatch task completes")
            .expect_err("Stop must prevent the ordinary acting broker request");
        let StoredResolution::Failed { error_code, .. } = resolution else {
            panic!("a stopped acting request fails the call");
        };
        assert_eq!(error_code, "stopped_by_user");
        assert!(broker_requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_prevents_a_post_consent_reissue_from_starting_after_it_completes() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let (consent_committed, consent_is_committed) = oneshot::channel::<()>();
        let (continue_reissue, reissue_continues) = oneshot::channel::<()>();
        let (broker_request, mut broker_requests) = tokio::sync::mpsc::unbounded_channel();

        let reissue_cu = std::sync::Arc::clone(&cu);
        let reissue = tokio::spawn(async move {
            consent_committed
                .send(())
                .expect("the test observes consent before the reissue");
            reissue_continues
                .await
                .expect("the authorized reissue may continue");
            reissue_cu
                .dispatch_acting(SessionId::new(), || async move {
                    broker_request
                        .send("post_consent_reissue")
                        .expect("the broker observer remains open");
                })
                .await
        });

        consent_is_committed
            .await
            .expect("consent reaches the pre-reissue pause");
        cu.halt().await;
        continue_reissue
            .send(())
            .expect("release the authorized reissue after Stop completes");

        let resolution = reissue
            .await
            .expect("the post-consent reissue task completes")
            .expect_err("Stop must prevent the post-consent broker request");
        let StoredResolution::Failed { error_code, .. } = resolution else {
            panic!("a stopped post-consent reissue fails the call");
        };
        assert_eq!(error_code, "stopped_by_user");
        assert!(broker_requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_after_native_approval_prevents_confirmation_redemption() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let (approved, native_approval) = oneshot::channel::<()>();
        let (continue_after_approval, continue_redemption) = oneshot::channel::<()>();
        let (broker_request, mut broker_requests) = tokio::sync::mpsc::unbounded_channel();

        let redemption_cu = std::sync::Arc::clone(&cu);
        let redemption = tokio::spawn(async move {
            native_approval.await.expect("native approval arrives");
            continue_redemption
                .await
                .expect("the approved action may continue");
            redemption_cu
                .dispatch_acting(SessionId::new(), || async move {
                    broker_request
                        .send(ControlRequest::CuConfirmControlAction(
                            CuConfirmControlActionRequest {
                                confirmation_id: Uuid::new_v4(),
                            },
                        ))
                        .expect("the broker observer remains open");
                })
                .await
        });

        approved.send(()).expect("approve the native prompt");
        cu.halt().await;
        continue_after_approval
            .send(())
            .expect("release the approved action after Stop");

        let resolution = redemption
            .await
            .expect("the redemption task completes")
            .expect_err("Stop must prevent the confirmation request");
        let StoredResolution::Failed { error_code, .. } = resolution else {
            panic!("a stopped confirmation fails the call");
        };
        assert_eq!(error_code, "stopped_by_user");
        assert!(broker_requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancelling_a_queued_session_preserves_the_live_owner() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let owner = SessionId::new();
        let queued = SessionId::new();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let owner_cu = cu.clone();
        let owner_task = tokio::spawn(async move {
            owner_cu
                .dispatch_acting(owner, || async move {
                    started.send(()).unwrap();
                    release_rx.await.unwrap();
                })
                .await
        });
        started_rx.await.unwrap();
        let queued_cu = cu.clone();
        let (queued_started, mut queued_started_rx) = oneshot::channel();
        let queued_task = tokio::spawn(async move {
            queued_cu
                .dispatch_acting(queued, || async move {
                    queued_started.send(()).unwrap();
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(!cu
            .cancel_session(queued, || -> Result<(), ()> {
                panic!("a queued session must not signal another owner's helper");
            })
            .unwrap());
        assert!(cu.owns_dispatch(owner));
        assert!(!cu.is_halted(), "another owner keeps running");
        assert!(!cu.snapshot().halted);
        assert_eq!(cu.snapshot().stopped_sessions, 1);
        assert!(!owner_task.is_finished());
        release.send(()).unwrap();
        owner_task.await.unwrap().unwrap();
        assert!(queued_task.await.unwrap().is_err());
        assert!(queued_started_rx.try_recv().is_err());
        assert!(cu.dispatch_acting(queued, || async {}).await.is_err());
        assert!(cu.dispatch_acting(owner, || async {}).await.is_ok());
        cu.resume();
        assert!(cu.dispatch_acting(queued, || async {}).await.is_ok());
    }

    #[tokio::test]
    async fn a_new_stop_invalidates_resume_waiting_for_the_dispatch_gate() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let gate = cu.acting_dispatch.lock().await;
        cu.stop_all(|| Ok::<_, ()>(())).unwrap();
        let approved_revision = cu.stop_revision();
        let resume_cu = cu.clone();
        let (waiting, waiting_rx) = oneshot::channel();
        let resume = tokio::spawn(async move {
            waiting.send(()).unwrap();
            let _gate = resume_cu.acting_dispatch.lock().await;
            resume_cu.resume_with(approved_revision, || -> Result<(), ()> {
                panic!("an older Resume must not replace the helper's stopped generation");
            })
        });
        waiting_rx.await.unwrap();
        cu.stop_all(|| Ok::<_, ()>(())).unwrap();
        drop(gate);
        assert!(!resume.await.unwrap().unwrap());
        assert!(cu.is_halted());
        cu.resume();
        assert!(!cu.is_halted());
    }

    #[tokio::test]
    async fn foreground_browser_waits_for_the_native_app_input_owner() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let native_session = SessionId::new();
        let browser_session = SessionId::new();
        let (native_started, native_started_rx) = oneshot::channel();
        let (release_native, release_native_rx) = oneshot::channel();
        let native_cu = cu.clone();
        let native = tokio::spawn(async move {
            native_cu
                .dispatch_acting(native_session, || async {
                    native_started.send(()).unwrap();
                    release_native_rx.await.unwrap();
                })
                .await
        });
        native_started_rx.await.unwrap();
        let (browser_started, mut browser_started_rx) = oneshot::channel();
        let browser_cu = cu.clone();
        let browser = tokio::spawn(async move {
            browser_cu
                .dispatch_foreground_browser(browser_session, || async {
                    browser_started.send(()).unwrap();
                    Ok(())
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(cu.owns_dispatch(native_session));
        assert!(matches!(
            browser_started_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        release_native.send(()).unwrap();
        native.await.unwrap().unwrap();
        browser_started_rx.await.unwrap();
        browser.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn an_ended_session_stays_revoked_without_offering_resume() {
        let cu = ComputerUseState::default();
        let session = SessionId::new();
        cu.revoke_session(session, || -> Result<(), ()> {
            panic!("an idle revoke must not stop another helper");
        })
        .unwrap();
        assert_eq!(cu.snapshot().stopped_sessions, 0);
        assert!(!cu.is_halted());
        cu.resume();
        assert!(cu.dispatch_acting(session, || async {}).await.is_err());
    }

    #[tokio::test]
    async fn cancelling_the_owner_signals_the_helper_before_releasing_ownership() {
        let cu = std::sync::Arc::new(ComputerUseState::default());
        let owner = SessionId::new();
        let (started, started_rx) = oneshot::channel();
        let (helper_cancel, helper_cancel_rx) = oneshot::channel();
        let signalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owner_cu = cu.clone();
        let observed = signalled.clone();
        let task = tokio::spawn(async move {
            owner_cu
                .dispatch_acting(owner, || async move {
                    started.send(()).unwrap();
                    helper_cancel_rx.await.unwrap();
                    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
                })
                .await
        });
        started_rx.await.unwrap();
        assert!(cu
            .cancel_session(owner, || -> Result<(), ()> {
                signalled.store(true, std::sync::atomic::Ordering::SeqCst);
                helper_cancel.send(()).unwrap();
                Ok(())
            })
            .unwrap());
        task.await.unwrap().unwrap();
        assert!(!cu.owns_dispatch(owner));
        assert!(cu.is_halted());
        assert!(cu.dispatch_acting(owner, || async {}).await.is_err());
    }

    #[test]
    fn tool_arguments_map_to_their_broker_operations() {
        let cu = ComputerUseState::default();
        let call = |name: &str, arguments: serde_json::Value| ToolCallRecord {
            id: CallId::new(),
            chat_id: SessionId::new(),
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
            Ok(CuAction::Broker(
                OperationRequest::CuCaptureScreenDetailed {
                    target: tidebreak_host_broker::CaptureTargetWire::Display { display_id: None },
                    annotate: true,
                    window_id: None,
                    max_dimension: None,
                }
            ))
        ));

        let capture_call = call(
            COMPUTER_CAPTURE_SCREEN_TOOL,
            serde_json::json!({
                "app_id": "dev.tidebreak.fixture", "window_id": 42,
                "max_dimension": 720, "annotate": false,
            }),
        );
        let Ok(CuAction::Broker(request)) = build_action(&cu, &capture_call) else {
            panic!("selected-window capture must be mapped");
        };
        assert!(matches!(
            &request,
            OperationRequest::CuCaptureScreenDetailed {
                window_id: Some(42),
                max_dimension: Some(720),
                annotate: false,
                ..
            }
        ));
        assert_eq!(request_bundle_id(&request), Some("dev.tidebreak.fixture"));
        assert_eq!(
            consent_capability(&capture_call, &request),
            ConsentCapability::CaptureScreen
        );

        let wait_call = call(
            COMPUTER_WAIT_TOOL,
            serde_json::json!({
                "app_id": "dev.tidebreak.fixture", "condition": { "kind": "text_present", "text": "Ready" },
                "condition_timeout_seconds": 3,
            }),
        );
        assert!(is_computer_use_call(&wait_call));
        assert!(
            matches!(build_action(&cu, &wait_call), Ok(CuAction::Broker(OperationRequest::CuWaitCondition {
            bundle_id, condition: ConditionWire::TextPresent { text }, timeout_seconds: Some(3.0),
        })) if bundle_id == "dev.tidebreak.fixture" && text == "Ready")
        );
        assert!(build_action(
            &cu,
            &call(
                COMPUTER_WAIT_TOOL,
                serde_json::json!({
                    "condition": { "kind": "app_running" },
                })
            )
        )
        .is_err());

        let mut drag_call = call(
            COMPUTER_DRAG_TOOL,
            serde_json::json!({
                "app_id": "dev.tidebreak.fixture", "from": { "mark": 1 }, "to": { "mark": 2 }, "duration_ms": 300,
            }),
        );
        cu.remember_marks(
            drag_call.chat_id.0,
            "dev.tidebreak.fixture",
            vec![mark(1, "0.1"), mark(2, "0.2")],
        );
        let Ok(CuAction::Broker(request)) = build_action(&cu, &drag_call) else {
            panic!("drag must resolve both marks");
        };
        assert!(
            matches!(&request, OperationRequest::CuDrag { from, to, duration_ms: Some(300), .. }
            if from.element_id.as_deref() == Some("0.1") && to.element_id.as_deref() == Some("0.2"))
        );
        assert_eq!(
            consent_capability(&drag_call, &request),
            ConsentCapability::ControlApp
        );
        drag_call.chat_id = SessionId::new();
        assert!(
            build_action(&cu, &drag_call).is_err(),
            "another chat cannot reuse marks"
        );

        for (name, args) in [
            (
                COMPUTER_LAUNCH_APP_TOOL,
                serde_json::json!({"app_id": "dev.tidebreak.fixture"}),
            ),
            (
                COMPUTER_HOVER_TOOL,
                serde_json::json!({"app_id": "dev.tidebreak.fixture", "x": 50, "y": 100}),
            ),
            (
                COMPUTER_RESIZE_WINDOW_TOOL,
                serde_json::json!({"app_id": "dev.tidebreak.fixture", "width": 800, "height": 600}),
            ),
        ] {
            let current = call(name, args);
            assert!(is_computer_use_call(&current));
            let Ok(CuAction::Broker(request)) = build_action(&cu, &current) else {
                panic!("{name} must map");
            };
            assert!(acts_on_host(name));
            assert_eq!(request_bundle_id(&request), Some("dev.tidebreak.fixture"));
            assert_eq!(
                consent_capability(&current, &request),
                ConsentCapability::ControlApp
            );
        }

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
        // Returning focus to Tidebreak is a focus steal like any other: the
        // background default refuses, and only an explicit foreground request
        // maps to the local action (still gated on takeover approval later).
        assert!(matches!(
            build_action(
                &cu,
                &call(COMPUTER_RETURN_TO_TIDEBREAK_TOOL, serde_json::json!({}))
            ),
            Err(StoredResolution::Failed { .. })
        ));
        assert!(matches!(
            build_action(
                &cu,
                &call(
                    COMPUTER_RETURN_TO_TIDEBREAK_TOOL,
                    serde_json::json!({ "execution_mode": "foreground" })
                )
            ),
            Ok(CuAction::ReturnToTidebreak)
        ));
    }

    #[test]
    fn control_actions_default_to_background_and_carry_an_explicit_mode() {
        let cu = ComputerUseState::default();
        let call = |name: &str, arguments: serde_json::Value| ToolCallRecord {
            id: CallId::new(),
            chat_id: SessionId::new(),
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

        // Absent on the tool call means background on the broker wire.
        let Ok(CuAction::Broker(request)) = build_action(
            &cu,
            &call(
                COMPUTER_LAUNCH_APP_TOOL,
                serde_json::json!({ "app_id": "dev.tidebreak.fixture" }),
            ),
        ) else {
            panic!("launch maps");
        };
        assert_eq!(
            request_execution_mode(&request),
            Some(ExecutionMode::Background)
        );

        // An explicit foreground request survives the mapping.
        let Ok(CuAction::Broker(request)) = build_action(
            &cu,
            &call(
                COMPUTER_CLICK_TOOL,
                serde_json::json!({
                    "app_id": "dev.tidebreak.fixture",
                    "x": 10.0,
                    "y": 20.0,
                    "execution_mode": "foreground",
                }),
            ),
        ) else {
            panic!("click maps");
        };
        assert_eq!(
            request_execution_mode(&request),
            Some(ExecutionMode::Foreground)
        );

        // Reads never carry a mode to gate on.
        let Ok(CuAction::Broker(request)) = build_action(
            &cu,
            &call(COMPUTER_LIST_WINDOWS_TOOL, serde_json::json!({})),
        ) else {
            panic!("list maps");
        };
        assert_eq!(request_execution_mode(&request), None);
    }

    #[test]
    fn focus_window_refuses_the_background_default_without_a_broker_round_trip() {
        let cu = ComputerUseState::default();
        let call = |arguments: serde_json::Value| ToolCallRecord {
            id: CallId::new(),
            chat_id: SessionId::new(),
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: COMPUTER_FOCUS_WINDOW_TOOL.into(),
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

        let refused = build_action(
            &cu,
            &call(serde_json::json!({ "app_id": "com.example.app" })),
        )
        .expect_err("background focus must refuse");
        let StoredResolution::Failed {
            error_code, result, ..
        } = &refused
        else {
            panic!("background focus fails the call");
        };
        assert_eq!(error_code, "requires_foreground");
        assert!(result.contains("Do not retry it automatically"));

        let action = build_action(
            &cu,
            &call(serde_json::json!({
                "app_id": "com.example.app",
                "execution_mode": "foreground",
            })),
        )
        .expect("foreground focus maps to the broker op");
        let CuAction::Broker(request) = action else {
            panic!("focus is a broker op");
        };
        assert_eq!(
            request_execution_mode(&request),
            Some(ExecutionMode::Foreground)
        );
    }

    #[tokio::test]
    async fn foreground_takeover_approvals_are_scoped_and_withdrawn_by_stop() {
        let cu = ComputerUseState::default();
        let chat = Uuid::new_v4();
        assert!(!cu.has_foreground_approval(chat, "com.example.app"));
        cu.remember_foreground_approval(chat, "com.example.app");
        assert!(cu.has_foreground_approval(chat, "com.example.app"));
        // Scoped to the exact (chat, app) pair — no bleed across apps or chats.
        assert!(!cu.has_foreground_approval(chat, "com.other.app"));
        assert!(!cu.has_foreground_approval(Uuid::new_v4(), "com.example.app"));
        // Stop withdraws every takeover approval; resume does not restore it.
        cu.halt().await;
        assert!(!cu.has_foreground_approval(chat, "com.example.app"));
        cu.resume();
        assert!(!cu.has_foreground_approval(chat, "com.example.app"));
    }

    #[test]
    fn requires_foreground_maps_to_a_hard_refusal_not_a_consent_prompt() {
        let error = BrokerClientError::Broker {
            code: ErrorCode::RequiresForeground,
            message: "wording is not the contract".to_owned(),
            retryable: false,
        };
        match map_broker_error(&error) {
            BrokerFailure::Resolution(StoredResolution::Failed {
                error_code, result, ..
            }) => {
                assert_eq!(error_code, "requires_foreground");
                assert!(result.contains("execution_mode"));
                assert!(result.contains("Do not retry it automatically"));
            }
            _ => panic!("requires_foreground must never become a consent card"),
        }
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
        let receipt = ComputerUseReceipt::new(SessionId::new(), CallId::new(), Uuid::new_v4());
        store.save_computer_use(&receipt).unwrap();
        let loaded = store.load_computer_uses().unwrap();
        assert_eq!(loaded, vec![receipt.clone()]);
        store.remove_computer_use(receipt.call_id).unwrap();
        assert!(store.load_computer_uses().unwrap().is_empty());
    }
}

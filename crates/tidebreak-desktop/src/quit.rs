//! Quitting while agents are working.
//!
//! Quitting used to end every chat and code turn mid-step with no warning:
//! chat turns sat on their leases until the next launch, and code engine
//! children were left running as orphans that boot recovery then fenced. Now a
//! quit request first asks the embedded server how many agents are working.
//! With none, the app quits at once, as it always has. With some, the exit is
//! held and the renderer asks the person what to do:
//!
//! - Quit and stop them: chat turns are cancelled the way the Stop button
//!   cancels them, code turns are interrupted, idle engine children park, and
//!   the app quits.
//! - Quit when they reach a safe point: the update quiesce (decision 0080)
//!   parks every session at its next turn boundary, with no deadline, and the
//!   app quits when the last one gets there. The prompt counts them down, and
//!   the person can still stop them or cancel.
//! - Cancel: nothing changes and the app keeps running.
//!
//! Every quit path arrives here: the Quit menu item and Cmd+Q, the Dock (as
//! `applicationShouldTerminate:` on macOS), and closing the window on Windows
//! and Linux. On macOS the red close button hides the window instead, so agents
//! keep working; the Dock icon and the Window menu bring it back.
//!
//! Three exits never ask. A logout, restart, or shutdown quits at once, so
//! Tidebreak never holds up the system. The restart that installs an update
//! exits through its own quiesce. And a second quit request while the prompt
//! is open raises the same prompt instead of stacking another.
//!
//! An agent parked on an approval, a question, or a plan never reaches a safe
//! point on its own. The prompt says how many are waiting for an answer and
//! offers the inbox, and a quit that waits for a safe point shows its progress
//! without covering the app, so the person can answer.
//!
//! The prompt is renderer UI, but a renderer that crashed or never loaded must
//! not leave a quit that does nothing. The renderer acknowledges each prompt;
//! without that within a few seconds, the shell asks in a native dialog. For
//! the same reason the count, the stop, and the wait run behind guards: a
//! count that panics or hangs lets the quit go ahead, a stop that panics or
//! hangs still quits, and a wait whose quiesce panics releases its hold and
//! asks again.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::FutureExt as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tokio::sync::watch;

use crate::host_access::HostAccess;

/// Raised to the renderer whenever the quit prompt changes. The payload is a
/// [`QuitPromptUpdate`].
const QUIT_PROMPT_EVENT: &str = "desktop-quit-prompt";

/// How long the renderer has to acknowledge a prompt before the shell asks in
/// a native dialog instead.
const RENDERER_ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// How often a quit that waits for a safe point recounts the agents, so the
/// prompt's count follows them down.
const WAITING_RECOUNT_INTERVAL: Duration = Duration::from_secs(1);

/// The most a quit that stops the agents waits for them before it exits
/// anyway, counting the wait for a safe-point wait to let go of the gate. The
/// server bounds its own waits; this guards against a worker that never
/// answers an interrupt.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// The most the quit flow waits for an agent count. A quit request whose
/// count takes longer goes ahead, and a recount while waiting is skipped.
const COUNT_TIMEOUT: Duration = Duration::from_secs(10);

/// Why the prompt asks again when the wait for a safe point broke down.
const WAIT_FAILED: &str = "The wait stopped unexpectedly. Try again in a moment.";

/// How long the Dock's quit waits for the agent count before it answers
/// macOS. Past this the count carries on in the background and the quit is
/// held until it lands.
#[cfg(target_os = "macos")]
const TERMINATE_COUNT_TIMEOUT: Duration = Duration::from_millis(500);

/// The agents a prompt is about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentCount {
    /// Agents not at a safe point.
    agents: usize,
    /// Of those, the ones waiting for the person to answer an approval, a
    /// question, or a plan. They reach a safe point only after the answer.
    waiting_for_you: usize,
}

impl From<tidebreak_server::QuitProgress> for AgentCount {
    fn from(progress: tidebreak_server::QuitProgress) -> Self {
        Self {
            agents: progress.agents,
            waiting_for_you: progress.waiting_for_you.min(progress.agents),
        }
    }
}

/// What the renderer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum QuitPrompt {
    /// Nothing to show.
    Idle,
    /// Ask what to do about the agents that are working.
    Asking(AgentCount),
    /// Quitting once the agents reach a safe point.
    Waiting(AgentCount),
    /// Stopping the agents, then quitting.
    Stopping,
}

/// One version of the prompt. `request` numbers it, so the renderer's
/// acknowledgement says which one it is showing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuitPromptUpdate {
    request: u64,
    prompt: QuitPrompt,
    /// Why waiting for a safe point failed, when it did.
    error: Option<String>,
    /// The person asked to restart, so the app opens again once it quits.
    restart: bool,
}

/// The person's answer to the prompt.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuitChoice {
    Stop,
    SafePoint,
    Cancel,
}

/// Why a wait for a safe point ended before it got there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitEnd {
    /// The person cancelled the quit: turn admission reopens.
    Cancelled,
    /// The person chose to stop the agents instead: the stop takes over.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No quit is under way.
    Idle,
    /// Counting the agents to decide whether to ask.
    Counting,
    Asking(AgentCount),
    Waiting(AgentCount),
    Stopping,
    /// The exit is under way. A further quit request goes straight through.
    Exiting,
}

struct QuitState {
    phase: Phase,
    /// The number of the latest prompt sent to the renderer.
    request: u64,
    /// The latest prompt the renderer said it is showing.
    acknowledged: u64,
    /// A native fallback dialog is open, so another must not stack on it.
    native_dialog_open: bool,
    error: Option<String>,
    /// Ends the wait for a safe point, when one is running.
    wait: Option<watch::Sender<Option<WaitEnd>>>,
    /// The quit under way is a restart the person asked for: the app opens
    /// again once it has exited. Cancelling the quit cancels the restart.
    restart: bool,
}

/// What a quit request does next.
#[derive(Debug, PartialEq, Eq)]
enum RequestAction {
    /// Exit now: an exit is already under way.
    Proceed,
    /// Count the agents, then exit or ask.
    Count,
    /// A prompt is already up. Bring it forward instead of stacking another.
    Resurface(QuitPromptUpdate),
    /// A count is already running and will decide.
    Wait,
}

/// What the agent count decides.
#[derive(Debug, PartialEq, Eq)]
enum CountAction {
    Exit,
    Ask(QuitPromptUpdate),
    /// The quit was settled some other way while the count ran.
    Nothing,
}

/// What the person's choice does next.
#[derive(Debug)]
enum ChoiceAction {
    Nothing,
    /// Tell the renderer; nothing else to do.
    Show(QuitPromptUpdate),
    /// Start waiting for a safe point.
    Wait {
        update: QuitPromptUpdate,
        end: watch::Receiver<Option<WaitEnd>>,
    },
    /// Stop the agents, then exit.
    Stop(QuitPromptUpdate),
}

/// What a wait that reached its end should do.
#[derive(Debug, PartialEq, Eq)]
enum AfterWait {
    Exit,
    /// The quit was cancelled: reopen turn admission.
    Resume,
    /// A stop took over; it owns what happens next.
    Nothing,
}

/// The quit flow's state, managed once per app.
pub(crate) struct QuitController {
    state: Mutex<QuitState>,
    /// Held by whichever wait or stop is driving the server's quiesce, so a
    /// wait that is ending finishes unwinding before the next one begins.
    gate: tokio::sync::Mutex<()>,
}

impl Default for QuitController {
    fn default() -> Self {
        Self {
            state: Mutex::new(QuitState {
                phase: Phase::Idle,
                request: 0,
                acknowledged: 0,
                native_dialog_open: false,
                error: None,
                wait: None,
                restart: false,
            }),
            gate: tokio::sync::Mutex::new(()),
        }
    }
}

impl QuitState {
    fn prompt(&self) -> QuitPrompt {
        match self.phase {
            Phase::Idle | Phase::Counting | Phase::Exiting => QuitPrompt::Idle,
            Phase::Asking(count) => QuitPrompt::Asking(count),
            Phase::Waiting(count) => QuitPrompt::Waiting(count),
            Phase::Stopping => QuitPrompt::Stopping,
        }
    }

    /// Number a new version of the prompt for the renderer.
    fn next_update(&mut self) -> QuitPromptUpdate {
        self.request += 1;
        QuitPromptUpdate {
            request: self.request,
            prompt: self.prompt(),
            error: self.error.clone(),
            restart: self.restart,
        }
    }

    fn end_wait(&mut self, end: WaitEnd) {
        if let Some(wait) = self.wait.take() {
            let _ = wait.send(Some(end));
        }
    }
}

impl QuitController {
    fn lock(&self) -> MutexGuard<'_, QuitState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask to quit. A quit asked for while a restart is under way turns it
    /// into a quit: Cmd+Q during a restart's wait means quit.
    fn begin_request(&self) -> RequestAction {
        self.begin(false)
    }

    /// Ask to restart: a quit that opens the app again once it has exited. It
    /// asks first when agents are working, exactly as a quit does.
    fn begin_restart(&self) -> RequestAction {
        self.begin(true)
    }

    /// The latest request decides whether the exit reopens the app, and the
    /// prompt it resurfaces is worded for it.
    fn begin(&self, restart: bool) -> RequestAction {
        let mut state = self.lock();
        state.restart = restart;
        match state.phase {
            Phase::Exiting => RequestAction::Proceed,
            Phase::Idle => {
                state.phase = Phase::Counting;
                RequestAction::Count
            }
            Phase::Counting => RequestAction::Wait,
            Phase::Asking(_) | Phase::Waiting(_) | Phase::Stopping => {
                RequestAction::Resurface(state.next_update())
            }
        }
    }

    fn counted(&self, count: Result<AgentCount, String>) -> CountAction {
        let mut state = self.lock();
        if state.phase != Phase::Counting {
            return CountAction::Nothing;
        }
        match count {
            Ok(count) if count.agents == 0 => {
                state.phase = Phase::Exiting;
                CountAction::Exit
            }
            Ok(count) => {
                state.phase = Phase::Asking(count);
                state.error = None;
                CountAction::Ask(state.next_update())
            }
            // Not knowing is not a reason to keep the person from quitting.
            Err(error) => {
                tracing::warn!("could not count working agents before quitting: {error}");
                state.phase = Phase::Exiting;
                CountAction::Exit
            }
        }
    }

    fn acknowledge(&self, request: u64) {
        let mut state = self.lock();
        state.acknowledged = state.acknowledged.max(request);
    }

    /// The prompt to ask natively, and whether it is for a restart, when the
    /// renderer has not acknowledged `request` and it is still the prompt that
    /// matters.
    fn native_fallback(&self, request: u64) -> Option<(QuitPrompt, bool)> {
        let mut state = self.lock();
        if state.request != request || state.acknowledged >= request || state.native_dialog_open {
            return None;
        }
        match state.phase {
            Phase::Asking(_) | Phase::Waiting(_) => {
                state.native_dialog_open = true;
                Some((state.prompt(), state.restart))
            }
            _ => None,
        }
    }

    fn native_dialog_closed(&self) {
        self.lock().native_dialog_open = false;
    }

    fn choose(&self, choice: QuitChoice) -> ChoiceAction {
        let mut state = self.lock();
        match (state.phase, choice) {
            (Phase::Asking(_), QuitChoice::Cancel) => {
                state.phase = Phase::Idle;
                state.restart = false;
                ChoiceAction::Show(state.next_update())
            }
            (Phase::Waiting(_), QuitChoice::Cancel) => {
                state.end_wait(WaitEnd::Cancelled);
                state.phase = Phase::Idle;
                state.restart = false;
                ChoiceAction::Show(state.next_update())
            }
            (Phase::Asking(count), QuitChoice::SafePoint) => {
                let (wait, end) = watch::channel(None);
                state.wait = Some(wait);
                state.phase = Phase::Waiting(count);
                state.error = None;
                ChoiceAction::Wait {
                    update: state.next_update(),
                    end,
                }
            }
            (Phase::Asking(_) | Phase::Waiting(_), QuitChoice::Stop) => {
                state.end_wait(WaitEnd::Stopped);
                state.phase = Phase::Stopping;
                state.error = None;
                ChoiceAction::Stop(state.next_update())
            }
            // Already waiting, or past the point of choosing.
            _ => ChoiceAction::Nothing,
        }
    }

    /// A fresh count while waiting, when it changed what the prompt says.
    fn waiting_count(&self, count: AgentCount) -> Option<QuitPromptUpdate> {
        let mut state = self.lock();
        match state.phase {
            Phase::Waiting(shown) if shown != count => {
                state.phase = Phase::Waiting(count);
                Some(state.next_update())
            }
            _ => None,
        }
    }

    fn safe_point_reached(&self) -> AfterWait {
        let mut state = self.lock();
        match state.phase {
            Phase::Waiting(_) => {
                state.wait = None;
                state.phase = Phase::Exiting;
                AfterWait::Exit
            }
            Phase::Stopping | Phase::Exiting => AfterWait::Nothing,
            Phase::Idle | Phase::Counting | Phase::Asking(_) => AfterWait::Resume,
        }
    }

    /// A wait that ended before its safe point: resume unless a stop or an
    /// exit took over.
    fn wait_ended(&self) -> AfterWait {
        match self.lock().phase {
            Phase::Stopping | Phase::Exiting => AfterWait::Nothing,
            _ => AfterWait::Resume,
        }
    }

    /// Waiting for a safe point failed: ask again, with the reason.
    fn safe_point_failed(&self, error: String, count: AgentCount) -> Option<QuitPromptUpdate> {
        let mut state = self.lock();
        if !matches!(state.phase, Phase::Waiting(_)) {
            return None;
        }
        state.wait = None;
        let agents = count.agents.max(1);
        state.phase = Phase::Asking(AgentCount {
            agents,
            waiting_for_you: count.waiting_for_you.min(agents),
        });
        state.error = Some(error);
        Some(state.next_update())
    }

    fn stopped(&self) {
        self.lock().phase = Phase::Exiting;
    }

    fn current(&self) -> QuitPromptUpdate {
        let state = self.lock();
        QuitPromptUpdate {
            request: state.request,
            prompt: state.prompt(),
            error: state.error.clone(),
            restart: state.restart,
        }
    }

    /// Whether the exit under way should open the app again.
    pub(crate) fn restarting(&self) -> bool {
        self.lock().restart
    }
}

/// Whether a quit may go ahead now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuitDecision {
    Proceed,
    /// The shell holds the exit and carries the request on from here.
    Hold,
}

/// What the quit flow asks of the app: agent counts, and the server's
/// quiesce. The app answers through [`HostAccess`]; tests answer with
/// stand-ins that panic or hang.
#[async_trait::async_trait]
trait QuitWork: Sync {
    /// The agents mid-turn, for deciding whether a quit asks first.
    async fn working_agents(&self) -> Result<AgentCount, String>;
    /// The agents a wait for a safe point is still waiting on.
    async fn safe_point_progress(&self) -> Result<AgentCount, String>;
    async fn quiesce_for_quit(&self) -> Result<(), String>;
    async fn stop_for_quit(&self) -> Result<(), String>;
    fn resume_after_cancelled_quit(&self);
}

/// Nothing can be working before setup installs host access, so a quit that
/// early counts none rather than waiting forever.
#[async_trait::async_trait]
impl QuitWork for AppHandle {
    async fn working_agents(&self) -> Result<AgentCount, String> {
        match self.try_state::<HostAccess>() {
            Some(host) => host.working_agents().await.map(AgentCount::from),
            None => Ok(AgentCount::default()),
        }
    }

    async fn safe_point_progress(&self) -> Result<AgentCount, String> {
        match self.try_state::<HostAccess>() {
            Some(host) => host.safe_point_progress().await.map(AgentCount::from),
            None => Ok(AgentCount::default()),
        }
    }

    async fn quiesce_for_quit(&self) -> Result<(), String> {
        match self.try_state::<HostAccess>() {
            Some(host) => host.quiesce_for_quit().await,
            None => Ok(()),
        }
    }

    async fn stop_for_quit(&self) -> Result<(), String> {
        match self.try_state::<HostAccess>() {
            Some(host) => host.stop_for_quit().await,
            None => Ok(()),
        }
    }

    fn resume_after_cancelled_quit(&self) {
        if let Some(host) = self.try_state::<HostAccess>() {
            host.resume_after_cancelled_quit();
        }
    }
}

/// Run `work` so that a panic, or a hang past `limit`, comes back as an error
/// instead of stranding the quit flow in the phase it was in. The panic
/// itself is already in the log, from the process's panic hook.
async fn guarded<T>(
    limit: Duration,
    work: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, AssertUnwindSafe(work).catch_unwind()).await {
        Ok(Ok(result)) => result,
        Ok(Err(_panic)) => Err("it panicked".to_owned()),
        Err(_elapsed) => Err(format!(
            "it did not finish within {} seconds",
            limit.as_secs()
        )),
    }
}

/// Ask to quit: from the Quit menu item, the Dock, or a window close that
/// quits. Returns at once; the exit, the prompt, or nothing follows.
pub(crate) fn request_quit(app: &AppHandle) -> QuitDecision {
    act_on_request(app, app.state::<QuitController>().begin_request())
}

/// Ask to restart, which is a quit that opens the app again once it has
/// exited. macOS applies a Screen Recording grant only to a process started
/// after it, so the permission setup offers this. Working agents get the same
/// prompt a quit gives them.
pub(crate) fn request_restart(app: &AppHandle) -> QuitDecision {
    act_on_request(app, app.state::<QuitController>().begin_restart())
}

fn act_on_request(app: &AppHandle, action: RequestAction) -> QuitDecision {
    match action {
        RequestAction::Proceed => QuitDecision::Proceed,
        RequestAction::Count => {
            spawn_count(app);
            QuitDecision::Hold
        }
        RequestAction::Resurface(update) => {
            present(app, update);
            QuitDecision::Hold
        }
        RequestAction::Wait => QuitDecision::Hold,
    }
}

/// Count the agents for a quit request and settle what that decides. A count
/// that fails, panics, or hangs lets the quit go ahead, so a later request is
/// never left waiting on a count that will not land.
async fn count_for_quit(controller: &QuitController, work: &impl QuitWork) -> CountAction {
    controller.counted(guarded(COUNT_TIMEOUT, work.working_agents()).await)
}

/// Count the agents in the background, then exit or ask.
fn spawn_count(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let action = count_for_quit(&app.state::<QuitController>(), &app).await;
        act_on_count(&app, action);
    });
}

fn act_on_count(app: &AppHandle, action: CountAction) {
    match action {
        CountAction::Exit => app.exit(0),
        CountAction::Ask(update) => present(app, update),
        CountAction::Nothing => {}
    }
}

/// Bring the window forward, show `update`, and fall back to a native dialog
/// if the renderer does not acknowledge it.
fn present(app: &AppHandle, update: QuitPromptUpdate) {
    crate::deep_link::focus_main_window(app);
    let request = update.request;
    emit(app, update);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(RENDERER_ACK_TIMEOUT).await;
        if let Some((prompt, restart)) = app.state::<QuitController>().native_fallback(request) {
            ask_natively(&app, prompt, restart);
        }
    });
}

fn emit(app: &AppHandle, update: QuitPromptUpdate) {
    if let Err(error) = app.emit(QUIT_PROMPT_EVENT, update) {
        eprintln!("tidebreak-desktop: could not raise the quit prompt: {error}");
    }
}

/// Carry out the person's choice.
fn choose(app: &AppHandle, choice: QuitChoice) {
    match app.state::<QuitController>().choose(choice) {
        ChoiceAction::Nothing => {}
        ChoiceAction::Show(update) => emit(app, update),
        ChoiceAction::Wait { update, end } => {
            emit(app, update);
            let app = app.clone();
            tauri::async_runtime::spawn(async move { wait_for_safe_point(app, end).await });
        }
        ChoiceAction::Stop(update) => {
            emit(app, update);
            let app = app.clone();
            tauri::async_runtime::spawn(async move { stop_and_exit(app).await });
        }
    }
}

/// How a wait for a safe point ended, for the app to carry out.
#[derive(Debug, PartialEq, Eq)]
enum WaitOutcome {
    /// Every agent reached a safe point: quit.
    Exit,
    /// The wait failed, and the prompt asks again with the reason.
    AskAgain(QuitPromptUpdate),
    /// Cancelled, or a stop took over: nothing is left for the wait to do.
    Done,
}

/// Park every session at a safe point, then quit. Ends early when the person
/// cancels or chooses to stop the agents instead.
async fn wait_for_safe_point(app: AppHandle, end: watch::Receiver<Option<WaitEnd>>) {
    let outcome = wait_until_safe(&app.state::<QuitController>(), &app, end, |update| {
        emit(&app, update);
    })
    .await;
    match outcome {
        WaitOutcome::Exit => app.exit(0),
        WaitOutcome::AskAgain(update) => present(&app, update),
        WaitOutcome::Done => {}
    }
}

/// Wait for every agent to reach a safe point, recounting as they go and
/// passing each new count to `show`. Whatever ends the wait, the quit's hold
/// is released here, under the gate, unless the exit or a stop takes it
/// over.
async fn wait_until_safe(
    controller: &QuitController,
    work: &impl QuitWork,
    mut end: watch::Receiver<Option<WaitEnd>>,
    mut show: impl FnMut(QuitPromptUpdate),
) -> WaitOutcome {
    let _gate = controller.gate.lock().await;
    if end.borrow().is_some() {
        // Ended before it began: the quiesce never started, so there is
        // nothing to release.
        return WaitOutcome::Done;
    }
    let reached = {
        // A quiesce that panicked would keep its hold, and no turn could
        // start again until the app quit.
        let quiesce = AssertUnwindSafe(work.quiesce_for_quit()).catch_unwind();
        tokio::pin!(quiesce);
        let mut recount = tokio::time::interval(WAITING_RECOUNT_INTERVAL);
        recount.tick().await;
        loop {
            tokio::select! {
                result = &mut quiesce => {
                    break Some(result.unwrap_or_else(|_| Err(WAIT_FAILED.to_owned())));
                }
                changed = end.changed() => {
                    if changed.is_err() || end.borrow().is_some() {
                        break None;
                    }
                }
                _ = recount.tick() => {
                    // A slow count must not hold up a cancel or a stop.
                    tokio::select! {
                        count = guarded(COUNT_TIMEOUT, work.safe_point_progress()) => {
                            if let Some(update) =
                                count.ok().and_then(|count| controller.waiting_count(count))
                            {
                                show(update);
                            }
                        }
                        changed = end.changed() => {
                            if changed.is_err() || end.borrow().is_some() {
                                break None;
                            }
                        }
                    }
                }
            }
        }
    };
    match reached {
        Some(Ok(())) => match controller.safe_point_reached() {
            AfterWait::Exit => WaitOutcome::Exit,
            AfterWait::Resume => {
                work.resume_after_cancelled_quit();
                WaitOutcome::Done
            }
            AfterWait::Nothing => WaitOutcome::Done,
        },
        Some(Err(error)) => {
            // A quiesce that failed released its own hold, but one that
            // panicked could not. Releasing twice is harmless.
            work.resume_after_cancelled_quit();
            let count = guarded(COUNT_TIMEOUT, work.working_agents())
                .await
                .unwrap_or(AgentCount {
                    agents: 1,
                    waiting_for_you: 0,
                });
            controller
                .safe_point_failed(error, count)
                .map_or(WaitOutcome::Done, WaitOutcome::AskAgain)
        }
        None => {
            if controller.wait_ended() == AfterWait::Resume {
                work.resume_after_cancelled_quit();
            }
            WaitOutcome::Done
        }
    }
}

/// Stop every turn in flight, then quit.
async fn stop_and_exit(app: AppHandle) {
    stop_agents(&app.state::<QuitController>(), &app).await;
    app.exit(0);
}

/// Stop the agents for a quit. The wait for the gate, which a wait for a safe
/// point may still hold while it unwinds, counts against [`STOP_TIMEOUT`]. A
/// stop that fails, panics, or runs out of time is logged, and the quit goes
/// ahead all the same.
async fn stop_agents(controller: &QuitController, work: &impl QuitWork) {
    let stopped = guarded(STOP_TIMEOUT, async {
        let _gate = controller.gate.lock().await;
        work.stop_for_quit().await
    })
    .await;
    if let Err(error) = stopped {
        tracing::warn!("quitting before every agent stopped: {error}");
    }
    controller.stopped();
}

/// The native fallback dialog: its title, its message, and its three button
/// labels.
#[derive(Debug, PartialEq, Eq)]
struct NativeQuestion {
    title: &'static str,
    message: String,
    stop: &'static str,
    safe_point: &'static str,
}

const NATIVE_CANCEL: &str = "Cancel";

/// The words of the native fallback dialog, which follow the renderer's,
/// including its words for a restart.
fn native_question(prompt: QuitPrompt, restart: bool) -> Option<NativeQuestion> {
    let (count, waiting) = match prompt {
        QuitPrompt::Asking(count) => (count, false),
        QuitPrompt::Waiting(count) => (count, true),
        QuitPrompt::Idle | QuitPrompt::Stopping => return None,
    };
    let one = count.agents == 1;
    let working = if one {
        "An agent is working".to_owned()
    } else {
        format!("{} agents are working", count.agents)
    };
    let (stop, reach) = match (restart, one) {
        (false, true) => ("Quit and stop it", "Quit when it reaches a safe point"),
        (false, false) => ("Quit and stop them", "Quit when they reach a safe point"),
        (true, true) => (
            "Restart and stop it",
            "Restart when it reaches a safe point",
        ),
        (true, false) => (
            "Restart and stop them",
            "Restart when they reach a safe point",
        ),
    };
    let (now, later, then) = if restart {
        ("Restarting", "restart", "restarts")
    } else {
        ("Quitting", "quit", "quits")
    };
    let mut message = match (waiting, one) {
        (false, true) => format!(
            "{working}. {now} now stops it. To keep its work, {later} when it reaches a safe point."
        ),
        (false, false) => format!(
            "{working}. {now} now stops them. To keep their work, {later} when they reach a safe point."
        ),
        (true, true) => format!("{working}. Tidebreak {then} when it reaches a safe point."),
        (true, false) => format!("{working}. Tidebreak {then} when they reach a safe point."),
    };
    if let Some(note) = waiting_for_you_note(count) {
        message.push(' ');
        message.push_str(&note);
        message.push_str(" Open the inbox to answer.");
    }
    Some(NativeQuestion {
        title: if restart {
            "Restart Tidebreak?"
        } else {
            "Quit Tidebreak?"
        },
        message,
        stop,
        safe_point: if waiting { "Keep waiting" } else { reach },
    })
}

/// The sentence about agents parked on the person's answer, as the renderer
/// words it. `None` when no agent is waiting for one.
fn waiting_for_you_note(count: AgentCount) -> Option<String> {
    let note = match (count.agents, count.waiting_for_you) {
        (_, 0) => return None,
        (1, _) => "It needs your answer before it can reach a safe point.".to_owned(),
        (_, 1) => "One of them needs your answer before it can reach a safe point.".to_owned(),
        (agents, waiting) if waiting >= agents => {
            "All of them need your answers before they can reach a safe point.".to_owned()
        }
        (_, waiting) => {
            format!("{waiting} of them need your answers before they can reach a safe point.")
        }
    };
    Some(note)
}

/// Ask in a native dialog because the renderer did not answer.
fn ask_natively(app: &AppHandle, prompt: QuitPrompt, restart: bool) {
    let Some(question) = native_question(prompt, restart) else {
        app.state::<QuitController>().native_dialog_closed();
        return;
    };
    crate::deep_link::focus_main_window(app);
    let mut dialog = app
        .dialog()
        .message(question.message.clone())
        .title(question.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            question.stop.to_owned(),
            question.safe_point.to_owned(),
            NATIVE_CANCEL.to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    let app = app.clone();
    dialog.show_with_result(move |result| {
        app.state::<QuitController>().native_dialog_closed();
        choose(&app, native_choice(&result, &question));
    });
}

fn native_choice(result: &MessageDialogResult, question: &NativeQuestion) -> QuitChoice {
    match result {
        MessageDialogResult::Yes => QuitChoice::Stop,
        MessageDialogResult::No => QuitChoice::SafePoint,
        MessageDialogResult::Custom(label) if label == question.stop => QuitChoice::Stop,
        MessageDialogResult::Custom(label) if label == question.safe_point => QuitChoice::SafePoint,
        _ => QuitChoice::Cancel,
    }
}

/// The prompt as it stands, for a renderer that just mounted. Reading it
/// counts as showing it.
#[tauri::command]
pub(crate) fn quit_prompt_state(controller: State<'_, QuitController>) -> QuitPromptUpdate {
    let current = controller.current();
    controller.acknowledge(current.request);
    current
}

/// The renderer is showing prompt `request`, so no native dialog is needed.
#[tauri::command]
pub(crate) fn quit_prompt_opened(controller: State<'_, QuitController>, request: u64) {
    controller.acknowledge(request);
}

/// The person answered the prompt.
#[tauri::command]
pub(crate) fn answer_quit_prompt(app: AppHandle, choice: QuitChoice) {
    choose(&app, choice);
}

/// Restart Tidebreak so macOS applies a permission it granted since launch.
/// Working agents get the quit prompt, worded for a restart.
#[tauri::command]
pub(crate) fn restart_app(app: AppHandle, webview: tauri::Webview) -> Result<(), String> {
    if webview.label() != "main" {
        return Err("Tidebreak can be restarted only from its own window.".to_owned());
    }
    request_restart(&app);
    Ok(())
}

/// Closing the main window. On macOS the window hides and the app keeps
/// running, like any Mac app. Elsewhere the app has no other way to stay
/// open, so closing the window is a quit request.
pub(crate) fn on_close_requested(app: &AppHandle, label: &str, api: &tauri::CloseRequestApi) {
    if label != "main" {
        return;
    }
    api.prevent_close();
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_window("main") {
        if let Err(error) = window.hide() {
            eprintln!("tidebreak-desktop: could not hide the window: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    request_quit(app);
}

#[cfg(target_os = "macos")]
pub(crate) use terminate::install_terminate_hook;

/// The Dock's Quit, a logout, and anything else that sends `terminate:` reach
/// the app delegate's `applicationShouldTerminate:`. Tao's delegate does not
/// answer it, so AppKit quits at once and no Tauri event can hold the exit.
/// This adds the method to the delegate's class.
#[cfg(target_os = "macos")]
mod terminate {
    use std::sync::OnceLock;

    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use tauri::{AppHandle, Manager};

    use super::{
        QuitController, QuitDecision, QuitWork as _, RequestAction, COUNT_TIMEOUT,
        TERMINATE_COUNT_TIMEOUT,
    };

    static APP: OnceLock<AppHandle> = OnceLock::new();

    const NS_TERMINATE_CANCEL: usize = 0;
    const NS_TERMINATE_NOW: usize = 1;

    const fn four_char_code(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    const K_CORE_EVENT_CLASS: u32 = four_char_code(b"aevt");
    const K_AE_QUIT_APPLICATION: u32 = four_char_code(b"quit");
    const K_AE_QUIT_REASON: u32 = four_char_code(b"why?");

    /// The quit reasons the system gives when the session is ending: a
    /// logout, a restart, or a shutdown, with or without its confirmation.
    const SESSION_ENDING_REASONS: [u32; 6] = [
        four_char_code(b"logo"), // kAELogOut
        four_char_code(b"rlgo"), // kAEReallyLogOut
        four_char_code(b"rrst"), // kAEShowRestartDialog
        four_char_code(b"rsdn"), // kAEShowShutdownDialog
        four_char_code(b"rest"), // kAERestart
        four_char_code(b"shut"), // kAEShutDown
    ];

    pub(super) fn is_session_ending_reason(reason: u32) -> bool {
        SESSION_ENDING_REASONS.contains(&reason)
    }

    /// Answer `applicationShouldTerminate:` for the app delegate from here on.
    pub(crate) fn install_terminate_hook(app: &AppHandle) {
        let _ = APP.set(app.clone());
        // SAFETY: called on the main thread during setup, where the shared
        // application and its delegate exist. The added method matches the
        // selector's signature: it takes the receiver, the selector, and one
        // object argument, and returns an `NSApplicationTerminateReply`
        // (`NSUInteger`, encoded `Q`).
        unsafe {
            let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = msg_send![application, delegate];
            if delegate.is_null() {
                eprintln!(
                    "tidebreak-desktop: no app delegate; quitting from the Dock will not ask first"
                );
                return;
            }
            let delegate_class = objc2::ffi::object_getClass(delegate) as *mut AnyClass;
            let answer: extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize =
                should_terminate;
            let added = objc2::ffi::class_addMethod(
                delegate_class,
                sel!(applicationShouldTerminate:),
                std::mem::transmute::<
                    extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
                    Imp,
                >(answer),
                c"Q@:@".as_ptr(),
            );
            if !added.as_bool() {
                eprintln!(
                    "tidebreak-desktop: the app delegate already answers applicationShouldTerminate:; quitting from the Dock will not ask first"
                );
            }
        }
    }

    extern "C-unwind" fn should_terminate(
        _delegate: *mut AnyObject,
        _selector: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        // A panic must not unwind into AppKit. Failing open lets the quit go.
        match std::panic::catch_unwind(decide) {
            Ok(QuitDecision::Hold) => NS_TERMINATE_CANCEL,
            Ok(QuitDecision::Proceed) | Err(_) => NS_TERMINATE_NOW,
        }
    }

    fn decide() -> QuitDecision {
        let Some(app) = APP.get() else {
            return QuitDecision::Proceed;
        };
        // A logout, restart, or shutdown never waits on Tidebreak.
        if session_is_ending() {
            return QuitDecision::Proceed;
        }
        let controller = app.state::<QuitController>();
        match controller.begin_request() {
            RequestAction::Proceed => QuitDecision::Proceed,
            RequestAction::Count => {
                // Answer macOS directly when the count is quick, so a quit
                // with nothing running is the ordinary immediate quit. A
                // count that panics comes back as an error, which lets the
                // quit go.
                let counted = tauri::async_runtime::block_on(tokio::time::timeout(
                    TERMINATE_COUNT_TIMEOUT,
                    super::guarded(COUNT_TIMEOUT, app.working_agents()),
                ));
                match counted {
                    Ok(count) => match controller.counted(count) {
                        super::CountAction::Exit => QuitDecision::Proceed,
                        super::CountAction::Ask(update) => {
                            super::present(app, update);
                            QuitDecision::Hold
                        }
                        super::CountAction::Nothing => QuitDecision::Hold,
                    },
                    // Past the timeout the count carries on in the
                    // background, and the quit is held until it lands.
                    Err(_) => {
                        super::spawn_count(app);
                        QuitDecision::Hold
                    }
                }
            }
            RequestAction::Resurface(update) => {
                super::present(app, update);
                QuitDecision::Hold
            }
            RequestAction::Wait => QuitDecision::Hold,
        }
    }

    /// Whether the Apple event being handled is the system's quit for a
    /// logout, restart, or shutdown.
    fn session_is_ending() -> bool {
        // SAFETY: called on the main thread inside `applicationShouldTerminate:`,
        // where the current Apple event, if any, stays valid. Every message is
        // sent to an object checked for nil first.
        unsafe {
            let manager: *mut AnyObject =
                msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
            if manager.is_null() {
                return false;
            }
            let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
            if event.is_null() {
                return false;
            }
            let event_class: u32 = msg_send![event, eventClass];
            let event_id: u32 = msg_send![event, eventID];
            if event_class != K_CORE_EVENT_CLASS || event_id != K_AE_QUIT_APPLICATION {
                return false;
            }
            let reason: *mut AnyObject =
                msg_send![event, attributeDescriptorForKeyword: K_AE_QUIT_REASON];
            if reason.is_null() {
                return false;
            }
            let mut code: u32 = msg_send![reason, typeCodeValue];
            if code == 0 {
                code = msg_send![reason, enumCodeValue];
            }
            is_session_ending_reason(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn agents(agents: usize) -> AgentCount {
        AgentCount {
            agents,
            waiting_for_you: 0,
        }
    }

    fn asking(controller: &QuitController, count: usize) -> QuitPromptUpdate {
        assert_eq!(controller.begin_request(), RequestAction::Count);
        match controller.counted(Ok(agents(count))) {
            CountAction::Ask(update) => update,
            other => panic!("expected a prompt, got {other:?}"),
        }
    }

    fn waiting(controller: &QuitController) -> watch::Receiver<Option<WaitEnd>> {
        match controller.choose(QuitChoice::SafePoint) {
            ChoiceAction::Wait { end, .. } => end,
            other => panic!("expected a wait, got {other:?}"),
        }
    }

    /// How a stand-in for the app answers one of the quit flow's calls.
    #[derive(Clone, Copy)]
    enum Act {
        Answer,
        Panic,
        Hang,
    }

    async fn act<T>(act: Act, value: T) -> Result<T, String> {
        match act {
            Act::Answer => Ok(value),
            Act::Panic => panic!("injected failure"),
            Act::Hang => std::future::pending().await,
        }
    }

    /// The app, as the quit flow sees it, with parts that panic or hang.
    struct FakeWork {
        count: Act,
        working: AgentCount,
        short_of_a_safe_point: AgentCount,
        quiesce: Act,
        stop: Act,
        resumed: AtomicUsize,
    }

    impl Default for FakeWork {
        fn default() -> Self {
            Self {
                count: Act::Answer,
                working: agents(2),
                short_of_a_safe_point: agents(2),
                quiesce: Act::Answer,
                stop: Act::Answer,
                resumed: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl QuitWork for FakeWork {
        async fn working_agents(&self) -> Result<AgentCount, String> {
            act(self.count, self.working).await
        }

        async fn safe_point_progress(&self) -> Result<AgentCount, String> {
            act(self.count, self.short_of_a_safe_point).await
        }

        async fn quiesce_for_quit(&self) -> Result<(), String> {
            act(self.quiesce, ()).await
        }

        async fn stop_for_quit(&self) -> Result<(), String> {
            act(self.stop, ()).await
        }

        fn resume_after_cancelled_quit(&self) {
            self.resumed.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// With nothing running, a quit is the immediate quit it always was, and
    /// a second request during the exit goes straight through.
    #[test]
    fn nothing_running_quits_at_once() {
        let controller = QuitController::default();
        assert_eq!(controller.begin_request(), RequestAction::Count);
        assert_eq!(
            controller.counted(Ok(AgentCount::default())),
            CountAction::Exit
        );
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    /// A failed count does not keep the person from quitting.
    #[test]
    fn a_failed_count_quits() {
        let controller = QuitController::default();
        assert_eq!(controller.begin_request(), RequestAction::Count);
        assert_eq!(
            controller.counted(Err("store unavailable".to_owned())),
            CountAction::Exit
        );
    }

    /// A count that panicked used to leave the flow counting for good, so
    /// every later Cmd+Q, Dock quit, and window close did nothing.
    #[tokio::test]
    async fn a_count_that_panics_lets_the_quit_go_ahead() {
        let controller = QuitController::default();
        let work = FakeWork {
            count: Act::Panic,
            ..FakeWork::default()
        };
        assert_eq!(controller.begin_request(), RequestAction::Count);
        assert_eq!(count_for_quit(&controller, &work).await, CountAction::Exit);
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_count_that_hangs_lets_the_quit_go_ahead() {
        let controller = QuitController::default();
        let work = FakeWork {
            count: Act::Hang,
            ..FakeWork::default()
        };
        assert_eq!(controller.begin_request(), RequestAction::Count);
        let started = tokio::time::Instant::now();
        assert_eq!(count_for_quit(&controller, &work).await, CountAction::Exit);
        assert_eq!(started.elapsed(), COUNT_TIMEOUT);
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    /// A stop that never finishes still quits, at the timeout.
    #[tokio::test(start_paused = true)]
    async fn a_stop_that_hangs_still_quits() {
        let controller = QuitController::default();
        asking(&controller, 2);
        assert!(matches!(
            controller.choose(QuitChoice::Stop),
            ChoiceAction::Stop(_)
        ));
        let work = FakeWork {
            stop: Act::Hang,
            ..FakeWork::default()
        };
        let started = tokio::time::Instant::now();
        stop_agents(&controller, &work).await;
        assert_eq!(started.elapsed(), STOP_TIMEOUT);
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    /// The wait for the gate counts against the stop's timeout, so a wait
    /// for a safe point that never lets go cannot hold up the quit.
    #[tokio::test(start_paused = true)]
    async fn a_stop_behind_a_gate_that_is_never_released_still_quits() {
        let controller = QuitController::default();
        asking(&controller, 2);
        assert!(matches!(
            controller.choose(QuitChoice::Stop),
            ChoiceAction::Stop(_)
        ));
        let _stuck_wait = controller.gate.lock().await;
        let started = tokio::time::Instant::now();
        stop_agents(&controller, &FakeWork::default()).await;
        assert_eq!(started.elapsed(), STOP_TIMEOUT);
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    /// A stop that panicked used to leave "Stopping agents" up with no
    /// buttons and no exit.
    #[tokio::test]
    async fn a_stop_that_panics_still_quits() {
        let controller = QuitController::default();
        asking(&controller, 1);
        assert!(matches!(
            controller.choose(QuitChoice::Stop),
            ChoiceAction::Stop(_)
        ));
        let work = FakeWork {
            stop: Act::Panic,
            ..FakeWork::default()
        };
        stop_agents(&controller, &work).await;
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    /// A quiesce that panics would keep the quit's hold, and no turn could
    /// start again. The wait releases the hold and asks again.
    #[tokio::test]
    async fn a_wait_whose_quiesce_panics_releases_its_hold_and_asks_again() {
        let controller = QuitController::default();
        asking(&controller, 1);
        let end = waiting(&controller);
        let work = FakeWork {
            quiesce: Act::Panic,
            working: agents(1),
            ..FakeWork::default()
        };
        match wait_until_safe(&controller, &work, end, |_| {}).await {
            WaitOutcome::AskAgain(update) => {
                assert_eq!(update.prompt, QuitPrompt::Asking(agents(1)));
                assert_eq!(update.error.as_deref(), Some(WAIT_FAILED));
            }
            other => panic!("expected the prompt again, got {other:?}"),
        }
        assert_eq!(work.resumed.load(Ordering::SeqCst), 1);
    }

    /// The count while waiting is what the wait waits on, parking engine
    /// children included, so it reaches zero when the wait ends.
    #[tokio::test(start_paused = true)]
    async fn the_wait_counts_what_it_waits_on() {
        let controller = QuitController::default();
        asking(&controller, 2);
        let end = waiting(&controller);
        let work = FakeWork {
            quiesce: Act::Hang,
            short_of_a_safe_point: AgentCount {
                agents: 3,
                waiting_for_you: 1,
            },
            ..FakeWork::default()
        };
        let mut shown = Vec::new();
        let wait = wait_until_safe(&controller, &work, end, |update| shown.push(update.prompt));
        assert!(tokio::time::timeout(Duration::from_millis(1_500), wait)
            .await
            .is_err());
        assert_eq!(
            shown,
            [QuitPrompt::Waiting(AgentCount {
                agents: 3,
                waiting_for_you: 1,
            })]
        );
    }

    /// A recount that hangs does not hold up a cancel: the quit's hold is
    /// released as soon as the person cancels.
    #[tokio::test(start_paused = true)]
    async fn a_slow_recount_does_not_hold_up_a_cancel() {
        let controller = QuitController::default();
        asking(&controller, 2);
        let end = waiting(&controller);
        let work = FakeWork {
            count: Act::Hang,
            quiesce: Act::Hang,
            ..FakeWork::default()
        };
        let started = tokio::time::Instant::now();
        let cancel = async {
            // The first recount starts at one second and never finishes.
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            assert!(matches!(
                controller.choose(QuitChoice::Cancel),
                ChoiceAction::Show(_)
            ));
        };
        let (outcome, ()) = tokio::join!(wait_until_safe(&controller, &work, end, |_| {}), cancel);
        assert_eq!(outcome, WaitOutcome::Done);
        assert_eq!(started.elapsed(), Duration::from_millis(1_500));
        assert_eq!(work.resumed.load(Ordering::SeqCst), 1);
    }

    /// Agents working: the prompt names how many. A second quit request while
    /// it is open raises the same prompt instead of stacking another, and one
    /// that arrives mid-count waits for the count.
    #[test]
    fn a_second_quit_request_does_not_stack_prompts() {
        let controller = QuitController::default();
        assert_eq!(controller.begin_request(), RequestAction::Count);
        assert_eq!(controller.begin_request(), RequestAction::Wait);
        let update = match controller.counted(Ok(agents(2))) {
            CountAction::Ask(update) => update,
            other => panic!("expected a prompt, got {other:?}"),
        };
        assert_eq!(update.prompt, QuitPrompt::Asking(agents(2)));

        match controller.begin_request() {
            RequestAction::Resurface(again) => {
                assert_eq!(again.prompt, QuitPrompt::Asking(agents(2)));
                assert!(again.request > update.request);
            }
            other => panic!("expected the same prompt again, got {other:?}"),
        }
        assert_eq!(controller.current().prompt, QuitPrompt::Asking(agents(2)));
    }

    #[test]
    fn cancel_closes_the_prompt_and_keeps_the_app_running() {
        let controller = QuitController::default();
        asking(&controller, 1);
        match controller.choose(QuitChoice::Cancel) {
            ChoiceAction::Show(update) => assert_eq!(update.prompt, QuitPrompt::Idle),
            other => panic!("expected the prompt to close, got {other:?}"),
        }
        // The next quit asks afresh.
        assert_eq!(controller.begin_request(), RequestAction::Count);
    }

    /// Waiting for a safe point counts the agents down, exits once they park,
    /// and a cancelled wait tells its waiter to reopen turn admission.
    #[test]
    fn a_safe_point_wait_counts_down_and_exits() {
        let controller = QuitController::default();
        asking(&controller, 3);
        let end = match controller.choose(QuitChoice::SafePoint) {
            ChoiceAction::Wait { update, end } => {
                assert_eq!(update.prompt, QuitPrompt::Waiting(agents(3)));
                end
            }
            other => panic!("expected a wait, got {other:?}"),
        };
        assert!(controller.waiting_count(agents(3)).is_none());
        assert_eq!(
            controller
                .waiting_count(agents(1))
                .map(|update| update.prompt),
            Some(QuitPrompt::Waiting(agents(1)))
        );
        assert_eq!(controller.safe_point_reached(), AfterWait::Exit);
        assert!(
            end.borrow().is_none(),
            "reaching the safe point is no cancel"
        );
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    #[test]
    fn cancelling_a_wait_reopens_admission() {
        let controller = QuitController::default();
        asking(&controller, 2);
        let end = waiting(&controller);
        match controller.choose(QuitChoice::Cancel) {
            ChoiceAction::Show(update) => assert_eq!(update.prompt, QuitPrompt::Idle),
            other => panic!("expected the prompt to close, got {other:?}"),
        }
        assert_eq!(*end.borrow(), Some(WaitEnd::Cancelled));
        assert_eq!(controller.wait_ended(), AfterWait::Resume);
        // A quiesce that finished in the same instant also reopens admission.
        assert_eq!(controller.safe_point_reached(), AfterWait::Resume);
    }

    /// Choosing to stop while waiting ends the wait without reopening
    /// admission: the stop owns the quiesce from there.
    #[test]
    fn stopping_while_waiting_hands_over_to_the_stop() {
        let controller = QuitController::default();
        asking(&controller, 2);
        let end = waiting(&controller);
        match controller.choose(QuitChoice::Stop) {
            ChoiceAction::Stop(update) => assert_eq!(update.prompt, QuitPrompt::Stopping),
            other => panic!("expected a stop, got {other:?}"),
        }
        assert_eq!(*end.borrow(), Some(WaitEnd::Stopped));
        assert_eq!(controller.wait_ended(), AfterWait::Nothing);
        // Nothing can be chosen once the stop runs.
        assert!(matches!(
            controller.choose(QuitChoice::Cancel),
            ChoiceAction::Nothing
        ));
        controller.stopped();
        assert_eq!(controller.begin_request(), RequestAction::Proceed);
    }

    #[test]
    fn a_failed_wait_asks_again_with_the_reason() {
        let controller = QuitController::default();
        asking(&controller, 1);
        waiting(&controller);
        let update = controller
            .safe_point_failed(
                "Chat turns could not be released in time.".to_owned(),
                agents(1),
            )
            .expect("the prompt asks again");
        assert_eq!(update.prompt, QuitPrompt::Asking(agents(1)));
        assert_eq!(
            update.error.as_deref(),
            Some("Chat turns could not be released in time.")
        );
    }

    /// The native dialog appears only for the latest prompt, only when the
    /// renderer did not acknowledge it, and never twice at once.
    #[test]
    fn the_native_dialog_is_a_fallback_for_an_unanswered_prompt() {
        let controller = QuitController::default();
        let update = asking(&controller, 2);
        controller.acknowledge(update.request);
        assert_eq!(controller.native_fallback(update.request), None);

        let again = match controller.begin_request() {
            RequestAction::Resurface(again) => again,
            other => panic!("expected the prompt again, got {other:?}"),
        };
        assert_eq!(controller.native_fallback(update.request), None, "stale");
        assert_eq!(
            controller.native_fallback(again.request),
            Some((QuitPrompt::Asking(agents(2)), false))
        );
        assert_eq!(
            controller.native_fallback(again.request),
            None,
            "one at a time"
        );
        controller.native_dialog_closed();
    }

    #[test]
    fn the_native_dialog_maps_every_answer_to_a_choice() {
        let asking = native_question(QuitPrompt::Asking(agents(2)), false).unwrap();
        assert_eq!(
            native_choice(
                &MessageDialogResult::Custom("Quit and stop them".to_owned()),
                &asking
            ),
            QuitChoice::Stop
        );
        assert_eq!(
            native_choice(
                &MessageDialogResult::Custom("Quit when they reach a safe point".to_owned()),
                &asking
            ),
            QuitChoice::SafePoint
        );
        let waiting = native_question(QuitPrompt::Waiting(agents(2)), false).unwrap();
        assert_eq!(
            native_choice(
                &MessageDialogResult::Custom("Keep waiting".to_owned()),
                &waiting
            ),
            QuitChoice::SafePoint
        );
        assert_eq!(
            native_choice(&MessageDialogResult::Yes, &asking),
            QuitChoice::Stop
        );
        assert_eq!(
            native_choice(&MessageDialogResult::No, &asking),
            QuitChoice::SafePoint
        );
        assert_eq!(
            native_choice(&MessageDialogResult::Cancel, &asking),
            QuitChoice::Cancel
        );
        assert_eq!(native_question(QuitPrompt::Stopping, false), None);
    }

    /// The fallback says the same thing as the renderer, in the singular for
    /// one agent.
    #[test]
    fn the_native_dialog_speaks_of_one_agent_in_the_singular() {
        assert_eq!(
            native_question(QuitPrompt::Asking(agents(1)), false),
            Some(NativeQuestion {
                title: "Quit Tidebreak?",
                message: "An agent is working. Quitting now stops it. To keep its work, quit when it reaches a safe point."
                    .to_owned(),
                stop: "Quit and stop it",
                safe_point: "Quit when it reaches a safe point",
            })
        );
        assert_eq!(
            native_question(QuitPrompt::Waiting(agents(3)), false).map(|question| question.message),
            Some("3 agents are working. Tidebreak quits when they reach a safe point.".to_owned())
        );
    }

    /// An agent parked on an approval never reaches a safe point on its own,
    /// so the fallback says so and where the question waits.
    #[test]
    fn the_native_dialog_says_an_agent_is_waiting_for_an_answer() {
        assert_eq!(
            native_question(
                QuitPrompt::Asking(AgentCount {
                    agents: 1,
                    waiting_for_you: 1,
                }),
                false
            )
            .map(|question| question.message),
            Some(
                "An agent is working. Quitting now stops it. To keep its work, quit when it reaches a safe point. \
                 It needs your answer before it can reach a safe point. Open the inbox to answer."
                    .to_owned()
            )
        );
        assert_eq!(
            native_question(
                QuitPrompt::Waiting(AgentCount {
                    agents: 3,
                    waiting_for_you: 1,
                }),
                false
            )
            .map(|question| question.message),
            Some(
                "3 agents are working. Tidebreak quits when they reach a safe point. \
                 One of them needs your answer before it can reach a safe point. Open the inbox to answer."
                    .to_owned()
            )
        );
    }

    /// A restart asks about working agents the way a quit does, says it is a
    /// restart, and a cancel takes the restart back with it.
    #[test]
    fn a_restart_is_a_quit_that_remembers_to_open_again() {
        let controller = QuitController::default();
        assert!(!controller.restarting());

        assert_eq!(controller.begin_restart(), RequestAction::Count);
        let CountAction::Ask(update) = controller.counted(Ok(agents(2))) else {
            panic!("working agents are asked about");
        };
        assert!(update.restart, "the prompt is worded for a restart");
        assert!(controller.restarting());

        let ChoiceAction::Show(update) = controller.choose(QuitChoice::Cancel) else {
            panic!("a cancel goes back to idle");
        };
        assert!(!update.restart);
        assert!(
            !controller.restarting(),
            "a later quit must not reopen the app"
        );

        // With nothing working, the restart exits at once and reopens.
        assert_eq!(controller.begin_restart(), RequestAction::Count);
        assert_eq!(controller.counted(Ok(agents(0))), CountAction::Exit);
        assert!(controller.restarting());
    }

    /// Cmd+Q while a restart waits for a safe point means quit: the prompt
    /// it resurfaces says quit, and the exit does not reopen the app. A
    /// restart asked for during a quit's wait turns it back into a restart.
    #[test]
    fn a_quit_during_a_restart_quits() {
        let controller = QuitController::default();
        assert_eq!(controller.begin_restart(), RequestAction::Count);
        let CountAction::Ask(update) = controller.counted(Ok(agents(1))) else {
            panic!("a working agent is asked about");
        };
        assert!(update.restart);
        let _end = waiting(&controller);

        let RequestAction::Resurface(update) = controller.begin_request() else {
            panic!("a quit during the wait brings the prompt back");
        };
        assert!(!update.restart, "the prompt now says quit");
        assert!(!controller.restarting(), "the exit must not reopen the app");
        assert_eq!(controller.safe_point_reached(), AfterWait::Exit);
        assert!(!controller.restarting());

        let controller = QuitController::default();
        let _ = asking(&controller, 1);
        let RequestAction::Resurface(update) = controller.begin_restart() else {
            panic!("a restart during a quit's prompt brings it back");
        };
        assert!(update.restart);
        assert!(controller.restarting());
    }

    /// The native fallback words a restart as a restart, title included.
    #[test]
    fn the_native_dialog_says_restart_for_a_restart() {
        assert_eq!(
            native_question(QuitPrompt::Asking(agents(2)), true),
            Some(NativeQuestion {
                title: "Restart Tidebreak?",
                message: "2 agents are working. Restarting now stops them. To keep their work, restart when they reach a safe point."
                    .to_owned(),
                stop: "Restart and stop them",
                safe_point: "Restart when they reach a safe point",
            })
        );
        assert_eq!(
            native_question(QuitPrompt::Waiting(agents(1)), true).map(|question| question.message),
            Some(
                "An agent is working. Tidebreak restarts when it reaches a safe point.".to_owned()
            )
        );
        let controller = QuitController::default();
        assert_eq!(controller.begin_restart(), RequestAction::Count);
        let CountAction::Ask(update) = controller.counted(Ok(agents(2))) else {
            panic!("working agents are asked about");
        };
        assert_eq!(
            controller.native_fallback(update.request),
            Some((QuitPrompt::Asking(agents(2)), true))
        );
    }

    /// A plain quit never reopens the app.
    #[test]
    fn a_quit_does_not_restart() {
        let controller = QuitController::default();
        assert_eq!(controller.begin_request(), RequestAction::Count);
        assert_eq!(controller.counted(Ok(agents(0))), CountAction::Exit);
        assert!(!controller.restarting());
    }

    /// The renderer reads the prompt as tagged JSON.
    #[test]
    fn the_prompt_serializes_for_the_renderer() {
        let update = QuitPromptUpdate {
            request: 4,
            prompt: QuitPrompt::Waiting(AgentCount {
                agents: 2,
                waiting_for_you: 1,
            }),
            error: None,
            restart: false,
        };
        assert_eq!(
            serde_json::to_value(update).unwrap(),
            serde_json::json!({
                "request": 4,
                "prompt": {"phase": "waiting", "agents": 2, "waitingForYou": 1},
                "error": null,
                "restart": false,
            })
        );
        assert_eq!(
            serde_json::to_value(QuitPrompt::Stopping).unwrap(),
            serde_json::json!({"phase": "stopping"})
        );
        assert_eq!(
            serde_json::from_value::<QuitChoice>(serde_json::json!("safe_point")).unwrap(),
            QuitChoice::SafePoint
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn logout_restart_and_shutdown_are_the_reasons_that_skip_the_prompt() {
        for reason in [b"logo", b"rlgo", b"rrst", b"rsdn", b"rest", b"shut"] {
            assert!(terminate::is_session_ending_reason(u32::from_be_bytes(
                *reason
            )));
        }
        // The Dock's Quit carries no reason, and nothing else counts.
        assert!(!terminate::is_session_ending_reason(0));
        assert!(!terminate::is_session_ending_reason(u32::from_be_bytes(
            *b"quit"
        )));
    }
}

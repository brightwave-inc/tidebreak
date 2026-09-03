//! `tidebreak -p "<prompt>"` — one unattended turn.
//!
//! Boots the same in-process server `tidebreak serve` runs — or, with
//! `--server`, attaches to one already running — and drives one turn over the
//! HTTP+WS API, then exits with a status that says how the turn ended.
//! stdout carries the output (assistant text, or the raw event stream under
//! `--output-format json`) and nothing else: logging is file-only and every
//! notice goes to stderr, so `tidebreak -p … > answer.txt` is exactly the answer.
//!
//! The turn can still reach a point only someone else can settle — an approval,
//! a proposed plan, a question. Two things answer those. A **driver** — another
//! process holding this one's stdin, opted in with `--output-format json` — is
//! asked over the NDJSON protocol in [`protocol`]. With no driver attached, the
//! missing driver ends the run with a distinct exit status and a
//! machine-readable reason. The parked turn is cancelled so the model cannot
//! reinterpret an unavailable decision as a user rejection or retry it.
//!
//! A request for another host folder is deliberately **not** one of those.
//! Folder access is host-machine consent, and the driving protocol has no way
//! to give it: no request event is emitted for it, it never becomes an
//! [`Interaction`], and no decision line can name it — the protocol's closed
//! vocabulary is approval, plan, and questions. A driven run refuses a folder
//! request exactly as an undriven one does, with the folder contract's own
//! `declined` result, the same answer an undecided desktop prompt gives.
//! Standing folder consent comes from `tidebreak folder connect` and nowhere
//! else. See [`crate::folder`].
//!
//! *Using* a folder an operator already connected is a different matter, and it
//! works: an embedded run starts [`crate::folder_executor`] over the chat it is
//! driving, which claims the parked folder tool calls and runs them through the
//! host broker's capability checks. An attached run starts no executor — the
//! process that owns the server (`tidebreak serve`, or an embedded peer) does —
//! and that same executor settles a mid-turn `request_folder_access` as the
//! typed `Declined` result rather than hanging the turn. Neither path can mint
//! a new grant; standing consent stays with `tidebreak folder connect`.

use std::collections::HashMap;
use std::future::Future as _;
use std::io::{IsTerminal as _, Write as _};

use tidebreak_core::{
    AgentError, CallId, ChatId, RequestFolderAccessResult, Result, TurnId,
    REQUEST_FOLDER_ACCESS_TOOL,
};

use crate::api::client::{Client, ClientExecutionOutcome, DurableTurn, DurableTurnStatus};
use crate::api::wire::{RendererAgentEvent, RendererToolName, RendererToolStatus};
use crate::event_stream::{EventStream as Stream, StreamNext};

mod driver;
pub mod protocol;

use driver::Driver;
use protocol::{Decision, Halt, HaltReason, Interaction, Undriven};

/// Exit status for a turn that ended without completing.
const EXIT_TURN_UNSUCCESSFUL: i32 = 1;
/// Exit status for a turn that parked on something no driver was there to
/// answer. Distinct from a failed turn: nothing went wrong, nobody answered.
const EXIT_INTERACTION_UNDRIVEN: i32 = 3;
/// Exit status for a decision that was made but could not be applied — the
/// driver answered and the server refused or was unreachable.
const EXIT_DECISION_FAILED: i32 = 4;
/// Exit status for interruption by SIGINT, following the shell's 128+signal
/// convention.
const EXIT_INTERRUPTED: i32 = 130;

/// How often a folder refusal asks whether its call has become claimable, and
/// the ceiling that interval backs off to.
///
/// There is no deadline. `ToolCallStarted` is announced the moment the provider
/// begins streaming the call, and what follows is not a short gap: the call
/// parks only if it survives the client checkpoint, and an isolated client call
/// is taken last, after every sibling in its step is terminal — which can be
/// minutes. A call that fails the checkpoint (invalid arguments, a capability
/// the tool does not offer) is declined by the agent itself and never parks at
/// all. So the refusal waits for as long as the turn runs and reports nothing
/// when the call never arrives; the turn ending is what ends the wait.
const FOLDER_REQUEST_POLL: std::time::Duration = std::time::Duration::from_millis(100);
const FOLDER_REQUEST_POLL_MAX: std::time::Duration = std::time::Duration::from_secs(2);

/// How the turn is written to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Assistant text only, streamed as it arrives.
    Text,
    /// One JSON object per line: every journaled frame of this turn, exactly as
    /// the server sent it.
    Json,
}

impl OutputFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Boot the engine, run one turn, and return the process exit status.
pub async fn run(
    prompt: String,
    chat: Option<ChatId>,
    format: OutputFormat,
    permission_mode: Option<String>,
    model: Option<String>,
    server: crate::connect::Server,
) -> Result<i32> {
    // Either binds the engine in-process (the default) or attaches to one that
    // is already running; the turn below is identical either way. The session
    // keeps an embedded engine alive — dropping it aborts the accept loop and
    // with it the turn worker.
    let session = crate::connect::Session::open(&server).await?;
    let mut client = session.client().clone();
    // Present only when this process is the server. Answering a client-owned
    // tool call is the trusted surface's job, and an attached run is not it.
    let executor_token = session.client_executor_token().map(str::to_owned);
    let chat = match chat {
        Some(chat) => {
            client.require_chat(chat).await?;
            chat
        }
        None => {
            // stderr, never stdout: text mode's stdout is the answer alone, and
            // json mode's stdout is the journal. A driving agent still needs
            // the id to continue with `--chat` on the next turn.
            let chat = client.create_chat().await?;
            eprintln!("tidebreak: chat {chat}");
            chat
        }
    };
    if let Some(mode) = permission_mode.as_deref() {
        // Fail before the turn starts: a run that asked for `allow` and got
        // `ask` would quietly do something else.
        client.set_chat_permission_mode(chat, Some(mode)).await?;
    }
    if let Some(model) = model.as_deref() {
        // Same fail-before-turn rule as permission mode: a typo or an
        // unavailable selection must not silently fall back to the default.
        client.set_chat_model(chat, Some(model)).await?;
    }

    // Driving needs somewhere to put the events and something on the other end
    // of stdin. A terminal on stdin means a person ran this by hand, and no
    // decision lines are coming.
    let driving = format == OutputFormat::Json && !std::io::stdin().is_terminal();
    let mut driver = Driver::from_stdin(driving);

    // The folder tools the agent may use on a folder an operator already
    // connected execute in this process when this process owns the broker
    // state they read. Scoped to this run's chat: a print run answers for the
    // conversation it drives and no other. An attached run has no executor
    // credential and starts nothing — `tidebreak serve` (or whichever process
    // embedded the engine) holds the credential and claims the parked calls,
    // including declining a mid-turn `request_folder_access`. The profile is
    // only resolved here for the embedded path that needs a local data dir.
    let folder_executor = match executor_token.as_deref() {
        Some(executor_token) => crate::folder_executor::FolderExecutor::new(
            client.clone(),
            Some(executor_token),
            &crate::profile_config()?.data_dir,
        )?
        .map(|executor| {
            FolderExecutorTask(tokio::spawn(
                executor.run(crate::folder_executor::Scope::Chat(chat)),
            ))
        }),
        None => None,
    };

    let outcome = one_turn(
        &mut client,
        executor_token.as_deref(),
        chat,
        &prompt,
        format,
        &mut driver,
    )
    .await;
    drop(folder_executor);
    outcome
}

/// Aborts the folder executor when the turn is over, however it ended.
struct FolderExecutorTask(tokio::task::JoinHandle<()>);

impl Drop for FolderExecutorTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Post the message and follow the event stream until the turn ends.
async fn one_turn(
    client: &mut Client,
    executor_token: Option<&str>,
    chat: ChatId,
    prompt: &str,
    format: OutputFormat,
    driver: &mut Driver<tokio::io::BufReader<tokio::io::Stdin>>,
) -> Result<i32> {
    let turn_id = TurnId::new();
    // Subscribe before posting: the turn can start before this returns, and the
    // socket's replay only reaches back to the cursor it was opened at.
    let mut stream = Stream::open(client, chat).await?;
    let attachments = crate::outputs::pending_image_attachments(chat)?;
    let file_attachments = crate::outputs::pending_document_attachments(chat)?;
    client
        .post_message(chat, turn_id, prompt, &attachments, &file_attachments)
        .await?;
    if !attachments.is_empty() {
        let _ = crate::outputs::clear_pending_image_attachments(chat);
    }
    if !file_attachments.is_empty() {
        let _ = crate::outputs::clear_pending_document_attachments(chat);
    }
    // Installed once the turn exists, and not before. Installing it earlier
    // would swallow signals over the handshake and the post — neither of which
    // watches the interrupt, and neither of which has a turn to cancel yet.
    let mut interrupt = Interrupt::watch().await;

    let mut printer = Printer::new(format);
    // A resumed chat replays its history first; nothing before this turn's own
    // `TurnStarted` is ours to print.
    let mut ours = false;
    let mut declines = FolderDeclines::new();

    let outcome = loop {
        let frame = tokio::select! {
            frame = stream.next(client, chat, turn_id) => frame?,
            () = interrupt.fired() => {
                break halted(client, chat, turn_id, &interrupted(), &mut printer).await;
            }
            report = declines.reported() => match report {
                FolderDecline::Noted { message, .. } => {
                    printer.notice(&message);
                    continue;
                }
                FolderDecline::Unanswerable { call_id, message } => {
                    let halt = Halt {
                        reason: HaltReason::FolderDeclineFailed,
                        call_id: Some(call_id),
                        message,
                    };
                    break halted(client, chat, turn_id, &halt, &mut printer).await;
                }
            },
        };

        let (raw, event) = match frame {
            StreamNext::Frame(raw, event) => (raw, *event),
            StreamNext::Ignore => continue,
            StreamNext::Durable(turn) => {
                printer.reconciled(turn_id, &turn);
                break match turn.status {
                    DurableTurnStatus::Completed => 0,
                    DurableTurnStatus::Failed | DurableTurnStatus::Cancelled => {
                        EXIT_TURN_UNSUCCESSFUL
                    }
                };
            }
        };
        if !ours {
            if !matches!(&event, RendererAgentEvent::TurnStarted { turn_id: id } if *id == turn_id)
            {
                continue;
            }
            ours = true;
        }
        printer.raw(&raw);

        match event {
            RendererAgentEvent::TextDelta { text } => printer.text(&text),
            RendererAgentEvent::ToolCallStarted { call_id, name } => {
                // Refused here, never routed through `settle`: a folder request
                // is not an `Interaction`, so the driver is never asked and no
                // decision line can answer it. Whoever is driving gets the same
                // outcome an unattended run gets. The refusal runs off the loop,
                // which keeps the turn's own output flowing while it waits.
                if name == RendererToolName::RequestFolderAccess {
                    declines.start(client, executor_token, chat, call_id, &mut printer);
                }
                printer.tool_started(call_id, name);
            }
            RendererAgentEvent::ToolCallCompleted {
                call_id,
                status,
                failure,
                ..
            } => {
                // The call is over however it ended, so a refusal still waiting
                // for it to park has nothing left to wait for.
                declines.finished(call_id);
                printer.tool_completed(call_id, status, failure.as_ref());
            }
            RendererAgentEvent::ApprovalRequired {
                call_id,
                action,
                approval,
                grant_rungs,
                preview,
                ..
            } => {
                let interaction = Interaction::Approval {
                    call_id,
                    action,
                    approval,
                    grant_rungs,
                    preview,
                };
                if let Some(halt) = settle(
                    client,
                    chat,
                    &interaction,
                    driver,
                    &mut interrupt,
                    &mut printer,
                )
                .await
                {
                    break halted(client, chat, turn_id, &halt, &mut printer).await;
                }
            }
            // Neither can be answered without a driver, so both end the run
            // loudly if there is none.
            RendererAgentEvent::PlanProposed { call_id, .. } => {
                match pending_plan(client, chat, Some(call_id)).await {
                    Ok(Some(interaction)) => {
                        if let Some(halt) = settle(
                            client,
                            chat,
                            &interaction,
                            driver,
                            &mut interrupt,
                            &mut printer,
                        )
                        .await
                        {
                            break halted(client, chat, turn_id, &halt, &mut printer).await;
                        }
                    }
                    // Already settled: a decision raced the event.
                    Ok(None) => {}
                    Err(halt) => break halted(client, chat, turn_id, &halt, &mut printer).await,
                }
            }
            RendererAgentEvent::UserQuestionsAsked { call_id, .. } => {
                match pending_questions(client, chat, Some(call_id)).await {
                    Ok(Some(interaction)) => {
                        if let Some(halt) = settle(
                            client,
                            chat,
                            &interaction,
                            driver,
                            &mut interrupt,
                            &mut printer,
                        )
                        .await
                        {
                            break halted(client, chat, turn_id, &halt, &mut printer).await;
                        }
                    }
                    Ok(None) => {}
                    Err(halt) => break halted(client, chat, turn_id, &halt, &mut printer).await,
                }
            }
            RendererAgentEvent::TurnCompleted { .. } => break 0,
            RendererAgentEvent::TurnFailed { category, .. } => {
                printer.finish();
                eprintln!("tidebreak: turn failed ({})", category.as_str());
                break EXIT_TURN_UNSUCCESSFUL;
            }
            RendererAgentEvent::TurnRefused { refusal, .. } => {
                printer.finish();
                let category = refusal.category.unwrap_or_else(|| "unspecified".to_owned());
                eprintln!("tidebreak: turn refused ({category})");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            RendererAgentEvent::TurnCancelled { .. } => {
                printer.finish();
                eprintln!("tidebreak: turn cancelled");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            _ => {}
        }
    };

    printer.finish();
    Ok(outcome)
}

/// Settle one parked interaction: ask the driver and carry the decision out.
/// If no driver answers, the undriven policy halts the run.
///
/// The interrupt is watched here too. A driven run waiting on a decision line
/// receives no further frames — the turn is parked on this very answer — so an
/// interrupt only the frame loop watched would never be seen, and Ctrl-C would
/// leave killing the process as the only way out.
async fn settle(
    client: &Client,
    chat: ChatId,
    interaction: &Interaction,
    driver: &mut Driver<tokio::io::BufReader<tokio::io::Stdin>>,
    interrupt: &mut Interrupt,
    printer: &mut Printer,
) -> Option<Halt> {
    let answered = {
        let mut control = |event| printer.control(event);
        tokio::select! {
            decision = driver.decide(interaction, &mut control) => decision,
            () = interrupt.fired() => return Some(interrupted()),
        }
    };
    let decision = match answered {
        Some(decision) => decision,
        None => {
            let Undriven::Halt(halt) = interaction.undriven();
            return Some(halt);
        }
    };
    apply(client, chat, interaction, decision, printer).await
}

/// SIGINT, observable from both waits that can outlast the user's patience: the
/// wait for the next journal frame, and a driven run's wait for a decision line.
///
/// It is not observable from everything. The HTTP client carries no request
/// timeout, so a call to an unresponsive `--server` host — applying a decision,
/// reading a pending request, cancelling the turn — still blocks with the
/// interrupt unwatched. A second Ctrl-C is the way out of those: the watcher
/// stays alive after the first one and exits the process on the next, which is
/// what the default disposition would have done anyway.
///
/// The signal is watched on its own task — a `ctrl_c()` future created per wait
/// could miss one that lands between two waits — and the flag it sets stays
/// set, so whichever wait is current sees the same interrupt.
struct Interrupt(tokio::sync::watch::Receiver<bool>);

impl Interrupt {
    /// Returns once the handler is registered, so no window is left in which
    /// SIGINT still takes its default disposition.
    async fn watch() -> Self {
        let (fired, seen) = tokio::sync::watch::channel(false);
        let (installed, registered) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut signal = std::pin::pin!(tokio::signal::ctrl_c());
            // Registration happens on the first poll, so report readiness only
            // after one has been made.
            let first = std::future::poll_fn(|context| {
                std::task::Poll::Ready(signal.as_mut().poll(context))
            })
            .await;
            let _ = installed.send(());
            let interrupted = match first {
                std::task::Poll::Ready(result) => result.is_ok(),
                std::task::Poll::Pending => signal.await.is_ok(),
            };
            if !interrupted {
                return;
            }
            let _ = fired.send(true);
            // The graceful path is now the run's to take. If it cannot — a
            // cancel to a server that never answers, say — the next Ctrl-C
            // leaves, because a run nobody can interrupt is worse than a turn
            // nobody cancelled.
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(EXIT_INTERRUPTED);
            }
        });
        let _ = registered.await;
        Self(seen)
    }

    /// Resolves once the interrupt has been seen, and immediately thereafter.
    async fn fired(&mut self) {
        while !*self.0.borrow_and_update() {
            if self.0.changed().await.is_err() {
                // The watcher is gone, so no signal is coming: never resolve,
                // leaving the other side of the `select!` to finish the run.
                std::future::pending::<()>().await;
            }
        }
    }
}

/// The halt an interrupt produces: the turn is cancelled rather than abandoned,
/// and the run exits 130.
fn interrupted() -> Halt {
    Halt {
        reason: HaltReason::Interrupted,
        call_id: None,
        message: "interrupted; cancelling the turn".to_owned(),
    }
}

/// Send one decision to the route that owns it.
///
/// A rejected approval that races the judge or a cancelled call is not this
/// process's failure — the turn carries on. A plan or answer that cannot be
/// delivered is different: the turn stays parked forever, so the run ends
/// rather than hanging.
async fn apply(
    client: &Client,
    chat: ChatId,
    interaction: &Interaction,
    decision: Decision,
    printer: &mut Printer,
) -> Option<Halt> {
    let call_id = interaction.call_id();
    let outcome = match &decision {
        Decision::Approval {
            approve,
            reason,
            grant,
        } => {
            client
                .decide_approval(chat, call_id, *approve, reason, *grant)
                .await
        }
        Decision::Plan {
            accept,
            feedback,
            permission_mode,
        } => {
            client
                .decide_plan(
                    chat,
                    call_id,
                    *accept,
                    feedback.as_deref(),
                    permission_mode.as_deref(),
                )
                .await
        }
        Decision::Questions { body } => client.answer_questions(chat, call_id, body.clone()).await,
    };
    match outcome {
        Ok(()) => None,
        Err(error) => {
            let message = format!(
                "the {} decision for call {call_id} could not be applied: {error}",
                interaction.kind()
            );
            if matches!(decision, Decision::Approval { .. }) {
                printer.notice(&message);
                return None;
            }
            Some(Halt {
                reason: HaltReason::DecisionFailed,
                call_id: Some(call_id),
                message,
            })
        }
    }
}

/// Report a halt in both directions and cancel the parked turn, returning the
/// exit status it produces.
async fn halted(
    client: &Client,
    chat: ChatId,
    turn_id: TurnId,
    halt: &Halt,
    printer: &mut Printer,
) -> i32 {
    printer.finish();
    printer.notice(&format!(
        "halted ({}): {}",
        halt.reason.as_str(),
        halt.message
    ));
    printer.halt(halt.event());
    // The parked turn must not outlive this process.
    let _ = client.cancel_turn(chat, turn_id).await;
    halt.reason.exit_code()
}

/// The proposed plan behind a `PlanProposed` event.
///
/// `Ok(None)` means nothing is pending — a decision raced the event, and there
/// is nothing left to answer — so the run carries on. A failed lookup is not
/// that, and must not read as it: the turn is parked on something this run
/// cannot see, so it halts instead of falling quiet and exiting zero.
async fn pending_plan(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> std::result::Result<Option<Interaction>, Halt> {
    let pending = client
        .list_pending_plans(chat)
        .await
        .map_err(|error| lookup_failed("plan", call_id, &error))?;
    Ok(pending
        .into_iter()
        .find(|plan| call_id.is_none_or(|id| plan.call_id == id))
        .map(Interaction::from_plan))
}

/// The question block behind a `UserQuestionsAsked` event, on the same terms.
async fn pending_questions(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> std::result::Result<Option<Interaction>, Halt> {
    let pending = client
        .list_pending_questions(chat)
        .await
        .map_err(|error| lookup_failed("questions", call_id, &error))?;
    Ok(pending
        .into_iter()
        .find(|block| call_id.is_none_or(|id| block.call_id == id))
        .map(Interaction::from_questions))
}

/// The halt a failed lookup produces, naming what could not be read.
fn lookup_failed(kind: &str, call_id: Option<CallId>, error: &AgentError) -> Halt {
    Halt {
        reason: HaltReason::PendingLookupFailed,
        call_id,
        message: format!("the pending {kind} the turn is parked on could not be read: {error}"),
    }
}

/// The `request_folder_access` refusals this run has in flight.
///
/// Each refusal runs on its own task and reports back over one channel, for two
/// reasons. Waiting inline blocked the event loop, so the assistant text
/// streaming while a refusal waited was held back and — when the wait ended the
/// run — never printed at all. And no wait bounded in advance is right: the
/// call may park immediately, park minutes later behind an isolated sibling, or
/// never park because the agent declined it at the checkpoint. Off the loop,
/// the refusal simply waits for as long as the turn does.
struct FolderDeclines {
    reports: tokio::sync::mpsc::UnboundedSender<FolderDecline>,
    inbox: tokio::sync::mpsc::UnboundedReceiver<FolderDecline>,
    waiting: HashMap<CallId, tokio::task::JoinHandle<()>>,
}

/// What a refusal has to say. Only a call seen sitting in the pending set can
/// produce [`FolderDecline::Unanswerable`]: a call that never parked is the
/// agent's own business, and ending the run over one would fail a turn the
/// server is completing perfectly well.
enum FolderDecline {
    /// Worth saying, not worth stopping for.
    Noted { call_id: CallId, message: String },
    /// The call is parked, and the refusal this run owes it cannot be
    /// delivered. Nothing else will answer it, so the turn would wait forever.
    Unanswerable { call_id: CallId, message: String },
}

impl FolderDecline {
    fn call_id(&self) -> CallId {
        match self {
            Self::Noted { call_id, .. } | Self::Unanswerable { call_id, .. } => *call_id,
        }
    }
}

impl FolderDeclines {
    fn new() -> Self {
        let (reports, inbox) = tokio::sync::mpsc::unbounded_channel();
        Self {
            reports,
            inbox,
            waiting: HashMap::new(),
        }
    }

    /// Refuse one announced `request_folder_access` call.
    ///
    /// The refusal is deliberately the folder contract's existing `Declined`
    /// variant and not a new failure code: to the model, a headless run must be
    /// indistinguishable from a user who closed the picker, so no prompt-shaped
    /// retry looks worthwhile and no path exists that could end in a grant. This
    /// never consults the driver — folder access is host-machine consent, and
    /// `tidebreak folder connect` is the only thing that gives it.
    fn start(
        &mut self,
        client: &Client,
        executor_token: Option<&str>,
        chat: ChatId,
        call_id: CallId,
        printer: &mut Printer,
    ) {
        // Attach mode has no client-executor credential and is not the trusted
        // surface for this server — the process that owns it is. Headless
        // owners (`tidebreak serve` or an embedded `-p`) settle the call
        // declined without granting; a desktop owner may show a picker. Say so
        // and leave the call alone rather than pretending to be that surface.
        let Some(executor_token) = executor_token else {
            printer.notice(&format!(
                "folder request {call_id} left for the attached server's own client executor; \
                 this process holds no executor credential"
            ));
            return;
        };
        let client = client.clone();
        let executor_token = executor_token.to_owned();
        let reports = self.reports.clone();
        let task = tokio::spawn(async move {
            if let Some(report) = decline(&client, &executor_token, chat, call_id).await {
                let _ = reports.send(report);
            }
        });
        self.waiting.insert(call_id, task);
    }

    /// The call is terminal, so stop waiting for it to park. This is the
    /// ordinary end for a refusal that never had a call to answer: the agent
    /// declines a request that fails its checkpoint itself, and the only sign of
    /// that on this side is the completion.
    fn finished(&mut self, call_id: CallId) {
        if let Some(task) = self.waiting.remove(&call_id) {
            task.abort();
        }
    }

    /// The next thing a refusal has to report. Pends forever when there is
    /// nothing to say, which is the common case: this side holds a sender, so
    /// the channel never closes and never resolves on its own.
    async fn reported(&mut self) -> FolderDecline {
        let report = match self.inbox.recv().await {
            Some(report) => report,
            None => std::future::pending().await,
        };
        // A refusal that has reported is done; its task is spent either way.
        self.waiting.remove(&report.call_id());
        report
    }
}

/// Ends every refusal still in flight when the turn is over, however it ended.
impl Drop for FolderDeclines {
    fn drop(&mut self) {
        for task in self.waiting.values() {
            task.abort();
        }
    }
}

/// Wait for one folder request to become claimable, then resolve it declined.
///
/// Only an embedded run gets here — an attached one holds no executor
/// credential and never starts a refusal — so every call below is to this
/// process's own server over loopback. That is what makes the asymmetry below
/// defensible: a poll is retried because the call may simply not have parked
/// yet, while a claim or a resolve that fails against a local, in-process
/// server is a real refusal of the operation rather than a network blip, and
/// retrying it would only postpone the same answer.
///
/// `None` means there is nothing to report — including the ordinary case where
/// the call never parks, because the agent refused it at the checkpoint and is
/// carrying the turn on without it. Each way this can end is answered on its
/// own terms rather than as one undifferentiated failure:
///
/// - **Never parked, or unreadable.** Not evidence of anything: keep asking. A
///   poll that fails says nothing about the call, so it is retried too. The
///   waiting ends when the call completes or the turn does, not on a clock.
/// - **Parked, and claimed by somebody else.** The headless
///   [`crate::folder_executor`] — this process's own, or `tidebreak serve`'s —
///   settles folder-access requests declined. Do not race it and do not halt:
///   the turn continues when that executor publishes the terminal result.
/// - **Parked, and this refusal claimed and resolved it.** The ordinary path.
/// - **Parked, and the claim or the resolve failed while the call is still
///   unclaimed.** The refusal is owed and undeliverable, which is the one shape
///   that must end the run.
/// - **Parked, but not a folder request.** Impossible by construction — the name
///   came from the announcement of this same call — and not this run's answer to
///   give if it happens. Say so and leave it.
async fn decline(
    client: &Client,
    executor_token: &str,
    chat: ChatId,
    call_id: CallId,
) -> Option<FolderDecline> {
    let mut wait = FOLDER_REQUEST_POLL;
    loop {
        if let Ok(pending) = client.pending_client_executions(executor_token, chat).await {
            match pending.into_iter().find(|call| call.id == call_id) {
                Some(call) if call.name != REQUEST_FOLDER_ACCESS_TOOL => {
                    return Some(FolderDecline::Noted {
                        call_id,
                        message: format!(
                            "folder request {call_id} parked as a {} call and was left alone",
                            call.name
                        ),
                    });
                }
                // Another executor (this process's folder_executor, or serve's)
                // already owns the call. Stand down; its terminal result ends
                // the wait via ToolCallCompleted.
                Some(call) if call.client_executor_id.is_some() => {
                    return None;
                }
                Some(_) => break,
                // Gone from the pending set: either never parked, or already
                // settled (including by folder_executor). Keep polling only for
                // the never-parked case; ToolCallCompleted aborts this task
                // when the call is terminal.
                None => {}
            }
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(FOLDER_REQUEST_POLL_MAX);
    }

    match resolve_declined(client, executor_token, chat, call_id).await {
        ResolveDeclined::Declined => Some(FolderDecline::Noted {
            call_id,
            message: "folder access declined: headless runs connect folders with `tidebreak \
                      folder connect`, never mid-turn"
                .to_owned(),
        }),
        // folder_executor won the race after we saw the call unclaimed. The
        // turn is not stuck; its completion event is what ends the wait.
        ResolveDeclined::HandledElsewhere => None,
        ResolveDeclined::Failed(error) => Some(FolderDecline::Unanswerable {
            call_id,
            message: format!(
                "the parked folder request could not be declined: {error}; the turn is waiting \
                 on an answer this run cannot deliver"
            ),
        }),
    }
}

/// Outcome of trying to claim and decline one folder-access request.
enum ResolveDeclined {
    Declined,
    /// Another executor already claimed or resolved the call.
    HandledElsewhere,
    Failed(AgentError),
}

/// Claim the parked call and resolve it declined.
async fn resolve_declined(
    client: &Client,
    executor_token: &str,
    chat: ChatId,
    call_id: CallId,
) -> ResolveDeclined {
    let executor_id = uuid::Uuid::new_v4();
    let lease_token = uuid::Uuid::new_v4();
    if let Err(error) = client
        .claim_client_execution(executor_token, chat, call_id, executor_id, lease_token)
        .await
    {
        // A conflict means the call is not claimable by this pair right now —
        // typically folder_executor already owns it or already finished it.
        return if error.is_conflict() {
            ResolveDeclined::HandledElsewhere
        } else {
            ResolveDeclined::Failed(error.into())
        };
    }
    let declined = match serde_json::to_string(&RequestFolderAccessResult::Declined) {
        Ok(declined) => declined,
        Err(error) => {
            return ResolveDeclined::Failed(AgentError::msg(format!(
                "could not encode the refusal: {error}"
            )));
        }
    };
    match client
        .resolve_client_execution(
            executor_token,
            chat,
            call_id,
            lease_token,
            // The same `{"status":"completed","result":…}` body this has always
            // sent: `rows` is omitted rather than sent as null.
            &ClientExecutionOutcome::Completed {
                result: declined,
                rows: None,
            },
        )
        .await
    {
        Ok(()) => ResolveDeclined::Declined,
        Err(error) if error.is_conflict() => ResolveDeclined::HandledElsewhere,
        Err(error) => ResolveDeclined::Failed(error.into()),
    }
}

/// Writes the turn out in the selected format.
struct Printer {
    format: OutputFormat,
    tools: HashMap<CallId, &'static str>,
    /// Whether stdout's last text ended without a newline, so the final flush
    /// can leave the shell prompt on its own line.
    dangling_line: bool,
    assistant_text: String,
}

impl Printer {
    fn new(format: OutputFormat) -> Self {
        Self {
            format,
            tools: HashMap::new(),
            dangling_line: false,
            assistant_text: String::new(),
        }
    }

    /// Echo the frame verbatim under `--output-format json`, so fields this
    /// build does not model still reach the consumer.
    fn raw(&mut self, frame: &str) {
        if self.format != OutputFormat::Json {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        // A frame is one line of JSON; a broken pipe downstream is the reader's
        // business, not an error to report on stderr.
        let _ = writeln!(stdout, "{frame}");
        let _ = stdout.flush();
    }

    fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.assistant_text.push_str(text);
        if self.format != OutputFormat::Text {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "{text}");
        // Unbuffered: a caller reading the pipe should see the answer form.
        let _ = stdout.flush();
        self.dangling_line = !text.ends_with('\n');
    }

    fn reconciled(&mut self, turn_id: TurnId, turn: &DurableTurn) {
        self.notice("the live event stream was unavailable; recovered the terminal turn from the durable transcript");
        if self.format == OutputFormat::Text {
            if let Some(suffix) = turn.content.strip_prefix(&self.assistant_text) {
                self.text(suffix);
            } else if !turn.content.is_empty() {
                if !self.assistant_text.is_empty() && !self.assistant_text.ends_with('\n') {
                    self.text("\n");
                }
                self.text(&turn.content);
            }
            return;
        }
        self.control(serde_json::json!({
            "tidebreak": protocol::PROTOCOL_VERSION,
            "type": "turn_reconciled",
            "turn_id": turn_id,
            "status": match turn.status {
                DurableTurnStatus::Completed => "completed",
                DurableTurnStatus::Failed => "failed",
                DurableTurnStatus::Cancelled => "cancelled",
            },
            "content": turn.content,
            "last_event_seq": turn.last_event_seq,
        }));
    }

    fn tool_started(&mut self, call_id: CallId, name: RendererToolName) {
        self.tools.insert(call_id, name.as_str());
    }

    fn tool_completed(
        &mut self,
        call_id: CallId,
        status: RendererToolStatus,
        failure: Option<&crate::api::wire::RendererToolFailure>,
    ) {
        let name = self.tools.remove(&call_id).unwrap_or("tool");
        let status = match status {
            RendererToolStatus::Completed => "ok",
            RendererToolStatus::Failed => "failed",
        };
        self.notice(&format!("tool: {name} {status}"));
        if let Some(failure) = failure {
            self.notice(tool_failure_notice(failure.reason));
        }
    }

    /// Progress and problems go to stderr in both formats, so stdout stays
    /// exactly the output the caller asked for.
    fn notice(&self, message: &str) {
        eprintln!("tidebreak: {message}");
    }

    /// A protocol event for the driver. Only the NDJSON stream carries these;
    /// text output has no driver to talk to.
    fn control(&mut self, event: serde_json::Value) {
        if self.format != OutputFormat::Json {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{event}");
        let _ = stdout.flush();
    }

    /// The terminating reason. Unlike other control events this one is emitted
    /// in both formats — a text-mode caller still has to be able to tell a halt
    /// apart from a failed turn without parsing prose, so the same object goes
    /// to stderr there.
    fn halt(&mut self, event: serde_json::Value) {
        if self.format == OutputFormat::Json {
            self.control(event);
        } else {
            eprintln!("{event}");
        }
    }

    /// Close out stdout before anything is written to stderr or the process
    /// exits.
    fn finish(&mut self) {
        if self.dangling_line {
            println!();
            self.dangling_line = false;
        }
        let _ = std::io::stdout().flush();
    }
}

fn tool_failure_notice(reason: crate::api::wire::RendererToolFailureReason) -> &'static str {
    match reason {
        crate::api::wire::RendererToolFailureReason::LeaseExpired => {
            "the client executor lease expired"
        }
        crate::api::wire::RendererToolFailureReason::ProviderUnavailable => {
            "the execution provider is unavailable"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A poll that fails is not "nothing pending". While the two were the same
    /// answer, a server the run could not reach read exactly like a plan that
    /// had already been settled: the run went quiet, left the turn parked, and
    /// exited zero — the worst shape for a scripted caller, which cannot tell a
    /// finished turn from a lost one.
    #[tokio::test]
    async fn an_unreadable_pending_plan_halts_rather_than_looking_settled() {
        // A port nothing is listening on, so the poll fails at the transport.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free local port");
        let address = listener.local_addr().expect("the bound address");
        drop(listener);
        let client = Client::attach(format!("http://{address}"), "token").expect("a client");

        let halt = pending_plan(&client, ChatId::new(), None)
            .await
            .expect_err("an unreachable server must not read as nothing pending");

        assert_eq!(halt.reason, HaltReason::PendingLookupFailed);
    }

    #[test]
    fn execution_provider_failures_have_a_bounded_cli_notice() {
        assert_eq!(
            tool_failure_notice(crate::api::wire::RendererToolFailureReason::ProviderUnavailable),
            "the execution provider is unavailable"
        );
    }
}

//! `openwave -p "<prompt>"` — one unattended turn.
//!
//! Boots the same in-process server `openwave serve` runs — or, with
//! `--server`, attaches to one already running — and drives one turn over the
//! HTTP+WS API, then exits with a status that says how the turn ended.
//! stdout carries the output (assistant text, or the raw event stream under
//! `--output-format json`) and nothing else: logging is file-only and every
//! notice goes to stderr, so `openwave -p … > answer.txt` is exactly the answer.
//!
//! The turn can still reach a point only someone else can settle — an approval,
//! a proposed plan, a question. Two things answer those. A **driver** — another
//! process holding this one's stdin, opted in with `--output-format json` — is
//! asked over the NDJSON protocol in [`protocol`]. With no driver attached, the
//! standing policy answers instead: approvals are rejected, so the model can
//! choose another route rather than hang, and a plan or a question ends the run
//! with a distinct exit status and a machine-readable reason. Nothing is ever
//! cancelled silently.
//!
//! A request for another host folder is deliberately **not** one of those.
//! Folder access is host-machine consent, and the driving protocol has no way
//! to give it: no request event is emitted for it, it never becomes an
//! [`Interaction`], and no decision line can name it — the protocol's closed
//! vocabulary is approval, plan, and questions. A driven run refuses a folder
//! request exactly as an undriven one does, with the folder contract's own
//! `declined` result, the same answer an undecided desktop prompt gives.
//! Standing folder consent comes from `openwave folder connect` and nowhere
//! else. See [`crate::folder`].
//!
//! *Using* a folder an operator already connected is a different matter, and it
//! works: an embedded run starts [`crate::folder_executor`] over the chat it is
//! driving, which claims the parked folder tool calls and runs them through the
//! host broker's capability checks. That executor answers only for folders that
//! are already connected; it has no path to a new grant.

use std::collections::HashMap;
use std::io::{IsTerminal as _, Write as _};

use futures::StreamExt as _;
use openwave_core::{
    AgentError, CallId, ChatId, RequestFolderAccessResult, Result, TurnId,
    REQUEST_FOLDER_ACCESS_TOOL,
};
use tokio_tungstenite::tungstenite::Message;

use crate::api::client::{Client, ClientExecutionOutcome, EventSocket};
use crate::api::wire::{ChatFrame, ClientEvent, ToolCallStatus};

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

/// Attempts to re-open the event socket after it closes mid-turn before giving
/// up. The retries cover a transient hiccup — an accept-loop stumble when the
/// server is in-process, a dropped connection when it is not.
const RECONNECT_ATTEMPTS: usize = 3;
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// How long to wait for a folder request to become claimable after its tool
/// call is announced. The call is checkpointed just after the provider streams
/// it, so this only covers that gap.
const FOLDER_REQUEST_SETTLE: std::time::Duration = std::time::Duration::from_secs(10);
const FOLDER_REQUEST_POLL: std::time::Duration = std::time::Duration::from_millis(100);

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
    server: crate::connect::Server,
) -> Result<i32> {
    // Either binds the engine in-process (the default) or attaches to one that
    // is already running; the turn below is identical either way. The session
    // keeps an embedded engine alive — dropping it aborts the accept loop and
    // with it the turn worker.
    let session = crate::connect::Session::open(&server).await?;
    let client = session.client().clone();
    // Present only when this process is the server. Answering a client-owned
    // tool call is the trusted surface's job, and an attached run is not it.
    let executor_token = session.client_executor_token().map(str::to_owned);
    let chat = match chat {
        Some(chat) => {
            client.require_chat(chat).await?;
            chat
        }
        None => client.create_chat().await?,
    };
    if let Some(mode) = permission_mode.as_deref() {
        // Fail before the turn starts: a run that asked for `allow` and got
        // `ask` would quietly do something else.
        client.set_chat_permission_mode(chat, Some(mode)).await?;
    }

    // Driving needs somewhere to put the events and something on the other end
    // of stdin. A terminal on stdin means a person ran this by hand, and no
    // decision lines are coming.
    let driving = format == OutputFormat::Json && !std::io::stdin().is_terminal();
    let mut driver = Driver::from_stdin(driving);

    // The folder tools the agent may use on a folder an operator already
    // connected execute in this process, because this process is the one that
    // owns the broker state they read. Scoped to this run's chat: a print run
    // answers for the conversation it drives and no other. An attached run has
    // no executor credential and starts nothing — it touches no local data
    // directory either, which is why the profile is only resolved here.
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
        &client,
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
    client: &Client,
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
    client.post_message(chat, turn_id, prompt).await?;

    let mut printer = Printer::new(format);
    // A resumed chat replays its history first; nothing before this turn's own
    // `TurnStarted` is ours to print.
    let mut ours = false;
    // Watch for the interrupt on its own task: a `ctrl_c()` future created per
    // loop iteration could miss a signal that lands between two iterations.
    let (interrupted, mut interrupt) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = interrupted.send(());
        }
    });

    let outcome = loop {
        let frame = tokio::select! {
            frame = stream.next(client, chat) => frame?,
            _ = &mut interrupt => {
                printer.finish();
                eprintln!("openwave: interrupted; cancelling the turn");
                // Best effort: the turn may already have finished, which the
                // server answers with a conflict.
                let _ = client.cancel_turn(chat, turn_id).await;
                return Ok(EXIT_INTERRUPTED);
            }
        };

        let Some((raw, event)) = frame else {
            continue;
        };
        if !ours {
            if !matches!(&event, ClientEvent::TurnStarted { turn_id: id } if *id == turn_id) {
                continue;
            }
            ours = true;
        }
        printer.raw(&raw);

        match event {
            ClientEvent::TextDelta { text } => printer.text(&text),
            ClientEvent::ToolCallStarted { call_id, name } => {
                // Refused here, never routed through `settle`: a folder request
                // is not an `Interaction`, so the driver is never asked and no
                // decision line can answer it. Whoever is driving gets the same
                // outcome an unattended run gets.
                if name == REQUEST_FOLDER_ACCESS_TOOL {
                    decline_folder_request(client, executor_token, chat, call_id, &mut printer)
                        .await;
                }
                printer.tool_started(call_id, name);
            }
            ClientEvent::ToolCallCompleted {
                call_id, status, ..
            } => {
                printer.tool_completed(call_id, status);
            }
            ClientEvent::ApprovalRequired {
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
                if let Some(halt) = settle(client, chat, &interaction, driver, &mut printer).await {
                    break halted(client, chat, turn_id, &halt, &mut printer).await;
                }
            }
            // Neither can be answered from the standing policy, so both reach
            // for the driver first and end the run loudly if there is none.
            ClientEvent::PlanProposed { call_id } => {
                if let Some(interaction) = pending_plan(client, chat, call_id).await {
                    if let Some(halt) =
                        settle(client, chat, &interaction, driver, &mut printer).await
                    {
                        break halted(client, chat, turn_id, &halt, &mut printer).await;
                    }
                }
            }
            ClientEvent::UserQuestionsAsked { call_id } => {
                if let Some(interaction) = pending_questions(client, chat, call_id).await {
                    if let Some(halt) =
                        settle(client, chat, &interaction, driver, &mut printer).await
                    {
                        break halted(client, chat, turn_id, &halt, &mut printer).await;
                    }
                }
            }
            ClientEvent::TurnCompleted { .. } => break 0,
            ClientEvent::TurnFailed { category, .. } => {
                printer.finish();
                eprintln!("openwave: turn failed ({category})");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            ClientEvent::TurnRefused { refusal, .. } => {
                printer.finish();
                let category = refusal.category.unwrap_or_else(|| "unspecified".to_owned());
                eprintln!("openwave: turn refused ({category})");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            ClientEvent::TurnCancelled { .. } => {
                printer.finish();
                eprintln!("openwave: turn cancelled");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            _ => {}
        }
    };

    printer.finish();
    Ok(outcome)
}

/// Settle one parked interaction: ask the driver, fall back to the standing
/// policy, and carry the decision out. `Some(halt)` ends the run.
async fn settle(
    client: &Client,
    chat: ChatId,
    interaction: &Interaction,
    driver: &mut Driver<tokio::io::BufReader<tokio::io::Stdin>>,
    printer: &mut Printer,
) -> Option<Halt> {
    let mut control = |event| printer.control(event);
    let decision = match driver.decide(interaction, &mut control).await {
        Some(decision) => decision,
        None => match interaction.undriven() {
            Undriven::Decide(decision) => {
                printer.notice(&format!(
                    "no driver attached; {} answered by standing policy",
                    interaction.kind()
                ));
                decision
            }
            Undriven::Halt(halt) => return Some(halt),
        },
    };
    apply(client, chat, interaction, decision, printer).await
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

/// The proposed plan behind a `PlanProposed` event. `None` when it is already
/// settled — a decision raced the event, and there is nothing left to answer.
async fn pending_plan(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> Option<Interaction> {
    let pending = client.list_pending_plans(chat).await.ok()?;
    pending
        .into_iter()
        .find(|plan| call_id.is_none_or(|id| plan.call_id == id))
        .map(Interaction::from_plan)
}

/// The question block behind a `UserQuestionsAsked` event, on the same terms.
async fn pending_questions(
    client: &Client,
    chat: ChatId,
    call_id: Option<CallId>,
) -> Option<Interaction> {
    let pending = client.list_pending_questions(chat).await.ok()?;
    pending
        .into_iter()
        .find(|block| call_id.is_none_or(|id| block.call_id == id))
        .map(Interaction::from_questions)
}

/// Refuse one parked `request_folder_access` call with the typed declined
/// result.
///
/// The refusal is deliberately the folder contract's existing `Declined`
/// variant and not a new failure code: to the model, a headless run must be
/// indistinguishable from a user who closed the picker, so no prompt-shaped
/// retry looks worthwhile and no path exists that could end in a grant. This
/// never consults the driver — folder access is host-machine consent, and
/// `openwave folder connect` is the only thing that gives it.
///
/// Reporting only, never fatal: a folder request the CLI could not answer is
/// the server owner's to settle, and cancelling someone else's turn over it
/// would be worse than saying so.
async fn decline_folder_request(
    client: &Client,
    executor_token: Option<&str>,
    chat: ChatId,
    call_id: CallId,
    printer: &mut Printer,
) {
    // Attach mode has no client-executor credential and is not the trusted
    // surface for this server — the process that owns it is, and if that is a
    // desktop it will show the user a picker. Say so and leave the call alone
    // rather than pretending to be it.
    let Some(executor_token) = executor_token else {
        printer.notice(&format!(
            "folder request {call_id} left for the attached server's own client executor; \
             this process holds no executor credential"
        ));
        return;
    };
    match decline(client, executor_token, chat, call_id).await {
        Ok(()) => printer.notice(
            "folder access declined: headless runs connect folders with `openwave folder \
             connect`, never mid-turn",
        ),
        Err(error) => printer.notice(&format!("could not decline the folder request: {error}")),
    }
}

/// Claim the parked call and resolve it declined, or say why not.
async fn decline(
    client: &Client,
    executor_token: &str,
    chat: ChatId,
    call_id: CallId,
) -> Result<()> {
    let deadline = std::time::Instant::now() + FOLDER_REQUEST_SETTLE;
    loop {
        let pending = client
            .pending_client_executions(executor_token, chat)
            .await?;
        match pending.into_iter().find(|call| call.id == call_id) {
            Some(call) if call.name != REQUEST_FOLDER_ACCESS_TOOL => {
                return Err(AgentError::msg("the parked call is not a folder request"));
            }
            Some(call) if call.client_executor_id.is_some() => {
                // Something else owns the call. Never race it: this process has
                // no way to grant anything, so leaving it alone is safe.
                return Err(AgentError::msg("the folder request is already claimed"));
            }
            Some(_) => break,
            None if std::time::Instant::now() >= deadline => {
                return Err(AgentError::msg("the folder request never parked"));
            }
            None => tokio::time::sleep(FOLDER_REQUEST_POLL).await,
        }
    }

    let executor_id = uuid::Uuid::new_v4();
    let lease_token = uuid::Uuid::new_v4();
    client
        .claim_client_execution(executor_token, chat, call_id, executor_id, lease_token)
        .await?;
    let declined = serde_json::to_string(&RequestFolderAccessResult::Declined)
        .map_err(|error| AgentError::msg(format!("could not encode the refusal: {error}")))?;
    client
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
        .await?;
    Ok(())
}

/// The event socket plus the cursor a reconnect resumes from.
struct Stream {
    socket: EventSocket,
    last_seq: i64,
}

impl Stream {
    async fn open(client: &Client, chat: ChatId) -> Result<Self> {
        Ok(Self {
            socket: client.open_events(chat, 0).await?,
            last_seq: 0,
        })
    }

    /// The next journaled frame: its raw JSON text and its decoded event.
    /// `Ok(None)` is a frame with nothing to act on (metadata, a ping, an
    /// undecodable payload), so the caller simply asks again.
    async fn next(
        &mut self,
        client: &Client,
        chat: ChatId,
    ) -> Result<Option<(String, ClientEvent)>> {
        match self.socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let Ok(ChatFrame::Event(frame)) = serde_json::from_str::<ChatFrame>(&text) else {
                    return Ok(None);
                };
                self.last_seq = frame.seq;
                Ok(Some((text.to_string(), frame.event)))
            }
            Some(Ok(_)) => Ok(None),
            Some(Err(_)) | None => {
                self.reconnect(client, chat).await?;
                Ok(None)
            }
        }
    }

    async fn reconnect(&mut self, client: &Client, chat: ChatId) -> Result<()> {
        let mut last = None;
        for _ in 0..RECONNECT_ATTEMPTS {
            tokio::time::sleep(RECONNECT_DELAY).await;
            match client.open_events(chat, self.last_seq).await {
                Ok(socket) => {
                    self.socket = socket;
                    return Ok(());
                }
                Err(error) => last = Some(error),
            }
        }
        Err(AgentError::msg(format!(
            "the event stream closed mid-turn and could not be reopened{}",
            last.map(|error| format!(": {error}")).unwrap_or_default()
        )))
    }
}

/// Writes the turn out in the selected format.
struct Printer {
    format: OutputFormat,
    tools: HashMap<CallId, String>,
    /// Whether stdout's last text ended without a newline, so the final flush
    /// can leave the shell prompt on its own line.
    dangling_line: bool,
}

impl Printer {
    fn new(format: OutputFormat) -> Self {
        Self {
            format,
            tools: HashMap::new(),
            dangling_line: false,
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
        if self.format != OutputFormat::Text || text.is_empty() {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "{text}");
        // Unbuffered: a caller reading the pipe should see the answer form.
        let _ = stdout.flush();
        self.dangling_line = !text.ends_with('\n');
    }

    fn tool_started(&mut self, call_id: CallId, name: String) {
        self.tools.insert(call_id, name);
    }

    fn tool_completed(&mut self, call_id: CallId, status: ToolCallStatus) {
        let name = self
            .tools
            .remove(&call_id)
            .unwrap_or_else(|| "tool".to_owned());
        let status = match status {
            ToolCallStatus::Completed => "ok",
            // An unrecognized status reads as a failure: the conservative note.
            ToolCallStatus::Failed | ToolCallStatus::Unknown => "failed",
            ToolCallStatus::Cancelled => "cancelled",
        };
        self.notice(&format!("tool: {name} {status}"));
    }

    /// Progress and problems go to stderr in both formats, so stdout stays
    /// exactly the output the caller asked for.
    fn notice(&self, message: &str) {
        eprintln!("openwave: {message}");
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

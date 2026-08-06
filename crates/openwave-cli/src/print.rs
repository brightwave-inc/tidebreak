//! `openwave -p "<prompt>"` — one non-interactive turn.
//!
//! Boots the same in-process server `openwave serve` runs and drives one turn
//! over the loopback API, then exits with a status that says how the turn ended.
//! stdout carries the output (assistant text, or the raw event stream under
//! `--output-format json`) and nothing else: logging is file-only and every
//! notice goes to stderr, so `openwave -p … > answer.txt` is exactly the answer.
//!
//! Nothing here can wait for a human. A parked approval is rejected as soon as
//! it arrives, which lets the model adapt and finish rather than hang; a turn
//! that asks the user a question or proposes a plan has no answer available at
//! all, so it is cancelled and reported.

use std::collections::HashMap;
use std::io::Write as _;

use futures::StreamExt as _;
use openwave_core::{AgentError, CallId, ChatId, Result, TurnId};
use tokio_tungstenite::tungstenite::Message;

use crate::api::client::{Client, EventSocket};
use crate::api::wire::{ChatFrame, ClientEvent, ToolCallStatus};

/// Exit status for a turn that ended without completing.
const EXIT_TURN_UNSUCCESSFUL: i32 = 1;
/// Exit status for interruption by SIGINT, following the shell's 128+signal
/// convention.
const EXIT_INTERRUPTED: i32 = 130;

/// The reason recorded against every approval this mode rejects. It reaches the
/// model, which is what lets it choose another route rather than retry.
const REJECTION_REASON: &str = "non-interactive print mode";

/// Attempts to re-open the event socket after it closes mid-turn before giving
/// up. The server is in-process, so a close means something is badly wrong; the
/// retries only cover a transient accept-loop hiccup.
const RECONNECT_ATTEMPTS: usize = 3;
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

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
pub async fn run(prompt: String, chat: Option<ChatId>, format: OutputFormat) -> Result<i32> {
    let config = crate::profile_config()?;
    // stdout belongs to the output, so logs go to the profile's log file only.
    openwave_server::logging::init_logging_file_only(&config.data_dir);
    let server = openwave_server::bind_configured(config).await?;
    let addr = server.local_addr();
    let token = server.token().to_owned();
    // Dropping the Server aborts its background workers, the turn worker
    // included; this handle is what keeps the engine alive for the turn.
    let serve = tokio::spawn(server.serve());

    let client = Client::new(addr, &token)?;
    let chat = match chat {
        Some(chat) => {
            client.require_chat(chat).await?;
            chat
        }
        None => client.create_chat().await?,
    };

    let result = one_turn(&client, chat, &prompt, format).await;
    serve.abort();
    result
}

/// Post the message and follow the event stream until the turn ends.
async fn one_turn(
    client: &Client,
    chat: ChatId,
    prompt: &str,
    format: OutputFormat,
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
            ClientEvent::ToolCallStarted { call_id, name } => printer.tool_started(call_id, name),
            ClientEvent::ToolCallCompleted { call_id, status } => {
                printer.tool_completed(call_id, status);
            }
            ClientEvent::ApprovalRequired {
                call_id, action, ..
            } => {
                printer.notice(&format!(
                    "approval for {action} rejected: {REJECTION_REASON}"
                ));
                if let Err(error) = client
                    .decide_approval(chat, call_id, false, REJECTION_REASON)
                    .await
                {
                    // A decision that races the approval judge or a cancelled
                    // call is not this process's failure; the turn continues.
                    printer.notice(&format!("could not reject the approval: {error}"));
                }
            }
            // Neither can be answered without a human. Cancelling is what stops
            // the parked turn from outliving this process.
            ClientEvent::UserQuestionsAsked | ClientEvent::PlanProposed => {
                printer.finish();
                eprintln!(
                    "openwave: the turn needs an interactive answer, which print mode cannot \
                     give; cancelling"
                );
                let _ = client.cancel_turn(chat, turn_id).await;
                break EXIT_TURN_UNSUCCESSFUL;
            }
            ClientEvent::TurnCompleted => break 0,
            ClientEvent::TurnFailed { category } => {
                printer.finish();
                eprintln!("openwave: turn failed ({category})");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            ClientEvent::TurnRefused { refusal } => {
                printer.finish();
                let category = refusal.category.unwrap_or_else(|| "unspecified".to_owned());
                eprintln!("openwave: turn refused ({category})");
                break EXIT_TURN_UNSUCCESSFUL;
            }
            ClientEvent::TurnCancelled => {
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
        };
        self.notice(&format!("tool: {name} {status}"));
    }

    /// Progress and problems go to stderr in both formats, so stdout stays
    /// exactly the output the caller asked for.
    fn notice(&self, message: &str) {
        eprintln!("openwave: {message}");
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

//! The interactive loop: terminal input, socket frames, and a redraw tick in
//! one `select!`. Finished blocks (user/assistant messages, tool results,
//! notices) are committed to real scrollback with `insert_before`; the inline
//! viewport below them repaints the live region.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use openwave_core::{AgentError, CallId, ChatId, Result, TurnId};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tui_textarea::{Input, TextArea};

use super::render::{self, Commit};
use super::theme;
use crate::api::client::{Client, EventSocket};
use crate::api::wire::{ChatFrame, ClientEvent, SequencedFrame, ToolCallStatus};

/// Fixed height of the repainting region: transient stack, composer, footer.
/// Clamped to the terminal at startup.
const LIVE_HEIGHT: u16 = 10;
/// Composer rows beyond this scroll inside the textarea.
const MAX_COMPOSER_ROWS: u16 = 4;
const TICK: Duration = Duration::from_millis(100);
/// One delayed reconnect after an unexpected socket close; further closes give
/// up rather than hammering the server.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Outcomes of HTTP actions spawned off the UI loop, plus the one reconnect.
enum ActionOutcome {
    Sent { turn_id: TurnId, content: String },
    SendFailed { content: String, error: String },
    CancelFailed(String),
    DecisionFailed(String),
    Reconnected(Box<EventSocket>),
    ReconnectFailed(String),
}

struct PendingApproval {
    call_id: CallId,
    action: String,
    approval: String,
    auto_judging: bool,
    preview: Option<String>,
}

struct ActiveTool {
    call_id: CallId,
    name: String,
}

struct App {
    client: Client,
    chat: ChatId,
    /// First group of the chat's UUID, for the header and the footer's right
    /// edge.
    short_id: String,
    actions: mpsc::UnboundedSender<ActionOutcome>,
    composer: TextArea<'static>,
    /// Assistant text of the in-flight turn, appended by `TextDelta`.
    streaming: String,
    /// Reasoning tail shown dimmed while the model thinks.
    thinking: String,
    active_tool: Option<ActiveTool>,
    approval: Option<PendingApproval>,
    running_turn: Option<TurnId>,
    /// A send whose POST hasn't resolved yet; blocks a second send in flight.
    send_pending: bool,
    /// Resume cursor for the reconnect: the highest journaled seq seen.
    last_seq: i64,
    commits: Vec<Commit>,
    /// A transient footer message and its remaining ticks.
    flash: Option<(String, u8)>,
    spinner: usize,
    should_quit: bool,
}

impl App {
    fn new(client: Client, chat: ChatId, actions: mpsc::UnboundedSender<ActionOutcome>) -> Self {
        Self {
            client,
            short_id: chat.to_string().chars().take(8).collect(),
            chat,
            actions,
            composer: new_composer(Vec::new()),
            streaming: String::new(),
            thinking: String::new(),
            active_tool: None,
            approval: None,
            running_turn: None,
            send_pending: false,
            last_seq: 0,
            commits: Vec::new(),
            flash: None,
            spinner: 0,
            should_quit: false,
        }
    }

    fn commit(&mut self, commit: Commit) {
        self.commits.push(commit);
    }

    /// Flush any streamed assistant text as a finished block.
    fn commit_streaming(&mut self) {
        let streamed = std::mem::take(&mut self.streaming);
        if !streamed.trim().is_empty() {
            self.commit(Commit::AssistantText(streamed));
        }
        self.thinking.clear();
    }

    fn flash(&mut self, message: impl Into<String>) {
        // ~4 seconds at the 100ms tick.
        self.flash = Some((message.into(), 40));
    }

    fn spinner(&self) -> char {
        SPINNER[self.spinner % SPINNER.len()]
    }

    fn on_frame(&mut self, frame: SequencedFrame) {
        self.last_seq = frame.seq;
        self.on_event(frame.event);
    }

    fn on_event(&mut self, event: ClientEvent) {
        match event {
            ClientEvent::TurnStarted { turn_id } => {
                self.running_turn = Some(turn_id);
                self.streaming.clear();
                self.thinking.clear();
            }
            ClientEvent::TextDelta { text } => self.streaming.push_str(&text),
            ClientEvent::ReasoningDelta { text } => {
                self.thinking.push_str(&text);
                // Only the tail is displayed; don't grow the buffer forever.
                if self.thinking.len() > 8192 {
                    self.thinking.drain(..self.thinking.len() - 4096);
                }
            }
            ClientEvent::StreamInterrupted => {
                // Partial deltas since the last stable boundary are void.
                self.streaming.clear();
                self.thinking.clear();
                self.commit(Commit::Notice("stream interrupted; retrying".into()));
            }
            ClientEvent::ToolCallStarted { call_id, name } => {
                self.commit_streaming();
                self.active_tool = Some(ActiveTool { call_id, name });
            }
            ClientEvent::ToolCallArgsDelta => {}
            ClientEvent::ApprovalRequired {
                call_id,
                action,
                approval,
                auto_judging,
                preview,
            } => {
                self.approval = Some(PendingApproval {
                    call_id,
                    action,
                    approval,
                    auto_judging,
                    preview: preview.as_ref().and_then(render::preview_summary),
                });
            }
            ClientEvent::ApprovalDecided { call_id } => {
                if self.approval.as_ref().is_some_and(|p| p.call_id == call_id) {
                    self.approval = None;
                }
            }
            ClientEvent::ToolCallCompleted { call_id, status } => {
                let name = match self.active_tool.take() {
                    Some(tool) if tool.call_id == call_id => tool.name,
                    // A completion without a matching start (replay edge) still
                    // earns its line; the name just isn't known.
                    other => {
                        self.active_tool = other;
                        "tool".to_owned()
                    }
                };
                self.commit(Commit::ToolDone {
                    name,
                    failed: status != ToolCallStatus::Completed,
                });
            }
            ClientEvent::TurnCompleted => {
                self.commit_streaming();
                self.active_tool = None;
                self.running_turn = None;
            }
            ClientEvent::TurnCancelled => {
                self.commit_streaming();
                self.active_tool = None;
                self.approval = None;
                self.running_turn = None;
                self.commit(Commit::Notice("turn cancelled".into()));
            }
            ClientEvent::TurnFailed { category } => {
                self.commit_streaming();
                self.active_tool = None;
                self.approval = None;
                self.running_turn = None;
                self.commit(Commit::Error(format!(
                    "turn failed ({})",
                    category.replace('_', " ")
                )));
            }
            ClientEvent::TurnRefused { refusal } => {
                self.commit_streaming();
                self.active_tool = None;
                self.running_turn = None;
                let detail = refusal
                    .category
                    .map(|category| format!(" ({})", category.replace('_', " ")))
                    .unwrap_or_default();
                self.commit(Commit::Notice(format!("the model refused{detail}")));
            }
            ClientEvent::UserSteered { text } => {
                self.commit(Commit::Notice(format!(
                    "steered from another client: {text}"
                )));
            }
            ClientEvent::ContextTruncated {
                original_tokens,
                fitted_tokens,
            } => {
                self.commit(Commit::Notice(format!(
                    "context truncated: ~{original_tokens} → ~{fitted_tokens} tokens"
                )));
            }
            ClientEvent::UserQuestionsAsked => {
                self.commit(Commit::Notice(
                    "the model is asking questions — answer them in the desktop app; \
                     interactive questions aren't supported in the TUI yet"
                        .into(),
                ));
            }
            ClientEvent::PlanProposed => {
                self.commit(Commit::Notice(
                    "a plan is awaiting review — decide it in the desktop app; \
                     plan mode isn't supported in the TUI yet"
                        .into(),
                ));
            }
            ClientEvent::Unknown => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.running_turn.is_some() {
                self.cancel_turn();
            } else {
                self.should_quit = true;
            }
            return;
        }
        if self.approval.is_some() {
            // The composer is suspended while a decision is pending.
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.decide(true),
                KeyCode::Char('n') | KeyCode::Char('N') => self.decide(false),
                _ => {}
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            if self.composer.is_empty() {
                self.should_quit = true;
            }
            return;
        }
        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.composer.insert_newline();
            }
            KeyCode::Enter => self.try_send(),
            _ => {
                self.composer.input(Input::from(key));
            }
        }
    }

    fn try_send(&mut self) {
        if self.running_turn.is_some() {
            self.flash("a turn is still running — ctrl+c cancels it");
            return;
        }
        if self.send_pending {
            return;
        }
        let content = self.composer.lines().join("\n");
        if content.trim().is_empty() {
            return;
        }
        self.send_pending = true;
        self.composer = new_composer(Vec::new());
        let turn_id = TurnId::new();
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.post_message(chat, turn_id, &content).await {
                Ok(()) => ActionOutcome::Sent { turn_id, content },
                Err(error) => ActionOutcome::SendFailed {
                    content,
                    error: error.to_string(),
                },
            };
            let _ = actions.send(outcome);
        });
    }

    fn cancel_turn(&mut self) {
        let Some(turn_id) = self.running_turn else {
            return;
        };
        self.flash("cancelling…");
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            if let Err(error) = client.cancel_turn(chat, turn_id).await {
                let _ = actions.send(ActionOutcome::CancelFailed(error.to_string()));
            }
        });
    }

    fn decide(&mut self, approve: bool) {
        let Some(pending) = self.approval.take() else {
            return;
        };
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            if let Err(error) = client.decide_approval(chat, pending.call_id, approve).await {
                let _ = actions.send(ActionOutcome::DecisionFailed(error.to_string()));
            }
        });
    }

    fn on_outcome(&mut self, outcome: ActionOutcome) {
        match outcome {
            ActionOutcome::Sent { turn_id, content } => {
                self.send_pending = false;
                self.commit(Commit::UserText(content));
                self.running_turn = Some(turn_id);
            }
            ActionOutcome::SendFailed { content, error } => {
                self.send_pending = false;
                self.composer = new_composer(content.split('\n').map(str::to_owned).collect());
                self.commit(Commit::Error(format!("message not sent: {error}")));
            }
            ActionOutcome::CancelFailed(error) => {
                self.commit(Commit::Error(format!("cancel failed: {error}")));
            }
            ActionOutcome::DecisionFailed(error) => {
                self.commit(Commit::Error(format!(
                    "approval decision failed: {error} — decide it in the desktop app"
                )));
            }
            ActionOutcome::Reconnected(_) | ActionOutcome::ReconnectFailed(_) => {
                // Handled by the loop, which owns the socket slot.
            }
        }
    }

    fn on_tick(&mut self) {
        if self.running_turn.is_some() || self.active_tool.is_some() {
            self.spinner = self.spinner.wrapping_add(1);
        }
        if let Some((_, remaining)) = &mut self.flash {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.flash = None;
            }
        }
    }

    /// The live region's transient stack, bottom-anchored within `max` lines:
    /// blank padding goes on top, so content sits directly above the composer.
    fn transient_lines(&self, width: usize, max: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if !self.thinking.is_empty() {
            lines.extend(render::thinking_lines(&self.thinking, width));
        }
        if !self.streaming.is_empty() {
            lines.extend(render::streaming_lines(&self.streaming, width));
        }
        if let Some(tool) = &self.active_tool {
            lines.push(render::tool_running_line(&tool.name, self.spinner()));
        }
        if let Some(approval) = &self.approval {
            lines.extend(render::approval_card(
                &approval.action,
                &approval.approval,
                approval.auto_judging,
                approval.preview.as_deref(),
                width,
            ));
        }
        if lines.len() > max {
            lines.drain(..lines.len() - max);
        }
        let pad = max.saturating_sub(lines.len());
        let mut anchored = vec![Line::default(); pad];
        anchored.extend(lines);
        anchored
    }

    /// The footer's left side: state segment in the accent, key hints muted,
    /// separated by a dim "·".
    fn footer_line(&self) -> Line<'static> {
        let sep = || Span::styled(" · ", theme::muted());
        if let Some((message, _)) = &self.flash {
            return Line::from(Span::styled(message.clone(), theme::accent()));
        }
        if self.approval.is_some() {
            Line::from(vec![
                Span::styled("awaiting approval", theme::accent_bold()),
                sep(),
                Span::styled("y approve", theme::muted()),
                sep(),
                Span::styled("n reject", theme::muted()),
                sep(),
                Span::styled("ctrl+c cancel", theme::muted()),
            ])
        } else if self.running_turn.is_some() {
            Line::from(vec![
                Span::styled(format!("{} working…", self.spinner()), theme::accent()),
                sep(),
                Span::styled("ctrl+c cancel", theme::muted()),
            ])
        } else {
            Line::from(vec![
                Span::styled("ready", theme::muted()),
                sep(),
                Span::styled("enter send", theme::muted()),
                sep(),
                Span::styled("alt+enter newline", theme::muted()),
                sep(),
                Span::styled("ctrl+c quit", theme::muted()),
            ])
        }
    }
}

fn new_composer(lines: Vec<String>) -> TextArea<'static> {
    let lines = if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    };
    let mut composer = TextArea::new(lines);
    composer.set_cursor_line_style(Style::default());
    composer.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
    composer.set_placeholder_text("type a message");
    composer.set_placeholder_style(theme::muted());
    composer
}

/// Restores the terminal on drop; the panic hook does the same for panics.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The inline viewport's height, clamped to the terminal's rows.
    live_height: u16,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode().map_err(terminal_error)?;
        let mut stdout = io::stdout();
        // Bracketed paste keeps pasted newlines from firing sends; the keyboard
        // enhancement flags are how terminals that support it deliver
        // shift+enter distinctly. Both are no-ops where unsupported.
        execute!(
            stdout,
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .map_err(terminal_error)?;
        let live_height =
            LIVE_HEIGHT.min(crossterm::terminal::size().map_or(LIVE_HEIGHT, |(_, r)| r));
        let terminal = Terminal::with_options(
            CrosstermBackend::new(stdout),
            TerminalOptions {
                viewport: Viewport::Inline(live_height),
            },
        )
        .map_err(terminal_error)?;
        Ok(Self {
            terminal,
            live_height,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            Show
        );
        let _ = disable_raw_mode();
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            Show
        );
        previous(info);
    }));
}

fn terminal_error(error: io::Error) -> AgentError {
    AgentError::msg(format!("terminal error: {error}"))
}

/// Pending forever when the socket is gone, so the `select!` arm simply never
/// fires between a close and a reconnect.
async fn next_frame(
    socket: &mut Option<EventSocket>,
) -> Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>> {
    match socket {
        Some(socket) => socket.next().await,
        None => std::future::pending().await,
    }
}

/// Run the chat until the user quits. Terminal state is restored on every
/// exit path, panic included.
pub async fn run(client: Client, chat: ChatId, resumed: bool) -> Result<()> {
    install_panic_hook();
    let mut guard = TerminalGuard::new()?;

    let (actions, mut outcomes) = mpsc::unbounded_channel();
    let mut app = App::new(client, chat, actions);
    // The banner is the first thing committed to scrollback.
    let header = render::header_lines(resumed, &app.short_id);
    app.commit(Commit::Lines(header));
    let mut socket = match app.client.open_events(chat, 0).await {
        Ok(socket) => Some(socket),
        Err(error) => {
            app.commit(Commit::Error(format!(
                "event stream failed to open: {error}"
            )));
            None
        }
    };
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(TICK);

    loop {
        tokio::select! {
            key = keys.next() => {
                match key {
                    Some(Ok(Event::Key(key))) => app.on_key(key),
                    Some(Ok(Event::Paste(text))) => {
                        if app.approval.is_none() {
                            app.composer.insert_str(text);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            frame = next_frame(&mut socket) => {
                match frame {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ChatFrame>(&text)
                    {
                        Ok(ChatFrame::Event(frame)) => app.on_frame(frame),
                        Ok(ChatFrame::Metadata(_)) => {}
                        // An undecodable frame is skipped, not fatal.
                        Err(_) => {}
                    },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        socket = None;
                        app.commit(Commit::Notice(
                            "event stream closed; reconnecting…".into(),
                        ));
                        let (client, chat, actions) =
                            (app.client.clone(), app.chat, app.actions.clone());
                        let after = app.last_seq;
                        tokio::spawn(async move {
                            tokio::time::sleep(RECONNECT_DELAY).await;
                            let outcome = match client.open_events(chat, after).await {
                                Ok(socket) => ActionOutcome::Reconnected(Box::new(socket)),
                                Err(error) => {
                                    ActionOutcome::ReconnectFailed(error.to_string())
                                }
                            };
                            let _ = actions.send(outcome);
                        });
                    }
                    Some(Ok(_)) => {}
                }
            }
            Some(outcome) = outcomes.recv() => {
                match outcome {
                    ActionOutcome::Reconnected(new_socket) => {
                        socket = Some(*new_socket);
                        app.commit(Commit::Notice("reconnected".into()));
                    }
                    ActionOutcome::ReconnectFailed(error) => {
                        app.commit(Commit::Error(format!(
                            "event stream reconnect failed: {error} — restart the TUI to resume"
                        )));
                    }
                    other => app.on_outcome(other),
                }
            }
            _ = tick.tick() => app.on_tick(),
        }

        flush_commits(&mut app, &mut guard.terminal)?;
        draw(&mut app, &mut guard.terminal, guard.live_height)?;
        if app.should_quit {
            return Ok(());
        }
    }

    Ok(())
}

/// Commit finished blocks to real scrollback above the live region.
fn flush_commits(app: &mut App, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    if app.commits.is_empty() {
        return Ok(());
    }
    let width = terminal.size().map_err(terminal_error)?.width.max(8) as usize;
    let lines: Vec<Line<'static>> = app
        .commits
        .drain(..)
        .flat_map(|commit| commit.lines(width))
        .collect();
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    terminal
        .insert_before(height, |buf| {
            Paragraph::new(lines).render(Rect::new(0, 0, buf.area.width, height), buf);
        })
        .map_err(terminal_error)
}

fn draw(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    live_height: u16,
) -> Result<()> {
    let width = terminal.size().map_err(terminal_error)?.width as usize;
    let composer_rows = (app.composer.lines().len() as u16).clamp(1, MAX_COMPOSER_ROWS);
    let transient_rows = live_height.saturating_sub(composer_rows + 1);
    let transient = app.transient_lines(width, transient_rows as usize);
    terminal
        .draw(|frame| {
            let rows = Layout::vertical([
                Constraint::Length(transient_rows),
                Constraint::Length(composer_rows),
                Constraint::Length(1),
            ])
            .split(frame.area());
            frame.render_widget(Paragraph::new(transient), rows[0]);
            // The composer gutter matches the scrollback marker for sent user
            // messages: accent ❯ on the first row, a muted │ on wrapped rows.
            let composer_cols =
                Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).split(rows[1]);
            let gutter: Vec<Line<'static>> = (0..composer_rows)
                .map(|row| {
                    if row == 0 {
                        Line::from(Span::styled("❯", theme::accent_bold()))
                    } else {
                        Line::from(Span::styled("│", theme::muted()))
                    }
                })
                .collect();
            frame.render_widget(Paragraph::new(gutter), composer_cols[0]);
            frame.render_widget(&app.composer, composer_cols[1]);
            let mut footer = app.footer_line();
            // The short chat id rides the right edge when there's room.
            let short_id = format!("chat {}", app.short_id);
            let pad = width.saturating_sub(footer.width() + short_id.len());
            if pad >= 2 {
                footer.spans.push(Span::raw(" ".repeat(pad)));
                footer.spans.push(Span::styled(short_id, theme::muted()));
            }
            frame.render_widget(Paragraph::new(footer), rows[2]);
        })
        .map_err(terminal_error)?;
    Ok(())
}

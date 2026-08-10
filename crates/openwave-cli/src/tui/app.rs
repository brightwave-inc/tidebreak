//! The interactive loop: terminal input, socket frames, and a redraw tick in
//! one `select!`. Finished blocks (user/assistant messages, tool results,
//! notices) are committed to real scrollback with `insert_before`; the inline
//! viewport below them repaints the live region.

use std::collections::{HashMap, HashSet};
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
use openwave_core::{AgentError, AgentRunId, CallId, ChatId, Result, TurnId};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::overlays::{
    AgentsOverlay, ChatsOverlay, HelpOverlay, ModeOverlay, ModelOverlay, MoveOverlay,
    OverlayOutcome, QuestionsOverlay,
};
use super::render::{self, Commit};
use super::theme;
use crate::api::client::{Client, EventSocket};
use crate::api::wire::{
    AgentRunSnapshot, ChatFrame, ChatSummary, ClientEvent, MetadataFrame, PendingQuestions,
    SequencedFrame, ToolCallStatus,
};

/// Fixed height of the repainting region: transient stack, composer, footer.
/// Clamped to the terminal at startup.
const LIVE_HEIGHT: u16 = 12;
/// Composer rows beyond this scroll inside the textarea.
const MAX_COMPOSER_ROWS: u16 = 5;
const TICK: Duration = Duration::from_millis(100);
/// One delayed reconnect after an unexpected socket close; further closes give
/// up rather than hammering the server.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Outcomes of HTTP actions spawned off the UI loop, plus the one reconnect.
enum ActionOutcome {
    Sent {
        turn_id: TurnId,
        content: String,
    },
    SendFailed {
        content: String,
        error: String,
    },
    CancelFailed(String),
    DecisionFailed(String),
    Reconnected(Box<EventSocket>),
    ReconnectFailed(String),
    /// The startup transcript + pending-state bundle for the current chat.
    Hydrated(Box<Hydration>),
    HydrationFailed(String),
    /// A chat list refresh for the switcher, with attention markers.
    ChatList(Vec<ChatSummary>, HashSet<ChatId>),
    ChatListFailed(String),
    /// Background-agent runs for the current chat.
    AgentRuns(Vec<AgentRunSnapshot>),
    /// One run's activity timeline, for the agents overlay's detail view.
    AgentActivity(AgentRunId, Vec<crate::api::wire::AgentActivityItem>),
    /// The selectable model catalog.
    Models(Vec<crate::api::wire::ModelInfo>),
    /// The projects list for the move picker.
    Projects(Vec<crate::api::wire::ProjectSummary>),
    /// A parked question block is ready to answer.
    QuestionsReady(PendingQuestions),
    /// A proposed plan is ready to review.
    PlanReady(crate::api::wire::PendingPlan),
    /// A new chat was created server-side; attach to it.
    ChatCreated(ChatId),
    ChatOpFailed(String),
    Steered {
        content: String,
    },
    PlanDecided,
    QuestionsAnswered,
    GenericOk(String),
}

/// Everything the TUI pulls to (re)hydrate a chat it just attached to.
struct Hydration {
    chat: ChatSummary,
    transcript: crate::api::wire::Transcript,
    approvals: Vec<crate::api::wire::PendingApprovalSnapshot>,
    plans: Vec<crate::api::wire::PendingPlan>,
    questions: Vec<PendingQuestions>,
    runs: Vec<AgentRunSnapshot>,
}

struct PendingApproval {
    call_id: CallId,
    action: String,
    approval: String,
    auto_judging: bool,
    preview: Option<serde_json::Value>,
    grant_rungs: Vec<crate::api::wire::GrantRung>,
    /// Whether the user pressed `a` to pick a standing grant.
    picking_grant: bool,
}

struct ActiveTool {
    call_id: CallId,
    name: String,
}

/// A plan review in flight: the proposed plan plus the feedback box state.
struct PendingPlanReview {
    call_id: CallId,
    title: String,
    plan: String,
    /// Whether the feedback textarea is open (reject-with-changes).
    feedback: Option<TextArea<'static>>,
}

/// Which overlay, if any, owns the keyboard right now.
enum Overlay {
    Chats(ChatsOverlay),
    Agents(AgentsOverlay),
    Models(ModelOverlay),
    Mode(ModeOverlay),
    Move(MoveOverlay),
    Questions(QuestionsOverlay),
    Help(HelpOverlay),
}

struct App {
    client: Client,
    chat: ChatId,
    /// First group of the chat's UUID, for the header and the footer's right
    /// edge.
    short_id: String,
    title: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    permission_mode: String,
    actions: mpsc::UnboundedSender<ActionOutcome>,
    composer: TextArea<'static>,
    /// Assistant text of the in-flight turn, appended by `TextDelta`.
    streaming: String,
    /// Reasoning tail shown dimmed while the model thinks.
    thinking: String,
    active_tool: Option<ActiveTool>,
    approval: Option<PendingApproval>,
    plan: Option<PendingPlanReview>,
    questions: Option<PendingQuestions>,
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
    /// The open overlay, if any. While one is open it owns the keyboard.
    overlay: Option<Overlay>,
    /// Background runs for this chat, keyed by run id.
    runs: HashMap<AgentRunId, AgentRunSnapshot>,
    /// Chats with a parked prompt somewhere, for the switcher's markers.
    attention: HashSet<ChatId>,
    /// Context tokens the last completed turn reported, for the footer meter.
    context_tokens: u64,
    /// The context window of the current model, if the catalog has loaded.
    context_window: Option<u32>,
    /// The model catalog, loaded once and reused by the picker.
    catalog: Option<Vec<crate::api::wire::ModelInfo>>,
    /// Whether the first hydration for this chat has landed. The event socket
    /// opens only once this is true, at the watermark hydration set.
    hydrated: bool,
    /// In-flight reconnect task identity, so the loop doesn't open a second
    /// socket while one is already connecting.
    reconnecting: Option<()>,
    /// The cursor in the slash-command autocomplete list, when the composer
    /// is a `/` prefix.
    slash_selected: usize,
}

impl App {
    fn new(client: Client, chat: ChatId, actions: mpsc::UnboundedSender<ActionOutcome>) -> Self {
        Self {
            client,
            short_id: chat.to_string().chars().take(8).collect(),
            chat,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: "ask".to_owned(),
            actions,
            composer: super::composer::new(Vec::new()),
            streaming: String::new(),
            thinking: String::new(),
            active_tool: None,
            approval: None,
            plan: None,
            questions: None,
            running_turn: None,
            send_pending: false,
            last_seq: 0,
            commits: Vec::new(),
            flash: None,
            spinner: 0,
            should_quit: false,
            overlay: None,
            runs: HashMap::new(),
            attention: HashSet::new(),
            context_tokens: 0,
            context_window: None,
            catalog: None,
            hydrated: false,
            reconnecting: None,
            slash_selected: 0,
        }
    }

    fn commit(&mut self, commit: Commit) {
        self.commits.push(commit);
    }

    /// Flush any streamed assistant text as a finished block.
    fn commit_streaming(&mut self) {
        let streamed = std::mem::take(&mut self.streaming);
        if !streamed.trim().is_empty() {
            self.commit(Commit::AssistantText {
                text: streamed,
                at: Some(chrono::Utc::now()),
            });
        }
        let thinking = std::mem::take(&mut self.thinking);
        if !thinking.trim().is_empty() {
            self.commit(Commit::Reasoning { text: thinking });
        }
    }

    fn flash(&mut self, message: impl Into<String>) {
        // ~4 seconds at the 100ms tick.
        self.flash = Some((message.into(), 40));
    }

    fn spinner(&self) -> char {
        render::spinner_at(self.spinner)
    }

    fn on_frame(&mut self, frame: SequencedFrame) {
        self.last_seq = frame.seq;
        self.on_event(frame.event);
    }

    fn on_metadata(&mut self, metadata: MetadataFrame) {
        match metadata {
            MetadataFrame::Titled { title } => {
                self.title = Some(title);
            }
            MetadataFrame::SandboxPreparing { preparing } => {
                if preparing {
                    self.flash("preparing the sandbox image — this can take a few minutes");
                }
            }
            MetadataFrame::FileChangesRecorded | MetadataFrame::Unknown => {}
        }
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
                // A spawn shows up as an agent run too; note it live.
                self.active_tool = Some(ActiveTool { call_id, name });
            }
            ClientEvent::ToolCallArgsDelta => {}
            ClientEvent::ApprovalRequired {
                call_id,
                action,
                approval,
                auto_judging,
                grant_rungs,
                preview,
            } => {
                self.commit_streaming();
                self.approval = Some(PendingApproval {
                    call_id,
                    action,
                    approval,
                    auto_judging,
                    preview,
                    grant_rungs,
                    picking_grant: false,
                });
            }
            ClientEvent::ApprovalDecided { call_id, .. } => {
                if self.approval.as_ref().is_some_and(|p| p.call_id == call_id) {
                    self.approval = None;
                }
            }
            ClientEvent::ToolCallCompleted {
                call_id,
                status,
                action,
                result,
            } => {
                let name = match self.active_tool.take() {
                    Some(tool) if tool.call_id == call_id => tool.name,
                    // A completion without a matching start (replay edge) still
                    // earns its line; the name just isn't known.
                    other => {
                        self.active_tool = other;
                        "tool".to_owned()
                    }
                };
                let action = action.as_ref().and_then(render::preview_summary);
                let output = result.as_ref().and_then(render::exec_output);
                // A spawn's completion names the delegated task as an agent
                // run; surface it as an agent line rather than a bare tool.
                if name == "spawn_sandbox_agent" {
                    let task = action
                        .as_deref()
                        .and_then(|a| a.strip_prefix("background agent: "))
                        .unwrap_or("background agent")
                        .to_owned();
                    self.commit(Commit::AgentRun {
                        task,
                        status: if status == ToolCallStatus::Completed {
                            "spawned".into()
                        } else {
                            "declined".into()
                        },
                    });
                    self.refresh_runs();
                } else {
                    self.commit(Commit::ToolDone {
                        name,
                        failed: status != ToolCallStatus::Completed,
                        cancelled: status == ToolCallStatus::Cancelled,
                        action,
                        output,
                    });
                }
            }
            ClientEvent::TurnCompleted { usage } => {
                self.commit_streaming();
                self.active_tool = None;
                self.running_turn = None;
                self.context_tokens = usage.context_tokens();
            }
            ClientEvent::TurnCancelled { usage } => {
                self.commit_streaming();
                self.active_tool = None;
                self.approval = None;
                self.running_turn = None;
                self.context_tokens = usage.context_tokens();
                self.commit(Commit::Notice("turn cancelled".into()));
            }
            ClientEvent::TurnFailed { category, model } => {
                self.commit_streaming();
                self.active_tool = None;
                self.approval = None;
                self.running_turn = None;
                let mut line = format!("turn failed ({})", category.replace('_', " "));
                if let Some(model) = model {
                    line.push_str(&format!(" — {}", model.id));
                }
                self.commit(Commit::Error(line));
            }
            ClientEvent::TurnRefused { refusal, usage } => {
                self.commit_streaming();
                self.active_tool = None;
                self.running_turn = None;
                self.context_tokens = usage.context_tokens();
                let detail = refusal
                    .category
                    .map(|category| format!(" ({})", category.replace('_', " ")))
                    .unwrap_or_default();
                self.commit(Commit::Notice(format!("the model refused{detail}")));
            }
            ClientEvent::UserSteered { text } => {
                self.commit(Commit::Notice(format!("steered: {text}")));
            }
            ClientEvent::ContextTruncated {
                original_tokens,
                fitted_tokens,
            } => {
                self.commit(Commit::Notice(format!(
                    "context truncated: ~{original_tokens} → ~{fitted_tokens} tokens"
                )));
            }
            ClientEvent::UserQuestionsAsked { call_id } => {
                self.commit_streaming();
                self.refresh_questions();
                if let Some(call_id) = call_id {
                    let _ = call_id;
                }
            }
            ClientEvent::PlanProposed { .. } => {
                self.commit_streaming();
                self.refresh_plans();
            }
            ClientEvent::Unknown => {}
        }
    }

    /// Pull the parked prompts for this chat (approvals are event-driven, but
    /// questions and plans need their recovery reads).
    fn refresh_questions(&mut self) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            match client.list_pending_questions(chat).await {
                Ok(mut pending) => {
                    if let Some(block) = pending.drain(..).next() {
                        let _ = actions.send(ActionOutcome::QuestionsReady(block));
                    }
                }
                Err(error) => {
                    let _ = actions.send(ActionOutcome::ChatOpFailed(error.to_string()));
                }
            }
        });
    }

    fn refresh_plans(&mut self) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            match client.list_pending_plans(chat).await {
                Ok(mut pending) => {
                    if let Some(plan) = pending.drain(..).next() {
                        let _ = actions.send(ActionOutcome::PlanReady(plan));
                    }
                }
                Err(error) => {
                    let _ = actions.send(ActionOutcome::ChatOpFailed(error.to_string()));
                }
            }
        });
    }

    fn refresh_runs(&mut self) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            if let Ok(runs) = client.list_agent_runs(chat).await {
                let _ = actions.send(ActionOutcome::AgentRuns(runs));
            }
        });
    }

    fn refresh_chat_list(&mut self) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let chats = client.list_chats().await;
            let inbox = client.list_inbox().await.unwrap_or_default();
            let attention = inbox.into_iter().map(|item| item.chat_id).collect();
            match chats {
                Ok(chats) => {
                    let _ = actions.send(ActionOutcome::ChatList(chats, attention));
                }
                Err(error) => {
                    let _ = actions.send(ActionOutcome::ChatListFailed(error.to_string()));
                }
            }
        });
    }

    fn refresh_models(&mut self) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            if let Ok(catalog) = client.list_models().await {
                let _ = actions.send(ActionOutcome::Models(catalog.models));
            }
        });
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        // Overlays own the keyboard while open.
        if self.overlay.is_some() {
            self.overlay_key(key);
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
        // The plan review flow suspends the composer. (Questions open as a
        // full overlay from the `QuestionsReady` outcome, not a key path.)
        if self.plan.is_some() {
            self.plan_key(key);
            return;
        }
        if self.approval.is_some() {
            // The composer is suspended while a decision is pending.
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.decide(true, None),
                KeyCode::Char('n') | KeyCode::Char('N') => self.decide(false, None),
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    if let Some(approval) = &mut self.approval {
                        if !approval.grant_rungs.is_empty() {
                            approval.picking_grant = true;
                        }
                    }
                }
                KeyCode::Char(digit @ '1'..='9') => {
                    let index = digit.to_digit(10).unwrap_or(1) as usize - 1;
                    let rung = self
                        .approval
                        .as_ref()
                        .and_then(|a| a.grant_rungs.get(index).copied());
                    if let (Some(true), Some(rung)) =
                        (self.approval.as_ref().map(|a| a.picking_grant), rung)
                    {
                        self.decide(true, Some(rung));
                    }
                }
                KeyCode::Esc => {
                    if let Some(approval) = &mut self.approval {
                        approval.picking_grant = false;
                    }
                }
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
        // Overlay-opening shortcuts.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('o') => {
                    self.open_chats();
                    return;
                }
                KeyCode::Char('g') => {
                    self.open_agents();
                    return;
                }
                KeyCode::Char('m') => {
                    self.open_models();
                    return;
                }
                KeyCode::Char('p') => {
                    self.open_mode();
                    return;
                }
                KeyCode::Char('w') => {
                    self.open_move();
                    return;
                }
                KeyCode::Char('n') => {
                    self.new_chat();
                    return;
                }
                KeyCode::Char('/') => {
                    self.overlay = Some(Overlay::Help(HelpOverlay::new()));
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.composer.insert_newline();
            }
            // Slash autocomplete: when the composer is a `/` prefix, Tab and
            // the arrows walk the matching command list and Enter completes.
            KeyCode::Enter if self.slash_matches().len() == 1 => {
                self.complete_slash();
                self.try_send();
            }
            KeyCode::Tab if !self.slash_matches().is_empty() => {
                let len = self.slash_matches().len();
                self.slash_selected = (self.slash_selected + 1) % len;
            }
            KeyCode::BackTab if !self.slash_matches().is_empty() => {
                let len = self.slash_matches().len();
                self.slash_selected = (self.slash_selected + len - 1) % len;
            }
            KeyCode::Down if !self.slash_matches().is_empty() => {
                let len = self.slash_matches().len();
                self.slash_selected = (self.slash_selected + 1) % len;
            }
            KeyCode::Up if !self.slash_matches().is_empty() => {
                let len = self.slash_matches().len();
                self.slash_selected = (self.slash_selected + len - 1) % len;
            }
            KeyCode::Enter => self.try_send(),
            _ => {
                super::composer::edit_key(&mut self.composer, key);
                self.slash_selected = 0;
            }
        }
    }

    /// The composer text as a slash-command prefix: matches when the input is
    /// a single line starting with `/` and no space yet (still naming the
    /// command). Returns the matching canonical command names.
    fn slash_matches(&self) -> Vec<&'static str> {
        let line = self.composer.lines().first().cloned().unwrap_or_default();
        if self.composer.lines().len() > 1 || !line.starts_with('/') || line.contains(' ') {
            return Vec::new();
        }
        let prefix = line.trim_start_matches('/').to_lowercase();
        super::overlays::SLASH_COMMANDS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| name.starts_with(prefix.as_str()))
            .collect()
    }

    /// Fill the composer with the selected slash command plus a trailing
    /// space, ready for args or Enter.
    fn complete_slash(&mut self) {
        let matches = self.slash_matches();
        let Some(name) = matches.get(self.slash_selected).or(matches.first()) else {
            return;
        };
        self.composer = super::composer::new(vec![format!("/{name} ")]);
        // Cursor to the end.
        self.composer.move_cursor(ratatui_textarea::CursorMove::End);
        self.slash_selected = 0;
    }

    fn overlay_key(&mut self, key: KeyEvent) {
        let Some(overlay) = self.overlay.take() else {
            return;
        };
        let outcome = match overlay {
            Overlay::Chats(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Chats(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Agents(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Agents(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Models(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Models(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Mode(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Mode(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Move(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Move(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Questions(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Questions(overlay));
                    None
                }
                outcome => Some(outcome),
            },
            Overlay::Help(mut overlay) => match overlay.key(key) {
                OverlayOutcome::Stay => {
                    self.overlay = Some(Overlay::Help(overlay));
                    None
                }
                outcome => Some(outcome),
            },
        };
        if let Some(outcome) = outcome {
            self.on_overlay_outcome(outcome);
        }
    }

    fn on_overlay_outcome(&mut self, outcome: OverlayOutcome) {
        match outcome {
            OverlayOutcome::Open(chat) => {
                let switching = chat != self.chat;
                if switching {
                    self.attach(chat);
                }
            }
            OverlayOutcome::NewChat => self.new_chat(),
            OverlayOutcome::Delete(chat) => self.delete_chat(chat),
            OverlayOutcome::Rename(chat, title) => self.rename_chat(chat, title),
            OverlayOutcome::MoveChat(project) => self.move_chat(project),
            OverlayOutcome::SetModel(model) => self.set_model(model),
            OverlayOutcome::SetEffort(effort) => self.set_effort(effort),
            OverlayOutcome::SetMode(mode) => self.set_mode(mode),
            OverlayOutcome::StopAgent(run) => self.stop_agent(run),
            OverlayOutcome::SubmitQuestions(body) => self.submit_questions(body),
            OverlayOutcome::Dismiss | OverlayOutcome::Stay => {}
        }
    }

    fn open_chats(&mut self) {
        self.refresh_chat_list();
        self.overlay = Some(Overlay::Chats(ChatsOverlay::new(Some(self.chat))));
    }

    fn open_agents(&mut self) {
        self.refresh_runs();
        self.overlay = Some(Overlay::Agents(AgentsOverlay::new()));
    }

    fn open_models(&mut self) {
        self.refresh_models();
        self.overlay = Some(Overlay::Models(ModelOverlay::new(
            self.model.clone(),
            self.reasoning_effort.clone(),
        )));
    }

    fn open_mode(&mut self) {
        self.overlay = Some(Overlay::Mode(ModeOverlay::new(&self.permission_mode)));
    }

    fn open_move(&mut self) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            match client.list_projects().await {
                Ok(projects) => {
                    let _ = actions.send(ActionOutcome::Projects(projects));
                }
                Err(error) => {
                    let _ = actions.send(ActionOutcome::ChatOpFailed(error.to_string()));
                }
            }
        });
    }

    /// The current questions overlay, if one is open.
    fn questions_overlay(&mut self) -> Option<&mut QuestionsOverlay> {
        match &mut self.overlay {
            Some(Overlay::Questions(overlay)) => Some(overlay),
            _ => None,
        }
    }

    fn plan_key(&mut self, key: KeyEvent) {
        let Some(plan) = &mut self.plan else {
            return;
        };
        if let Some(feedback) = &mut plan.feedback {
            match key.code {
                KeyCode::Esc => plan.feedback = None,
                KeyCode::Enter => {
                    let text = feedback.lines().join("\n");
                    let call_id = plan.call_id;
                    self.decide_plan(call_id, false, Some(text));
                }
                _ => super::composer::single_line_key(feedback, key),
            }
            return;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let call_id = plan.call_id;
                self.decide_plan(call_id, true, None);
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let call_id = plan.call_id;
                self.decide_plan(call_id, false, None);
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                plan.feedback = Some(super::composer::new_single_line("", "what should change?"));
            }
            _ => {}
        }
    }

    /// The slash-command table, Claude Code style: `/name args` in the
    /// composer. Each entry is the canonical name plus a one-line blurb the
    /// help overlay and the autocomplete list share.
    /// Run a slash command. Returns true when the input was a command (so the
    /// composer resets and nothing is sent to the model). The command table
    /// lives in `overlays::SLASH_COMMANDS`, shared with the help overlay.
    fn run_slash_command(&mut self, input: &str) -> bool {
        let trimmed = input.trim();
        let Some(body) = trimmed.strip_prefix('/') else {
            return false;
        };
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_lowercase();
        let arg = parts.next().map(str::trim).unwrap_or("");
        // The composer is consumed either way; unknown commands just flash.
        self.composer = super::composer::new(Vec::new());
        match name.as_str() {
            "model" | "models" => self.open_models(),
            "effort" => {
                if arg.is_empty() {
                    // No argument: open the model overlay's effort picker.
                    self.open_models();
                    if let Some(Overlay::Models(overlay)) = &mut self.overlay {
                        overlay.picking_effort = true;
                    }
                } else {
                    self.set_effort(Some(arg.to_owned()));
                }
            }
            "mode" | "permission" | "permissions" => {
                if arg.is_empty() {
                    self.open_mode();
                } else {
                    self.set_mode(arg.to_owned());
                }
            }
            "chats" | "chat" | "threads" | "switch" => self.open_chats(),
            "new" => self.new_chat(),
            "rename" | "title" => {
                if arg.is_empty() {
                    self.flash("/rename <title>");
                } else {
                    let chat = self.chat;
                    self.rename_chat(chat, arg.to_owned());
                }
            }
            "move" | "project" => self.open_move(),
            "agents" | "agent" | "runs" => self.open_agents(),
            "questions" | "question" | "ask" => {
                let chat = self.chat;
                self.refresh_questions_for(chat);
            }
            "help" | "?" => self.overlay = Some(Overlay::Help(HelpOverlay::new())),
            "quit" | "exit" | "q" => self.should_quit = true,
            _ => {
                self.flash(format!("unknown command /{name} — try /help"));
            }
        }
        true
    }

    /// Fetch this chat's parked questions and open the answer overlay.
    fn refresh_questions_for(&mut self, chat: ChatId) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            match client.list_pending_questions(chat).await {
                Ok(mut pending) => {
                    if let Some(block) = pending.drain(..).next() {
                        let _ = actions.send(ActionOutcome::QuestionsReady(block));
                    } else {
                        let _ =
                            actions.send(ActionOutcome::GenericOk("no pending questions".into()));
                    }
                }
                Err(error) => {
                    let _ = actions.send(ActionOutcome::ChatOpFailed(error.to_string()));
                }
            }
        });
    }

    fn try_send(&mut self) {
        let content = self.composer.lines().join("\n");
        if content.trim().is_empty() {
            return;
        }
        // Slash commands are handled locally, never sent to the model.
        if content.trim_start().starts_with('/') && self.run_slash_command(&content) {
            return;
        }
        // A running turn steers rather than rejects: the desktop's composer
        // becomes "guide the active response", and so does this one.
        if let Some(turn_id) = self.running_turn {
            self.composer = super::composer::new(Vec::new());
            let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
            tokio::spawn(async move {
                let outcome = match client.steer(chat, turn_id, TurnId::new(), &content).await {
                    Ok(()) => ActionOutcome::Steered { content },
                    Err(error) => ActionOutcome::SendFailed {
                        content,
                        error: error.to_string(),
                    },
                };
                let _ = actions.send(outcome);
            });
            return;
        }
        if self.send_pending {
            return;
        }
        self.send_pending = true;
        self.composer = super::composer::new(Vec::new());
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

    fn decide(&mut self, approve: bool, grant: Option<crate::api::wire::GrantRung>) {
        let Some(pending) = self.approval.take() else {
            return;
        };
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            if let Err(error) = client
                .decide_approval(
                    chat,
                    pending.call_id,
                    approve,
                    "declined from terminal",
                    grant,
                )
                .await
            {
                let _ = actions.send(ActionOutcome::DecisionFailed(error.to_string()));
            }
        });
    }

    fn decide_plan(&mut self, call_id: CallId, accept: bool, feedback: Option<String>) {
        self.plan = None;
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client
                .decide_plan(chat, call_id, accept, feedback.as_deref(), None)
                .await
            {
                Ok(()) => ActionOutcome::PlanDecided,
                Err(error) => ActionOutcome::DecisionFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn submit_questions(&mut self, body: serde_json::Value) {
        let call_id = self
            .questions_overlay()
            .map(|overlay| overlay.call_id())
            .or_else(|| self.questions.as_ref().map(|pending| pending.call_id));
        let Some(call_id) = call_id else {
            return;
        };
        self.questions = None;
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.answer_questions(chat, call_id, body).await {
                Ok(()) => ActionOutcome::QuestionsAnswered,
                Err(error) => ActionOutcome::DecisionFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    /// The footer shows the current chat's title when it has one.
    fn title_label(&self) -> String {
        self.title
            .as_deref()
            .map(|title| render::truncate(title, 24))
            .unwrap_or_else(|| format!("chat {}", self.short_id))
    }

    fn new_chat(&mut self) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.create_chat().await {
                Ok(chat) => ActionOutcome::ChatCreated(chat),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn delete_chat(&mut self, chat: ChatId) {
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.delete_chat(chat).await {
                Ok(()) => ActionOutcome::GenericOk("chat deleted".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn rename_chat(&mut self, chat: ChatId, title: String) {
        let title = (!title.trim().is_empty()).then_some(title);
        let (client, actions) = (self.client.clone(), self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.rename_chat(chat, title.as_deref()).await {
                Ok(()) => ActionOutcome::GenericOk("renamed".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn move_chat(&mut self, project: Option<openwave_core::ProjectId>) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.set_chat_project(chat, project).await {
                Ok(_) => ActionOutcome::GenericOk("moved".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn set_model(&mut self, model: Option<String>) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.set_chat_model(chat, model.as_deref()).await {
                Ok(()) => ActionOutcome::GenericOk("model updated".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn set_effort(&mut self, effort: Option<String>) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.set_chat_effort(chat, effort.as_deref()).await {
                Ok(()) => ActionOutcome::GenericOk("effort updated".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn set_mode(&mut self, mode: String) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.set_chat_permission_mode(chat, Some(&mode)).await {
                Ok(_) => ActionOutcome::GenericOk("permission mode updated".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn stop_agent(&mut self, run: AgentRunId) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let outcome = match client.cancel_agent_run(chat, run).await {
                Ok(()) => ActionOutcome::GenericOk("agent stopping…".into()),
                Err(error) => ActionOutcome::ChatOpFailed(error.to_string()),
            };
            let _ = actions.send(outcome);
        });
    }

    fn on_outcome(&mut self, outcome: ActionOutcome) {
        match outcome {
            ActionOutcome::Sent { turn_id, content } => {
                self.send_pending = false;
                self.commit(Commit::UserText {
                    text: content,
                    at: Some(chrono::Utc::now()),
                    images: Vec::new(),
                    files: Vec::new(),
                });
                self.running_turn = Some(turn_id);
            }
            ActionOutcome::Steered { content } => {
                self.commit(Commit::UserText {
                    text: content,
                    at: Some(chrono::Utc::now()),
                    images: Vec::new(),
                    files: Vec::new(),
                });
            }
            ActionOutcome::SendFailed { content, error } => {
                self.send_pending = false;
                self.composer =
                    super::composer::new(content.split('\n').map(str::to_owned).collect());
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
            ActionOutcome::Hydrated(hydration) => self.apply_hydration(hydration),
            ActionOutcome::HydrationFailed(error) => {
                self.commit(Commit::Error(format!("could not load the chat: {error}")));
            }
            ActionOutcome::ChatList(chats, attention) => {
                self.attention = attention;
                if let Some(Overlay::Chats(overlay)) = &mut self.overlay {
                    overlay.set_chats(chats, self.attention.clone());
                }
            }
            ActionOutcome::ChatListFailed(error) => {
                self.flash(format!("couldn't load chats: {error}"));
            }
            ActionOutcome::AgentRuns(runs) => {
                self.runs = runs
                    .into_iter()
                    .filter(|run| run.tier.as_deref() != Some("foreground"))
                    .map(|run| (run.id, run))
                    .collect();
                if let Some(Overlay::Agents(overlay)) = &mut self.overlay {
                    overlay.set_runs(self.runs.values().cloned().collect());
                }
            }
            ActionOutcome::AgentActivity(run, items) => {
                if let Some(Overlay::Agents(overlay)) = &mut self.overlay {
                    overlay.set_detail(run, items);
                }
            }
            ActionOutcome::Models(models) => {
                if let Some(current) = &self.model {
                    self.context_window = models
                        .iter()
                        .find(|model| &model.key == current || &model.id == current)
                        .map(|model| model.context_window);
                } else {
                    // No explicit selection: the default model's window.
                    self.context_window = models
                        .iter()
                        .find(|model| model.available)
                        .map(|model| model.context_window);
                }
                self.catalog = Some(models.clone());
                if let Some(Overlay::Models(overlay)) = &mut self.overlay {
                    overlay.set_models(models);
                }
            }
            ActionOutcome::ChatCreated(chat) => {
                self.attach(chat);
            }
            ActionOutcome::ChatOpFailed(error) => {
                self.commit(Commit::Error(error));
            }
            ActionOutcome::PlanDecided => {
                self.commit(Commit::Notice("plan decided".into()));
            }
            ActionOutcome::QuestionsAnswered => {
                self.commit(Commit::Notice("answers sent".into()));
            }
            ActionOutcome::GenericOk(message) => {
                self.flash(message);
                self.refresh_chat_list();
            }
            ActionOutcome::Projects(projects) => {
                self.overlay = Some(Overlay::Move(MoveOverlay::new(projects)));
            }
            ActionOutcome::QuestionsReady(pending) => {
                // Questions park the turn; open the answer form immediately
                // rather than waiting for a keypress.
                self.overlay = Some(Overlay::Questions(QuestionsOverlay::new(pending)));
            }
            ActionOutcome::PlanReady(plan) => {
                self.plan = Some(PendingPlanReview {
                    call_id: plan.call_id,
                    title: plan.title,
                    plan: plan.plan,
                    feedback: None,
                });
            }
        }
    }

    /// Move the session to another chat: close the old socket, reset the live
    /// state, and rehydrate from the transcript.
    fn attach(&mut self, chat: ChatId) {
        self.chat = chat;
        self.short_id = chat.to_string().chars().take(8).collect();
        self.title = None;
        self.streaming.clear();
        self.thinking.clear();
        self.active_tool = None;
        self.approval = None;
        self.plan = None;
        self.questions = None;
        self.running_turn = None;
        self.send_pending = false;
        self.runs.clear();
        self.hydrated = false;
        self.reconnecting = None;
        // The loop notices `hydrated == false` once the fresh hydration lands
        // and opens the new chat's socket at its watermark.
        // A rule separates the previous chat's history from this one's.
        self.commit(Commit::Lines(vec![
            Line::from(Span::styled("─".repeat(40), theme::muted())),
            Line::default(),
        ]));
        self.commit(Commit::Lines(render::header_lines(true, &self.short_id)));
        self.hydrate();
    }

    /// Pull the transcript + parked state for the current chat.
    fn hydrate(&mut self) {
        let (client, chat, actions) = (self.client.clone(), self.chat, self.actions.clone());
        tokio::spawn(async move {
            let chat_record = client.get_chat(chat).await;
            let transcript = client.get_transcript(chat).await;
            let approvals = client
                .list_pending_approvals(chat)
                .await
                .unwrap_or_default();
            let plans = client.list_pending_plans(chat).await.unwrap_or_default();
            let questions = client
                .list_pending_questions(chat)
                .await
                .unwrap_or_default();
            let runs = client.list_agent_runs(chat).await.unwrap_or_default();
            let outcome = match (chat_record, transcript) {
                (Ok(chat), Ok(transcript)) => ActionOutcome::Hydrated(Box::new(Hydration {
                    chat,
                    transcript,
                    approvals,
                    plans,
                    questions,
                    runs,
                })),
                (Err(error), _) | (_, Err(error)) => {
                    ActionOutcome::HydrationFailed(error.to_string())
                }
            };
            let _ = actions.send(outcome);
        });
    }

    /// Render the hydrated transcript into scrollback commits.
    fn apply_hydration(&mut self, hydration: Box<Hydration>) {
        let Hydration {
            chat,
            transcript,
            approvals,
            plans,
            questions,
            runs,
        } = *hydration;
        self.title = chat.title;
        self.model = chat.model;
        self.reasoning_effort = chat.reasoning_effort;
        self.permission_mode = chat.permission_mode.unwrap_or_else(|| "ask".into());
        self.last_seq = self.last_seq.max(transcript.last_event_seq);
        self.hydrated = true;

        // History replay: user and assistant messages with their timestamps,
        // and the settled tool activity between them.
        for message in &transcript.messages {
            match message.role {
                crate::api::wire::TranscriptRole::User => {
                    self.commit(Commit::UserText {
                        text: message.content.clone(),
                        at: Some(message.created_at),
                        images: message.image_attachments.clone().unwrap_or_default(),
                        files: message.file_attachments.clone().unwrap_or_default(),
                    });
                }
                crate::api::wire::TranscriptRole::Assistant => {
                    self.commit(Commit::AssistantText {
                        text: message.content.clone(),
                        at: Some(message.created_at),
                    });
                }
                crate::api::wire::TranscriptRole::System => {
                    self.commit(Commit::Notice(message.content.clone()));
                }
                crate::api::wire::TranscriptRole::Other => {}
            }
        }
        for tool in &transcript.tool_activity {
            if tool.tool == "spawn_sandbox_agent" {
                continue; // agent runs get their own lines below
            }
            let status = tool
                .status
                .unwrap_or(crate::api::wire::ToolCallStatus::Completed);
            self.commit(Commit::ToolDone {
                name: tool.tool.clone(),
                failed: status != crate::api::wire::ToolCallStatus::Completed,
                cancelled: status == crate::api::wire::ToolCallStatus::Cancelled,
                action: tool.action.as_ref().and_then(render::preview_summary),
                output: tool.result.as_ref().and_then(render::exec_output),
            });
        }
        for turn in &transcript.terminal_turns {
            self.context_tokens = turn.usage.context_tokens();
            match turn.status {
                crate::api::wire::TerminalTurnStatus::Failed => {
                    self.commit(Commit::Error(format!(
                        "turn failed ({})",
                        turn.failure_category
                            .clone()
                            .unwrap_or_else(|| "unknown".into())
                            .replace('_', " ")
                    )));
                }
                crate::api::wire::TerminalTurnStatus::Cancelled => {
                    self.commit(Commit::Notice("turn cancelled".into()));
                }
                _ => {}
            }
            if let Some(refusal) = &turn.refusal {
                let detail = refusal
                    .category
                    .as_ref()
                    .map(|category| format!(" ({})", category.replace('_', " ")))
                    .unwrap_or_default();
                self.commit(Commit::Notice(format!("the model refused{detail}")));
            }
        }
        // Settled background runs show as their own transcript lines.
        self.runs = runs
            .into_iter()
            .filter(|run| run.tier.as_deref() != Some("foreground"))
            .map(|run| (run.id, run))
            .collect();
        let settled: Vec<(String, String)> = self
            .runs
            .values()
            .filter(|run| run.started_at.is_some() || run.finished_at.is_some())
            .map(|run| {
                (
                    run.task
                        .clone()
                        .unwrap_or_else(|| "background agent".into()),
                    run.status.clone(),
                )
            })
            .collect();
        for (task, status) in settled {
            self.commit(Commit::AgentRun { task, status });
        }
        // Parked state reopens its card.
        if let Some(approval) = approvals.into_iter().next() {
            self.approval = Some(PendingApproval {
                call_id: approval.call_id,
                action: approval.action,
                approval: approval.approval,
                auto_judging: approval.auto_judge_status.is_some(),
                preview: approval.preview,
                grant_rungs: approval.grant_rungs,
                picking_grant: false,
            });
        }
        if let Some(plan) = plans.into_iter().next() {
            self.plan = Some(PendingPlanReview {
                call_id: plan.call_id,
                title: plan.title,
                plan: plan.plan,
                feedback: None,
            });
        }
        if let Some(pending) = questions.into_iter().next() {
            self.questions = Some(pending);
        }
    }

    fn on_tick(&mut self) {
        if self.running_turn.is_some() || self.active_tool.is_some() || self.runs_live() {
            self.spinner = self.spinner.wrapping_add(1);
        }
        if let Some((_, remaining)) = &mut self.flash {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.flash = None;
            }
        }
    }

    /// Whether any background run is live (drives the spinner and footer).
    fn runs_live(&self) -> bool {
        self.runs
            .values()
            .any(|run| render::run_is_live(&run.status))
    }

    /// The live region's transient stack, bottom-anchored within `max` lines:
    /// blank padding goes on top, so content sits directly above the composer.
    fn transient_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(plan) = &self.plan {
            lines.extend(render::plan_card(
                &plan.title,
                &plan.plan,
                plan.feedback.is_some(),
                width,
            ));
        } else if !self.thinking.is_empty()
            || !self.streaming.is_empty()
            || self.active_tool.is_some()
            || self.approval.is_some()
        {
            // While the model works, show a spinner rather than the raw
            // reasoning stream — the reasoning still folds into the transcript
            // at turn end, so nothing is lost, but the live view stays calm.
            if self.streaming.is_empty() && self.active_tool.is_none() && self.approval.is_none() {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.spinner()), theme::accent()),
                    Span::styled("working…", theme::muted()),
                ]));
            }
            if !self.streaming.is_empty() {
                lines.extend(render::streaming_lines(&self.streaming, width));
            }
            if let Some(tool) = &self.active_tool {
                lines.push(render::tool_running_line(&tool.name, self.spinner()));
            }
            if let Some(approval) = &self.approval {
                if approval.picking_grant {
                    lines.extend(render::grant_ladder_lines(
                        &approval.grant_rungs,
                        approval.preview.as_ref(),
                    ));
                } else {
                    lines.extend(render::approval_card(
                        &approval.action,
                        &approval.approval,
                        approval.auto_judging,
                        approval.preview.as_ref(),
                        &approval.grant_rungs,
                        width,
                    ));
                }
            }
        }
        // Live background agents ride the transient stack below the turn's
        // own state, so long delegations stay visible.
        let live: Vec<&AgentRunSnapshot> = self
            .runs
            .values()
            .filter(|run| render::run_is_live(&run.status))
            .collect();
        for run in live.into_iter().take(3) {
            let status = run
                .activity
                .as_ref()
                .map(|activity| format!("{} {}", activity.status, activity.kind.label()))
                .unwrap_or_else(|| run.status.clone());
            lines.push(render::agent_running_line(
                run.task.as_deref().unwrap_or("background agent"),
                &status,
                self.spinner(),
            ));
        }
        // The region is sized to the content (see `draw`), so nothing is
        // drained or padded here — a growing stream grows the terminal.
        lines
    }

    /// The footer's left side: state segment in the accent, key hints muted,
    /// separated by a dim "·".
    fn footer_line(&self) -> Line<'static> {
        let sep = || Span::styled(" · ", theme::muted());
        if let Some((message, _)) = &self.flash {
            return Line::from(Span::styled(message.clone(), theme::accent()));
        }
        if self.plan.is_some() {
            return Line::from(vec![
                Span::styled("plan review", theme::accent_bold()),
                sep(),
                Span::styled("y approve", theme::muted()),
                sep(),
                Span::styled("f changes", theme::muted()),
                sep(),
                Span::styled("n reject", theme::muted()),
            ]);
        }
        if self.approval.is_some() {
            return Line::from(vec![
                Span::styled("awaiting approval", theme::accent_bold()),
                sep(),
                Span::styled("y once", theme::muted()),
                sep(),
                Span::styled("a always…", theme::muted()),
                sep(),
                Span::styled("n reject", theme::muted()),
            ]);
        }
        let mut spans = Vec::new();
        if self.running_turn.is_some() {
            spans.push(Span::styled(
                format!("{} working…", self.spinner()),
                theme::accent(),
            ));
            spans.push(sep());
            spans.push(Span::styled("enter steer", theme::muted()));
            spans.push(sep());
            spans.push(Span::styled("ctrl+c cancel", theme::muted()));
        } else {
            spans.push(Span::styled("ready", theme::muted()));
            spans.push(sep());
            spans.push(Span::styled("enter send", theme::muted()));
        }
        spans.push(sep());
        spans.push(Span::styled("/ commands", theme::muted()));
        spans.push(sep());
        spans.push(Span::styled("ctrl+o chats", theme::muted()));
        spans.push(sep());
        spans.push(Span::styled("ctrl+g agents", theme::muted()));
        spans.push(sep());
        spans.push(Span::styled("ctrl+/ help", theme::muted()));
        Line::from(spans)
    }

    /// The footer's right edge: model, effort, permission mode, context meter,
    /// chat id.
    fn footer_right(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        // The permission mode pill, tinted when elevated.
        let mode = self.permission_mode.as_str();
        let (mode_style, mode_label) = match mode {
            "plan" => (theme::muted(), "plan"),
            "auto" => (theme::warning(), "auto"),
            "allow" => (theme::warning(), "allow all"),
            _ => (theme::muted(), "ask"),
        };
        spans.push(Span::styled(mode_label, mode_style));
        spans.push(Span::styled(" · ", theme::muted()));
        // The model, or "default" when the chat inherits.
        let model = self.model.as_deref().unwrap_or("default");
        spans.push(Span::styled(model.to_owned(), theme::muted()));
        if let Some(effort) = &self.reasoning_effort {
            spans.push(Span::styled(format!(" · {effort}"), theme::muted()));
        }
        // The context meter, when we know the window.
        if let Some(window) = self.context_window.filter(|window| *window > 0) {
            let pct = (self.context_tokens * 100 / u64::from(window)).min(100);
            let style = if pct >= 90 {
                theme::destructive()
            } else if pct >= 75 {
                theme::warning()
            } else {
                theme::muted()
            };
            spans.push(Span::styled(" · ", theme::muted()));
            spans.push(Span::styled(
                format!(
                    "{}/{}",
                    render::token_count(self.context_tokens),
                    render::token_count(u64::from(window))
                ),
                theme::muted(),
            ));
            spans.push(Span::styled(format!(" {pct}%"), style));
        }
        spans.push(Span::styled(" · ", theme::muted()));
        spans.push(Span::styled(self.title_label(), theme::muted()));
        spans
    }
}

/// Restores the terminal on drop; the panic hook does the same for panics.
pub(super) struct TerminalGuard {
    pub(super) terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The inline viewport's height, clamped to the terminal's rows.
    live_height: u16,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        Self::with_height(LIVE_HEIGHT)
    }

    /// Raw mode plus an inline viewport `rows` tall (clamped to the terminal).
    /// The startup picker takes a taller region than the chat's live area.
    pub(super) fn with_height(rows: u16) -> Result<Self> {
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
        let live_height = rows.min(crossterm::terminal::size().map_or(rows, |(_, r)| r));
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

pub(super) fn install_panic_hook() {
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

pub(super) fn terminal_error(error: io::Error) -> AgentError {
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
    app.hydrate();
    app.refresh_models();
    app.refresh_chat_list();
    // The socket opens only after hydration lands, at the transcript's
    // watermark — opening it earlier would replay history hydration already
    // printed.
    let mut socket: Option<EventSocket> = None;
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    // Periodic refresh for background runs while any are live.
    let mut runs_tick = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            key = keys.next() => {
                match key {
                    Some(Ok(Event::Key(key))) => app.on_key(key),
                    Some(Ok(Event::Paste(text))) => {
                        if app.approval.is_none() && app.overlay.is_none() && app.plan.is_none() {
                            super::composer::paste(&mut app.composer, &text);
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
                        Ok(ChatFrame::Metadata(metadata)) => app.on_metadata(metadata),
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
                        app.reconnecting = Some(());
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
                        app.reconnecting = None;
                        socket = Some(*new_socket);
                        app.commit(Commit::Notice("reconnected".into()));
                    }
                    ActionOutcome::ReconnectFailed(error) => {
                        app.reconnecting = None;
                        app.commit(Commit::Error(format!(
                            "event stream reconnect failed: {error} — restart the TUI to resume"
                        )));
                    }
                    other => app.on_outcome(other),
                }
            }
            _ = tick.tick() => app.on_tick(),
            _ = runs_tick.tick() => {
                if app.runs_live() || matches!(app.overlay, Some(Overlay::Agents(_))) {
                    app.refresh_runs();
                }
                // An open agents overlay with an empty detail wants the
                // run's timeline.
                if let Some(Overlay::Agents(overlay)) = &app.overlay {
                    if let Some(run) = overlay.detail_run() {
                        let (client, chat, actions) =
                            (app.client.clone(), app.chat, app.actions.clone());
                        tokio::spawn(async move {
                            if let Ok(items) = client.list_agent_run_activity(chat, run).await {
                                let _ = actions.send(ActionOutcome::AgentActivity(run, items));
                            }
                        });
                    }
                }
            }
        }

        // The socket opens once hydration lands (or reopens on a chat switch
        // after the fresh hydration). The watermark is the resume cursor, so
        // history hydration and the replay never double-print.
        if socket.is_none() && app.hydrated && app.reconnecting.is_none() {
            let chat = app.chat;
            let after = app.last_seq;
            match app.client.open_events(chat, after).await {
                Ok(new_socket) => socket = Some(new_socket),
                Err(error) => {
                    app.commit(Commit::Error(format!(
                        "event stream failed to open: {error}"
                    )));
                    // Back off briefly rather than spinning on a dead server.
                    tokio::time::sleep(RECONNECT_DELAY).await;
                }
            }
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
        // Defensive: never let a line wider than the viewport reach
        // `insert_before`, whose fixed buffer panics on an over-wide line.
        .map(|line| render::clip_line(line, width))
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
    let area = terminal.size().map_err(terminal_error)?;
    let width = area.width as usize;
    let composer_rows = (app.composer.lines().len() as u16).clamp(1, MAX_COMPOSER_ROWS);
    // The transient region grows with its content (so streaming text expands
    // the terminal rather than scrolling inside a capped block), bounded by
    // the viewport minus the composer and footer. `live_height` is the floor
    // the loop keeps stable between turns.
    let max_transient = area
        .height
        .saturating_sub(composer_rows + 1)
        .max(live_height.saturating_sub(composer_rows + 1));
    let transient = app.transient_lines(width);
    let transient_rows = (transient.len() as u16).clamp(1, max_transient);
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
            // The footer is the state line on the left, the chat/model/mode
            // cluster on the right.
            let mut footer = app.footer_line();
            let right = app.footer_right();
            let right_width: usize = right.iter().map(Span::width).sum();
            let pad = width.saturating_sub(footer.width() + right_width);
            if pad >= 2 {
                footer.spans.push(Span::raw(" ".repeat(pad)));
                footer.spans.extend(right);
            }
            frame.render_widget(Paragraph::new(footer), rows[2]);
            // The slash-command autocomplete floats just above the composer.
            let slash = app.slash_matches();
            if !slash.is_empty() {
                let selected = app.slash_selected.min(slash.len() - 1);
                let items: Vec<Line<'static>> = slash
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let blurb = super::overlays::SLASH_COMMANDS
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, b)| *b)
                            .unwrap_or("");
                        let marker = if i == selected { "▸ " } else { "  " };
                        let style = if i == selected {
                            theme::selected()
                        } else {
                            Style::default()
                        };
                        Line::from(vec![
                            Span::styled(marker, style),
                            Span::styled(format!("/{name:<10}"), style),
                            Span::styled(blurb, style),
                        ])
                    })
                    .collect();
                let height = items.len() as u16;
                // Sit on top of the composer's top edge, growing upward.
                let y = rows[1].y.saturating_sub(height);
                let area = Rect::new(rows[1].x, y, rows[1].width.min(56), height);
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new(items).style(Style::default().bg(theme::PANEL_BG)),
                    area,
                );
            }
            // An overlay floats on top of the live region.
            if let Some(overlay) = &mut app.overlay {
                render_overlay(overlay, frame);
            }
            // The plan review's feedback box, when open, replaces the composer.
            if let Some(plan) = &mut app.plan {
                if let Some(feedback) = &mut plan.feedback {
                    frame.render_widget(Clear, rows[1]);
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::border())
                        .title(" request changes ");
                    let inner = block.inner(rows[1]);
                    frame.render_widget(block, rows[1]);
                    frame.render_widget(&*feedback, inner);
                }
            }
        })
        .map_err(terminal_error)?;
    Ok(())
}

/// Draw the open overlay, centered and sized to its content.
fn render_overlay(overlay: &mut Overlay, frame: &mut ratatui::Frame) {
    let area = frame.area();
    let (title, height_hint, width_hint): (&str, u16, u16) = match overlay {
        Overlay::Chats(_) => ("chats", 18, 60),
        Overlay::Agents(_) => ("agents", 16, 72),
        Overlay::Models(_) => ("model", 16, 56),
        Overlay::Mode(_) => ("permission mode", 10, 56),
        Overlay::Move(_) => ("move to project", 12, 56),
        Overlay::Questions(_) => ("the model is asking", 18, 72),
        Overlay::Help(_) => ("shortcuts", 20, 56),
    };
    let height = height_hint.min(area.height.saturating_sub(2)).max(6);
    let width = width_hint.min(area.width.saturating_sub(4)).max(30);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let rect = Rect::new(x, y, width, height);
    super::overlays::panel(frame, rect, title, |width| match overlay {
        Overlay::Chats(overlay) => overlay.lines(width),
        Overlay::Agents(overlay) => overlay.lines(width),
        Overlay::Models(overlay) => overlay.lines(width),
        Overlay::Mode(overlay) => overlay.lines(width),
        Overlay::Move(overlay) => overlay.lines(width),
        Overlay::Questions(overlay) => overlay.lines(width),
        Overlay::Help(overlay) => overlay.lines(width),
    });
}

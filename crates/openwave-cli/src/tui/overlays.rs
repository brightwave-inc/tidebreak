//! The overlay surfaces: chat switcher, agents panel, model picker,
//! permission-mode menu, move-to-project picker, user-questions form, and the
//! help sheet. Each owns its own key handling and renders to plain styled
//! lines; `app.rs` draws the shared panel chrome.
//!
//! Every overlay is a small state machine returning [`OverlayOutcome`]s; the
//! app translates those into spawned HTTP calls, keeping the overlays free of
//! async and of the client. Every `lines(width)` takes the panel's inner
//! width so text clips to the panel rather than wrapping into the border.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use openwave_core::{AgentRunId, ChatId, ProjectId};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tui_textarea::TextArea;

use super::render;
use super::theme;
use crate::api::wire::{AgentRunSnapshot, ChatSummary, ModelInfo, PendingQuestions};

/// The slash-command table, Claude Code style: `/name args` in the composer.
/// Each entry is the canonical name plus a one-line blurb the help overlay and
/// the autocomplete list share.
pub(crate) const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("model", "pick the model (opens the selector)"),
    (
        "effort",
        "set reasoning effort: none|low|medium|high|xhigh|max",
    ),
    ("mode", "permission mode: plan|ask|auto|allow"),
    ("chats", "open the chat switcher"),
    ("new", "start a new chat"),
    ("rename", "rename this chat: /rename <title>"),
    ("move", "move this chat to a project"),
    ("agents", "list background agents"),
    ("questions", "answer the model's parked questions"),
    ("help", "shortcut and command reference"),
    ("quit", "leave"),
];

/// What an overlay decided. `Stay` keeps it open; `Dismiss` closes it; the
/// rest close it and hand the app an action.
pub enum OverlayOutcome {
    Stay,
    Dismiss,
    Open(ChatId),
    NewChat,
    Delete(ChatId),
    Rename(ChatId, String),
    MoveChat(Option<ProjectId>),
    SetModel(Option<String>),
    SetEffort(Option<String>),
    SetMode(String),
    StopAgent(AgentRunId),
    SubmitQuestions(serde_json::Value),
}

/// Row styling helpers: the selected row gets the selection background, the
/// rest stay plain.
fn row(selected: bool, spans: Vec<Span<'static>>) -> Line<'static> {
    let style = if selected {
        theme::selected()
    } else {
        Style::default()
    };
    Line::from(
        spans
            .into_iter()
            .map(|span| span.patch_style(style))
            .collect::<Vec<_>>(),
    )
}

/// A selectable bar row: the selected row is padded to the panel's full width
/// so its highlight reads as a solid bar, the way a menu cursor should.
fn bar(selected: bool, width: usize, spans: Vec<Span<'static>>) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let mut spans = spans;
    if selected {
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), theme::selected()));
        }
    }
    row(selected, spans)
}

/// Move a selection cursor. Arrows and Tab step down, Shift-Tab steps up,
/// wrapping at the ends — the menu convention.
fn nav_step(selected: usize, len: usize, down: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if down {
        (selected + 1) % len
    } else {
        (selected + len - 1) % len
    }
}

fn title_or_untitled(title: Option<&str>) -> String {
    title
        .map(str::to_owned)
        .unwrap_or_else(|| "new chat".to_owned())
}

// ---------------------------------------------------------------------------
// Chats
// ---------------------------------------------------------------------------

pub struct ChatsOverlay {
    chats: Vec<ChatSummary>,
    attention: std::collections::HashSet<ChatId>,
    selected: usize,
    current: ChatId,
    /// Inline rename state: which row and the input.
    renaming: Option<(ChatId, TextArea<'static>)>,
}

impl ChatsOverlay {
    pub fn new(current: ChatId) -> Self {
        Self {
            chats: Vec::new(),
            attention: std::collections::HashSet::new(),
            selected: 0,
            current,
            renaming: None,
        }
    }

    pub fn set_chats(
        &mut self,
        chats: Vec<ChatSummary>,
        attention: std::collections::HashSet<ChatId>,
    ) {
        self.chats = chats;
        self.attention = attention;
        if self.selected >= self.chats.len() {
            self.selected = self.chats.len().saturating_sub(1);
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        // Inline rename captures keys until it commits or cancels.
        if let Some((chat, input)) = &mut self.renaming {
            match key.code {
                KeyCode::Esc => {
                    self.renaming = None;
                    return OverlayOutcome::Stay;
                }
                KeyCode::Enter => {
                    let title = input.lines().join("\n");
                    let chat = *chat;
                    self.renaming = None;
                    return OverlayOutcome::Rename(chat, title);
                }
                _ => {
                    super::composer::single_line_key(input, key);
                    return OverlayOutcome::Stay;
                }
            }
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Dismiss,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if !self.chats.is_empty() {
                    self.selected = nav_step(self.selected, self.chats.len(), true);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                if !self.chats.is_empty() {
                    self.selected = nav_step(self.selected, self.chats.len(), false);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => self
                .chats
                .get(self.selected)
                .map(|chat| OverlayOutcome::Open(chat.id))
                .unwrap_or(OverlayOutcome::Dismiss),
            KeyCode::Char('n') if key.modifiers.is_empty() => OverlayOutcome::NewChat,
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                if let Some(chat) = self.chats.get(self.selected) {
                    let input = super::composer::new_single_line(
                        chat.title.as_deref().unwrap_or(""),
                        "chat title",
                    );
                    self.renaming = Some((chat.id, input));
                }
                OverlayOutcome::Stay
            }
            KeyCode::Char('d') | KeyCode::Backspace => self
                .chats
                .get(self.selected)
                .map(|chat| OverlayOutcome::Delete(chat.id))
                .unwrap_or(OverlayOutcome::Stay),
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(20);
        let mut out = Vec::new();
        if let Some((_, input)) = &self.renaming {
            out.push(Line::from(Span::styled("rename:", theme::accent_bold())));
            let mut line = String::new();
            for (i, text) in input.lines().iter().enumerate() {
                if i > 0 {
                    line.push(' ');
                }
                line.push_str(text);
            }
            out.push(Line::from(Span::styled(
                render::truncate(&line, width.saturating_sub(2)),
                theme::bold(),
            )));
            out.push(Line::from(Span::styled(
                "enter commit · esc cancel",
                theme::muted(),
            )));
            return out;
        }
        if self.chats.is_empty() {
            out.push(Line::from(Span::styled("loading…", theme::muted())));
            return out;
        }
        for (i, chat) in self.chats.iter().enumerate() {
            let selected = i == self.selected;
            let marker = if chat.id == self.current {
                Span::styled("● ", theme::accent())
            } else if self.attention.contains(&chat.id) {
                Span::styled("⚠ ", theme::warning())
            } else {
                Span::raw("  ")
            };
            let spans = vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    if selected {
                        theme::accent_bold()
                    } else {
                        theme::muted()
                    },
                ),
                marker,
                Span::styled(
                    render::truncate(
                        &title_or_untitled(chat.title.as_deref()),
                        width.saturating_sub(6),
                    ),
                    if selected {
                        theme::bold()
                    } else {
                        Style::default()
                    },
                ),
            ];
            out.push(bar(selected, width, spans));
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "↑↓/tab move · enter open · n new · r rename · d delete · esc close",
            theme::muted(),
        )));
        out
    }
}

// ---------------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------------

pub struct AgentsOverlay {
    runs: Vec<AgentRunSnapshot>,
    selected: usize,
    /// The expanded run's activity timeline, when one is open.
    detail: Option<(AgentRunId, Vec<crate::api::wire::AgentActivityItem>)>,
}

impl AgentsOverlay {
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            selected: 0,
            detail: None,
        }
    }

    pub fn set_runs(&mut self, mut runs: Vec<AgentRunSnapshot>) {
        // Live runs first, then settled, most recently created first.
        runs.sort_by_key(|run| {
            let live = matches!(
                run.status.as_str(),
                "queued" | "running" | "cancelling" | "waiting" | "retry_wait"
            );
            (!live, std::cmp::Reverse(run.created_at))
        });
        self.runs = runs;
        if self.selected >= self.runs.len() {
            self.selected = self.runs.len().saturating_sub(1);
        }
    }

    /// The run whose timeline the app should fetch, when the detail is open
    /// but hasn't loaded yet.
    pub fn detail_run(&self) -> Option<AgentRunId> {
        match &self.detail {
            Some((run, items)) if items.is_empty() => Some(*run),
            _ => None,
        }
    }

    pub fn set_detail(&mut self, run: AgentRunId, items: Vec<crate::api::wire::AgentActivityItem>) {
        if matches!(&self.detail, Some((open, _)) if *open == run) {
            self.detail = Some((run, items));
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        match key.code {
            KeyCode::Esc => {
                if self.detail.take().is_some() {
                    OverlayOutcome::Stay
                } else {
                    OverlayOutcome::Dismiss
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if !self.runs.is_empty() {
                    self.selected = nav_step(self.selected, self.runs.len(), true);
                    self.detail = None;
                }
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                if !self.runs.is_empty() {
                    self.selected = nav_step(self.selected, self.runs.len(), false);
                    self.detail = None;
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                if let Some(run) = self.runs.get(self.selected) {
                    self.detail = Some((run.id, Vec::new()));
                }
                OverlayOutcome::Stay
            }
            KeyCode::Char('s') => self
                .runs
                .get(self.selected)
                .filter(|run| {
                    matches!(
                        run.status.as_str(),
                        "queued" | "running" | "waiting" | "retry_wait"
                    )
                })
                .map(|run| OverlayOutcome::StopAgent(run.id))
                .unwrap_or(OverlayOutcome::Stay),
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(24);
        let mut out = Vec::new();
        if self.runs.is_empty() {
            out.push(Line::from(Span::styled(
                "no background agents yet",
                theme::muted(),
            )));
            out.push(Line::from(Span::styled(
                "the model spawns them with delegate_agent",
                theme::muted(),
            )));
            return out;
        }
        for (i, run) in self.runs.iter().enumerate() {
            let selected = i == self.selected;
            let (dot, style) = match run.status.as_str() {
                "completed" => ("✓", theme::success()),
                "failed" => ("✗", theme::destructive()),
                "cancelled" => ("⊘", theme::muted()),
                "running" | "queued" | "waiting" | "retry_wait" => ("●", theme::agent()),
                "cancelling" => ("◌", theme::warning()),
                _ => ("·", theme::muted()),
            };
            let mut spans = vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    if selected {
                        theme::accent_bold()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(format!("{dot} "), style),
                Span::styled(
                    render::truncate(
                        run.task.as_deref().unwrap_or("background agent"),
                        width.saturating_sub(18),
                    ),
                    if selected {
                        theme::bold()
                    } else {
                        Style::default()
                    },
                ),
            ];
            // The live activity detail ("running a command") when there is
            // one; otherwise the settled status.
            if let Some(activity) = &run.activity {
                spans.push(Span::styled(
                    format!("  {} {}", activity.status, activity.kind.label()),
                    theme::muted(),
                ));
            } else if let Some(error) = &run.last_error_code {
                spans.push(Span::styled(format!("  {error}"), theme::destructive()));
            } else {
                spans.push(Span::styled(format!("  {}", run.status), theme::muted()));
            }
            out.push(bar(selected, width, spans));
        }
        // The expanded run's detail: activity timeline plus a result slice.
        if let Some((open, items)) = &self.detail {
            if let Some(run) = self.runs.iter().find(|run| run.id == *open) {
                out.push(Line::default());
                out.push(Line::from(Span::styled("activity", theme::accent_bold())));
                if items.is_empty() {
                    out.push(Line::from(Span::styled("  loading…", theme::muted())));
                } else {
                    for item in items {
                        let mark = match item.outcome.as_str() {
                            "completed" => ("✓", theme::success()),
                            "failed" => ("✗", theme::destructive()),
                            "cancelled" => ("⊘", theme::muted()),
                            _ => ("·", theme::muted()),
                        };
                        out.push(Line::from(vec![
                            Span::styled(format!("  {} ", mark.0), mark.1),
                            Span::styled(item.kind.label(), Style::default()),
                            Span::styled(
                                format!("  {}", render::timestamp(item.at)),
                                theme::muted(),
                            ),
                        ]));
                    }
                }
                if let Some(terminal) = run.terminal_text.as_deref() {
                    out.push(Line::default());
                    out.push(Line::from(Span::styled("result", theme::accent_bold())));
                    for line in terminal.lines().take(6) {
                        out.push(Line::from(Span::styled(
                            format!("  {}", render::truncate(line, width.saturating_sub(4))),
                            theme::muted().add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "enter detail · s stop selected · esc close",
            theme::muted(),
        )));
        out
    }
}

// ---------------------------------------------------------------------------
// Model picker (with the reasoning-effort submenu)
// ---------------------------------------------------------------------------

/// The stock reasoning-effort ladder, in the order the desktop shows it.
const EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];

pub struct ModelOverlay {
    models: Vec<ModelInfo>,
    selected: usize,
    current: Option<String>,
    effort: Option<String>,
    /// Whether the effort list is showing instead of the model list.
    pub(crate) picking_effort: bool,
    /// The cursor within the effort list.
    effort_selected: usize,
}

impl ModelOverlay {
    pub fn new(current: Option<String>, effort: Option<String>) -> Self {
        Self {
            models: Vec::new(),
            selected: 0,
            current,
            effort,
            picking_effort: false,
            effort_selected: 0,
        }
    }

    pub fn set_models(&mut self, models: Vec<ModelInfo>) {
        // Keep the selection on the current model the first time the catalog
        // lands.
        if self.models.is_empty() {
            if let Some(current) = &self.current {
                self.selected = models
                    .iter()
                    .position(|model| &model.key == current || &model.id == current)
                    .unwrap_or(0);
            }
        }
        self.models = models;
    }

    /// The effort levels the selected model accepts, or the stock list when
    /// the catalog doesn't name one.
    fn efforts(&self) -> Vec<String> {
        let listed = self
            .models
            .get(self.selected)
            .map(|model| model.reasoning_efforts.clone())
            .unwrap_or_default();
        if listed.is_empty() {
            EFFORTS.iter().map(|effort| effort.to_string()).collect()
        } else {
            listed
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if self.picking_effort {
            let efforts = self.efforts();
            match key.code {
                KeyCode::Esc => {
                    self.picking_effort = false;
                    return OverlayOutcome::Stay;
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    if !efforts.is_empty() {
                        self.effort_selected = nav_step(self.effort_selected, efforts.len(), true);
                    }
                    return OverlayOutcome::Stay;
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    if !efforts.is_empty() {
                        self.effort_selected = nav_step(self.effort_selected, efforts.len(), false);
                    }
                    return OverlayOutcome::Stay;
                }
                KeyCode::Enter => {
                    return OverlayOutcome::SetEffort(efforts.get(self.effort_selected).cloned());
                }
                KeyCode::Char('d') => return OverlayOutcome::SetEffort(None),
                _ => return OverlayOutcome::Stay,
            }
        }
        match key.code {
            KeyCode::Esc => OverlayOutcome::Dismiss,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if !self.models.is_empty() {
                    self.selected = nav_step(self.selected, self.models.len(), true);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                if !self.models.is_empty() {
                    self.selected = nav_step(self.selected, self.models.len(), false);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Enter => self
                .models
                .get(self.selected)
                .filter(|model| model.available)
                .map(|model| OverlayOutcome::SetModel(Some(model.key.clone())))
                .unwrap_or(OverlayOutcome::Stay),
            KeyCode::Char('d') => OverlayOutcome::SetModel(None),
            KeyCode::Char('e') => {
                self.picking_effort = true;
                self.effort_selected = 0;
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(20);
        let mut out = Vec::new();
        if self.picking_effort {
            out.push(Line::from(Span::styled(
                "reasoning effort",
                theme::accent_bold(),
            )));
            for (i, effort) in self.efforts().iter().enumerate() {
                let selected = i == self.effort_selected;
                let current = self.effort.as_deref() == Some(effort.as_str());
                out.push(bar(
                    selected,
                    width,
                    vec![
                        Span::styled(
                            if selected { "▸ " } else { "  " },
                            if selected {
                                theme::accent_bold()
                            } else {
                                theme::muted()
                            },
                        ),
                        Span::styled(
                            if current { "● " } else { "  " },
                            if current {
                                theme::accent()
                            } else {
                                theme::muted()
                            },
                        ),
                        Span::styled(effort.clone(), Style::default()),
                    ],
                ));
            }
            out.push(Line::default());
            out.push(Line::from(Span::styled(
                "enter set · d provider default · esc back",
                theme::muted(),
            )));
            return out;
        }
        if self.models.is_empty() {
            out.push(Line::from(Span::styled("loading…", theme::muted())));
            return out;
        }
        for (i, model) in self.models.iter().enumerate() {
            let selected = i == self.selected;
            let current = self
                .current
                .as_ref()
                .is_some_and(|current| current == &model.key || current == &model.id);
            let mut spans = vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    if selected {
                        theme::accent_bold()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(
                    if current { "● " } else { "  " },
                    if current {
                        theme::accent()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(
                    render::truncate(&model.display_name, width.saturating_sub(18)),
                    if model.available {
                        Style::default()
                    } else {
                        theme::muted()
                    },
                ),
            ];
            if !model.available {
                spans.push(Span::styled("  unavailable", theme::muted()));
            }
            out.push(bar(selected, width, spans));
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "↑↓/tab move · enter select · d default · e effort… · esc close",
            theme::muted(),
        )));
        out
    }
}

// ---------------------------------------------------------------------------
// Permission mode
// ---------------------------------------------------------------------------

const MODES: &[(&str, &str, &str)] = &[
    ("plan", "Plan", "read-only; the model proposes a plan first"),
    ("ask", "Ask", "ask before anything consequential (default)"),
    ("auto", "Auto", "workspace edits run without asking"),
    ("allow", "Allow all", "no prompts in this chat"),
];

pub struct ModeOverlay {
    selected: usize,
}

impl ModeOverlay {
    pub fn new(current: &str) -> Self {
        let selected = MODES
            .iter()
            .position(|(key, _, _)| *key == current)
            .unwrap_or(1);
        Self { selected }
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        match key.code {
            KeyCode::Esc => OverlayOutcome::Dismiss,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = nav_step(self.selected, MODES.len(), true);
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.selected = nav_step(self.selected, MODES.len(), false);
                OverlayOutcome::Stay
            }
            KeyCode::Enter => OverlayOutcome::SetMode(MODES[self.selected].0.to_owned()),
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(24);
        let mut out = Vec::new();
        for (i, (_, label, blurb)) in MODES.iter().enumerate() {
            let selected = i == self.selected;
            out.push(bar(
                selected,
                width,
                vec![
                    Span::styled(
                        if selected { "▸ " } else { "  " },
                        if selected {
                            theme::accent_bold()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(
                        format!("{label:<10}"),
                        if i >= 2 {
                            theme::warning()
                        } else {
                            theme::bold()
                        },
                    ),
                    Span::styled(
                        render::truncate(blurb, width.saturating_sub(16)),
                        theme::muted(),
                    ),
                ],
            ));
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "↑↓/tab move · enter set · esc close",
            theme::muted(),
        )));
        out
    }
}

// ---------------------------------------------------------------------------
// Move to project
// ---------------------------------------------------------------------------

pub struct MoveOverlay {
    projects: Vec<crate::api::wire::ProjectSummary>,
    selected: usize,
}

impl MoveOverlay {
    pub fn new(projects: Vec<crate::api::wire::ProjectSummary>) -> Self {
        Self {
            projects,
            selected: 0,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        // Row 0 is "no project"; projects follow.
        let len = self.projects.len() + 1;
        match key.code {
            KeyCode::Esc => OverlayOutcome::Dismiss,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = nav_step(self.selected, len, true);
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.selected = nav_step(self.selected, len, false);
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                if self.selected == 0 {
                    OverlayOutcome::MoveChat(None)
                } else {
                    OverlayOutcome::MoveChat(Some(self.projects[self.selected - 1].id))
                }
            }
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(20);
        let mut out = Vec::new();
        out.push(bar(
            self.selected == 0,
            width,
            vec![
                Span::styled(
                    if self.selected == 0 { "▸ " } else { "  " },
                    if self.selected == 0 {
                        theme::accent_bold()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled("no project", Style::default()),
            ],
        ));
        for (i, project) in self.projects.iter().enumerate() {
            let selected = self.selected == i + 1;
            out.push(bar(
                selected,
                width,
                vec![
                    Span::styled(
                        if selected { "▸ " } else { "  " },
                        if selected {
                            theme::accent_bold()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(
                        render::truncate(
                            project.title.as_deref().unwrap_or("untitled project"),
                            width.saturating_sub(4),
                        ),
                        Style::default(),
                    ),
                ],
            ));
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled(
            "↑↓/tab move · enter move · esc close",
            theme::muted(),
        )));
        out
    }
}

// ---------------------------------------------------------------------------
// User questions
// ---------------------------------------------------------------------------

pub struct QuestionsOverlay {
    pending: PendingQuestions,
    /// The question index being answered.
    index: usize,
    /// The option cursor within the current question.
    cursor: usize,
    /// Picked option indices per question, in question order.
    picks: Vec<std::collections::BTreeSet<usize>>,
    /// Free-form text per question, in question order.
    custom: Vec<String>,
    /// Whether the free-form box is open.
    editing: Option<TextArea<'static>>,
}

impl QuestionsOverlay {
    pub fn new(pending: PendingQuestions) -> Self {
        let count = pending.questions.len();
        Self {
            pending,
            index: 0,
            cursor: 0,
            picks: vec![std::collections::BTreeSet::new(); count],
            custom: vec![String::new(); count],
            editing: None,
        }
    }

    /// The parked call these answers resolve.
    pub fn call_id(&self) -> openwave_core::CallId {
        self.pending.call_id
    }

    fn done(&self) -> bool {
        self.index >= self.pending.questions.len()
    }

    fn build_answers(&self) -> serde_json::Value {
        let answers: Vec<serde_json::Value> = self
            .pending
            .questions
            .iter()
            .enumerate()
            .map(|(i, question)| {
                let selections: Vec<String> = self.picks[i]
                    .iter()
                    .filter_map(|pick| question.options.get(*pick))
                    .map(|option| option.id.clone())
                    .collect();
                let mut answer = serde_json::json!({
                    "question_id": question.id,
                    "selected_option_ids": selections,
                });
                if !self.custom[i].trim().is_empty() {
                    answer["custom_answer"] = serde_json::json!(self.custom[i]);
                }
                answer
            })
            .collect();
        serde_json::json!({ "answers": answers })
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        if let Some(input) = &mut self.editing {
            match key.code {
                KeyCode::Esc => {
                    self.editing = None;
                    return OverlayOutcome::Stay;
                }
                KeyCode::Enter => {
                    let text = input.lines().join("\n");
                    self.custom[self.index] = text;
                    self.editing = None;
                    return OverlayOutcome::Stay;
                }
                _ => {
                    super::composer::single_line_key(input, key);
                    return OverlayOutcome::Stay;
                }
            }
        }
        if self.done() {
            return match key.code {
                KeyCode::Esc => OverlayOutcome::Dismiss,
                KeyCode::Enter => OverlayOutcome::SubmitQuestions(self.build_answers()),
                _ => OverlayOutcome::Stay,
            };
        }
        let question = &self.pending.questions[self.index];
        let option_count = question.options.len();
        let multi = question.question_type == "multi";
        match key.code {
            KeyCode::Esc => OverlayOutcome::Dismiss,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if option_count > 0 {
                    self.cursor = nav_step(self.cursor, option_count, true);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                if option_count > 0 {
                    self.cursor = nav_step(self.cursor, option_count, false);
                }
                OverlayOutcome::Stay
            }
            KeyCode::Char(' ') if multi => {
                self.picks[self.index].insert(self.cursor);
                OverlayOutcome::Stay
            }
            KeyCode::Char('c') if question.allow_free_form => {
                self.editing = Some(super::composer::new_single_line(
                    &self.custom[self.index],
                    "your own answer",
                ));
                OverlayOutcome::Stay
            }
            KeyCode::Enter => {
                if !multi && option_count > 0 {
                    self.picks[self.index].clear();
                    self.picks[self.index].insert(self.cursor);
                }
                self.index += 1;
                self.cursor = 0;
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(24);
        let mut out = Vec::new();
        if self.done() {
            out.push(Line::from(Span::styled(
                "all questions answered",
                theme::accent_bold(),
            )));
            out.push(Line::from(Span::styled(
                "enter submit · esc cancel",
                theme::muted(),
            )));
            return out;
        }
        let question = &self.pending.questions[self.index];
        out.push(Line::from(vec![
            Span::styled(
                format!("{}/{} ", self.index + 1, self.pending.questions.len()),
                theme::muted(),
            ),
            Span::styled(question.header.clone(), theme::accent_bold()),
        ]));
        for line in question.question.lines() {
            out.push(Line::from(Span::styled(
                render::truncate(line, width.saturating_sub(2)),
                Style::default(),
            )));
        }
        out.push(Line::default());
        let multi = question.question_type == "multi";
        for (i, option) in question.options.iter().enumerate() {
            let selected = i == self.cursor;
            let picked = self.picks[self.index].contains(&i);
            let mark = if multi {
                if picked {
                    "☑"
                } else {
                    "☐"
                }
            } else if picked {
                "●"
            } else {
                "○"
            };
            let mut spans = vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    if selected {
                        theme::accent_bold()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(format!("{mark} "), theme::accent()),
                Span::styled(
                    render::truncate(&option.label, width.saturating_sub(8)),
                    Style::default(),
                ),
            ];
            if let Some(description) = &option.description {
                spans.push(Span::styled(
                    render::truncate(description, width.saturating_sub(20)),
                    theme::muted(),
                ));
            }
            out.push(row(selected, spans));
        }
        if question.allow_free_form {
            let custom = &self.custom[self.index];
            if let Some(input) = &self.editing {
                let text = input.lines().join(" ");
                out.push(Line::from(vec![
                    Span::styled("  ✎ ", theme::accent()),
                    Span::styled(
                        render::truncate(&text, width.saturating_sub(6)),
                        theme::bold(),
                    ),
                ]));
            } else if !custom.is_empty() {
                out.push(Line::from(vec![
                    Span::styled("  ✎ ", theme::accent()),
                    Span::styled(
                        render::truncate(custom, width.saturating_sub(6)),
                        theme::bold(),
                    ),
                ]));
            }
        }
        out.push(Line::default());
        let mut hints = vec!["enter next"];
        if multi {
            hints.push("space toggle");
        }
        if question.allow_free_form {
            hints.push("c custom");
        }
        hints.push("esc cancel");
        out.push(Line::from(Span::styled(hints.join(" · "), theme::muted())));
        out
    }
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn new() -> Self {
        Self
    }

    pub fn key(&mut self, key: KeyEvent) -> OverlayOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => OverlayOutcome::Dismiss,
            _ if key.modifiers.contains(KeyModifiers::CONTROL) => OverlayOutcome::Dismiss,
            _ => OverlayOutcome::Stay,
        }
    }

    pub fn lines(&self, _width: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        out.push(Line::from(Span::styled(
            "slash commands",
            theme::accent_bold(),
        )));
        for (name, blurb) in SLASH_COMMANDS {
            out.push(Line::from(vec![
                Span::styled(format!("/{name:<10}"), theme::accent()),
                Span::styled(blurb.to_string(), theme::muted()),
            ]));
        }
        out.push(Line::default());
        out.push(Line::from(Span::styled("keys", theme::accent_bold())));
        let rows: &[(&str, &str)] = &[
            ("enter", "send · steer while a turn runs"),
            ("alt+enter", "newline"),
            ("ctrl+c", "cancel the turn · quit when idle"),
            ("ctrl+o", "chats — open, new, rename, delete"),
            ("ctrl+n", "new chat"),
            ("ctrl+g", "background agents"),
            ("ctrl+m", "model & effort"),
            ("ctrl+p", "permission mode"),
            ("ctrl+/", "this help"),
            ("y / n / a", "approve once · reject · always…"),
            ("esc", "close a panel"),
        ];
        out.extend(rows.iter().map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!("{key:<10}"), theme::accent()),
                Span::styled(what.to_string(), theme::muted()),
            ])
        }));
        out
    }
}

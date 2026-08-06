//! Pure line-building for the TUI: committed scrollback blocks and the pieces
//! of the repainting live region. No widget state lives here, and no color is
//! named here — all styling comes from [`super::theme`].
//!
//! Wrapping counts characters, not display cells, so a line heavy with wide
//! glyphs can overflow its width — acceptable for this slice's plain-text
//! rendering.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::theme;
use crate::api::wire::{FileAttachment, ImageAttachment};

/// A finished block ready to commit to real scrollback.
pub enum Commit {
    UserText {
        text: String,
        at: Option<chrono::DateTime<chrono::Utc>>,
        images: Vec<ImageAttachment>,
        files: Vec<FileAttachment>,
    },
    AssistantText {
        text: String,
        at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// A settled reasoning block, folded behind a one-line summary so a long
    /// thinking stream doesn't dominate the transcript. The full text stays
    /// in the session state for the transcript view.
    Reasoning {
        text: String,
    },
    ToolDone {
        name: String,
        failed: bool,
        cancelled: bool,
        /// The call's closed action preview, humanized to one line.
        action: Option<String>,
        /// An exec result's output tail, already bounded by the server.
        output: Option<ToolOutput>,
    },
    /// A background agent run changed state (spawned, settled, …).
    AgentRun {
        task: String,
        status: String,
    },
    Notice(String),
    Error(String),
    /// Pre-rendered one-offs (the startup header).
    Lines(Vec<Line<'static>>),
}

/// The bounded exec output a completed call carried.
pub struct ToolOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Word-wrap `text` to `width` columns, preserving hard newlines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut col = 0usize;
        for word in source.split(' ') {
            let word_len = word.chars().count();
            let gap = usize::from(!line.is_empty());
            if col + gap + word_len > width && col > 0 {
                out.push(std::mem::take(&mut line));
                col = 0;
            } else if gap == 1 {
                line.push(' ');
                col += 1;
            }
            // A word longer than the width hard-breaks.
            let mut rest = word;
            while rest.chars().count() > width - col && col < width {
                let take = width - col;
                let cut = rest.char_indices().nth(take).map_or(rest.len(), |(i, _)| i);
                line.push_str(&rest[..cut]);
                out.push(std::mem::take(&mut line));
                rest = &rest[cut..];
                col = 0;
            }
            line.push_str(rest);
            col += rest.chars().count();
        }
        out.push(line);
    }
    out
}

/// Long preview fields (a command, a query) get one bounded line, not the
/// whole payload.
pub fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

/// Clip a line to a display width, dropping spans (or parts of spans) that
/// would run past it. Scrollback and panels render into fixed buffers that
/// panic on an over-wide line, so everything that reaches one is clipped here.
pub fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in line.spans {
        if used >= width {
            break;
        }
        let text = span.content.as_ref();
        let remaining = width - used;
        if text.width() <= remaining {
            used += text.width();
            out.push(span);
        } else {
            let mut taken = String::new();
            let mut w = 0usize;
            for ch in text.chars() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if w + cw > remaining {
                    break;
                }
                w += cw;
                taken.push(ch);
            }
            if !taken.is_empty() {
                out.push(Span::styled(taken, span.style));
            }
            used = width;
        }
    }
    Line::from(out)
}

/// The timestamp format used across the transcript: local wall time.
pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

/// A dimmed right-side timestamp span, for block headers that carry one.
fn timestamp_span(at: Option<chrono::DateTime<chrono::Utc>>) -> Option<Span<'static>> {
    at.map(|at| Span::styled(format!("  {}", timestamp(at)), theme::muted()))
}

impl Commit {
    /// Render the block to styled lines at the current terminal width, with one
    /// blank line after it so every block type breathes evenly.
    pub fn lines(self, width: usize) -> Vec<Line<'static>> {
        let mut lines = match self {
            // The user block: a bold accent "❯ you" header, then the body in
            // the same accent gutter the composer uses, so a sent message
            // reads as the same element it was typed into.
            Commit::UserText {
                text,
                at,
                images,
                files,
            } => {
                let mut header = vec![
                    Span::styled("❯ ", theme::accent_bold()),
                    Span::styled("you", theme::user()),
                ];
                if let Some(stamp) = timestamp_span(at) {
                    header.push(stamp);
                }
                let mut out = vec![Line::from(header)];
                // Attachment chips ride above the text, as in the desktop.
                for image in &images {
                    out.push(Line::from(vec![
                        Span::styled("│ ", theme::muted()),
                        Span::styled("🖼 ", theme::muted()),
                        Span::styled(
                            format!(
                                "image · {} · {}×{}",
                                image.media_type, image.width, image.height
                            ),
                            theme::muted(),
                        ),
                    ]));
                }
                for file in &files {
                    out.push(Line::from(vec![
                        Span::styled("│ ", theme::muted()),
                        Span::styled("📄 ", theme::muted()),
                        Span::styled(file.name.clone(), theme::muted()),
                    ]));
                }
                out.extend(
                    wrap(&text, width.saturating_sub(2))
                        .into_iter()
                        .map(|line| {
                            Line::from(vec![
                                Span::styled("│ ", theme::muted()),
                                Span::styled(line, theme::bold()),
                            ])
                        }),
                );
                out
            }
            // The assistant block: a violet "◆ openwave" header, then
            // markdown-rendered prose with no gutter — the desktop gives the
            // assistant a transparent, unframed column too.
            Commit::AssistantText { text, at } => {
                let mut header = vec![
                    Span::styled("◆ ", theme::agent_bold()),
                    Span::styled("openwave", theme::agent_bold()),
                ];
                if let Some(stamp) = timestamp_span(at) {
                    header.push(stamp);
                }
                let mut out = vec![Line::from(header)];
                out.extend(markdown::lines(&text, width));
                out
            }
            // Settled reasoning folds to one dim line; the full text stays in
            // the session for the transcript view.
            Commit::Reasoning { text } => {
                let words = text.split_whitespace().count();
                vec![Line::from(vec![
                    Span::styled("💭 ", theme::muted()),
                    Span::styled("thought ", theme::muted().add_modifier(Modifier::ITALIC)),
                    Span::styled(
                        format!("({words} words)"),
                        theme::muted().add_modifier(Modifier::ITALIC),
                    ),
                ])]
            }
            // Completed tool noise recedes behind the assistant text: the
            // whole line is muted except the status mark.
            Commit::ToolDone {
                name,
                failed,
                cancelled,
                action,
                output,
            } => {
                let (mark, style) = if cancelled {
                    ("⊘", theme::muted())
                } else if failed {
                    ("✗", theme::destructive())
                } else {
                    ("✓", theme::success())
                };
                let mut out = vec![Line::from(vec![
                    Span::styled(format!("⚙ {name} "), theme::muted()),
                    Span::styled(mark, style),
                ])];
                if let Some(action) = action {
                    for line in wrap(&action, width.saturating_sub(4)) {
                        out.push(Line::from(vec![
                            Span::styled("  ", theme::muted()),
                            Span::styled(line, theme::muted().add_modifier(Modifier::ITALIC)),
                        ]));
                    }
                }
                if let Some(output) = output {
                    out.extend(output_lines(&output, width));
                }
                out
            }
            Commit::AgentRun { task, status } => {
                let (mark, style) = match status.as_str() {
                    "completed" => ("✓", theme::success()),
                    "failed" => ("✗", theme::destructive()),
                    "cancelled" => ("⊘", theme::muted()),
                    _ => ("▸", theme::agent()),
                };
                vec![Line::from(vec![
                    Span::styled("🤖 agent ", theme::agent()),
                    Span::styled(mark, style),
                    Span::styled(format!("  {}", truncate(&task, 120)), theme::muted()),
                    Span::styled(format!("  · {status}"), theme::muted()),
                ])]
            }
            Commit::Notice(text) => wrap(&text, width.saturating_sub(2))
                .into_iter()
                .map(|line| Line::from(Span::styled(format!("· {line}"), theme::muted())))
                .collect(),
            Commit::Error(text) => wrap(&text, width.saturating_sub(2))
                .into_iter()
                .map(|line| Line::from(Span::styled(format!("✗ {line}"), theme::destructive())))
                .collect(),
            Commit::Lines(lines) => lines,
        };
        lines.push(Line::default());
        lines
    }
}

/// The bounded exec output a completed command carried: an indented,
/// dim block under the tool line, capped at a few lines per stream.
fn output_lines(output: &ToolOutput, width: usize) -> Vec<Line<'static>> {
    const MAX_STREAM_LINES: usize = 4;
    let mut out = Vec::new();
    let status = match (output.exit_code, output.timed_out) {
        (Some(0), false) => None,
        (Some(code), false) => Some(format!("exit {code}")),
        (None, false) => Some("killed".to_owned()),
        (_, true) => Some("timed out".to_owned()),
    };
    if let Some(status) = status {
        out.push(Line::from(vec![
            Span::styled("  ", theme::muted()),
            Span::styled(status, theme::warning()),
        ]));
    }
    for (label, text) in [("out", &output.stdout), ("err", &output.stderr)] {
        let text = text.trim_end();
        if text.is_empty() {
            continue;
        }
        let mut lines = text.lines();
        let mut shown = 0;
        for line in lines.by_ref().take(MAX_STREAM_LINES) {
            shown += 1;
            out.push(Line::from(vec![
                Span::styled(format!("  {label} "), theme::muted()),
                Span::styled(truncate(line, width.saturating_sub(8)), theme::muted()),
            ]));
        }
        if lines.next().is_some() {
            out.push(Line::from(Span::styled(
                "      …".to_owned(),
                theme::muted(),
            )));
        }
        let _ = shown;
    }
    if output.truncated {
        out.push(Line::from(Span::styled(
            "  (output truncated by the provider)".to_owned(),
            theme::muted(),
        )));
    }
    out
}

/// The compact startup banner committed to scrollback on launch.
pub fn header_lines(resumed: bool, short_id: &str) -> Vec<Line<'static>> {
    let kind = if resumed { "resumed chat" } else { "new chat" };
    vec![
        Line::from(Span::styled("OpenWave", theme::accent_bold())),
        Line::from(Span::styled(format!("{kind} · {short_id}"), theme::muted())),
        Line::from(Span::styled(
            "enter send · alt+enter newline · ctrl+o chats · ctrl+g agents · ctrl+p mode · ctrl+c quit",
            theme::muted(),
        )),
    ]
}

/// The dim reasoning tail shown while the model thinks: at most the last two
/// wrapped lines, so a long stream doesn't crowd the live region.
/// Streaming assistant text, rendered as live markdown so code fences and
/// emphasis form as the text arrives, exactly as the settled block will.
pub fn streaming_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    super::markdown::lines(text, width)
}

/// The spinner frames, shared across the header, running tools, and live
/// agent lines so they stay in phase.
pub const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn spinner_at(tick: usize) -> char {
    SPINNER[tick % SPINNER.len()]
}

/// The running tool's line: spinner in the accent, name plain.
pub fn tool_running_line(name: &str, spinner: char) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{spinner} "), theme::accent()),
        Span::raw(name.to_owned()),
        Span::styled(" …", theme::muted()),
    ])
}

/// A live background-agent line for the transient stack: spinner, task, and
/// the run's current activity.
pub fn agent_running_line(task: &str, status: &str, spinner: char) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{spinner} "), theme::agent()),
        Span::styled("🤖 ", theme::agent()),
        Span::styled(truncate(task, 80), Style::default()),
        Span::styled(format!("  {status}"), theme::muted()),
    ])
}

/// One human-readable line for a tool's closed action preview, if it has one
/// this build understands. Unknown preview variants fall back to nothing
/// rather than leaking a raw JSON dump into the consent card.
pub fn preview_summary(preview: &serde_json::Value) -> Option<String> {
    let get = |key: &str| preview.get(key).and_then(|v| v.as_str());
    let summary = match get("tool")? {
        "exec" => {
            let command = get("command").unwrap_or_default();
            let args = preview
                .get("args")
                .and_then(|v| v.as_array())
                .map(|args| {
                    args.iter()
                        .filter_map(|a| a.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            format!("$ {command} {args}").trim_end().to_owned()
        }
        "search" => format!("search: {}", get("query").unwrap_or_default()),
        "web_search" => format!("web: {}", get("query").unwrap_or_default()),
        "web_extract" => format!("fetch: {}", get("url").unwrap_or_default()),
        "write_file" => format!("write: {}", get("path").unwrap_or_default()),
        "delegate_agent" => format!("background agent: {}", get("task").unwrap_or_default()),
        _ => return None,
    };
    Some(truncate(&summary, 200))
}

/// Pull the bounded exec output out of a tool result preview, if that's what
/// the preview is. Anything else yields `None` — only exec output is text the
/// TUI can usefully print.
pub fn exec_output(result: &serde_json::Value) -> Option<ToolOutput> {
    if result.get("tool")?.as_str()? != "exec" {
        return None;
    }
    let get_str = |key: &str| result.get(key).and_then(|v| v.as_str()).unwrap_or_default();
    Some(ToolOutput {
        exit_code: result
            .get("exit_code")
            .and_then(|v| v.as_i64())
            .map(|code| code as i32),
        timed_out: result
            .get("timed_out")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        truncated: result
            .get("output_truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        stdout: get_str("stdout").to_owned(),
        stderr: get_str("stderr").to_owned(),
    })
}

/// One human-readable line describing a standing-grant rung, for the
/// approval card's option list.
pub fn grant_rung_label(rung: &crate::api::wire::GrantRung, action: &serde_json::Value) -> String {
    use crate::api::wire::GrantRung;
    match rung {
        GrantRung::ExactAction => "always allow exactly this".to_owned(),
        GrantRung::CommandPrefix { tokens } => {
            let command = action
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("this command");
            let args = action
                .get("args")
                .and_then(|v| v.as_array())
                .map(|args| {
                    args.iter()
                        .filter_map(|a| a.as_str())
                        .take(tokens.saturating_sub(1))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let prefix = if args.is_empty() {
                command.to_owned()
            } else {
                format!("{command} {args}")
            };
            format!("always allow any `{prefix}` command")
        }
        GrantRung::PathPrefix { segments } => {
            let path = action.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let prefix: String = path
                .split('/')
                .take(*segments)
                .collect::<Vec<_>>()
                .join("/");
            format!("always allow writes under {prefix}/")
        }
        GrantRung::WholeTool => "don't ask again for this tool in this chat".to_owned(),
    }
}

/// The consent card shown while a decision is pending: an accent gutter on
/// every line, bold title, the humanized kind dimmed on its own line, the
/// preview as a dim italic block, and the options styled as a list.
pub fn approval_card(
    action: &str,
    approval: &str,
    auto_judging: bool,
    preview: Option<&serde_json::Value>,
    grant_rungs: &[crate::api::wire::GrantRung],
    width: usize,
) -> Vec<Line<'static>> {
    let gutter = || Span::styled("▌ ", theme::accent());
    let mut title = vec![
        gutter(),
        Span::styled("Approval required: ", theme::bold()),
        Span::styled(action.to_owned(), theme::accent_bold()),
    ];
    if auto_judging {
        title.push(Span::styled("  (auto-judge deciding…)", theme::muted()));
    }
    let mut lines = vec![
        Line::from(title),
        Line::from(vec![
            gutter(),
            Span::styled(approval.replace('_', " "), theme::muted()),
        ]),
    ];
    if let Some(preview) = preview.and_then(preview_summary) {
        for line in wrap(&preview, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                gutter(),
                Span::styled(line, theme::muted().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    lines.push(Line::from(vec![
        gutter(),
        Span::styled("y", theme::accent_bold()),
        Span::styled(" once", theme::accent()),
        Span::styled("  ·  ", theme::muted()),
        Span::styled("n", theme::bold()),
        Span::styled(" reject", theme::muted()),
        Span::styled("  ·  ", theme::muted()),
        Span::styled("a", theme::bold()),
        Span::styled(" always…", theme::muted()),
    ]));
    let _ = grant_rungs;
    lines
}

/// The standing-grant options list, shown when the user picks "always" on an
/// approval. One line per rung, numbered.
pub fn grant_ladder_lines(
    rungs: &[crate::api::wire::GrantRung],
    action: Option<&serde_json::Value>,
) -> Vec<Line<'static>> {
    let gutter = || Span::styled("▌ ", theme::accent());
    let mut lines = vec![Line::from(vec![
        gutter(),
        Span::styled("remember this decision:", theme::bold()),
    ])];
    let empty = serde_json::Value::Null;
    let action = action.unwrap_or(&empty);
    for (i, rung) in rungs.iter().enumerate() {
        lines.push(Line::from(vec![
            gutter(),
            Span::styled(format!("{} ", i + 1), theme::accent_bold()),
            Span::styled(grant_rung_label(rung, action), theme::muted()),
        ]));
    }
    lines.push(Line::from(vec![
        gutter(),
        Span::styled("esc", theme::muted()),
        Span::styled(" back", theme::muted()),
    ]));
    lines
}

/// The plan-review card for the live region: the plan's title and a bounded
/// slice of its body behind an accent gutter, plus the decision hints. The
/// full plan text stays in the review state; this card shows the head.
pub fn plan_card(title: &str, plan: &str, feedback_open: bool, width: usize) -> Vec<Line<'static>> {
    let gutter = || Span::styled("▌ ", theme::accent());
    let mut lines = vec![
        Line::from(vec![
            gutter(),
            Span::styled("Proposed plan: ", theme::bold()),
            Span::styled(title.to_owned(), theme::accent_bold()),
        ]),
        Line::from(vec![
            gutter(),
            Span::styled(
                "review it below; the full text is in the chat",
                theme::muted(),
            ),
        ]),
    ];
    // Show the plan's own text, bounded, so the decision doesn't need a
    // second surface.
    for line in plan.lines().take(8) {
        for wrapped in wrap(line, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                gutter(),
                Span::styled(wrapped, Style::default()),
            ]));
        }
    }
    if plan.lines().count() > 8 {
        lines.push(Line::from(vec![
            gutter(),
            Span::styled("…", theme::muted()),
        ]));
    }
    if feedback_open {
        lines.push(Line::from(vec![
            gutter(),
            Span::styled(
                "type the changes below · enter sends · esc back",
                theme::muted(),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            gutter(),
            Span::styled("y", theme::accent_bold()),
            Span::styled(" approve & run", theme::accent()),
            Span::styled("  ·  ", theme::muted()),
            Span::styled("f", theme::bold()),
            Span::styled(" request changes", theme::muted()),
            Span::styled("  ·  ", theme::muted()),
            Span::styled("n", theme::bold()),
            Span::styled(" reject", theme::muted()),
        ]));
    }
    lines
}

/// Humanize a token count for the footer meter: `12.3k`, `1.2M`.
pub fn token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Whether the status names a live run (drives the spinner and polling).
pub fn run_is_live(status: &str) -> bool {
    matches!(
        status,
        "queued" | "running" | "cancelling" | "waiting" | "retry_wait"
    )
}

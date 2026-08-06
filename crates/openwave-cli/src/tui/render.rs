//! Pure line-building for the TUI: committed scrollback blocks and the pieces
//! of the repainting live region. No widget state lives here.
//!
//! Wrapping counts characters, not display cells, so a line heavy with wide
//! glyphs can overflow its width — acceptable for this slice's plain-text
//! rendering.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// A finished block ready to commit to real terminal scrollback.
pub enum Commit {
    UserText(String),
    AssistantText(String),
    ToolDone { name: String, failed: bool },
    Notice(String),
    Error(String),
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
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
fn truncate(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

impl Commit {
    /// Render the block to styled lines at the current terminal width.
    pub fn lines(self, width: usize) -> Vec<Line<'static>> {
        match self {
            Commit::UserText(text) => {
                let mut lines: Vec<Line<'static>> = wrap(&text, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let marker = if i == 0 { "❯ " } else { "  " };
                        Line::from(vec![
                            Span::styled(marker, Style::default().fg(Color::Cyan)),
                            Span::styled(line, Style::default().add_modifier(Modifier::BOLD)),
                        ])
                    })
                    .collect();
                lines.push(Line::default());
                lines
            }
            Commit::AssistantText(text) => {
                let mut lines: Vec<Line<'static>> =
                    wrap(&text, width).into_iter().map(Line::from).collect();
                lines.push(Line::default());
                lines
            }
            Commit::ToolDone { name, failed } => {
                let (mark, style) = if failed {
                    ("✗", Style::default().fg(Color::Red))
                } else {
                    ("✓", Style::default().fg(Color::Green))
                };
                vec![Line::from(vec![
                    Span::styled("⚙ ", dim()),
                    Span::raw(name),
                    Span::styled(format!(" {mark}"), style),
                ])]
            }
            Commit::Notice(text) => wrap(&text, width.saturating_sub(2))
                .into_iter()
                .map(|line| Line::from(Span::styled(format!("· {line}"), dim())))
                .collect(),
            Commit::Error(text) => wrap(&text, width.saturating_sub(2))
                .into_iter()
                .map(|line| {
                    Line::from(Span::styled(
                        format!("✗ {line}"),
                        Style::default().fg(Color::Red),
                    ))
                })
                .collect(),
        }
    }
}

/// The dim reasoning tail shown while the model thinks: at most the last two
/// wrapped lines, so a long stream doesn't crowd the live region.
pub fn thinking_lines(thinking: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = wrap(thinking, width.saturating_sub(2));
    if lines.len() > 2 {
        lines.drain(..lines.len() - 2);
    }
    lines
        .into_iter()
        .map(|line| {
            Line::from(Span::styled(
                format!("  {line}"),
                dim().add_modifier(Modifier::ITALIC),
            ))
        })
        .collect()
}

/// Streaming assistant text, wrapped plain.
pub fn streaming_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap(text, width).into_iter().map(Line::from).collect()
}

/// The running tool's spinner line.
pub fn tool_running_line(name: &str, spinner: char) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{spinner} "), Style::default().fg(Color::Yellow)),
        Span::styled("⚙ ", dim()),
        Span::raw(name.to_owned()),
        Span::styled(" …", dim()),
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
        "search" | "web_search" => format!("query: {}", get("query").unwrap_or_default()),
        "web_extract" => format!("fetch: {}", get("url").unwrap_or_default()),
        "write_file" => format!("write: {}", get("path").unwrap_or_default()),
        "delegate_agent" => format!("background agent: {}", get("task").unwrap_or_default()),
        _ => return None,
    };
    Some(truncate(&summary, 200))
}

/// The inline approval card shown while a decision is pending.
pub fn approval_card(
    action: &str,
    approval: &str,
    auto_judging: bool,
    preview: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut heading = vec![
        Span::styled("▲ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("approval required: {action}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" — {}", approval.replace('_', " ")), dim()),
    ];
    if auto_judging {
        heading.push(Span::styled(" (auto-judge deciding…)", dim()));
    }
    lines.push(Line::from(heading));
    if let Some(preview) = preview {
        for line in wrap(preview, width.saturating_sub(2)) {
            lines.push(Line::from(Span::styled(format!("  {line}"), dim())));
        }
    }
    lines.push(Line::from(vec![
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::styled(" approve · ", dim()),
        Span::styled("n", Style::default().fg(Color::Red)),
        Span::styled(" reject", dim()),
    ]));
    lines
}

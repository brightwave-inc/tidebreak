//! Pure line-building for the TUI: committed scrollback blocks and the pieces
//! of the repainting live region. No widget state lives here, and no color is
//! named here — all styling comes from [`super::theme`].
//!
//! Wrapping counts characters, not display cells, so a line heavy with wide
//! glyphs can overflow its width — acceptable for this slice's plain-text
//! rendering.

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme;

/// A finished block ready to commit to real terminal scrollback.
pub enum Commit {
    UserText(String),
    AssistantText(String),
    ToolDone {
        name: String,
        failed: bool,
    },
    Notice(String),
    Error(String),
    /// Pre-rendered one-offs (the startup header).
    Lines(Vec<Line<'static>>),
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
    /// Render the block to styled lines at the current terminal width, with one
    /// blank line after it so every block type breathes evenly.
    pub fn lines(self, width: usize) -> Vec<Line<'static>> {
        let mut lines = match self {
            // The ❯/│ gutter matches the composer's, so a sent message reads
            // as the same element it was typed into.
            Commit::UserText(text) => wrap(&text, width.saturating_sub(2))
                .into_iter()
                .enumerate()
                .map(|(i, line)| {
                    let marker = if i == 0 {
                        Span::styled("❯ ", theme::accent_bold())
                    } else {
                        Span::styled("│ ", theme::muted())
                    };
                    Line::from(vec![marker, Span::styled(line, theme::bold())])
                })
                .collect(),
            Commit::AssistantText(text) => wrap(&text, width).into_iter().map(Line::from).collect(),
            // Completed tool noise recedes behind the assistant text: the whole
            // line is muted except the status mark.
            Commit::ToolDone { name, failed } => {
                let (mark, style) = if failed {
                    ("✗", theme::destructive())
                } else {
                    ("✓", theme::success())
                };
                vec![Line::from(vec![
                    Span::styled(format!("⚙ {name} "), theme::muted()),
                    Span::styled(mark, style),
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

/// The compact startup banner committed to scrollback on launch.
pub fn header_lines(resumed: bool, short_id: &str) -> Vec<Line<'static>> {
    let kind = if resumed { "resumed chat" } else { "new chat" };
    vec![
        Line::from(Span::styled("OpenWave", theme::accent_bold())),
        Line::from(Span::styled(format!("{kind} · {short_id}"), theme::muted())),
        Line::from(Span::styled(
            "enter send · alt+enter newline · ctrl+c quit",
            theme::muted(),
        )),
    ]
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
                theme::muted().add_modifier(Modifier::ITALIC),
            ))
        })
        .collect()
}

/// Streaming assistant text, wrapped plain.
pub fn streaming_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap(text, width).into_iter().map(Line::from).collect()
}

/// The running tool's line: spinner in the accent, name plain.
pub fn tool_running_line(name: &str, spinner: char) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{spinner} "), theme::accent()),
        Span::raw(name.to_owned()),
        Span::styled(" …", theme::muted()),
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

/// The consent card shown while a decision is pending: an accent gutter on
/// every line, bold title, the humanized kind dimmed on its own line, the
/// preview as a dim italic block, and the two actions styled as buttons.
pub fn approval_card(
    action: &str,
    approval: &str,
    auto_judging: bool,
    preview: Option<&str>,
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
    if let Some(preview) = preview {
        for line in wrap(preview, width.saturating_sub(2)) {
            lines.push(Line::from(vec![
                gutter(),
                Span::styled(line, theme::muted().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    lines.push(Line::from(vec![
        gutter(),
        Span::styled("y", theme::accent_bold()),
        Span::styled(" approve", theme::accent()),
        Span::styled("  ·  ", theme::muted()),
        Span::styled("n", theme::bold()),
        Span::styled(" reject", theme::muted()),
    ]));
    lines
}

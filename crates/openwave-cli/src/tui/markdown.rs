//! Terminal markdown for committed assistant text: pulldown-cmark parses,
//! syntect highlights fenced code blocks, and everything lands as styled
//! lines built from the [`super::theme`] palette — nothing is colored inline
//! here. Only [`super::render`]'s commit path calls this; the streaming live
//! region stays plain text until the block is final.
//!
//! Wrapping counts characters, not display cells, so the wide-glyph caveat
//! from [`super::render`] applies unchanged. Code block lines are never
//! word-wrapped: they truncate with `…` past the width.

use std::str::FromStr;
use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SyntectColor, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use super::theme;

/// One block of parsed markdown.
enum Block {
    Heading(u8, Vec<Inline>),
    Paragraph(Vec<Inline>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Rule,
}

/// A run of inline text with its style flags resolved at parse time.
struct Inline {
    text: String,
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
}

/// Container stack for the block builder: quotes and lists nest.
enum Ctx {
    Root {
        blocks: Vec<Block>,
    },
    Quote {
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
        current: Vec<Block>,
    },
}

impl Ctx {
    fn blocks_mut(&mut self) -> &mut Vec<Block> {
        match self {
            Ctx::Root { blocks } | Ctx::Quote { blocks } => blocks,
            Ctx::List { current, .. } => current,
        }
    }
}

/// Event-driven block builder. Inline runs accumulate for the current
/// paragraph or heading; finished blocks land on the innermost container.
struct Builder {
    stack: Vec<Ctx>,
    inlines: Vec<Inline>,
    heading: Option<u8>,
    bold: bool,
    italic: bool,
    strike: bool,
    link: Option<String>,
    code: Option<(Option<String>, String)>,
}

impl Builder {
    fn new() -> Self {
        Self {
            stack: vec![Ctx::Root { blocks: Vec::new() }],
            inlines: Vec::new(),
            heading: None,
            bold: false,
            italic: false,
            strike: false,
            link: None,
            code: None,
        }
    }

    fn push_text(&mut self, text: &str, code: bool) {
        if text.is_empty() {
            return;
        }
        // Merge into the previous run when the style hasn't changed.
        if let Some(last) = self.inlines.last_mut() {
            if last.bold == self.bold
                && last.italic == self.italic
                && last.strike == self.strike
                && last.code == code
                && last.link == self.link
            {
                last.text.push_str(text);
                return;
            }
        }
        self.inlines.push(Inline {
            text: text.to_owned(),
            bold: self.bold,
            italic: self.italic,
            strike: self.strike,
            code,
            link: self.link.clone(),
        });
    }

    fn end_block(&mut self, block: Block) {
        if let Some(ctx) = self.stack.last_mut() {
            ctx.blocks_mut().push(block);
        }
    }

    fn take_inlines(&mut self) -> Vec<Inline> {
        std::mem::take(&mut self.inlines)
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Parse assistant text into blocks. Anything the parser can't recognize
/// degrades to plain text runs rather than disappearing.
fn parse(text: &str) -> Vec<Block> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    let mut b = Builder::new();
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => b.heading = Some(heading_level(level)),
                Tag::BlockQuote(_) => b.stack.push(Ctx::Quote { blocks: Vec::new() }),
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(lang) => {
                            let lang = lang.trim();
                            (!lang.is_empty()).then(|| lang.to_owned())
                        }
                        CodeBlockKind::Indented => None,
                    };
                    b.code = Some((lang, String::new()));
                }
                Tag::List(start) => b.stack.push(Ctx::List {
                    start,
                    items: Vec::new(),
                    current: Vec::new(),
                }),
                Tag::Item => {}
                Tag::Strong => b.bold = true,
                Tag::Emphasis => b.italic = true,
                Tag::Strikethrough => b.strike = true,
                Tag::Link { dest_url, .. } => b.link = Some(dest_url.into_string()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    let inlines = b.take_inlines();
                    b.end_block(Block::Paragraph(inlines));
                }
                TagEnd::Heading(_) => {
                    let level = b.heading.take().unwrap_or(1);
                    let inlines = b.take_inlines();
                    b.end_block(Block::Heading(level, inlines));
                }
                TagEnd::BlockQuote(_) => {
                    if let Some(Ctx::Quote { blocks }) = b.stack.pop() {
                        b.end_block(Block::Quote(blocks));
                    }
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, text)) = b.code.take() {
                        b.end_block(Block::Code { lang, text });
                    }
                }
                TagEnd::List(_) => {
                    if let Some(Ctx::List { start, items, .. }) = b.stack.pop() {
                        b.end_block(Block::List { start, items });
                    }
                }
                TagEnd::Item => {
                    // Tight lists emit item text without Paragraph tags, so
                    // flush any pending inlines as the item's paragraph.
                    if !b.inlines.is_empty() {
                        let inlines = b.take_inlines();
                        b.end_block(Block::Paragraph(inlines));
                    }
                    if let Some(Ctx::List { items, current, .. }) = b.stack.last_mut() {
                        items.push(std::mem::take(current));
                    }
                }
                TagEnd::Strong => b.bold = false,
                TagEnd::Emphasis => b.italic = false,
                TagEnd::Strikethrough => b.strike = false,
                TagEnd::Link => b.link = None,
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, buf)) = &mut b.code {
                    buf.push_str(&text);
                } else {
                    b.push_text(&text, false);
                }
            }
            Event::Code(text) => b.push_text(&text, true),
            Event::SoftBreak => b.push_text(" ", false),
            Event::HardBreak => b.push_text("\n", false),
            Event::Rule => b.end_block(Block::Rule),
            Event::TaskListMarker(checked) => b.push_text(if checked { "☑ " } else { "☐ " }, false),
            _ => {}
        }
    }
    // The root is the stack's bottom, not its top — taking the first element
    // keeps the blocks even if malformed input left containers unclosed.
    match b.stack.into_iter().next() {
        Some(Ctx::Root { blocks }) => blocks,
        _ => Vec::new(),
    }
}

/// Render committed assistant markdown to styled lines at `width`.
pub fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let blocks = parse(text);
    let mut out = Vec::new();
    render_blocks(&blocks, width.max(8), &mut out);
    out
}

/// Blocks are separated by one blank line, matching the rhythm the other
/// commit types get from their trailing blank.
fn render_blocks(blocks: &[Block], width: usize, out: &mut Vec<Line<'static>>) {
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            out.push(Line::default());
        }
        render_block(block, width, out);
    }
}

fn render_block(block: &Block, width: usize, out: &mut Vec<Line<'static>>) {
    match block {
        Block::Heading(level, inlines) => render_heading(*level, inlines, width, out),
        Block::Paragraph(inlines) => {
            out.extend(wrap_spans(&inline_spans(inlines, None), width));
        }
        Block::Code { lang, text } => render_code(lang.as_deref(), text, width, out),
        Block::Quote(blocks) => {
            // Quote contents render narrower, then every line — blanks
            // included — picks up the muted gutter so the bar stays unbroken.
            let mut inner = Vec::new();
            render_blocks(blocks, width.saturating_sub(2).max(8), &mut inner);
            out.extend(inner.into_iter().map(|line| guttered(line)));
        }
        Block::List { start, items } => render_list(*start, items, width, out),
        Block::Rule => out.push(Line::from(Span::styled("─".repeat(width), theme::muted()))),
    }
}

/// Headings read accent + bold behind a muted `#`-per-level cue; wrapped
/// continuations align under the text, not the cue.
fn render_heading(level: u8, inlines: &[Inline], width: usize, out: &mut Vec<Line<'static>>) {
    let cue = "#".repeat(level as usize);
    let indent = level as usize + 1;
    let wrapped = wrap_spans(
        &inline_spans(inlines, Some(theme::ACCENT)),
        width.saturating_sub(indent),
    );
    for (i, line) in wrapped.into_iter().enumerate() {
        let prefix = if i == 0 {
            Span::styled(format!("{cue} "), theme::muted())
        } else {
            Span::raw(" ".repeat(indent))
        };
        let mut spans = vec![prefix];
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }
}

/// Lists: muted markers, continuations aligned under the item text. Nested
/// lists fall out of the recursion.
fn render_list(
    start: Option<u64>,
    items: &[Vec<Block>],
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let mut index = start.unwrap_or(1);
    for item in items {
        let marker = if start.is_some() {
            let marker = format!("{index}.");
            index += 1;
            marker
        } else {
            "•".to_owned()
        };
        let indent = marker.chars().count() + 1;
        let mut inner = Vec::new();
        render_blocks(item, width.saturating_sub(indent).max(8), &mut inner);
        for (i, line) in inner.into_iter().enumerate() {
            let prefix = if i == 0 {
                Span::styled(format!("{marker} "), theme::muted())
            } else {
                Span::raw(" ".repeat(indent))
            };
            let mut spans = vec![prefix];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
    }
}

/// Fenced code: a dim language label line when the fence names one, then
/// highlighted (or, for unknown/absent languages, plain muted) lines behind
/// a muted gutter. Lines truncate with `…` rather than wrapping.
fn render_code(lang: Option<&str>, text: &str, width: usize, out: &mut Vec<Line<'static>>) {
    let inner = width.saturating_sub(2).max(8);
    if let Some(lang) = lang {
        let mut label = vec![Span::styled("▎ ", theme::muted())];
        label.push(Span::styled(lang.to_owned(), theme::muted()));
        out.push(Line::from(label));
    }
    let highlighted = match lang {
        Some(lang) => highlight(lang, text),
        None => plain_code_lines(text),
    };
    for spans in highlighted {
        let line = Line::from(
            truncate_spans(spans, inner)
                .into_iter()
                .map(|(text, style)| Span::styled(text, style))
                .collect::<Vec<_>>(),
        );
        out.push(guttered(line));
    }
}

/// Prefix a line with the muted `▎` gutter shared by quotes and code blocks.
fn guttered(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::styled("▎ ", theme::muted())];
    spans.extend(line.spans);
    Line::from(spans)
}

/// The resolved style for one inline run. `base_fg` overrides the foreground
/// for heading text; inline code keeps its own color either way.
fn inline_style(inline: &Inline, base_fg: Option<Color>) -> Style {
    let mut style = if inline.code {
        theme::inline_code()
    } else {
        match base_fg {
            Some(fg) => Style::default().fg(fg),
            None => Style::default(),
        }
    };
    if inline.bold || base_fg.is_some() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if inline.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if inline.strike {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if inline.link.is_some() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// Flatten inline runs to styled text segments. Links render as underlined
/// text followed by the dim URL — terminals can't click reliably, so the
/// destination stays visible.
fn inline_spans(inlines: &[Inline], base_fg: Option<Color>) -> Vec<(String, Style)> {
    let mut spans = Vec::new();
    for inline in inlines {
        let style = inline_style(inline, base_fg);
        spans.push((inline.text.clone(), style));
        if let Some(url) = &inline.link {
            if url != &inline.text {
                spans.push((format!(" ({url})"), theme::muted()));
            }
        }
    }
    spans
}

/// Word-wrap styled segments to `width`, preserving hard newlines; adjacent
/// text sharing a style merges into one span. Mirrors `render::wrap`'s
/// char-count semantics, hard-breaking words longer than the width.
fn wrap_spans(segments: &[(String, Style)], width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out: Vec<Vec<(String, Style)>> = Vec::new();
    let mut line: Vec<(String, Style)> = Vec::new();
    let mut col = 0usize;

    fn push_word(line: &mut Vec<(String, Style)>, word: &str, style: Style) {
        if word.is_empty() {
            return;
        }
        if let Some((text, existing)) = line.last_mut() {
            if *existing == style {
                text.push_str(word);
                return;
            }
        }
        line.push((word.to_owned(), style));
    }

    for (text, style) in segments {
        for (piece_i, piece) in text.split('\n').enumerate() {
            if piece_i > 0 {
                out.push(std::mem::take(&mut line));
                col = 0;
            }
            for word in piece.split(' ') {
                let word_len = word.chars().count();
                let gap = usize::from(col > 0);
                if col + gap + word_len > width && col > 0 {
                    out.push(std::mem::take(&mut line));
                    col = 0;
                } else if gap == 1 {
                    push_word(&mut line, " ", *style);
                    col += 1;
                }
                let mut rest = word;
                while rest.chars().count() > width - col && col < width {
                    let take = width - col;
                    let cut = rest.char_indices().nth(take).map_or(rest.len(), |(i, _)| i);
                    push_word(&mut line, &rest[..cut], *style);
                    out.push(std::mem::take(&mut line));
                    rest = &rest[cut..];
                    col = 0;
                }
                push_word(&mut line, rest, *style);
                col += rest.chars().count();
            }
        }
    }
    out.push(line);
    out.into_iter()
        .map(|spans| {
            Line::from(
                spans
                    .into_iter()
                    .map(|(text, style)| Span::styled(text, style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Truncate a span list to `width` characters, ending with `…` when cut.
fn truncate_spans(spans: Vec<(String, Style)>, width: usize) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let mut col = 0usize;
    for (text, style) in spans {
        let len = text.chars().count();
        if col + len <= width {
            col += len;
            out.push((text, style));
        } else {
            let budget = width.saturating_sub(col + 1);
            let cut = text
                .char_indices()
                .nth(budget)
                .map_or(text.len(), |(i, _)| i);
            let mut piece = text[..cut].to_owned();
            piece.push('…');
            out.push((piece, style));
            break;
        }
    }
    out
}

/// Unhighlighted code: one muted segment per source line.
fn plain_code_lines(code: &str) -> Vec<Vec<(String, Style)>> {
    code.lines()
        .map(|line| vec![(line.to_owned(), theme::muted())])
        .collect()
}

/// The syntax set and the hand-built theme, loaded once. Regex compilation
/// stays lazy per syntax, so grammars the chat never sees never compile.
struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static HIGHLIGHT: LazyLock<Highlighter> = LazyLock::new(|| Highlighter {
    syntaxes: SyntaxSet::load_defaults_newlines(),
    theme: syntax_theme(),
});

fn syntect_color(color: Color) -> Option<SyntectColor> {
    match color {
        Color::Rgb(r, g, b) => Some(SyntectColor { r, g, b, a: 255 }),
        _ => None,
    }
}

/// A small scope-class → palette mapping, built from [`theme`] constants
/// rather than a syntect theme dump so highlighting stays cohesive with the
/// rest of the TUI. Foreground-only; unmapped scopes keep the terminal's
/// default text color.
fn syntax_theme() -> Theme {
    let item = |scope: &str, color: Color| ThemeItem {
        scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
        style: StyleModifier {
            foreground: syntect_color(color),
            background: None,
            font_style: None,
        },
    };
    Theme {
        name: Some("openwave-tui".to_owned()),
        author: None,
        settings: ThemeSettings::default(),
        scopes: vec![
            item("comment", theme::CODE_COMMENT),
            item("string", theme::CODE_STRING),
            item("constant", theme::CODE_NUMBER),
            item("keyword, storage", theme::CODE_KEYWORD),
            item(
                "entity.name.function, support.function",
                theme::CODE_FUNCTION,
            ),
            item(
                "entity.name.type, entity.name.class, support.type, support.class",
                theme::CODE_TYPE,
            ),
        ],
    }
}

/// Highlight `code` as `lang`, one span list per line. Unknown languages and
/// highlighter errors degrade to the plain muted rendering — only the
/// coloring is ever at risk, never the text.
fn highlight(lang: &str, code: &str) -> Vec<Vec<(String, Style)>> {
    let highlighter = &*HIGHLIGHT;
    let Some(syntax) = highlighter.syntaxes.find_syntax_by_token(lang) else {
        return plain_code_lines(code);
    };
    let mut state = HighlightLines::new(syntax, &highlighter.theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        match state.highlight_line(line, &highlighter.syntaxes) {
            Ok(ranges) => {
                let mut spans: Vec<(String, Style)> = ranges
                    .into_iter()
                    .map(|(style, text)| {
                        (
                            text.to_owned(),
                            Style::default().fg(Color::Rgb(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            )),
                        )
                    })
                    .collect();
                // `load_defaults_newlines` keeps the trailing newline inside
                // the last range's text; strip it so it can't pad the line.
                if let Some((text, _)) = spans.last_mut() {
                    let trimmed = text.trim_end_matches(['\n', '\r']).len();
                    text.truncate(trimmed);
                }
                out.push(spans);
            }
            Err(_) => out.push(vec![(
                line.trim_end_matches(['\n', '\r']).to_owned(),
                theme::muted(),
            )]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract worth pinning: a fenced block whose language the syntax
    /// set doesn't know still renders — no panic, and its text survives
    /// verbatim (only the coloring degrades).

    #[test]
    fn unknown_language_code_block_preserves_text() {
        let rendered = lines("intro\n\n```not-a-real-lang\nfn main() {}\n```\n", 80);
        let text: String = rendered
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(text.contains("intro"));
        assert!(text.contains("not-a-real-lang"));
        assert!(text.contains("fn main() {}"));
    }
}

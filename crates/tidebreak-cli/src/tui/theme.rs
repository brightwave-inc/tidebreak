//! The TUI's single palette: every color and style in the terminal UI comes
//! from these constants — nothing inline elsewhere.
//!
//! Values derive from the desktop app's dark-theme design tokens
//! (`crates/tidebreak-desktop/ui/src/styles.css`, the `.dark` block), converted
//! from oklch to sRGB. Brand and muted tones are fixed RGB — terminals are
//! overwhelmingly truecolor and the TUI assumes the dark theme, as the desktop
//! does — while pure semantic success/error stay on ANSI colors so they keep
//! respecting the user's own terminal theme.

use ratatui::style::{Color, Modifier, Style};

/// The one brand accent: the wordmark, ❯ input markers, the working spinner,
/// the approval card's gutter and approve action. From `--brand-accent`
/// (oklch 0.87 0.2 102).
pub const ACCENT: Color = Color::Rgb(240, 215, 0);

/// Secondary text: notices, placeholders, key hints, dim continuation
/// markers, and the receded body of completed tool lines. From
/// `--muted-foreground` (oklch 0.68 0.006 262).
pub const MUTED: Color = Color::Rgb(150, 152, 156);

/// Semantic success. Deliberately ANSI green rather than the `--success`
/// token's RGB: the user's terminal theme owns what "green" looks like.
pub const SUCCESS: Color = Color::Green;

/// Semantic error. Deliberately ANSI red (the desktop reference is
/// `--destructive`), same reasoning as [`SUCCESS`].
pub const DESTRUCTIVE: Color = Color::Red;

/// Inline code spans in assistant markdown. A soft teal — distinct from both
/// prose and the accent, and light enough to sit next to [`MUTED`] on a dark
/// terminal.
pub const INLINE_CODE: Color = Color::Rgb(86, 182, 194);

/// Agent (assistant) designation. A soft violet — clearly not the user's
/// accent, clearly not an error. From the desktop's agent-chrome hue family.
pub const AGENT: Color = Color::Rgb(178, 148, 214);

/// Warning / attention. ANSI yellow, same reasoning as [`SUCCESS`]: the
/// user's terminal theme owns the exact tone.
pub const WARNING: Color = Color::Yellow;

/// Selected-row highlight in overlays (chat switcher, agents, pickers). A
/// very dark cool gray — enough contrast to read as selection without
/// fighting the foreground.
pub const SELECTION_BG: Color = Color::Rgb(56, 58, 64);

/// Subtle panel fill behind overlays.
pub const PANEL_BG: Color = Color::Rgb(32, 33, 36);

/// Border color for cards and overlay panels.
pub const BORDER: Color = Color::Rgb(64, 66, 72);

/// Code-block token classes for syntax highlighting. Foreground-only by
/// design: background fills read wrong against arbitrary terminal themes.
/// Hues follow the One Dark family (the desktop dark theme's spiritual
/// cousin), held near the muted foreground's lightness so the ramp stays
/// cohesive with [`ACCENT`] and [`MUTED`]. Keywords reuse the brand accent;
/// comments reuse the muted foreground.
pub const CODE_KEYWORD: Color = ACCENT;
/// String literals: soft green.
pub const CODE_STRING: Color = Color::Rgb(152, 195, 121);
/// Numbers and language constants: soft orange.
pub const CODE_NUMBER: Color = Color::Rgb(209, 154, 102);
/// Function names: soft blue.
pub const CODE_FUNCTION: Color = Color::Rgb(97, 175, 239);
/// Type and class names: soft violet (kept apart from [`INLINE_CODE`]).
pub const CODE_TYPE: Color = Color::Rgb(178, 148, 214);
/// Comments (and the unknown-language fallback): the muted foreground.
pub const CODE_COMMENT: Color = MUTED;

/// Accent text.
pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

/// Accent, bold — the wordmark, markers, the card title's tool name.
pub fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

/// Muted secondary text.
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

/// Bold default foreground — the user's own input echo, card titles.
pub fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Semantic success (the ✓ status mark).
pub fn success() -> Style {
    Style::default().fg(SUCCESS)
}

/// Semantic error (error lines, the ✗ status mark).
pub fn destructive() -> Style {
    Style::default().fg(DESTRUCTIVE)
}

/// Inline code within assistant prose.
pub fn inline_code() -> Style {
    Style::default().fg(INLINE_CODE)
}

/// Agent designation — the assistant's role label and agent-run markers.
pub fn agent() -> Style {
    Style::default().fg(AGENT)
}

/// Agent designation, bold — the assistant role label.
pub fn agent_bold() -> Style {
    agent().add_modifier(Modifier::BOLD)
}

/// Warning / needs-attention text.
pub fn warning() -> Style {
    Style::default().fg(WARNING)
}

/// The selected row in an overlay list.
pub fn selected() -> Style {
    Style::default().bg(SELECTION_BG)
}

/// Card and overlay borders.
pub fn border() -> Style {
    Style::default().fg(BORDER)
}

/// User designation — the user's role label, same accent family as the
/// composer marker so the two read as one surface.
pub fn user() -> Style {
    accent_bold()
}

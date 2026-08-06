//! The TUI's single palette: every color and style in the terminal UI comes
//! from these constants — nothing inline elsewhere.
//!
//! Values derive from the desktop app's dark-theme design tokens
//! (`crates/openwave-desktop/ui/src/styles.css`, the `.dark` block), converted
//! from oklch to sRGB. Brand and muted tones are fixed RGB — terminals are
//! overwhelmingly truecolor and the TUI assumes the dark theme, as the desktop
//! does — while pure semantic success/error stay on ANSI colors so they keep
//! respecting the user's own terminal theme.

use ratatui::style::{Color, Modifier, Style};

/// The one brand accent: the wordmark, ❯ input markers, the working spinner,
/// the approval card's gutter and approve action. From `--brightwave`
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

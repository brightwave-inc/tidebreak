//! One text-input surface for the TUI, wrapping the composer textarea with
//! the small editing conveniences the desktop's input has: alt+backspace word
//! delete, alt+arrow word motion, and ctrl+z/ctrl+shift+z undo/redo.
//!
//! Everything funnels through [`Composer::key`], which maps the raw key onto
//! either a `tui-textarea` `Input` or a direct method call (undo/redo live on
//! the textarea itself, not in its input vocabulary).

use ratatui::style::{Modifier, Style};
use tui_textarea::{CursorMove, Input, TextArea};

use super::theme;

/// Build a composer with its initial lines (empty string for one blank).
pub fn new(lines: Vec<String>) -> TextArea<'static> {
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

/// Feed one key to the textarea, with the word-wise conveniences the raw
/// input mapping doesn't have. Returns whether the key was consumed here
/// (the caller's own bindings get first refusal, so this only runs for keys
/// the app treats as text editing).
pub fn edit_key(composer: &mut TextArea<'static>, key: crossterm::event::KeyEvent) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match (key.code, alt, ctrl, shift) {
        // Word deletion: alt+backspace deletes the word behind the cursor;
        // ctrl+w does the same, matching every shell the user knows.
        (KeyCode::Backspace, true, false, false) | (KeyCode::Char('w'), false, true, false) => {
            composer.delete_word();
        }
        // Word motion on alt+arrows.
        (KeyCode::Left, true, false, false) => composer.move_cursor(CursorMove::WordBack),
        (KeyCode::Right, true, false, false) => composer.move_cursor(CursorMove::WordForward),
        // Line motion on cmd/super-ish terminals that deliver ctrl+arrows.
        (KeyCode::Left, false, true, false) => composer.move_cursor(CursorMove::Head),
        (KeyCode::Right, false, true, false) => composer.move_cursor(CursorMove::End),
        // Undo/redo, the desktop's own binding.
        (KeyCode::Char('z'), false, true, false) => {
            composer.undo();
        }
        (KeyCode::Char('z'), false, true, true) | (KeyCode::Char('y'), false, true, false) => {
            composer.redo();
        }
        // Kill-to-line-end on ctrl+k, matching readline.
        (KeyCode::Char('k'), false, true, false) => {
            composer.delete_line_by_end();
        }
        // Kill-to-line-start on ctrl+u.
        (KeyCode::Char('u'), false, true, false) => {
            composer.delete_line_by_head();
        }
        _ => {
            composer.input(Input::from(key));
        }
    }
}

/// A single-line input (rename boxes, feedback prompts): same surface, no
/// newlines accepted.
pub fn new_single_line(initial: &str, placeholder: &str) -> TextArea<'static> {
    let mut input = new(vec![initial.to_owned()]);
    input.set_placeholder_text(placeholder);
    input
}

/// Enter never inserts a newline in a single-line input; the caller owns what
/// Enter means (submit).
pub fn single_line_key(input: &mut TextArea<'static>, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;
    if key.code == KeyCode::Enter {
        return;
    }
    edit_key(input, key);
}

/// The Input mapping the textarea crate ships already covers plain keys; this
/// re-export keeps the one place the app constructs `Input` values (paste).
pub fn paste(composer: &mut TextArea<'static>, text: &str) {
    composer.insert_str(text);
}

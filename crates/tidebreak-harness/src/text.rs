//! Byte-capped text handling that never splits a character.
//!
//! Engine stdout, shell stderr, and unrecognized payloads are all capped by
//! byte length before they are logged or buffered. `String::truncate` panics
//! when the cap lands inside a multi-byte character, so every cap in this
//! crate goes through the helpers here instead.

/// Largest index at or below `max_bytes` that is a character boundary of
/// `text`. Returns `text.len()` when the cap is not reached.
pub(crate) fn floor_char_boundary(text: &str, max_bytes: usize) -> usize {
    if max_bytes >= text.len() {
        return text.len();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Shorten `text` to at most `max_bytes`, cutting on a character boundary.
pub(crate) fn truncate_on_char_boundary(text: &mut String, max_bytes: usize) {
    let end = floor_char_boundary(text, max_bytes);
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_landing_inside_a_character_drops_that_character() {
        // "é" is two bytes, so a cap of 2 lands between them.
        let mut text = String::from("aéb");
        truncate_on_char_boundary(&mut text, 2);
        assert_eq!(text, "a");

        // A three-byte character straddling the cap from either side.
        let mut text = String::from("a€b");
        truncate_on_char_boundary(&mut text, 3);
        assert_eq!(text, "a");
        let mut text = String::from("a€b");
        truncate_on_char_boundary(&mut text, 4);
        assert_eq!(text, "a€");

        // Caps at or past the end leave the string alone.
        let mut text = String::from("a€b");
        truncate_on_char_boundary(&mut text, 64);
        assert_eq!(text, "a€b");

        // A cap that cannot fit even the first character yields nothing.
        let mut text = String::from("€");
        truncate_on_char_boundary(&mut text, 1);
        assert!(text.is_empty());
    }
}

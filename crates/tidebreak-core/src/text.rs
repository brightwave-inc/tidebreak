//! Small UTF-8 helpers.

/// The leading `max_bytes` of `value` on a character boundary, and whether
/// the value was longer than that.
#[must_use]
pub fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

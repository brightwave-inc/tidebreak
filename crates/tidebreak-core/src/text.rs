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

#[cfg(test)]
mod tests {
    use super::truncate_utf8;

    #[test]
    fn byte_limits_preserve_complete_characters_and_report_omitted_bytes() {
        for (value, max_bytes, expected, truncated) in [
            ("", 0, "", false),
            ("a", 0, "", true),
            ("é", 1, "", true),
            ("é", 2, "é", false),
            ("a€z", 3, "a", true),
            ("a€z", 4, "a€", true),
            ("a€z", 5, "a€z", false),
            ("a€z", usize::MAX, "a€z", false),
        ] {
            assert_eq!(
                truncate_utf8(value, max_bytes),
                (expected.to_owned(), truncated),
                "value={value:?}, max_bytes={max_bytes}"
            );
        }
    }
}

//! Canonical text decoding for uploaded source bytes.

/// Decode source bytes according to their declared media type.
///
/// Text, JSON, and XML are decoded lossily so one malformed byte does not hide
/// an otherwise-readable source. Unknown formats retain valid UTF-8 text but
/// treat other bytes as binary and produce no canonical text.
pub fn decode_document(media_type: &str, raw: &[u8]) -> String {
    if is_text_media_type(media_type) {
        String::from_utf8_lossy(raw).into_owned()
    } else {
        std::str::from_utf8(raw)
            .map(str::to_owned)
            .unwrap_or_default()
    }
}

fn is_text_media_type(media_type: &str) -> bool {
    let base = media_type.split(';').next().unwrap_or(media_type).trim();
    let base = base.to_ascii_lowercase();
    base.is_empty()
        || base.starts_with("text/")
        || base == "application/json"
        || base == "application/xml"
        || base.ends_with("+json")
        || (!base.starts_with("image/") && base.ends_with("+xml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_structured_text_media_decode_lossily() {
        for media_type in [
            "text/plain",
            "text/markdown; charset=utf-8",
            "TEXT/PLAIN",
            "",
            "application/json",
            "application/xml",
            "application/ld+json",
        ] {
            assert_eq!(
                decode_document(media_type, &[b'a', 0xFF, b'b']),
                "a\u{FFFD}b",
                "{media_type}"
            );
        }
    }

    #[test]
    fn unknown_utf8_stays_readable_and_binary_is_empty() {
        assert_eq!(
            decode_document("application/octet-stream", b"level=info msg=\"started\""),
            "level=info msg=\"started\""
        );
        assert!(decode_document("application/octet-stream", &[0x00, 0xFF, 0x89, 0x50]).is_empty());
        assert!(decode_document("image/png", &[0x89, 0x50, 0x4E, 0x47]).is_empty());
    }
}

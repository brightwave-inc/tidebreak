//! Shared Server-Sent Events framing helpers for streaming provider adapters.
//!
//! Provider streams arrive as byte chunks that may split mid-frame or mid-UTF-8
//! character. These helpers accumulate raw bytes, drain complete frames, and
//! extract the `data:` JSON payload — the same shape Anthropic and OpenAI-compat
//! both speak.

/// Naive byte-substring search.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Drain all complete SSE frames from `buffer`, returning each frame's decoded
/// text and leaving any incomplete trailing bytes behind.
///
/// Frames are separated by a blank line (`\n\n` or `\r\n\r\n`). Decoding to UTF-8
/// happens only on a complete frame, so a multi-byte character split across
/// network chunks is never decoded until all its bytes have arrived.
pub fn drain_frames(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let lf = find_subslice(buffer, b"\n\n");
        let crlf = find_subslice(buffer, b"\r\n\r\n");
        let (content_end, consume_to) = match (lf, crlf) {
            (Some(i), Some(j)) => {
                if i <= j {
                    (i, i + 2)
                } else {
                    (j, j + 4)
                }
            }
            (Some(i), None) => (i, i + 2),
            (None, Some(j)) => (j, j + 4),
            (None, None) => break,
        };
        frames.push(String::from_utf8_lossy(&buffer[..content_end]).into_owned());
        buffer.drain(..consume_to);
    }
    frames
}

/// Extract the concatenated `data:` payload from one SSE frame.
///
/// Returns `None` for comment/ping frames with no data, or when the payload is
/// the OpenAI stream terminator `[DONE]`.
pub fn frame_data_raw(frame: &str) -> Option<String> {
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() || data == "[DONE]" {
        return None;
    }
    Some(data)
}

/// Extract and parse the `data:` JSON payload from one SSE frame.
pub fn frame_data(frame: &str) -> Option<serde_json::Value> {
    frame_data_raw(frame).and_then(|data| serde_json::from_str(&data).ok())
}

/// Build a client-safe provider error: status + optional stable `error.type` /
/// `error.code` from the JSON body. Never include the raw body (it can echo
/// secrets, and `AgentError` strings reach the client via `TurnFailed`).
pub fn safe_http_error(provider: &str, status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let err = v.get("error").unwrap_or(&v);
            err.get("type")
                .or_else(|| err.get("code"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
    match detail {
        Some(code) => format!("{provider} returned {status} ({code})"),
        None => format!("{provider} returned {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_frames_handles_lf_crlf_and_partial_tail() {
        let mut buf = b"data: {\"a\":1}\n\ndata: partial".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].contains("{\"a\":1}"));
        assert_eq!(buf, b"data: partial");

        let mut buf = b"event: x\r\ndata: {\"b\":2}\r\n\r\n".to_vec();
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["b"], 2);
        assert!(buf.is_empty());
    }

    #[test]
    fn multibyte_char_split_across_chunks_is_not_corrupted() {
        let full = "data: {\"t\":\"café\"}\n\n".as_bytes().to_vec();
        let split = full.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let (head, tail) = full.split_at(split);

        let mut buf = head.to_vec();
        assert!(drain_frames(&mut buf).is_empty(), "no complete frame yet");
        buf.extend_from_slice(tail);
        let frames = drain_frames(&mut buf);
        assert_eq!(frames.len(), 1);
        assert_eq!(frame_data(&frames[0]).unwrap()["t"], "café");
    }

    #[test]
    fn frame_data_skips_done_terminator() {
        assert!(frame_data_raw("data: [DONE]").is_none());
        assert!(frame_data("event: ping").is_none());
    }
}

//! Recover what an oversized stream line started with.
//!
//! A line longer than the parse budget reaches the parser cut at the cap
//! ([`crate::StreamLine::cut`]), and a cut line is not valid JSON. The large
//! part of such a line is nearly always a payload: a tool's output, an image,
//! a file's contents. The envelope that says what the line is — its type, the
//! call it belongs to, the call's status — comes first. Dropping the line
//! loses the event, and a tool call whose completion is dropped stays running
//! in the transcript forever.
//!
//! [`CutLine::recover`] reads the complete values before the cut, leaves out
//! the value the cut split, and closes every object and array left open. It
//! records where the cut landed and the start of a string it split. A parser
//! uses that to tell a lost payload, which it replaces with
//! [`OVERSIZED_PAYLOAD`], from a lost envelope field, which it restores from
//! what it already knows or goes without. Part of an id names nothing, so a
//! partial string is never left standing in the value.

use serde_json::{Map, Value};

/// What the transcript shows in place of a payload that did not fit.
pub const OVERSIZED_PAYLOAD: &str = "Output too large to show.";

/// Longest partial text kept from the string a cut landed in.
///
/// Parsers use it only for text a person reads, which every event bounds to
/// far less than this.
const MAX_CUT_TEXT_BYTES: usize = 64 * 1_024;

/// One key or index on the way from a line's root to the cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutStep {
    /// An object member.
    Key(String),
    /// An array element.
    Index(usize),
}

/// A cut JSON line, closed into a value.
#[derive(Debug, Clone, PartialEq)]
pub struct CutLine {
    /// Every value that was complete before the cut.
    ///
    /// A string, number, literal, or key the cut split is left out, and so
    /// is every member after the cut. An object or array the cut landed in
    /// keeps the members that arrived.
    pub value: Value,
    /// Keys and indices from the root to the value the cut landed in. The
    /// last step can name a member the cut left out.
    pub cut_at: Vec<CutStep>,
    /// The text of the string the cut landed in, up to the cut and capped.
    /// Empty when the cut did not land in a string.
    pub cut_text: String,
}

impl CutLine {
    /// Close a cut line. `None` when the line is not the start of a JSON
    /// object.
    #[must_use]
    pub fn recover(line: &str) -> Option<Self> {
        let mut reader = Reader {
            text: line.trim_start(),
            pos: 0,
        };
        match reader.value().ok()? {
            Parsed::Whole(value) => {
                reader.skip_whitespace();
                (value.is_object() && reader.pos == reader.text.len()).then_some(Self {
                    value,
                    cut_at: Vec::new(),
                    cut_text: String::new(),
                })
            }
            Parsed::Cut {
                value: Some(value),
                at,
                text,
            } if value.is_object() => Some(Self {
                value,
                cut_at: at,
                cut_text: text,
            }),
            Parsed::Cut { .. } => None,
        }
    }

    /// Whether the cut landed inside the member at `path`, or removed it.
    ///
    /// `path` names object keys only; an index between two keys matches any
    /// element, so `["message", "content"]` covers every content block.
    #[must_use]
    pub fn cut_within(&self, path: &[&str]) -> bool {
        let mut keys = self.cut_at.iter().filter_map(|step| match step {
            CutStep::Key(key) => Some(key.as_str()),
            CutStep::Index(_) => None,
        });
        path.iter().all(|want| keys.next() == Some(*want))
    }

    /// The index of the array element the cut landed in, directly under the
    /// member at `path`.
    #[must_use]
    pub fn index_under(&self, path: &[&str]) -> Option<usize> {
        let mut steps = self.cut_at.iter();
        for want in path {
            match steps.next()? {
                CutStep::Key(key) if key == want => {}
                _ => return None,
            }
        }
        match steps.next()? {
            CutStep::Index(index) => Some(*index),
            CutStep::Key(_) => None,
        }
    }
}

/// A value read from the prefix.
enum Parsed {
    /// A value that ended before the cut.
    Whole(Value),
    /// The value the cut landed in, closed where it stopped. `value` is
    /// `None` when nothing whole survived: a string, number, literal, or key
    /// cut short, or no value begun at all. `text` is the start of a string
    /// the cut split.
    Cut {
        value: Option<Value>,
        at: Vec<CutStep>,
        text: String,
    },
}

impl Parsed {
    fn cut_here(value: Option<Value>) -> Self {
        Self::Cut {
            value,
            at: Vec::new(),
            text: String::new(),
        }
    }
}

/// The prefix is not JSON at all, cut or not.
struct Invalid;

struct Reader<'a> {
    text: &'a str,
    pos: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Result<Parsed, Invalid> {
        self.skip_whitespace();
        match self.peek() {
            None => Ok(Parsed::cut_here(None)),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(match self.string()? {
                Text::Whole(text) => Parsed::Whole(Value::String(text)),
                // Left out: the caller decides whether the string was a
                // payload or part of an id.
                Text::Cut(partial) => Parsed::Cut {
                    value: None,
                    at: Vec::new(),
                    text: partial,
                },
            }),
            Some(_) => self.scalar(),
        }
    }

    fn object(&mut self) -> Result<Parsed, Invalid> {
        self.pos += 1;
        let mut map = Map::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Parsed::Whole(Value::Object(map)));
        }
        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Ok(Parsed::cut_here(Some(Value::Object(map)))),
                Some(b'"') => {}
                Some(_) => return Err(Invalid),
            }
            let key = match self.string()? {
                Text::Whole(key) => key,
                Text::Cut(_) => return Ok(Parsed::cut_here(Some(Value::Object(map)))),
            };
            self.skip_whitespace();
            match self.peek() {
                None => {
                    return Ok(Parsed::Cut {
                        value: Some(Value::Object(map)),
                        at: vec![CutStep::Key(key)],
                        text: String::new(),
                    })
                }
                Some(b':') => self.pos += 1,
                Some(_) => return Err(Invalid),
            }
            match self.value()? {
                Parsed::Whole(value) => {
                    map.insert(key, value);
                }
                Parsed::Cut { value, at, text } => {
                    let mut path = vec![CutStep::Key(key.clone())];
                    path.extend(at);
                    if let Some(value) = value {
                        map.insert(key, value);
                    }
                    return Ok(Parsed::Cut {
                        value: Some(Value::Object(map)),
                        at: path,
                        text,
                    });
                }
            }
            self.skip_whitespace();
            match self.peek() {
                None => return Ok(Parsed::cut_here(Some(Value::Object(map)))),
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Parsed::Whole(Value::Object(map)));
                }
                Some(_) => return Err(Invalid),
            }
        }
    }

    fn array(&mut self) -> Result<Parsed, Invalid> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Parsed::Whole(Value::Array(items)));
        }
        loop {
            match self.value()? {
                Parsed::Whole(value) => items.push(value),
                Parsed::Cut { value, at, text } => {
                    let mut path = vec![CutStep::Index(items.len())];
                    path.extend(at);
                    if let Some(value) = value {
                        items.push(value);
                    }
                    return Ok(Parsed::Cut {
                        value: Some(Value::Array(items)),
                        at: path,
                        text,
                    });
                }
            }
            self.skip_whitespace();
            match self.peek() {
                None => return Ok(Parsed::cut_here(Some(Value::Array(items)))),
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Parsed::Whole(Value::Array(items)));
                }
                Some(_) => return Err(Invalid),
            }
        }
    }

    /// A string starting at the opening quote.
    fn string(&mut self) -> Result<Text, Invalid> {
        let bytes = self.text.as_bytes();
        let open = self.pos;
        let mut index = open + 1;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    self.pos = index + 1;
                    return serde_json::from_str(&self.text[open..=index])
                        .map(Text::Whole)
                        .map_err(|_| Invalid);
                }
                b'\\' => index += 2,
                _ => index += 1,
            }
        }
        self.pos = bytes.len();
        Ok(Text::Cut(decode_partial(&self.text[open + 1..])))
    }

    /// A number or literal. One the cut split is dropped: `12` may have been
    /// `123`, and `tru` is nothing.
    fn scalar(&mut self) -> Result<Parsed, Invalid> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        {
            self.pos += 1;
        }
        if self.pos == self.text.len() {
            return Ok(Parsed::cut_here(None));
        }
        serde_json::from_str::<Value>(&self.text[start..self.pos])
            .map(Parsed::Whole)
            .map_err(|_| Invalid)
    }
}

enum Text {
    Whole(String),
    Cut(String),
}

/// Decode the start of a string the cut split, dropping an escape the cut
/// left incomplete.
fn decode_partial(raw: &str) -> String {
    let raw = &raw[..crate::text::floor_char_boundary(raw, MAX_CUT_TEXT_BYTES)];
    // An escape is at most a `\uXXXX\uXXXX` surrogate pair, so trimming a
    // few characters from the end always reaches a decodable prefix.
    let mut end = raw.len();
    for _ in 0..=12 {
        if let Ok(text) = serde_json::from_str::<String>(&format!("\"{}\"", &raw[..end])) {
            return text;
        }
        match raw[..end].char_indices().next_back() {
            Some((index, _)) => end = index,
            None => break,
        }
    }
    String::new()
}

/// Run one line through a line buffer capped at `max_partial_line` and take
/// what a session would hand its parser.
#[cfg(test)]
pub(crate) fn through_the_line_buffer(line: &str, max_partial_line: usize) -> crate::StreamLine {
    let budget = crate::StreamBudget {
        max_partial_line,
        ..crate::StreamBudget::default()
    };
    let mut buffer = crate::StreamLineBuffer::new();
    let mut tick = buffer.push(format!("{line}\n").as_bytes(), budget);
    assert_eq!(tick.lines.len(), 1, "one line in, one line out");
    tick.lines.remove(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(name: &str) -> CutStep {
        CutStep::Key(name.to_owned())
    }

    /// The string the cut split is left out, not kept as a fragment: the
    /// parser knows whether it was a payload, which it replaces with the
    /// marker, or an id, which it restores. Its start is kept for text a
    /// person reads.
    #[test]
    fn a_cut_string_is_left_out_and_the_envelope_survives() {
        let line = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"aaaaaaaaaa"#;
        let cut = CutLine::recover(line).expect("an object prefix recovers");
        assert_eq!(
            cut.value,
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"tool_use_id": "toolu_1", "type": "tool_result"}
                ]}
            })
        );
        assert_eq!(
            cut.cut_at,
            [
                key("message"),
                key("content"),
                CutStep::Index(0),
                key("content")
            ]
        );
        assert_eq!(cut.cut_text, "aaaaaaaaaa");
        assert!(cut.cut_within(&["message", "content"]));
        assert!(!cut.cut_within(&["tool_use_result"]));
        assert_eq!(cut.index_under(&["message", "content"]), Some(0));
    }

    /// Part of an id names nothing. A cut in an envelope string after the
    /// payload leaves the member out rather than holding a fragment the
    /// parser could mistake for a real id.
    #[test]
    fn a_cut_id_is_left_out_rather_than_kept_as_a_fragment() {
        let cut =
            CutLine::recover(r#"{"params":{"item":{"id":"call-1"},"threadId":"01a0cf"#).unwrap();
        assert_eq!(cut.value, json!({"params": {"item": {"id": "call-1"}}}));
        assert_eq!(cut.cut_at, [key("params"), key("threadId")]);
        assert!(!cut.cut_within(&["params", "item"]));
    }

    #[test]
    fn a_number_or_literal_the_cut_split_is_left_out() {
        let cut = CutLine::recover(r#"{"id":7,"status":"completed","exitCode":12"#).unwrap();
        assert_eq!(cut.value, json!({"id": 7, "status": "completed"}));
        assert_eq!(cut.cut_at, [key("exitCode")]);
        assert!(cut.cut_text.is_empty());

        let cut = CutLine::recover(r#"{"ok":tru"#).unwrap();
        assert_eq!(cut.value, json!({}));
    }

    #[test]
    fn a_cut_between_members_or_inside_a_key_closes_the_object() {
        for line in [
            r#"{"a":1,"#,
            r#"{"a":1,"b"#,
            r#"{"a":1,"b":"#,
            r#"{"a":1, "#,
        ] {
            let cut = CutLine::recover(line).unwrap_or_else(|| panic!("{line} recovers"));
            assert_eq!(cut.value, json!({"a": 1}), "{line}");
        }
    }

    #[test]
    fn nested_arrays_close_at_the_element_the_cut_landed_in() {
        let cut =
            CutLine::recover(r#"{"items":[{"t":"text","text":"ok"},{"t":"image","data":"iVBOR"#)
                .unwrap();
        assert_eq!(
            cut.value,
            json!({"items": [
                {"t": "text", "text": "ok"},
                {"t": "image"}
            ]})
        );
        assert_eq!(cut.index_under(&["items"]), Some(1));
    }

    #[test]
    fn an_escape_the_cut_split_is_dropped_from_the_partial_text() {
        for (line, text) in [
            (r#"{"text":"line one\nline two\"#, "line one\nline two"),
            (r#"{"text":"café and \u00"#, "café and "),
            (r#"{"text":"smile 🙂 \ud83d"#, "smile 🙂 "),
        ] {
            let cut = CutLine::recover(line).unwrap();
            assert_eq!(cut.cut_text, text, "{line}");
            assert_eq!(cut.value, json!({}), "{line}");
        }
    }

    #[test]
    fn a_complete_object_comes_back_unchanged() {
        let cut = CutLine::recover(r#"{"type":"result","is_error":false}"#).unwrap();
        assert_eq!(cut.value, json!({"type": "result", "is_error": false}));
        assert!(cut.cut_at.is_empty());
    }

    #[test]
    fn text_that_is_not_an_object_prefix_does_not_recover() {
        for line in [
            "",
            "   ",
            "not json",
            r#"["an","array"]"#,
            r#"{"a" 1}"#,
            r#"{"a":1}}"#,
            r#""just a string"#,
        ] {
            assert_eq!(CutLine::recover(line), None, "{line:?}");
        }
    }
}

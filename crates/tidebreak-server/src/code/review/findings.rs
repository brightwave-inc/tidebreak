//! The reviewer's answer, read strictly into findings and placed on the diff.
//!
//! The answer is engine output about code that may itself try to steer the
//! engine, so nothing in it is trusted. It is read as one JSON object, and
//! each finding is checked field by field: a path inside the repository, whole
//! line numbers in order, a severity from the three the prompt names, and
//! bounded text with no control characters. A finding that fails any check is
//! left out and counted, never repaired into something the reviewer did not
//! say. An answer with no readable object is kept as text, so it can still
//! reach the person as one summary comment.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::code::types::{CodeReviewFinding, CodeReviewSeverity};

/// Findings one review keeps. A reviewer that returns more is not reviewing.
pub const MAX_FINDINGS: usize = 50;
/// Longest title kept, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest explanation kept, in characters.
pub const MAX_EXPLANATION_CHARS: usize = 4_000;
/// Longest overall summary kept, in characters.
pub const MAX_SUMMARY_CHARS: usize = 2_000;
/// Longest unreadable answer kept as text, in characters.
pub const MAX_RAW_CHARS: usize = 8_000;
/// Longest path accepted, in characters.
pub const MAX_PATH_CHARS: usize = 1_024;
/// Most lines one finding may span.
pub const MAX_FINDING_LINES: u32 = 200;
/// Highest line number accepted.
const MAX_LINE: u64 = 10_000_000;
/// Most of an answer read for JSON. A longer answer is not a findings list.
const MAX_ANSWER_BYTES: usize = 1 << 20;

/// What the reviewer's final message said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAnswer {
    /// A findings object, validated.
    Findings {
        summary: Option<String>,
        findings: Vec<CodeReviewFinding>,
        /// Entries left out: invalid, or past the cap. A repeat of a finding
        /// already kept is dropped without counting.
        rejected: u32,
    },
    /// No findings object could be read. The text is bounded.
    Unreadable { text: String },
}

/// Read the reviewer's final message.
///
/// The object may sit in a fenced `json` block, be the whole message, or
/// follow a sentence of prose; the last readable one wins. Its `findings`
/// must be an array. Anything else is [`ReviewAnswer::Unreadable`].
#[must_use]
pub fn read_answer(text: &str) -> ReviewAnswer {
    let bounded = prefix_bytes(text, MAX_ANSWER_BYTES);
    match findings_object(bounded) {
        Some(object) => validate_object(&object),
        None => ReviewAnswer::Unreadable {
            text: bounded_text(text.trim(), MAX_RAW_CHARS),
        },
    }
}

fn findings_object(text: &str) -> Option<Map<String, Value>> {
    let parse = |candidate: &str| -> Option<Map<String, Value>> {
        match serde_json::from_str::<Value>(candidate.trim()) {
            Ok(Value::Object(object)) if object.contains_key("findings") => Some(object),
            _ => None,
        }
    };
    for block in fenced_blocks(text).iter().rev() {
        if let Some(object) = parse(block) {
            return Some(object);
        }
    }
    if let Some(object) = parse(text) {
        return Some(object);
    }
    // A sentence before the object, or after it, with no fence.
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| parse(&text[start..=end])).flatten()
}

/// The bodies of fenced code blocks tagged `json`, or untagged, in order.
fn fenced_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let ticks = trimmed.chars().take_while(|c| *c == '`').count();
        if ticks < 3 {
            continue;
        }
        let info = trimmed[ticks..].trim().to_ascii_lowercase();
        let wanted = info.is_empty() || info == "json";
        let mut body = Vec::new();
        for inner in lines.by_ref() {
            let closing = inner.trim();
            if closing.len() >= ticks && closing.chars().all(|c| c == '`') {
                break;
            }
            body.push(inner);
        }
        if wanted {
            blocks.push(body.join("\n"));
        }
    }
    blocks
}

fn validate_object(object: &Map<String, Value>) -> ReviewAnswer {
    let Some(Value::Array(entries)) = object.get("findings") else {
        return ReviewAnswer::Unreadable {
            text: bounded_text(
                &serde_json::to_string(object).unwrap_or_default(),
                MAX_RAW_CHARS,
            ),
        };
    };
    let summary = object
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && !has_control(summary, true))
        .map(|summary| bounded_text(summary, MAX_SUMMARY_CHARS));
    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    let mut rejected = 0u32;
    for entry in entries {
        match validate_finding(entry) {
            Some(finding) if findings.len() < MAX_FINDINGS => {
                let key = (
                    finding.path.clone(),
                    finding.start_line,
                    finding.end_line,
                    finding.title.clone(),
                );
                if seen.insert(key) {
                    findings.push(finding);
                }
            }
            _ => rejected = rejected.saturating_add(1),
        }
    }
    ReviewAnswer::Findings {
        summary,
        findings,
        rejected,
    }
}

/// One finding, or `None` when any field is missing, of the wrong type, or
/// out of bounds.
#[must_use]
pub fn validate_finding(value: &Value) -> Option<CodeReviewFinding> {
    let object = value.as_object()?;
    let path = repository_path(object.get("file")?.as_str()?)?;
    let start_line = line_number(object.get("start_line")?)?;
    let end_line = line_number(object.get("end_line")?)?;
    if end_line < start_line || end_line - start_line >= MAX_FINDING_LINES {
        return None;
    }
    let severity = match object.get("severity")?.as_str()?.trim() {
        s if s.eq_ignore_ascii_case("high") => CodeReviewSeverity::High,
        s if s.eq_ignore_ascii_case("medium") => CodeReviewSeverity::Medium,
        s if s.eq_ignore_ascii_case("low") => CodeReviewSeverity::Low,
        _ => return None,
    };
    let title = object.get("title")?.as_str()?.trim();
    if title.is_empty() || has_control(title, false) {
        return None;
    }
    let explanation = object.get("explanation")?.as_str()?.trim();
    if explanation.is_empty() || has_control(explanation, true) {
        return None;
    }
    Some(CodeReviewFinding {
        path,
        start_line,
        end_line,
        severity,
        title: bounded_text(title, MAX_TITLE_CHARS),
        explanation: bounded_text(&explanation.replace("\r\n", "\n"), MAX_EXPLANATION_CHARS),
    })
}

/// A whole, positive line number. Strings, fractions, and zero are refused.
fn line_number(value: &Value) -> Option<u32> {
    let number = value.as_u64()?;
    if number == 0 || number > MAX_LINE {
        return None;
    }
    u32::try_from(number).ok()
}

/// A path relative to the repository root, the way git names it: forward
/// slashes, no `.` or `..` segment, nothing absolute. A leading `./` is
/// the same path and is dropped.
fn repository_path(raw: &str) -> Option<String> {
    let path = raw.trim();
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty()
        || path.chars().count() > MAX_PATH_CHARS
        || has_control(path, false)
        || path.contains('\\')
        || path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }
    Some(path.to_owned())
}

/// Whether `text` holds a control character. Line breaks and tabs are
/// allowed in a block of prose.
fn has_control(text: &str, block: bool) -> bool {
    text.chars().any(|c| {
        (c.is_control() && !(block && matches!(c, '\n' | '\t' | '\r')))
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
}

/// `text` cut to `max` characters, marked when cut.
#[must_use]
pub fn bounded_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn prefix_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The lines each hunk of one file's diff shows on its new side, as
/// inclusive `(first, last)` spans. A hunk that only removes lines shows
/// none.
#[must_use]
pub fn new_side_spans(file_diff: &str) -> Vec<(u32, u32)> {
    file_diff
        .lines()
        .filter_map(|line| {
            let header = line.strip_prefix("@@ ")?;
            let new = header
                .split_whitespace()
                .find(|part| part.starts_with('+'))?;
            let mut numbers = new[1..].splitn(2, ',');
            let start: u32 = numbers.next()?.parse().ok()?;
            let count: u32 = match numbers.next() {
                Some(count) => count.parse().ok()?,
                None => 1,
            };
            (count > 0 && start > 0).then(|| (start, start + count - 1))
        })
        .collect()
}

/// Whether a finding's lines sit inside one hunk of the file's diff.
#[must_use]
pub fn on_the_diff(finding: &CodeReviewFinding, spans: &[(u32, u32)]) -> bool {
    spans
        .iter()
        .any(|(first, last)| finding.start_line >= *first && finding.end_line <= *last)
}

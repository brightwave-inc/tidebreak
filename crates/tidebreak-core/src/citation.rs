//! Lightweight, model-authored locators for durable assistant citations.
//!
//! The model wraps the wording a source supports in `:cit[…]` and names the
//! document plus a human-scale position. The directive is stored exactly as
//! authored; there are no opaque references to resolve or spans to rewrite.

use crate::{AssistantCitationId, DocumentId};

const CITATION_DIRECTIVE_PREFIX: &str = ":cit[";
const MAX_CITATION_ATTRIBUTE_BYTES: usize = 512;
const MAX_LOCATOR_NUMBER: u32 = 10_000_000;
const MAX_SHEET_NAME_CHARS: usize = 120;
const MAX_CELL_RANGE_BYTES: usize = 32;

/// Most citation directives one assistant message persists.
pub const MAX_ASSISTANT_CITATIONS: usize = 20;

/// A small, human-scale position inside a cited document.
///
/// Validation is intentionally loose. A page or line that does not exist still
/// renders and opens the document as close to that position as the reader can.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CitationLocator {
    /// Open the document without a more precise position.
    Document,
    /// One page in a paginated document.
    Page { page: u32 },
    /// Inclusive page range in a paginated document.
    Pages { start: u32, end: u32 },
    /// Inclusive line range in canonical text.
    Lines { start: u32, end: u32 },
    /// A workbook sheet, optionally narrowed to one cell or rectangular range.
    Sheet {
        sheet: String,
        cells: Option<String>,
    },
}

impl CitationLocator {
    /// Whether the locator has a bounded, internally coherent shape.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Document => true,
            Self::Page { page } => valid_number(*page),
            Self::Pages { start, end } | Self::Lines { start, end } => {
                valid_number(*start) && *start <= *end && valid_number(*end)
            }
            Self::Sheet { sheet, cells } => {
                let sheet_chars = sheet.chars().count();
                sheet_chars > 0
                    && sheet_chars <= MAX_SHEET_NAME_CHARS
                    && !sheet.chars().any(char::is_control)
                    && cells.as_deref().is_none_or(valid_cells)
            }
        }
    }
}

/// One citation parsed from model-authored assistant text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssistantCitationInput {
    pub document_id: DocumentId,
    pub locator: CitationLocator,
}

impl AssistantCitationInput {
    /// Whether both the document identity and locator are usable.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.document_id.0.is_nil() && self.locator.is_valid()
    }
}

/// Final assistant text plus citations in directive order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAssistantCitations {
    pub content: String,
    pub citations: Vec<AssistantCitationInput>,
}

/// Renderer-safe historical citation stored beside an assistant message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
pub struct AssistantCitationSnapshot {
    pub id: AssistantCitationId,
    pub ordinal: u16,
    pub document_id: DocumentId,
    pub locator: CitationLocator,
}

/// Produce one model-facing citation directive.
#[must_use]
pub fn format_citation_directive(
    phrase: &str,
    document_id: DocumentId,
    locator: &CitationLocator,
) -> String {
    match locator {
        CitationLocator::Document => {
            format!("{CITATION_DIRECTIVE_PREFIX}{phrase}]{{doc={document_id}}}")
        }
        _ => format!(
            "{CITATION_DIRECTIVE_PREFIX}{phrase}]{{doc={document_id} {}}}",
            format_locator(locator)
        ),
    }
}

/// Teach a model how to cite one document it has just read.
#[must_use]
pub fn citation_authoring_instruction(document_id: DocumentId) -> String {
    format!(
        "To cite this source, wrap the wording it supports in \
         `:cit[your phrasing]{{doc={document_id} lines=N-M}}`. Use exactly one \
         locator after `doc`: `page=N`, `pages=N-M`, `lines=N-M`, or \
         `sheet=\"NAME\"` with optional `cells=A1:B9`. Copy the document id \
         exactly and author the locator from the positions shown in the source."
    )
}

/// Parse valid lightweight directives without rewriting assistant text.
///
/// Malformed directives, including the historical `citation_id` form, are
/// preserved as ordinary text. Renderer compatibility decides how to present
/// those old directives; storage never guesses a new locator for them.
#[must_use]
pub fn parse_assistant_citations(text: &str) -> ParsedAssistantCitations {
    let mut citations = Vec::new();
    let mut remainder = text;
    while citations.len() < MAX_ASSISTANT_CITATIONS {
        let Some(start) = remainder.find(CITATION_DIRECTIVE_PREFIX) else {
            break;
        };
        let candidate = &remainder[start..];
        let Some((_, citation, rest)) = split_citation_directive(candidate) else {
            remainder = &candidate[CITATION_DIRECTIVE_PREFIX.len()..];
            continue;
        };
        citations.push(citation);
        remainder = rest;
    }
    ParsedAssistantCitations {
        content: text.to_owned(),
        citations,
    }
}

fn split_citation_directive(candidate: &str) -> Option<(&str, AssistantCitationInput, &str)> {
    let opened = candidate.strip_prefix(CITATION_DIRECTIVE_PREFIX)?;
    let close = opened.find(']')?;
    let phrase = &opened[..close];
    if phrase.is_empty() {
        return None;
    }
    let attributes = opened.get(close + 1..)?.strip_prefix('{')?;
    let end = attributes.find('}')?;
    let raw = attributes.get(..end)?;
    if raw.len() > MAX_CITATION_ATTRIBUTE_BYTES {
        return None;
    }
    let citation = parse_citation_attributes(raw)?;
    let rest = attributes.get(end + 1..)?;
    Some((phrase, citation, rest))
}

fn parse_citation_attributes(raw: &str) -> Option<AssistantCitationInput> {
    let attributes = parse_attributes(raw)?;
    let mut document_id = None;
    let mut page = None;
    let mut pages = None;
    let mut lines = None;
    let mut sheet = None;
    let mut cells = None;
    for (name, value) in attributes {
        let slot = match name.as_str() {
            "doc" => &mut document_id,
            "page" => &mut page,
            "pages" => &mut pages,
            "lines" => &mut lines,
            "sheet" => &mut sheet,
            "cells" => &mut cells,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    let document_id = document_id?.parse().ok()?;
    let locator_count = usize::from(page.is_some())
        + usize::from(pages.is_some())
        + usize::from(lines.is_some())
        + usize::from(sheet.is_some());
    if locator_count > 1 || (cells.is_some() && sheet.is_none()) {
        return None;
    }
    let locator = if let Some(page) = page {
        CitationLocator::Page {
            page: parse_number(&page)?,
        }
    } else if let Some(pages) = pages {
        let (start, end) = parse_range(&pages)?;
        CitationLocator::Pages { start, end }
    } else if let Some(lines) = lines {
        let (start, end) = parse_range(&lines)?;
        CitationLocator::Lines { start, end }
    } else if let Some(sheet) = sheet {
        CitationLocator::Sheet { sheet, cells }
    } else {
        CitationLocator::Document
    };
    let citation = AssistantCitationInput {
        document_id,
        locator,
    };
    citation.is_valid().then_some(citation)
}

fn parse_attributes(raw: &str) -> Option<Vec<(String, String)>> {
    let bytes = raw.as_bytes();
    let mut attributes = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }
        let name_start = cursor;
        while cursor < bytes.len() && (bytes[cursor].is_ascii_lowercase() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        if cursor == name_start || bytes.get(cursor) != Some(&b'=') {
            return None;
        }
        let name = raw.get(name_start..cursor)?.to_owned();
        cursor += 1;
        let value = if bytes.get(cursor) == Some(&b'"') {
            let value_start = cursor;
            cursor += 1;
            let mut escaped = false;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'"' if !escaped => {
                        cursor += 1;
                        break;
                    }
                    b'\\' if !escaped => escaped = true,
                    _ => escaped = false,
                }
                cursor += 1;
            }
            let encoded = raw.get(value_start..cursor)?;
            serde_json::from_str::<String>(encoded).ok()?
        } else {
            let value_start = cursor;
            while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            raw.get(value_start..cursor)?.to_owned()
        };
        if value.is_empty() {
            return None;
        }
        attributes.push((name, value));
    }
    Some(attributes)
}

fn format_locator(locator: &CitationLocator) -> String {
    match locator {
        CitationLocator::Document => String::new(),
        CitationLocator::Page { page } => format!("page={page}"),
        CitationLocator::Pages { start, end } => format!("pages={start}-{end}"),
        CitationLocator::Lines { start, end } => format!("lines={start}-{end}"),
        CitationLocator::Sheet { sheet, cells } => {
            let sheet = serde_json::to_string(sheet).expect("a string always serializes");
            cells.as_ref().map_or_else(
                || format!("sheet={sheet}"),
                |cells| format!("sheet={sheet} cells={cells}"),
            )
        }
    }
}

fn parse_number(value: &str) -> Option<u32> {
    let number = value.parse().ok()?;
    valid_number(number).then_some(number)
}

fn parse_range(value: &str) -> Option<(u32, u32)> {
    let (start, end) = value.split_once('-')?;
    let start = parse_number(start)?;
    let end = parse_number(end)?;
    (start <= end).then_some((start, end))
}

const fn valid_number(number: u32) -> bool {
    number > 0 && number <= MAX_LOCATOR_NUMBER
}

fn valid_cells(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_CELL_RANGE_BYTES || !value.is_ascii() {
        return false;
    }
    let mut parts = value.split(':');
    parts.next().is_some_and(valid_cell)
        && parts.next().is_none_or(valid_cell)
        && parts.next().is_none()
}

fn valid_cell(value: &str) -> bool {
    let letters = value.bytes().take_while(u8::is_ascii_alphabetic).count();
    let digits = value.len().saturating_sub(letters);
    (1..=4).contains(&letters)
        && (1..=7).contains(&digits)
        && value[letters..].bytes().all(|byte| byte.is_ascii_digit())
        && !value[letters..].starts_with('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_id() -> DocumentId {
        "018f4f8f-6c6e-7f80-8000-000000000001".parse().unwrap()
    }

    #[test]
    fn parses_each_locator_without_rewriting_the_message() {
        let document = document_id();
        let text = format!(
            "{} {} {} {}",
            format_citation_directive("one", document, &CitationLocator::Page { page: 3 }),
            format_citation_directive(
                "two",
                document,
                &CitationLocator::Pages { start: 4, end: 6 }
            ),
            format_citation_directive(
                "three",
                document,
                &CitationLocator::Lines { start: 10, end: 14 }
            ),
            format_citation_directive(
                "four",
                document,
                &CitationLocator::Sheet {
                    sheet: "Q1 Budget".into(),
                    cells: Some("A1:B9".into())
                }
            )
        );
        let parsed = parse_assistant_citations(&text);
        assert_eq!(parsed.content, text);
        assert_eq!(
            parsed
                .citations
                .iter()
                .map(|citation| citation.locator.clone())
                .collect::<Vec<_>>(),
            vec![
                CitationLocator::Page { page: 3 },
                CitationLocator::Pages { start: 4, end: 6 },
                CitationLocator::Lines { start: 10, end: 14 },
                CitationLocator::Sheet {
                    sheet: "Q1 Budget".into(),
                    cells: Some("A1:B9".into())
                },
            ]
        );
    }

    #[test]
    fn malformed_and_historical_directives_remain_plain_content() {
        let old = ":cit[old words]{citation_id=018f4f8f-6c6e-7f80-8000-000000000002}";
        let malformed = ":cit[new words]{doc=018f4f8f-6c6e-7f80-8000-000000000001 pages=8-2}";
        let text = format!("{old} {malformed}");
        let parsed = parse_assistant_citations(&text);
        assert_eq!(parsed.content, text);
        assert!(parsed.citations.is_empty());
    }

    #[test]
    fn validation_is_bounded_but_does_not_check_document_contents() {
        assert!(CitationLocator::Page { page: 999_999 }.is_valid());
        assert!(!CitationLocator::Page { page: 0 }.is_valid());
        assert!(CitationLocator::Lines { start: 50, end: 50 }.is_valid());
        assert!(!CitationLocator::Pages { start: 9, end: 3 }.is_valid());
        assert!(CitationLocator::Sheet {
            sheet: "A sheet that may not exist".into(),
            cells: Some("ZZ10:AAA20".into())
        }
        .is_valid());
    }
}

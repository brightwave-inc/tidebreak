//! Node paths for structured sources: which part of the tree a byte span came
//! from.
//!
//! JSON, XML, and HTML are indexed as their own text — nothing is extracted out
//! of them, so a chunk's byte span already addresses the file verbatim. That is
//! enough to highlight a passage in the extracted text and not enough to open
//! the file as the tree it is, because "bytes 4100–5300" names no node. This
//! module walks the source once and records, for each run of leaf content, the
//! path of the node it belongs to. Those become [`SourceRegion`]s beside the
//! spans they describe, so a passage retrieved later resolves to a node without
//! reparsing the document.
//!
//! Paths are written in the notation each tree is addressed by: dotted keys and
//! indices for JSON (`items.0.invoice_number`), an XPath for XML and HTML
//! (`/invoices[1]/invoice[3]/total[1]`).
//!
//! Both scanners are lenient about what they cannot read: a document that does
//! not parse yields no regions at all, which costs the node view and leaves
//! everything else — text, chunks, spans, citations — exactly as it was.

use std::collections::HashMap;

use openwave_core::{ByteSpan, EvidenceLocation, SourceLocation, SourceRegion, StructuredPathType};

/// Deepest nesting either scanner follows.
///
/// Past this the paths are longer than anything a reader navigates by, and the
/// documents that reach it are generated rather than written. Exceeding it
/// abandons the walk rather than truncating paths, because a truncated path
/// addresses the wrong node.
const MAX_DEPTH: usize = 128;

/// Most regions one document's structure map holds.
///
/// A structure map is stored with the document and read back on every search
/// that touches it, so a machine-generated file with a million leaves must not
/// turn into a million rows. Past the limit the document keeps its text, spans,
/// and citations and loses only the node view.
const MAX_REGIONS: usize = 50_000;

/// Which tree shape a source parses into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StructuredFormat {
    Json,
    Xml,
}

impl StructuredFormat {
    const fn path_type(self) -> StructuredPathType {
        match self {
            Self::Json => StructuredPathType::JsonDotNotation,
            Self::Xml => StructuredPathType::XmlXpath,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Xml => "xml",
        }
    }
}

/// The tree shape a media type declares, if it declares one.
///
/// The `+json` and `+xml` suffixes are matched as well as the base types, so a
/// source typed `application/ld+json` or `application/xhtml+xml` is read as the
/// tree it is. Images are excluded even when they carry an XML syntax: an SVG
/// is opened as a picture, and a path into one addresses nothing a reader sees.
pub(crate) fn structured_format(media_type: &str) -> Option<StructuredFormat> {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    if base.starts_with("image/") {
        return None;
    }
    if base == "application/json" || base.ends_with("+json") {
        return Some(StructuredFormat::Json);
    }
    if base == "application/xml"
        || base == "text/xml"
        || base == "text/html"
        || base.ends_with("+xml")
    {
        return Some(StructuredFormat::Xml);
    }
    None
}

/// Map `text` to the node each of its leaf runs belongs to.
///
/// Returns an empty map for a document that does not parse, which is the same
/// answer as "this format carries no structure" and is handled identically
/// everywhere downstream.
pub(crate) fn structured_regions(text: &str, format: StructuredFormat) -> Vec<SourceRegion> {
    let regions = match format {
        StructuredFormat::Json => JsonScanner::new(text).run(),
        StructuredFormat::Xml => scan_markup(text),
    };
    match regions {
        Some(regions) if regions.len() <= MAX_REGIONS => regions
            .into_iter()
            .map(|(span, path)| SourceRegion {
                span,
                location: SourceLocation::StructuredPath {
                    path,
                    path_type: format.path_type(),
                },
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A stable identity for this module's path-building behavior, per format.
///
/// Part of the parser fingerprint: changing how a path is written changes what
/// a stored region means, and stored regions are only rebuilt when the
/// fingerprint moves.
pub(crate) fn structure_fingerprint(format: StructuredFormat) -> String {
    format!("structure-v1:{}", format.name())
}

/// One recorded run of leaf content: the bytes it covers and the node it is in.
type PathRegion = (ByteSpan, String);

/// Record `span` under `path`, unless the path addresses nothing a reader can
/// open: the root of a tree has no path, and one longer than evidence carries
/// would be dropped later anyway.
fn record(regions: &mut Vec<PathRegion>, span: ByteSpan, path: &str) {
    if span.is_empty()
        || path.is_empty()
        || path.len() > EvidenceLocation::MAX_STRUCTURED_PATH_BYTES
    {
        return;
    }
    regions.push((span, path.to_owned()));
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// What a JSON value turned out to be, which decides whether its siblings can
/// be addressed together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Scalar,
    Container,
}

struct JsonScanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    path: String,
    regions: Vec<PathRegion>,
}

impl<'a> JsonScanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
            path: String::new(),
            regions: Vec::new(),
        }
    }

    fn run(mut self) -> Option<Vec<PathRegion>> {
        self.value(0, 0)?;
        self.skip_whitespace();
        (self.pos == self.bytes.len()).then_some(self.regions)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Parse one value, recording it under the path currently in scope.
    ///
    /// `start` is where a region for this value begins, which for an object
    /// member is its key: a reader whose passage starts at `"total":` is at the
    /// total, not in the gap before it.
    fn value(&mut self, start: usize, depth: usize) -> Option<Shape> {
        if depth > MAX_DEPTH || self.regions.len() > MAX_REGIONS {
            return None;
        }
        self.skip_whitespace();
        match self.peek()? {
            b'{' => self.object(start, depth),
            b'[' => self.array(start, depth),
            b'"' => {
                self.string()?;
                record(
                    &mut self.regions,
                    ByteSpan::new(start, self.pos),
                    &self.path,
                );
                Some(Shape::Scalar)
            }
            _ => {
                self.literal()?;
                record(
                    &mut self.regions,
                    ByteSpan::new(start, self.pos),
                    &self.path,
                );
                Some(Shape::Scalar)
            }
        }
    }

    fn object(&mut self, start: usize, depth: usize) -> Option<Shape> {
        self.pos += 1;
        let mut members = 0_usize;
        loop {
            self.skip_whitespace();
            match self.peek()? {
                b'}' => {
                    self.pos += 1;
                    break;
                }
                b'"' => {
                    let key_start = self.pos;
                    let key = self.string()?;
                    self.skip_whitespace();
                    if self.peek()? != b':' {
                        return None;
                    }
                    self.pos += 1;
                    let scope = self.path.len();
                    self.push_step(&key);
                    self.value(key_start, depth + 1)?;
                    self.path.truncate(scope);
                    members += 1;
                    self.skip_whitespace();
                    match self.peek()? {
                        b',' => self.pos += 1,
                        b'}' => {
                            self.pos += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        if members == 0 {
            record(
                &mut self.regions,
                ByteSpan::new(start, self.pos),
                &self.path,
            );
        }
        Some(Shape::Container)
    }

    fn array(&mut self, start: usize, depth: usize) -> Option<Shape> {
        self.pos += 1;
        let mark = self.regions.len();
        let mut elements = 0_usize;
        let mut all_scalar = true;
        loop {
            self.skip_whitespace();
            if self.peek()? == b']' {
                self.pos += 1;
                break;
            }
            let scope = self.path.len();
            self.push_step(&elements.to_string());
            let element_start = self.pos;
            all_scalar &= self.value(element_start, depth + 1)? == Shape::Scalar;
            self.path.truncate(scope);
            elements += 1;
            self.skip_whitespace();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    break;
                }
                _ => return None,
            }
        }
        // A list of plain values is one thing a reader looks at, not a hundred:
        // `readings.417` is a number on its own, while `readings` is the series
        // it belongs to. Lists holding objects keep their elements addressable,
        // because those are the records the document is about.
        if elements == 0 || (all_scalar && elements > 1) {
            self.regions.truncate(mark);
            record(
                &mut self.regions,
                ByteSpan::new(start, self.pos),
                &self.path,
            );
        }
        Some(Shape::Container)
    }

    fn push_step(&mut self, step: &str) {
        if !self.path.is_empty() {
            self.path.push('.');
        }
        self.path.push_str(step);
    }

    /// Consume a quoted string, returning it with its escapes resolved.
    fn string(&mut self) -> Option<String> {
        if self.peek()? != b'"' {
            return None;
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    match self.peek()? {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self.text.get(self.pos + 1..self.pos + 5)?;
                            let code = u32::from_str_radix(hex, 16).ok()?;
                            // Lone surrogates and pairs alike become the
                            // replacement character: a key spelled with one is
                            // not a key anybody navigates by, and the walk must
                            // still reach the values after it.
                            out.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                            self.pos += 4;
                        }
                        _ => return None,
                    }
                    self.pos += 1;
                }
                _ => {
                    // Advance by whole characters so a multi-byte value cannot
                    // leave the cursor inside one.
                    let rest = self.text.get(self.pos..)?;
                    let next = rest.chars().next()?;
                    out.push(next);
                    self.pos += next.len_utf8();
                }
            }
        }
    }

    /// Consume a number, `true`, `false`, or `null`.
    fn literal(&mut self) -> Option<()> {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        (self.pos > start).then_some(())
    }
}

// ---------------------------------------------------------------------------
// XML and HTML
// ---------------------------------------------------------------------------

/// HTML elements that never have a closing tag. Treated as self-closing so an
/// `<img>` in the middle of a page does not swallow everything after it.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// One element the scanner is inside.
struct OpenElement {
    name: String,
    /// Length of the path string before this element's own step, for unwinding.
    path_scope: usize,
    /// Byte offset of this element's `<`.
    start: usize,
    /// How many children of each tag name have been opened so far, which is
    /// what makes an XPath position predicate.
    children: HashMap<String, usize>,
    /// Whether anything recorded inside this element belongs to a child rather
    /// than to the element itself.
    has_element_child: bool,
    /// Region count when this element opened, for collapsing a leaf element's
    /// text into the element.
    mark: usize,
}

/// Walk markup once, recording each run of text under the element that owns it
/// and each leaf element under itself.
///
/// Leniency is deliberate and matches what a reader's browser does with the
/// same file: unmatched close tags are ignored, an element closed out of order
/// closes what it contains, and anything still open at the end of the file
/// contributes nothing. HTML is read by the same walk as XML — the difference
/// that matters here is the void elements, which are listed above.
fn scan_markup(text: &str) -> Option<Vec<PathRegion>> {
    let bytes = text.as_bytes();
    let mut regions: Vec<PathRegion> = Vec::new();
    let mut stack: Vec<OpenElement> = Vec::new();
    let mut path = String::new();
    let mut pos = 0;
    let mut text_start = 0;

    while pos < bytes.len() {
        if bytes[pos] != b'<' {
            pos += 1;
            continue;
        }
        flush_text(&mut regions, text, text_start, pos, &path);
        let tag_start = pos;
        let rest = text.get(pos..)?;
        if rest.starts_with("<!--") {
            pos = find_after(text, pos + 4, "-->").unwrap_or(bytes.len());
        } else if rest.starts_with("<![CDATA[") {
            let end = find_after(text, pos + 9, "]]>").unwrap_or(bytes.len());
            flush_text(&mut regions, text, pos + 9, end.saturating_sub(3), &path);
            pos = end;
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            pos = find_after(text, pos + 2, ">").unwrap_or(bytes.len());
        } else if rest.starts_with("</") {
            let (name, end) = read_tag(text, pos + 2)?;
            pos = end;
            close_element(&mut regions, &mut stack, &mut path, &name, tag_start, pos);
        } else {
            let (name, end) = read_tag(text, pos + 1)?;
            if name.is_empty() {
                // A bare `<` in text is not a tag; step over it and keep the
                // character as content.
                pos += 1;
                text_start = tag_start;
                continue;
            }
            pos = end;
            let self_closing = (pos >= 2 && bytes[pos - 1] == b'>' && bytes[pos - 2] == b'/')
                || VOID_ELEMENTS.contains(&name.to_ascii_lowercase().as_str());
            if stack.len() >= MAX_DEPTH {
                return None;
            }
            open_element(&mut stack, &mut path, name, tag_start, regions.len());
            if self_closing {
                let element = stack.pop().expect("just opened");
                finish_element(&mut regions, element, &mut path, pos);
            }
        }
        text_start = pos;
        if regions.len() > MAX_REGIONS {
            return None;
        }
    }
    flush_text(&mut regions, text, text_start, bytes.len(), &path);
    Some(regions)
}

fn open_element(
    stack: &mut Vec<OpenElement>,
    path: &mut String,
    name: String,
    start: usize,
    mark: usize,
) {
    let index = match stack.last_mut() {
        Some(parent) => {
            parent.has_element_child = true;
            let count = parent.children.entry(name.clone()).or_insert(0);
            *count += 1;
            *count
        }
        None => 1,
    };
    let path_scope = path.len();
    path.push('/');
    path.push_str(&name);
    path.push('[');
    path.push_str(&index.to_string());
    path.push(']');
    stack.push(OpenElement {
        name,
        path_scope,
        start,
        children: HashMap::new(),
        has_element_child: false,
        mark,
    });
}

/// Close `name`, and with it anything left open inside it.
fn close_element(
    regions: &mut Vec<PathRegion>,
    stack: &mut Vec<OpenElement>,
    path: &mut String,
    name: &str,
    tag_start: usize,
    tag_end: usize,
) {
    let Some(depth) = stack
        .iter()
        .rposition(|element| element.name.eq_ignore_ascii_case(name))
    else {
        return;
    };
    while stack.len() > depth + 1 {
        let element = stack.pop().expect("checked depth");
        finish_element(regions, element, path, tag_start);
    }
    let element = stack.pop().expect("checked depth");
    finish_element(regions, element, path, tag_end);
}

/// Retire an element: an element with no element children *is* the leaf its
/// text belongs to, so its text runs collapse into one region covering the
/// element itself — markup included, so a passage starting at the tag still
/// resolves.
fn finish_element(
    regions: &mut Vec<PathRegion>,
    element: OpenElement,
    path: &mut String,
    end: usize,
) {
    if !element.has_element_child {
        regions.truncate(element.mark);
        record(regions, ByteSpan::new(element.start, end), path);
    }
    path.truncate(element.path_scope);
}

/// Record the text between two tags under the element that owns it, trimmed of
/// the whitespace that only formats the markup.
fn flush_text(regions: &mut Vec<PathRegion>, text: &str, start: usize, end: usize, path: &str) {
    let Some(slice) = text.get(start..end) else {
        return;
    };
    let leading = slice.len() - slice.trim_start().len();
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return;
    }
    record(
        regions,
        ByteSpan::new(start + leading, start + leading + trimmed.len()),
        path,
    );
}

/// Read a tag's name from `start`, returning it with the offset just past the
/// tag's `>`. Quoted attribute values are honoured so a `>` inside one does not
/// end the tag early.
fn read_tag(text: &str, start: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut pos = start;
    while pos < bytes.len()
        && (bytes[pos].is_ascii_alphanumeric() || matches!(bytes[pos], b':' | b'-' | b'_' | b'.'))
    {
        pos += 1;
    }
    let name = text.get(start..pos)?.to_owned();
    let mut quote: Option<u8> = None;
    while pos < bytes.len() {
        let byte = bytes[pos];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => return Some((name, pos + 1)),
            None => {}
        }
        pos += 1;
    }
    Some((name, bytes.len()))
}

/// The offset just past the next `needle` at or after `from`.
fn find_after(text: &str, from: usize, needle: &str) -> Option<usize> {
    let rest = text.get(from..)?;
    rest.find(needle).map(|at| from + at + needle.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::chunk::{Chunker, TextChunker};
    use crate::document::{Document, DocumentSource};
    use crate::parse::DocumentParser;

    const INVOICES_JSON: &str = r#"{
  "invoices": [
    {"number": "A-1", "customer": "Acme", "total": 42, "tags": ["paid", "eu"]},
    {"number": "B-2", "customer": "Globex", "total": 7, "tags": []}
  ],
  "count": 2
}"#;

    const INVOICES_XML: &str = r#"<?xml version="1.0"?>
<!-- exported nightly -->
<invoices>
  <invoice id="A-1"><customer>Acme</customer><total>42</total></invoice>
  <invoice id="B-2"><customer>Globex</customer><total>7</total></invoice>
  <note>Totals are <em>net</em> of tax</note>
</invoices>"#;

    /// The paths recorded for `text`, paired with the source they address.
    fn located(text: &str, format: StructuredFormat) -> Vec<(String, &str)> {
        structured_regions(text, format)
            .into_iter()
            .map(|region| {
                let (path, _) = region
                    .location
                    .structured_path()
                    .expect("structured sources record structured paths");
                (path.to_owned(), &text[region.span.start..region.span.end])
            })
            .collect()
    }

    #[test]
    fn json_paths_address_the_value_a_span_covers() {
        assert_eq!(
            located(INVOICES_JSON, StructuredFormat::Json),
            vec![
                ("invoices.0.number".into(), "\"number\": \"A-1\""),
                ("invoices.0.customer".into(), "\"customer\": \"Acme\""),
                ("invoices.0.total".into(), "\"total\": 42"),
                // A list of plain values is one thing to look at, so the list
                // is the node rather than each of its elements.
                ("invoices.0.tags".into(), "\"tags\": [\"paid\", \"eu\"]"),
                ("invoices.1.number".into(), "\"number\": \"B-2\""),
                ("invoices.1.customer".into(), "\"customer\": \"Globex\""),
                ("invoices.1.total".into(), "\"total\": 7"),
                ("invoices.1.tags".into(), "\"tags\": []"),
                ("count".into(), "\"count\": 2"),
            ]
        );
    }

    #[test]
    fn markup_paths_are_xpaths_into_the_element_a_span_sits_in() {
        assert_eq!(
            located(INVOICES_XML, StructuredFormat::Xml),
            vec![
                (
                    "/invoices[1]/invoice[1]/customer[1]".into(),
                    "<customer>Acme</customer>"
                ),
                (
                    "/invoices[1]/invoice[1]/total[1]".into(),
                    "<total>42</total>"
                ),
                (
                    "/invoices[1]/invoice[2]/customer[1]".into(),
                    "<customer>Globex</customer>"
                ),
                (
                    "/invoices[1]/invoice[2]/total[1]".into(),
                    "<total>7</total>"
                ),
                // Text an element owns directly belongs to that element; the
                // child between the two runs is addressed on its own.
                ("/invoices[1]/note[1]".into(), "Totals are"),
                ("/invoices[1]/note[1]/em[1]".into(), "<em>net</em>"),
                ("/invoices[1]/note[1]".into(), "of tax"),
            ]
        );
    }

    #[test]
    fn html_is_read_leniently_and_void_elements_do_not_swallow_the_page() {
        let html = "<html><body><h1>Q4</h1><p>Revenue <b>rose</b><br>sharply</body></html>";
        assert_eq!(
            located(html, StructuredFormat::Xml),
            vec![
                ("/html[1]/body[1]/h1[1]".into(), "<h1>Q4</h1>"),
                ("/html[1]/body[1]/p[1]".into(), "Revenue"),
                ("/html[1]/body[1]/p[1]/b[1]".into(), "<b>rose</b>"),
                ("/html[1]/body[1]/p[1]/br[1]".into(), "<br>"),
                ("/html[1]/body[1]/p[1]".into(), "sharply"),
            ]
        );
    }

    #[tokio::test]
    async fn a_chunk_of_a_structured_source_is_located_at_the_node_it_covers() {
        let parsed = crate::parse::StructuredTextParser::new()
            .parse(INVOICES_JSON.as_bytes(), "application/json")
            .await
            .unwrap();
        assert_eq!(parsed.text, INVOICES_JSON);
        let document = Document::new(DocumentSource::Inline, "application/json", &parsed.text)
            .with_source_regions(parsed.source_regions);
        // A window small enough to fall inside one invoice, so the passage has
        // an enclosing record rather than the whole file.
        let chunks = TextChunker::new(60, 0)
            .chunk(&document, "application/json")
            .unwrap();
        let located: Vec<_> = chunks
            .iter()
            .map(|chunk| {
                EvidenceLocation::for_source_regions(
                    chunk.heading_path.clone(),
                    chunk.source_regions.clone(),
                )
            })
            .collect();
        assert!(
            located.contains(&EvidenceLocation::StructuredPath {
                path: "invoices.0".into(),
                path_type: StructuredPathType::JsonDotNotation,
            }),
            "a passage across one invoice's fields is at that invoice: {located:?}"
        );
    }

    #[tokio::test]
    async fn a_source_that_does_not_parse_keeps_its_text_and_gains_no_paths() {
        let broken = b"{\"invoices\": [ truncated";
        let parsed = crate::parse::StructuredTextParser::new()
            .parse(broken, "application/json")
            .await
            .unwrap();
        assert_eq!(parsed.text, String::from_utf8_lossy(broken));
        assert!(parsed.source_regions.is_empty());
    }

    #[test]
    fn a_passage_crossing_more_nodes_than_evidence_carries_stays_valid() {
        let mut json = String::from("{\"rows\":[");
        for row in 0..600 {
            if row > 0 {
                json.push(',');
            }
            json.push_str(&format!("{{\"a\":{row},\"b\":{row}}}"));
        }
        json.push_str("]}");
        let document = Document::new(DocumentSource::Inline, "application/json", &json)
            .with_source_regions(structured_regions(&json, StructuredFormat::Json));
        let chunks = TextChunker::default()
            .chunk(&document, "application/json")
            .unwrap();
        for chunk in &chunks {
            chunk
                .validate_source_regions()
                .expect("a dense structured passage must still be valid evidence");
            let located = EvidenceLocation::for_source_regions(
                chunk.heading_path.clone(),
                chunk.source_regions.clone(),
            );
            assert!(
                matches!(located, EvidenceLocation::StructuredPath { .. })
                    && located.is_well_formed(),
                "every chunk of a structured source is at a node: {located:?}"
            );
        }
    }
}

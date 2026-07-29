//! The chunking seam: split a document's text into overlapping, span-tracked
//! pieces small enough to embed.
//!
//! [`TextChunker`] is a boundary-aware sliding window. It walks the text in
//! character units (so it never splits a UTF-8 codepoint), caps each window at a
//! character budget, and prefers to cut at a natural boundary — a line break,
//! else whitespace — within the back half of the window. Markdown documents are
//! first partitioned at ATX headings outside fenced code, and prefer paragraph
//! boundaries within each section. Consecutive chunks overlap by a configurable
//! number of characters so a passage straddling a cut still lands wholly inside
//! at least one chunk, but that overlap never crosses a Markdown section.
//!
//! Every emitted [`Chunk`] records its exact [`ByteSpan`], which is what makes
//! citations precise. Markdown chunks also carry their containing ATX heading
//! hierarchy as derived retrieval context without changing those source spans.

use crate::document::{ByteSpan, Chunk, Document};
use crate::error::Result;

/// Splits a document into chunks.
///
/// Object-safe so strategies can be swapped or composed as `Box<dyn Chunker>`.
/// Fallible like the other seams: today's [`TextChunker`] never errors, but a
/// future service- or model-backed chunker (sentence tokenizers, layout models)
/// can, and returning [`Result`] now keeps that from being a breaking change.
pub trait Chunker: Send + Sync {
    /// Stable identity for chunk boundaries used in index watermarks.
    ///
    /// Custom chunkers should override this whenever an implementation change
    /// can alter emitted chunks. The value must remain stable for this chunker
    /// instance's lifetime; runtime reconfiguration requires a new instance.
    fn fingerprint(&self) -> String {
        format!("custom-chunker:type={}", std::any::type_name::<Self>())
    }

    /// Produce the chunks for `document`, in document order.
    fn chunk(&self, document: &Document) -> Result<Vec<Chunk>>;
}

/// A boundary-aware, fixed-budget, overlapping chunker.
#[derive(Debug, Clone, Copy)]
pub struct TextChunker {
    /// Maximum characters per chunk (the hard window size).
    max_chars: usize,
    /// Characters of overlap carried from one chunk into the next.
    overlap: usize,
}

impl TextChunker {
    /// Default window size, in characters (~a few hundred tokens of English).
    pub const DEFAULT_MAX_CHARS: usize = 1200;
    /// Default overlap between consecutive chunks, in characters.
    pub const DEFAULT_OVERLAP: usize = 150;

    /// Build a chunker with the given window and overlap.
    ///
    /// `max_chars` is clamped to at least 1. `overlap` is clamped to at most half
    /// the window: that keeps the window advancing (so chunking terminates) and
    /// bounds the chunk count at `O(len / (max_chars / 2))`, avoiding the blow-up
    /// a near-`max_chars` overlap would cause on boundary-free text.
    #[must_use]
    pub fn new(max_chars: usize, overlap: usize) -> Self {
        let max_chars = max_chars.max(1);
        let overlap = overlap.min(max_chars / 2);
        Self { max_chars, overlap }
    }
}

impl Default for TextChunker {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_CHARS, Self::DEFAULT_OVERLAP)
    }
}

impl Chunker for TextChunker {
    fn fingerprint(&self) -> String {
        format!(
            "text-window-v3:markdown=atx-heading-context:max_chars={}:overlap={}",
            self.max_chars, self.overlap
        )
    }

    fn chunk(&self, document: &Document) -> Result<Vec<Chunk>> {
        let text = &document.text;

        // Character view plus each char's starting byte offset. `byte_at[i]` is
        // the byte offset of char `i`; the final entry is `text.len()`, so
        // `byte_at[j]` is always a valid slice end for a char index `j`.
        let chars: Vec<char> = text.chars().collect();
        let mut byte_at: Vec<usize> = Vec::with_capacity(chars.len() + 1);
        let mut offset = 0;
        for c in &chars {
            byte_at.push(offset);
            offset += c.len_utf8();
        }
        byte_at.push(text.len());

        let n = chars.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        if is_markdown(&document.media_type) {
            return Ok(self.chunk_markdown(document, &chars, &byte_at));
        }

        Ok(self.chunk_range(document, &chars, &byte_at, 0, n, false, 0, &[]))
    }
}

impl TextChunker {
    fn chunk_markdown(&self, document: &Document, chars: &[char], byte_at: &[usize]) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        let mut section_start = 0;

        let headings = markdown_headings(&document.text);
        let mut heading_stack: Vec<(usize, String)> = Vec::new();
        for heading_info in headings {
            let heading_byte = heading_info.offset;
            let heading = byte_at
                .binary_search(&heading_byte)
                .expect("line starts are UTF-8 character boundaries");
            if heading > section_start {
                let heading_path = visible_heading_path(&heading_stack);
                chunks.extend(self.chunk_range(
                    document,
                    chars,
                    byte_at,
                    section_start,
                    heading,
                    true,
                    chunks.len(),
                    &heading_path,
                ));
            }
            while heading_stack
                .last()
                .is_some_and(|(level, _)| *level >= heading_info.level)
            {
                heading_stack.pop();
            }
            heading_stack.push((heading_info.level, heading_info.title));
            section_start = heading;
        }

        let heading_path = visible_heading_path(&heading_stack);
        chunks.extend(self.chunk_range(
            document,
            chars,
            byte_at,
            section_start,
            chars.len(),
            true,
            chunks.len(),
            &heading_path,
        ));
        chunks
    }

    #[allow(clippy::too_many_arguments)]
    fn chunk_range(
        &self,
        document: &Document,
        chars: &[char],
        byte_at: &[usize],
        range_start: usize,
        range_end: usize,
        prefer_paragraphs: bool,
        first_ordinal: usize,
        heading_path: &[String],
    ) -> Vec<Chunk> {
        let text = &document.text;
        let mut chunks = Vec::new();
        let mut ordinal = 0;
        let mut start = range_start;

        while start < range_end {
            let hard_end = (start + self.max_chars).min(range_end);
            // Cut at a natural boundary unless we're already at the text's end.
            let end = if hard_end == range_end {
                range_end
            } else {
                boundary_before(chars, start, hard_end, prefer_paragraphs)
            };

            let span = ByteSpan::new(byte_at[start], byte_at[end]);
            // Slice on byte offsets we computed from the same char view.
            let piece = &text[span.start..span.end];
            if !piece.trim().is_empty() {
                let mut chunk = Chunk::with_heading_path(
                    document.id,
                    first_ordinal + ordinal,
                    span,
                    piece,
                    heading_path.to_vec(),
                );
                chunk.source_regions = document.source_regions_for(span);
                chunks.push(chunk);
                ordinal += 1;
            }

            if end >= range_end {
                break;
            }
            // Advance, carrying `overlap` chars back. Guarantee forward progress:
            // never move the window start backwards or leave it put.
            let next = end.saturating_sub(self.overlap);
            start = if next <= start { start + 1 } else { next };
        }

        chunks
    }
}

/// Derive the public breadcrumb without discarding empty structural frames.
///
/// A valid empty ATX heading still changes section boundaries and nesting, but
/// contributes no user-visible context of its own.
fn visible_heading_path(stack: &[(usize, String)]) -> Vec<String> {
    stack
        .iter()
        .filter(|(_, title)| !title.is_empty())
        .map(|(_, title)| title.clone())
        .collect()
}

fn is_markdown(media_type: &str) -> bool {
    let base = media_type.split(';').next().unwrap_or(media_type).trim();
    matches!(
        base.to_ascii_lowercase().as_str(),
        "text/markdown" | "text/x-markdown"
    )
}

/// Return the byte offset of each ATX heading that starts a Markdown section.
///
/// Fence recognition intentionally needs only line-local CommonMark syntax: a
/// run of at least three backticks or tildes, indented by no more than three
/// spaces. Heading-looking lines inside a fence remain ordinary section text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MarkdownHeading {
    offset: usize,
    level: usize,
    title: String,
}

fn markdown_headings(text: &str) -> Vec<MarkdownHeading> {
    let mut headings = Vec::new();
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;

    while offset < text.len() {
        let remaining = &text.as_bytes()[offset..];
        let content_len = remaining
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .unwrap_or(remaining.len());
        let line = &remaining[..content_len];
        let indent = line.iter().take_while(|&&byte| byte == b' ').count();
        let body = if indent <= 3 { &line[indent..] } else { &[] };

        if let Some((marker, opening_len)) = fence {
            let run = body.iter().take_while(|&&byte| byte == marker).count();
            if run >= opening_len && body[run..].iter().all(|byte| matches!(byte, b' ' | b'\t')) {
                fence = None;
            }
        } else {
            let marker = body.first().copied();
            let run = marker.map_or(0, |marker| {
                body.iter().take_while(|&&byte| byte == marker).count()
            });
            if matches!(marker, Some(b'`' | b'~')) && run >= 3 {
                // Backtick fence info strings cannot contain a backtick.
                if marker != Some(b'`') || !body[run..].contains(&b'`') {
                    fence = Some((marker.expect("matched marker"), run));
                }
            } else {
                let hashes = body.iter().take_while(|&&byte| byte == b'#').count();
                if (1..=6).contains(&hashes)
                    && (body.len() == hashes || matches!(body[hashes], b' ' | b'\t'))
                {
                    let mut title = &body[hashes..];
                    while title
                        .first()
                        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                    {
                        title = &title[1..];
                    }
                    while title
                        .last()
                        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                    {
                        title = &title[..title.len() - 1];
                    }
                    let closing_start = title
                        .iter()
                        .rposition(|byte| !matches!(byte, b'#'))
                        .map_or(0, |index| index + 1);
                    if closing_start < title.len()
                        && (closing_start == 0 || matches!(title[closing_start - 1], b' ' | b'\t'))
                    {
                        title = if closing_start == 0 {
                            &[]
                        } else {
                            &title[..closing_start - 1]
                        };
                        while title
                            .last()
                            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
                        {
                            title = &title[..title.len() - 1];
                        }
                    }
                    headings.push(MarkdownHeading {
                        offset,
                        level: hashes,
                        title: String::from_utf8_lossy(title).into_owned(),
                    });
                }
            }
        }

        let line_ending_len = match &remaining[content_len..] {
            [b'\r', b'\n', ..] => 2,
            [b'\r' | b'\n', ..] => 1,
            _ => 0,
        };
        offset += content_len + line_ending_len;
    }

    headings
}

/// Find a good char index to end a chunk within `(start, hard_end)`.
///
/// Searches the back half of the window for the last blank line when paragraph
/// preference is enabled, then the last line break, then the last whitespace,
/// cutting just after it. Falls back to `hard_end` (a hard cut) when the window
/// has no boundary. The returned index is an exclusive char index.
fn boundary_before(
    chars: &[char],
    start: usize,
    hard_end: usize,
    prefer_paragraphs: bool,
) -> usize {
    let floor = start + (hard_end - start) / 2;
    let mut last_blank_line = None;
    let mut last_newline = None;
    let mut last_whitespace = None;
    for (offset, &c) in chars[floor..hard_end].iter().enumerate() {
        let i = floor + offset;
        let is_line_end =
            c == '\n' || (prefer_paragraphs && c == '\r' && chars.get(i + 1) != Some(&'\n'));
        if is_line_end {
            if prefer_paragraphs {
                let previous_line_start = (0..i)
                    .rev()
                    .find(|&index| {
                        chars[index] == '\n'
                            || (chars[index] == '\r' && chars.get(index + 1) != Some(&'\n'))
                    })
                    .map_or(0, |newline| newline + 1);
                if chars[previous_line_start..i]
                    .iter()
                    .all(|character| character.is_whitespace())
                {
                    last_blank_line = Some(i + 1);
                }
            }
            last_newline = Some(i + 1);
        } else if c.is_whitespace() {
            last_whitespace = Some(i + 1);
        }
    }
    last_blank_line
        .or(last_newline)
        .or(last_whitespace)
        .unwrap_or(hard_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DocumentSource, SourceLocation, SourceRegion};
    use std::num::NonZeroU32;

    fn doc(text: &str) -> Document {
        Document::new(DocumentSource::Inline, "text/plain", text)
    }

    fn markdown(media_type: &str, text: &str) -> Document {
        Document::new(DocumentSource::Inline, media_type, text)
    }

    #[test]
    fn empty_document_yields_no_chunks() {
        assert!(TextChunker::default().chunk(&doc("")).unwrap().is_empty());
        assert!(TextChunker::default()
            .chunk(&doc("   \n  "))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn short_document_is_a_single_chunk() {
        let d = doc("hello world");
        let chunks = TextChunker::default().chunk(&d).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello world");
        assert_eq!(chunks[0].span, ByteSpan::new(0, 11));
        assert_eq!(chunks[0].ordinal, 0);
    }

    #[test]
    fn spans_slice_back_to_the_exact_source_text() {
        let d = doc("The quick brown fox jumps over the lazy dog. It was a fine day.");
        let chunks = TextChunker::new(20, 5).chunk(&d).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            // The recorded span must reproduce the chunk text exactly.
            assert_eq!(d.slice(chunk.span).unwrap(), chunk.text);
        }
    }

    #[test]
    fn chunks_clip_and_preserve_document_global_page_regions() {
        let text = "alpha bravo café delta echo";
        let page_two_start = text.find("café").unwrap();
        let document = doc(text).with_source_regions(vec![
            SourceRegion {
                span: ByteSpan::new(0, page_two_start),
                location: SourceLocation::Page {
                    number: NonZeroU32::new(1).unwrap(),
                    bounds: None,
                },
            },
            SourceRegion {
                span: ByteSpan::new(page_two_start, text.len()),
                location: SourceLocation::Page {
                    number: NonZeroU32::new(2).unwrap(),
                    bounds: None,
                },
            },
        ]);

        let chunks = TextChunker::new(15, 4).chunk(&document).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            for region in &chunk.source_regions {
                assert!(region.span.start >= chunk.span.start);
                assert!(region.span.end <= chunk.span.end);
                assert_eq!(
                    document.slice(region.span),
                    Some(&text[region.span.start..region.span.end])
                );
            }
        }
        assert!(chunks.iter().any(|chunk| chunk.source_regions.len() == 2));
    }

    #[test]
    fn ordinals_are_contiguous_from_zero() {
        let d = doc("alpha beta gamma delta epsilon zeta eta theta iota kappa");
        let chunks = TextChunker::new(15, 4).chunk(&d).unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, i);
        }
    }

    #[test]
    fn consecutive_chunks_overlap() {
        let d = doc("one two three four five six seven eight nine ten eleven twelve");
        let chunks = TextChunker::new(20, 8).chunk(&d).unwrap();
        assert!(chunks.len() >= 2);
        // With overlap, each chunk after the first starts before the previous ends.
        for pair in chunks.windows(2) {
            assert!(pair[1].span.start < pair[0].span.end);
        }
    }

    #[test]
    fn prefers_newline_boundaries() {
        let d = doc("first line here\nsecond line here\nthird line here");
        let chunks = TextChunker::new(24, 0).chunk(&d).unwrap();
        // The first cut should land right after a newline, so the first chunk
        // ends cleanly on a line and the next starts a fresh one.
        assert!(chunks[0].text.ends_with('\n') || chunks[0].text.ends_with("here"));
        assert!(!chunks[1].text.starts_with(' '));
    }

    #[test]
    fn handles_multibyte_without_splitting_codepoints() {
        // Each `é` is two bytes; a naive byte window could split one.
        let d = doc("café café café café café café café café");
        let chunks = TextChunker::new(6, 1).chunk(&d).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            // Spans land on char boundaries => slicing never panics and matches.
            assert_eq!(d.slice(chunk.span).unwrap(), chunk.text);
        }
    }

    #[test]
    fn terminates_when_overlap_would_stall() {
        // overlap >= max_chars is clamped so the window still advances.
        let chunker = TextChunker::new(4, 999);
        let d = doc("abcdefghijklmnop");
        let chunks = chunker.chunk(&d).unwrap();
        assert!(!chunks.is_empty());
        // Every byte is covered by the union of spans.
        assert_eq!(chunks.first().unwrap().span.start, 0);
        assert_eq!(chunks.last().unwrap().span.end, d.text.len());
    }

    #[test]
    fn adjacent_markdown_sections_never_share_overlap() {
        let text = "# Alpha\nalpha alpha alpha\n# Beta\nbeta beta beta";
        let d = markdown("text/markdown", text);
        let chunks = TextChunker::new(18, 8).chunk(&d).unwrap();
        let heading = text.find("# Beta").unwrap();

        assert!(chunks.iter().any(|chunk| chunk.span.end == heading));
        assert!(chunks.iter().any(|chunk| chunk.span.start == heading));
        assert!(chunks
            .iter()
            .all(|chunk| chunk.span.end <= heading || chunk.span.start >= heading));
    }

    #[test]
    fn long_markdown_section_keeps_internal_overlap() {
        let d = markdown(
            "text/markdown",
            "# One\none two three four five six seven eight nine ten eleven twelve",
        );
        let chunks = TextChunker::new(20, 6).chunk(&d).unwrap();

        assert!(chunks.len() > 2);
        for pair in chunks.windows(2) {
            assert!(pair[1].span.start < pair[0].span.end);
        }
    }

    #[test]
    fn markdown_windows_prefer_paragraph_boundaries() {
        let text = "# H\nfirst line\n\nsecond paragraph has more words to split";
        let d = markdown("text/markdown", text);
        let chunks = TextChunker::new(30, 0).chunk(&d).unwrap();

        assert_eq!(chunks[0].text, "# H\nfirst line\n\n");
        assert_eq!(chunks[0].span.end, text.find("second").unwrap());
    }

    #[test]
    fn fenced_and_invalid_headings_do_not_start_sections() {
        let text = concat!(
            "before\n",
            "```rust\n# fenced backtick\n```\n",
            "~~~\n## fenced tilde\n~~~~\n",
            "    # too indented\n",
            "####### too many\n",
            "###no separator\n",
            "   ### valid\nafter",
        );
        let d = markdown("text/markdown", text);
        let chunks = TextChunker::new(10_000, 100).chunk(&d).unwrap();
        let valid = text.find("   ### valid").unwrap();

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].span, ByteSpan::new(0, valid));
        assert_eq!(chunks[1].span, ByteSpan::new(valid, text.len()));
        assert_eq!(chunks[0].heading_path, Vec::<String>::new());
        assert_eq!(chunks[1].heading_path, ["valid"]);
    }

    #[test]
    fn markdown_chunks_carry_normalized_nested_heading_paths() {
        let text = concat!(
            "preamble\n",
            "#   Guide ###  \nroot\n",
            "### Install ##\nchild\n",
            "## Configure\nsibling\n",
            "# API\nend",
        );
        let document = markdown("text/x-markdown; charset=utf-8", text);
        let chunks = TextChunker::new(10_000, 0).chunk(&document).unwrap();

        assert_eq!(chunks.len(), 5);
        assert_eq!(chunks[0].heading_path, Vec::<String>::new());
        assert_eq!(chunks[1].heading_path, ["Guide"]);
        assert_eq!(chunks[2].heading_path, ["Guide", "Install"]);
        assert_eq!(chunks[3].heading_path, ["Guide", "Configure"]);
        assert_eq!(chunks[4].heading_path, ["API"]);
        for chunk in &chunks {
            assert_eq!(document.slice(chunk.span), Some(chunk.text.as_str()));
        }
    }

    #[test]
    fn markdown_heading_stack_uses_actual_levels_for_skipped_nesting() {
        let text = concat!(
            "### Initial\na\n",
            "## Parent\nb\n",
            "#### Deep\nc\n",
            "### Sibling\nd",
        );
        let document = markdown("text/markdown", text);
        let chunks = TextChunker::new(10_000, 0).chunk(&document).unwrap();

        assert_eq!(chunks[0].heading_path, ["Initial"]);
        assert_eq!(chunks[1].heading_path, ["Parent"]);
        assert_eq!(chunks[2].heading_path, ["Parent", "Deep"]);
        assert_eq!(chunks[3].heading_path, ["Parent", "Sibling"]);
    }

    #[test]
    fn closing_only_hashes_are_structural_but_absent_from_breadcrumbs() {
        let document = markdown("text/markdown", "# ###\none\n# #\ntwo\n## Child\nthree");
        let chunks = TextChunker::new(10_000, 0).chunk(&document).unwrap();

        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].heading_path.is_empty());
        assert!(chunks[1].heading_path.is_empty());
        assert_eq!(chunks[0].retrieval_text(), chunks[0].text);
        assert_eq!(chunks[1].retrieval_text(), chunks[1].text);
        assert_eq!(chunks[2].heading_path, ["Child"]);
        assert!(chunks[2].retrieval_text().starts_with("Child\n\n## Child"));
    }

    #[test]
    fn every_window_gets_uniform_context_including_the_heading_window() {
        let document = markdown(
            "text/markdown",
            "# Guide\none two three four five six seven eight nine ten eleven twelve",
        );
        let chunks = TextChunker::new(20, 4).chunk(&document).unwrap();

        assert!(chunks.len() > 2);
        assert!(chunks.iter().all(|chunk| chunk.heading_path == ["Guide"]));
        assert!(chunks[0].text.starts_with("# Guide"));
        assert!(chunks[0].retrieval_text().starts_with("Guide\n\n# Guide"));
        for chunk in &chunks {
            assert!(chunk.retrieval_text().starts_with("Guide\n\n"));
            assert_eq!(document.slice(chunk.span), Some(chunk.text.as_str()));
        }
    }

    #[test]
    fn heading_like_lines_in_fences_do_not_change_context() {
        let text = "# Outer\n```md\n## Not a child\n```\nafter";
        let document = markdown("text/markdown", text);
        let chunks = TextChunker::new(10_000, 0).chunk(&document).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, ["Outer"]);
        assert_eq!(chunks[0].text, text);
    }

    #[test]
    fn markdown_utf8_and_crlf_spans_are_exact_source_slices() {
        let text = "préface\r\n# Café\r\naé日🙂 b c d e\r\n## Thé\r\nfin";
        let d = markdown("text/markdown", text);
        let chunks = TextChunker::new(12, 3).chunk(&d).unwrap();

        for (ordinal, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, ordinal);
            assert_eq!(d.slice(chunk.span).unwrap(), chunk.text);
            assert!(text.is_char_boundary(chunk.span.start));
            assert!(text.is_char_boundary(chunk.span.end));
        }
        let headings = [text.find("# Café").unwrap(), text.find("## Thé").unwrap()];
        for heading in headings {
            assert!(chunks
                .iter()
                .all(|chunk| chunk.span.end <= heading || chunk.span.start >= heading));
        }
    }

    #[test]
    fn markdown_lone_cr_headings_and_paragraphs_are_boundaries() {
        let text = "intro words\r\r# Héading\rfirst paragraph\r\rsecond paragraph tail";
        let d = markdown("text/markdown", text);
        let chunks = TextChunker::new(26, 4).chunk(&d).unwrap();
        let heading = text.find("# Héading").unwrap();

        assert!(chunks.iter().any(|chunk| chunk.span.end == heading));
        assert!(chunks.iter().any(|chunk| chunk.span.start == heading));
        assert!(chunks
            .iter()
            .all(|chunk| chunk.span.end <= heading || chunk.span.start >= heading));
        assert!(chunks.iter().any(|chunk| chunk.text.ends_with("\r\r")));
        for (ordinal, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal, ordinal);
            assert_eq!(d.slice(chunk.span).unwrap(), chunk.text);
        }
    }

    #[test]
    fn lone_cr_blank_line_wins_over_a_later_line_boundary() {
        let chars: Vec<_> = "aaaaaaaaaaaa\r\rbbbb\rcccccccccccc".chars().collect();

        // The blank line ends at 14; the ordinary line ends later at 19.
        assert_eq!(boundary_before(&chars, 0, 24, true), 14);
    }

    #[test]
    fn plain_text_keeps_v1_boundaries_byte_for_byte() {
        let d = doc("first paragraph\n\nsecond line here\nthird line here and tail");
        let chunks = TextChunker::new(24, 4).chunk(&d).unwrap();
        let spans: Vec<_> = chunks.iter().map(|chunk| chunk.span).collect();
        let texts: Vec<_> = chunks.iter().map(|chunk| chunk.text.as_str()).collect();

        assert_eq!(
            spans,
            vec![
                ByteSpan::new(0, 17),
                ByteSpan::new(13, 34),
                ByteSpan::new(30, 54),
                ByteSpan::new(50, 58),
            ]
        );
        assert_eq!(
            texts,
            vec![
                "first paragraph\n\n",
                "ph\n\nsecond line here\n",
                "ere\nthird line here and ",
                "and tail",
            ]
        );
    }

    #[test]
    fn plain_text_keeps_lone_cr_as_v1_whitespace() {
        let text = "aaaaaaaaaaaa\rb bbbbbbbbbbbbbbbbb";
        let d = doc(text);
        let chunks = TextChunker::new(24, 0).chunk(&d).unwrap();

        // In v1 a lone CR is generic whitespace, so the later space wins. If
        // CR were promoted to a line ending, this chunk would stop at byte 13.
        assert_eq!(chunks[0].span, ByteSpan::new(0, 15));
        assert_eq!(chunks[0].text, "aaaaaaaaaaaa\rb ");
        assert_eq!(d.slice(chunks[0].span).unwrap(), chunks[0].text);
    }

    #[test]
    fn markdown_mime_matching_ignores_case_and_parameters() {
        for media_type in [
            "TEXT/MARKDOWN; Charset=UTF-8",
            " text/x-markdown ; charset=utf-8",
        ] {
            let d = markdown(media_type, "before\n# Heading\nafter");
            let chunks = TextChunker::new(1_000, 0).chunk(&d).unwrap();
            assert_eq!(chunks.len(), 2, "{media_type}");
        }
    }

    #[test]
    fn fingerprint_invalidates_v2_markdown_indexes() {
        assert_eq!(
            TextChunker::new(90, 10).fingerprint(),
            "text-window-v3:markdown=atx-heading-context:max_chars=90:overlap=10"
        );
    }
}

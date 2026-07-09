//! The chunking seam: split a document's text into overlapping, span-tracked
//! pieces small enough to embed.
//!
//! [`TextChunker`] is a boundary-aware sliding window. It walks the text in
//! character units (so it never splits a UTF-8 codepoint), caps each window at a
//! character budget, and prefers to cut at a natural boundary — a line break,
//! else whitespace — within the back half of the window. Consecutive chunks
//! overlap by a configurable number of characters so a passage straddling a cut
//! still lands wholly inside at least one chunk.
//!
//! Every emitted [`Chunk`] records its exact [`ByteSpan`], which is what makes
//! citations precise. A richer stack — hierarchical heading/paragraph chunkers,
//! sentence shingles — can be layered behind the [`Chunker`] trait later; this is
//! the dependable single-strategy floor.

use crate::document::{ByteSpan, Chunk, Document};
use crate::error::Result;

/// Splits a document into chunks.
///
/// Object-safe so strategies can be swapped or composed as `Box<dyn Chunker>`.
/// Fallible like the other seams: today's [`TextChunker`] never errors, but a
/// future service- or model-backed chunker (sentence tokenizers, layout models)
/// can, and returning [`Result`] now keeps that from being a breaking change.
pub trait Chunker: Send + Sync {
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

        let mut chunks = Vec::new();
        let mut ordinal = 0;
        let mut start = 0; // char index

        while start < n {
            let hard_end = (start + self.max_chars).min(n);
            // Cut at a natural boundary unless we're already at the text's end.
            let end = if hard_end == n {
                n
            } else {
                boundary_before(&chars, start, hard_end)
            };

            let span = ByteSpan::new(byte_at[start], byte_at[end]);
            // Slice on byte offsets we computed from the same char view.
            let piece = &text[span.start..span.end];
            if !piece.trim().is_empty() {
                chunks.push(Chunk::new(document.id, ordinal, span, piece));
                ordinal += 1;
            }

            if end >= n {
                break;
            }
            // Advance, carrying `overlap` chars back. Guarantee forward progress:
            // never move the window start backwards or leave it put.
            let next = end.saturating_sub(self.overlap);
            start = if next <= start { start + 1 } else { next };
        }

        Ok(chunks)
    }
}

/// Find a good char index to end a chunk within `(start, hard_end)`.
///
/// Searches the back half of the window for the last line break, then the last
/// whitespace, cutting just after it. Falls back to `hard_end` (a hard cut) when
/// the window has no boundary. The returned index is an exclusive char index.
fn boundary_before(chars: &[char], start: usize, hard_end: usize) -> usize {
    let floor = start + (hard_end - start) / 2;
    let mut last_newline = None;
    let mut last_whitespace = None;
    for (offset, &c) in chars[floor..hard_end].iter().enumerate() {
        let i = floor + offset;
        if c == '\n' {
            last_newline = Some(i + 1);
        } else if c.is_whitespace() {
            last_whitespace = Some(i + 1);
        }
    }
    last_newline.or(last_whitespace).unwrap_or(hard_end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSource;

    fn doc(text: &str) -> Document {
        Document::new(DocumentSource::Inline, "text/plain", text)
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
}

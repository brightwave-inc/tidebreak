/** A citation's half-open byte range in a document's canonical text. */
export type CitationSpan = { start: number; end: number };

/** A half-open range of JavaScript string indices. */
export type CharRange = { start: number; end: number };

/**
 * Where a citation's byte span falls in the JavaScript string holding the same
 * text.
 *
 * Spans are UTF-8 byte offsets, which is how the text of record is indexed
 * where it is produced; a JavaScript string is indexed by UTF-16 code unit. The
 * two agree only while the text stays ASCII, so a single accent or emoji before
 * the citation would otherwise shift the highlight off the passage. The walk
 * stops at the end of the span rather than scanning the whole document, so the
 * cost is the offset rather than the file.
 *
 * Returns `null` for a span that does not describe a passage in this text — an
 * empty range, or one starting past the end because the document was re-read
 * since the citation was made. The document is still worth opening in that
 * case, just not at a position invented for it.
 */
export function charRangeForByteSpan(
  text: string,
  span: CitationSpan,
): CharRange | null {
  if (!Number.isSafeInteger(span.start) || !Number.isSafeInteger(span.end)) return null;
  if (span.start < 0 || span.end <= span.start) return null;

  let byte = 0;
  let index = 0;
  let start: number | null = null;
  while (index < text.length) {
    if (start === null && byte >= span.start) start = index;
    if (byte >= span.end) break;
    const codePoint = text.codePointAt(index)!;
    byte += utf8Length(codePoint);
    index += codePoint > 0xffff ? 2 : 1;
  }

  if (start === null) {
    if (byte < span.start) return null;
    start = text.length;
  }
  const end = Math.min(index, text.length);
  return end > start ? { start, end } : null;
}

function utf8Length(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

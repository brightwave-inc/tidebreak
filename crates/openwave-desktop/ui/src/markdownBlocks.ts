import { marked } from "marked";

/**
 * Split Markdown source into top-level block strings using marked's lexer.
 *
 * Each returned string is the verbatim source (`token.raw`) of one block-level
 * token, so concatenating the result reproduces the input. The streaming
 * renderer relies on this to memoize already-settled blocks and re-parse only
 * the trailing (still-growing) block as content streams in, avoiding an
 * O(n^2) full-document re-parse on every typewriter tick.
 *
 * marked is used purely as a block tokenizer here — the blocks are handed to
 * react-markdown (remark/rehype) for the actual rendering, so marked never sees
 * our math or GFM extensions. Block grouping (loose lists, tables, fenced code)
 * follows CommonMark, which matches remark's block boundaries.
 */
export function splitMarkdownBlocks(content: string): string[] {
  if (content.length === 0) return [];
  try {
    const blocks = marked
      .lexer(content)
      .map((token) => token.raw)
      .filter((raw) => raw.length > 0);
    // Fall back to a single block if the lexer produced nothing usable
    // (e.g. content that is entirely whitespace was filtered out).
    return blocks.length > 0 ? blocks : [content];
  } catch {
    return [content];
  }
}

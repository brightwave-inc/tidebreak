import { fromMarkdown } from "mdast-util-from-markdown";

/**
 * Split Markdown source into top-level block strings using mdast's parser.
 *
 * Each returned string is the verbatim source of one block-level node plus the
 * blank lines that follow it, so concatenating the result reproduces the input.
 * The streaming renderer relies on this to memoize already-settled blocks and
 * re-parse only the trailing (still-growing) block as content streams in,
 * avoiding an O(n^2) full-document re-parse on every typewriter tick.
 *
 * mdast is the same parser react-markdown (remark/rehype) uses for rendering,
 * so block grouping (loose lists, tables, fenced code) matches the renderer.
 */
export function splitMarkdownBlocks(content: string): string[] {
  if (content.length === 0) return [];
  try {
    const nodes = fromMarkdown(content).children;
    if (nodes.length === 0) return [content];
    const blocks: string[] = [];
    const firstStart = nodes[0]?.position?.start.offset ?? 0;
    if (firstStart > 0) {
      blocks.push(content.slice(0, firstStart));
    }
    for (let i = 0; i < nodes.length; i += 1) {
      const start = nodes[i]?.position?.start.offset;
      if (start == null) continue;
      const nextStart = nodes[i + 1]?.position?.start.offset ?? content.length;
      const raw = content.slice(start, nextStart);
      if (raw.length > 0) blocks.push(raw);
    }
    return blocks.length > 0 ? blocks : [content];
  } catch {
    return [content];
  }
}

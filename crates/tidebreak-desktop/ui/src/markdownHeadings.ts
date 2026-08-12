/**
 * A markdown document's headings, and the ids they are addressed by.
 *
 * The ids are derived from the heading text rather than assigned, so the
 * outline can compute one from the raw source and the renderer can compute the
 * same one from the rendered node without either passing anything to the other.
 * That only holds while both call {@link slugify} — a heading whose id is
 * generated two different ways is a heading the outline cannot scroll to.
 */

export type MarkdownHeading = {
  level: number;
  text: string;
  id: string;
};

/** Heading text reduced to a stable, URL-shaped id. */
export function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

/** Drop the inline markup a heading's text may carry, keeping the words. */
function stripInline(text: string): string {
  return text
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/_([^_]+)_/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .trim();
}

const HEADING = /^(#{1,6})\s+(.+)$/;
const FENCE = /^\s{0,3}(```+|~~~+)/;

/**
 * The ATX headings of a raw markdown document, in the order they appear.
 *
 * Fenced blocks are skipped: a shell comment or a C preprocessor line inside
 * one starts with `#` but is not a heading, and listing it would put an entry
 * in the outline that scrolls nowhere — the renderer never made a heading for
 * it to scroll to. An unclosed fence runs to the end of the document, which is
 * how the renderer treats it too.
 *
 * Indented code needs no such handling: a heading's `#` has to be within three
 * spaces of the margin, and a four-space block is already past that.
 */
export function extractHeadings(markdown: string): MarkdownHeading[] {
  const headings: MarkdownHeading[] = [];
  let fence: string | null = null;

  for (const line of markdown.split("\n")) {
    const fenceMatch = FENCE.exec(line);
    if (fenceMatch) {
      const marker = fenceMatch[1]!;
      if (fence === null) {
        fence = marker[0]!;
        continue;
      }
      // Only a fence of the same character closes one, and a closing fence has
      // to be at least as long as the one it closes.
      if (marker[0] === fence) fence = null;
      continue;
    }
    if (fence !== null) continue;

    const match = HEADING.exec(line);
    if (!match) continue;
    const text = stripInline(match[2]!);
    if (!text) continue;
    headings.push({ level: match[1]!.length, text, id: slugify(text) });
  }

  return headings;
}

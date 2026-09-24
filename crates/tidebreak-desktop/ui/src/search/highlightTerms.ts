import { foldText } from "./messageSearch";

/**
 * Marking a query's words inside the transcript on screen.
 *
 * The marks go through the CSS Custom Highlight API, which paints ranges
 * without touching the DOM React owns: wrapping matches in elements would
 * fight every re-render of a streaming answer. Where the API is missing, the
 * revealed row still gets its ring and nothing else changes.
 */

/** Every match of the find bar's query in the rows on screen. */
export const FIND_HIGHLIGHT = "transcript-find";
/** The match the find bar or the palette is pointing at. */
export const FOCUSED_HIGHLIGHT = "transcript-find-current";

/** The class a revealed row wears while it is pointed out. */
export const REVEALED_CLASS = "is-search-revealed";
/** How long the ring stays up. */
export const REVEALED_MS = 2_000;

const WORD = /[\p{L}\p{N}]+/gu;

/**
 * Where a word can start matching: its first letter, and each camelCase
 * part, the way the index splits `SubmitButton` into `submit` and `button`.
 */
function partStarts(word: string): number[] {
  const starts = [0];
  for (let index = 1; index < word.length; index += 1) {
    const previous = word[index - 1] ?? "";
    const current = word[index] ?? "";
    if (/\p{Ll}/u.test(previous) && /\p{Lu}/u.test(current)) {
      starts.push(index);
    }
  }
  return starts;
}

/** How many characters of `text` fold to something starting with `term`. */
function matchedLength(text: string, term: string): number {
  if (!foldText(text).startsWith(term)) return 0;
  for (let end = 1; end <= text.length; end += 1) {
    if (foldText(text.slice(0, end)).length >= term.length) return end;
  }
  return text.length;
}

/**
 * The spans of `text` where a word, or a camelCase part of one, starts with
 * one of `terms`. Folded the way the index folds, so `cafe` marks `Café`.
 */
export function termSpans(
  text: string,
  terms: readonly string[],
): { start: number; end: number }[] {
  const wanted = terms.filter((term) => term.length > 0);
  if (wanted.length === 0) return [];
  const spans: { start: number; end: number }[] = [];
  for (const found of text.matchAll(WORD)) {
    const word = found[0];
    const offset = found.index ?? 0;
    for (const start of partStarts(word)) {
      const rest = word.slice(start);
      let longest = 0;
      for (const term of wanted) {
        longest = Math.max(longest, matchedLength(rest, term));
      }
      if (longest > 0) {
        spans.push({ start: offset + start, end: offset + start + longest });
      }
    }
  }
  return spans;
}

/** DOM ranges over every span of `root`'s text that `terms` match. */
export function termRanges(root: Node, terms: readonly string[]): Range[] {
  if (terms.length === 0) return [];
  const document = root.ownerDocument ?? (root as Document);
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const ranges: Range[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = node as Text;
    for (const span of termSpans(text.data, terms)) {
      const range = document.createRange();
      range.setStart(text, span.start);
      range.setEnd(text, span.end);
      ranges.push(range);
    }
  }
  return ranges;
}

type HighlightRegistry = {
  set: (name: string, highlight: unknown) => void;
  delete: (name: string) => void;
};

function registry(): HighlightRegistry | null {
  const css = globalThis.CSS as { highlights?: HighlightRegistry } | undefined;
  const Highlight = (globalThis as { Highlight?: unknown }).Highlight;
  if (!css?.highlights || typeof Highlight !== "function") return null;
  return css.highlights;
}

/** Paint `ranges` under `name`, replacing whatever it painted before. */
export function paintHighlight(name: string, ranges: readonly Range[]): void {
  const highlights = registry();
  if (!highlights) return;
  if (ranges.length === 0) {
    highlights.delete(name);
    return;
  }
  const Highlight = (
    globalThis as unknown as {
      Highlight: new (...ranges: Range[]) => unknown;
    }
  ).Highlight;
  highlights.set(name, new Highlight(...ranges));
}

export function clearHighlight(name: string): void {
  registry()?.delete(name);
}

/** Ring `element` for a moment, so the eye lands on it after the scroll. */
export function ringElement(element: Element): void {
  element.classList.add(REVEALED_CLASS);
  globalThis.setTimeout(
    () => element.classList.remove(REVEALED_CLASS),
    REVEALED_MS,
  );
}

/**
 * Scroll `scroller` so `element` sits a little below its top edge, where a
 * reader expects a found line to land. Respects reduced motion.
 */
export function scrollToElement(scroller: Element, element: Element): void {
  const containerRect = scroller.getBoundingClientRect();
  const targetRect = element.getBoundingClientRect();
  const reduce =
    typeof window !== "undefined" &&
    window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
  const room = Math.max(24, Math.round(containerRect.height / 4));
  scroller.scrollTo({
    top: Math.max(
      0,
      scroller.scrollTop + targetRect.top - containerRect.top - room,
    ),
    behavior: reduce ? "auto" : "smooth",
  });
}

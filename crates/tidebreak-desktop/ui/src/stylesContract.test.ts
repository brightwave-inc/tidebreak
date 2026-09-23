import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The design system's mechanical rules, enforced where prose cannot reach.
 * DESIGN.md says what the vocabulary means; this test keeps the vocabulary
 * closed. If a failure here is the right change, the fix is to grow the
 * system — a scale rung in styles.css, a tone quad, an allowlist entry with
 * a reason — never to reword a class until the regex misses it.
 */

const SRC = join(import.meta.dirname, ".");

/** Document-viewer conventions that legitimately sit outside the tokens. */
const RAW_PALETTE_ALLOWLIST = new Set([
  // Syntax highlighting: JSON and XML keys/values are code, not UI state.
  "components/document/json-viewer.tsx",
  "components/document/xml-viewer.tsx",
  // The highlighter-pen yellow marking cited passages inside documents.
  "components/document/citationMark.ts",
]);

/**
 * Tailwind's raw palette, e.g. `text-emerald-600`. State goes through the
 * status tones, identity through `--icon-*`; a raw step is neither.
 */
const RAW_PALETTE =
  /\b(?:text|bg|border|ring|fill|stroke|from|via|to|outline|decoration|caret|shadow)-(?:red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose|slate|gray|zinc|neutral|stone)-[0-9]{2,3}\b/;

/** An arbitrary font size, e.g. `text-[13px]` — the scale is pinned. */
const ARBITRARY_TEXT_SIZE = /\btext-\[[0-9]/;

/**
 * Text dimmed with an alpha below 90%, e.g. `text-muted-foreground/70`. The
 * alpha took secondary text to 2.5–3.8:1. Every text token already clears
 * 4.5:1 (colorContrast.test.ts), so dimmer text is a token, not an alpha.
 * Size rungs such as `text-sm/6` are line heights, not alpha.
 */
const DIMMED_TEXT =
  /\btext-(?!(?:2xs|xs|sm|md|base|lg|[2-9]?xl)\/)[a-z][\w-]*\/(?:[0-8]?\d(?!\d)|\[)/;

/** Class lists that dim ink on purpose, outside the app's own text. */
const DIMMED_TEXT_ALLOWLIST = new Set([
  // Syntax punctuation in the JSON and XML document viewers.
  "components/document/json-viewer.tsx",
  "components/document/xml-viewer.tsx",
  // A mock third-party web page drawn inside the embedded browser story.
  "stories/Browser.stories.tsx",
]);

/**
 * The class list a match sits in: the quoted string around it. A list that
 * sizes a glyph with `size-*` is an icon. Icons are marks, not text; the
 * contrast test holds the mark tokens to 3:1.
 */
function isIconClassList(line: string, index: number): boolean {
  let start = 0;
  let end = line.length;
  for (const quote of line.matchAll(/["'`]/g)) {
    const at = quote.index ?? 0;
    if (at < index) start = at + 1;
    else {
      end = at;
      break;
    }
  }
  return /(?:^|\s)size-/.test(line.slice(start, end));
}

function sourceFiles(): string[] {
  return readdirSync(SRC, { recursive: true, encoding: "utf8" })
    .filter((path) => /\.(ts|tsx)$/.test(path) && !path.includes("generated/"))
    .map((path) => path.replaceAll("\\", "/"));
}

function offenders(
  pattern: RegExp,
  skip?: ReadonlySet<string>,
  exempt?: (line: string, index: number) => boolean,
): string[] {
  const everywhere = new RegExp(pattern.source, `${pattern.flags}g`);
  const hits: string[] = [];
  for (const file of sourceFiles()) {
    if (skip?.has(file)) continue;
    if (file === relative(SRC, import.meta.filename).replaceAll("\\", "/")) {
      continue;
    }
    const lines = readFileSync(join(SRC, file), "utf8").split("\n");
    lines.forEach((line, index) => {
      const match = [...line.matchAll(everywhere)].find(
        (candidate) => !exempt?.(line, candidate.index ?? 0),
      );
      if (match) hits.push(`${file}:${index + 1}  ${match[0]}`);
    });
  }
  return hits;
}

/** RefreshCw / RotateCw / Loader2 / LoaderCircle JSX that also carries animate-spin. */
const SPINNING_LUCIDE_REFRESH =
  /<(RefreshCw|RotateCw|Loader2|LoaderCircle)\b[^>]*animate-spin/;

function spinningRefreshIcons(): string[] {
  const hits: string[] = [];
  for (const file of sourceFiles()) {
    if (/\.test\.(ts|tsx)$/.test(file)) continue;
    const text = readFileSync(join(SRC, file), "utf8");
    if (SPINNING_LUCIDE_REFRESH.test(text)) hits.push(file);
  }
  return hits;
}

describe("styles contract (see DESIGN.md)", () => {
  it("uses the pinned type scale, not arbitrary sizes", () => {
    expect(
      offenders(ARBITRARY_TEXT_SIZE),
      "Use a text-* rung from the scale in styles.css. If a real new rung " +
        "emerged, add it to the scale and DESIGN.md instead.",
    ).toEqual([]);
  });

  it("uses semantic tokens, not the raw Tailwind palette", () => {
    expect(
      offenders(RAW_PALETTE, RAW_PALETTE_ALLOWLIST),
      "Color state through the status tones (statusTone.ts, Badge) and " +
        "identity through --icon-*. See DESIGN.md; allowlist a genuine " +
        "document-viewer convention here with a reason.",
    ).toEqual([]);
  });

  it("dims text with a token, not an alpha below 90%", () => {
    expect(
      offenders(DIMMED_TEXT, DIMMED_TEXT_ALLOWLIST, isIconClassList),
      "Text that states something takes a text token at full strength: " +
        "text-muted-foreground, or a status -foreground rung. See DESIGN.md.",
    ).toEqual([]);
  });

  it("does not spin Lucide refresh icons", () => {
    expect(
      spinningRefreshIcons(),
      "Busy refresh controls swap to Spinner from components/ui/spinner.tsx. " +
        "Lucide RefreshCw, RotateCw, Loader2, and LoaderCircle orbit under animate-spin. See DESIGN.md.",
    ).toEqual([]);
  });
});

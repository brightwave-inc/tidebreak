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

/**
 * The notice shapes `Notice` (components/ui/notice.tsx) replaced: the
 * `notice-surface` classes some panels boxed and others did not, the
 * transcript's bracketed `.message-notice`, the turn failure's own layout,
 * and the settings verdict's. A panel that draws its own notice is a second
 * answer to a question the component settles.
 */
export const RETIRED_NOTICE_CLASS =
  /(?<![\w-])(?:notice-surface|message-notice|message-turn-failure|settings-status)(?!\w)/;

/**
 * Their rules in styles.css, tones included, so the classes cannot come back
 * as selectors either.
 */
const RETIRED_NOTICE_SELECTOR =
  /\.(?:notice-surface|notice-(?:critical|warning|info|success)|message-notice|message-turn-failure|settings-status)(?!\w)/;

/**
 * `text-destructive` paints the same red as `text-critical` under a second
 * name, and the two drifted into two reds. Text uses `text-critical`; a fill
 * or a border may still say `destructive`, and `text-destructive-foreground`
 * is the ink on that fill.
 */
export const DESTRUCTIVE_TEXT = /\btext-destructive(?![-\w])/;

function cssOffenders(pattern: RegExp): string[] {
  const hits: string[] = [];
  const lines = readFileSync(join(SRC, "styles.css"), "utf8").split("\n");
  lines.forEach((line, index) => {
    const match = line.match(pattern);
    if (match) hits.push(`styles.css:${index + 1}  ${match[0]}`);
  });
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

  it("uses type scale tokens in styles.css, not raw font-size", () => {
    const allowed = /^var\(--text-(?:2xs|xs|sm|md|base|lg|xl|2xl|3xl)\)$/;
    const hits: string[] = [];
    const lines = readFileSync(join(SRC, "styles.css"), "utf8").split("\n");
    lines.forEach((line, index) => {
      const match = line.match(/font-size:\s*([^;]+);/);
      if (!match) return;
      const value = match[1].trim();
      if (!allowed.test(value)) {
        hits.push(`styles.css:${index + 1}  ${value}`);
      }
    });
    expect(
      hits,
      "Every font-size in styles.css must be a var(--text-*) rung.",
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

  it("draws every notice and failure through Notice", () => {
    const guidance =
      "Render a notice or a failure with <Notice> from " +
      "components/ui/notice.tsx; it owns the radius, border, padding, icon, " +
      "and action slot. See DESIGN.md, Notices and errors.";
    expect(offenders(RETIRED_NOTICE_CLASS), guidance).toEqual([]);
    expect(cssOffenders(RETIRED_NOTICE_SELECTOR), guidance).toEqual([]);
  });

  it("recognizes every retired notice shape", () => {
    for (const retired of [
      'className="notice-surface notice-critical rounded-lg border"',
      "cn(`message-notice is-${role}`)",
      '<aside className="message-turn-failure">',
      'className="settings-status notice-warning"',
    ]) {
      expect(RETIRED_NOTICE_CLASS.test(retired), retired).toBe(true);
    }
    expect(RETIRED_NOTICE_SELECTOR.test(".message-notice.is-error {")).toBe(
      true,
    );
    // Neighbors that only share a word stay allowed.
    expect(RETIRED_NOTICE_CLASS.test('className="message-branch-notice"')).toBe(
      false,
    );
  });

  it("colors error text with text-critical, not text-destructive", () => {
    expect(
      offenders(DESTRUCTIVE_TEXT),
      "Use text-critical for red text; destructive is the same red under a " +
        "second name. See DESIGN.md, Color.",
    ).toEqual([]);
    expect(DESTRUCTIVE_TEXT.test("hover:text-destructive")).toBe(true);
    expect(DESTRUCTIVE_TEXT.test("text-destructive-foreground")).toBe(false);
  });

  it("does not spin Lucide refresh icons", () => {
    expect(
      spinningRefreshIcons(),
      "Busy refresh controls swap to Spinner from components/ui/spinner.tsx. " +
        "Lucide RefreshCw, RotateCw, Loader2, and LoaderCircle orbit under animate-spin. See DESIGN.md.",
    ).toEqual([]);
  });

  it("paints code-mode status through statusTone.ts, Badge, or the diff tints", () => {
    expect(
      codeStatusRungLiterals(),
      "In src/code, pick a tone and paint with STATUS_TEXT, STATUS_CHIP, " +
        "STATUS_MARK, or Badge. Do not write bg-*-background or " +
        "text-*-foreground by hand. DiffPanel may keep add and delete tints.",
    ).toEqual([]);
  });
});

/**
 * Status-rung literals in code mode. StatusTone maps and Badge own the
 * vocabulary; DiffPanel keeps add/delete tints as document-like markup.
 */
const CODE_STATUS_RUNG =
  /\b(?:bg|text)-(?:success|warning|critical|info|merged|live)-(?:background|foreground(?:-muted)?)\b/;

const CODE_STATUS_RUNG_ALLOWLIST = new Set([
  "code/statusTone.ts",
  "code/DiffPanel.tsx",
]);

function codeStatusRungLiterals(): string[] {
  const everywhere = new RegExp(CODE_STATUS_RUNG.source, "g");
  const hits: string[] = [];
  for (const file of sourceFiles()) {
    if (!file.startsWith("code/")) continue;
    if (CODE_STATUS_RUNG_ALLOWLIST.has(file)) continue;
    if (/\.test\.(ts|tsx)$/.test(file)) continue;
    const lines = readFileSync(join(SRC, file), "utf8").split("\n");
    lines.forEach((line, index) => {
      const match = line.match(everywhere);
      if (match) hits.push(`${file}:${index + 1}  ${match[0]}`);
    });
  }
  return hits;
}

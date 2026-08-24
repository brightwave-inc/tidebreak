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

function sourceFiles(): string[] {
  return readdirSync(SRC, { recursive: true, encoding: "utf8" })
    .filter((path) => /\.(ts|tsx)$/.test(path) && !path.includes("generated/"))
    .map((path) => path.replaceAll("\\", "/"));
}

function offenders(pattern: RegExp, skip?: ReadonlySet<string>): string[] {
  const hits: string[] = [];
  for (const file of sourceFiles()) {
    if (skip?.has(file)) continue;
    if (file === relative(SRC, import.meta.filename).replaceAll("\\", "/")) {
      continue;
    }
    const lines = readFileSync(join(SRC, file), "utf8").split("\n");
    lines.forEach((line, index) => {
      const match = pattern.exec(line);
      if (match) hits.push(`${file}:${index + 1}  ${match[0]}`);
    });
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
});

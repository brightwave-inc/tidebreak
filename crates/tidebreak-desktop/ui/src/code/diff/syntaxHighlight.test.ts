import { beforeAll, describe, expect, it } from "vitest";

import { groupUnifiedDiff } from "../unifiedDiff";
import {
  FileSyntax,
  MAX_HIGHLIGHT_HUNK_LINES,
  MAX_HIGHLIGHT_LINE_CHARS,
  RECENT_HIGHLIGHT_CHARS,
  diffLanguage,
  highlightLines,
  hunkSpans,
  isLanguageReady,
  loadLanguage,
  recentHighlightChars,
  syntaxLinesFromMarkup,
  type SyntaxLine,
} from "./syntaxHighlight";

function roleOf(line: SyntaxLine | undefined, text: string) {
  return line?.find((run) => run.text.includes(text))?.role;
}

describe("syntaxLinesFromMarkup", () => {
  it("carries a span that crosses a newline into the next line", () => {
    const lines = syntaxLinesFromMarkup(
      '<span class="hljs-comment">/* one\ntwo */</span> <span class="hljs-keyword">let</span> x;',
    );
    expect(lines).toEqual([
      [{ text: "/* one", role: "comment" }],
      [
        { text: "two */", role: "comment" },
        { text: " ", role: null },
        { text: "let", role: "keyword" },
        { text: " x;", role: null },
      ],
    ]);
  });

  it("decodes the engine's entities and lets a template's code read as code", () => {
    const [line] = syntaxLinesFromMarkup(
      '<span class="hljs-string">`a &lt; b &amp;&amp; <span class="hljs-subst">${c}</span>`</span>',
    );
    expect(line).toEqual([
      { text: "`a < b && ", role: "string" },
      { text: "${c}", role: null },
      { text: "`", role: "string" },
    ]);
  });
});

describe("highlightLines", () => {
  beforeAll(async () => {
    expect(await loadLanguage("typescript")).toBe(true);
  });

  it("loads a grammar on demand and colors by role", () => {
    expect(isLanguageReady("typescript")).toBe(true);
    const [line] = highlightLines(
      ['const label = "queue"; // why'],
      "typescript",
    )!;
    expect(roleOf(line, "const")).toBe("keyword");
    expect(roleOf(line, '"queue"')).toBe("string");
    expect(roleOf(line, "// why")).toBe("comment");
  });

  it("stays plain past the caps", () => {
    expect(
      highlightLines(["x".repeat(MAX_HIGHLIGHT_LINE_CHARS + 1)], "typescript"),
    ).toBeNull();
    expect(
      highlightLines(
        Array.from({ length: MAX_HIGHLIGHT_HUNK_LINES + 1 }, () => "x;"),
        "typescript",
      ),
    ).toBeNull();
    expect(highlightLines(["let x = 1;"], "cobol")).toBeNull();
  });
});

describe("highlighting a hunk", () => {
  beforeAll(async () => {
    await loadLanguage("typescript");
  });

  // A hunk is a fragment of a file. Each side is read as one run, so a
  // comment that opens on a context line colors the changed lines inside it,
  // which read alone would parse as code.
  const DIFF = [
    "diff --git a/queue.ts b/queue.ts",
    "@@ -1,5 +1,5 @@",
    " /* The queue drains",
    "-   in send order",
    "+   in the order sent",
    "    and never reorders. */",
    " const limit = 10;",
  ].join("\n");

  it("reads each side of the hunk as one run", () => {
    const group = groupUnifiedDiff(DIFF)[0]!;
    const syntax = FileSyntax.for(group)!;
    expect(syntax.language).toBe("typescript");
    const hunk = syntax.compute(0)!;
    const removed = group.lines.findIndex(
      (line) => line.text === "-   in send order",
    );
    const added = group.lines.findIndex(
      (line) => line.text === "+   in the order sent",
    );
    const code = group.lines.findIndex(
      (line) => line.text === " const limit = 10;",
    );
    expect(roleOf(hunk.old.get(removed), "send order")).toBe("comment");
    expect(roleOf(hunk.new.get(added), "order sent")).toBe("comment");
    expect(roleOf(hunk.new.get(code), "const")).toBe("keyword");

    // The same changed line on its own would have read as code.
    const [alone] = highlightLines(["   in send order"], "typescript")!;
    expect(roleOf(alone, "in")).not.toBe("comment");
  });

  it("recalls a hunk a refreshed diff did not change, without highlighting it again", () => {
    const before = FileSyntax.for(groupUnifiedDiff(DIFF)[0]!)!;
    const computed = before.compute(0)!;
    // The same text arrives again as a new group, as a refetch hands it over.
    const after = FileSyntax.for(groupUnifiedDiff(DIFF)[0]!)!;
    expect(after.has(0)).toBe(false);
    expect(after.recall(0)).toBe(true);
    expect(after.get(0)).toEqual(computed);

    const changed = FileSyntax.for(
      groupUnifiedDiff(DIFF.replace("in send order", "in arrival order"))[0]!,
    )!;
    expect(changed.recall(0)).toBe(false);
  });

  /** One hunk of `removed` and `added` lines, each line `width` long. */
  function bigHunk(removed: number, added: number, width = 20, seed = "") {
    const line = (sign: string, index: number) =>
      `${sign}${`let v${seed}${index} = ${index};`.padEnd(width, " ")}`;
    return [
      "diff --git a/big.ts b/big.ts",
      `@@ -1,${removed} +1,${added} @@`,
      ...Array.from({ length: removed }, (_, index) => line("-", index)),
      ...Array.from({ length: added }, (_, index) => line("+", index)),
    ].join("\n");
  }

  it("leaves a hunk plain when its sides together pass the line cap", () => {
    const half = Math.ceil(MAX_HIGHLIGHT_HUNK_LINES / 2) + 1;
    const group = groupUnifiedDiff(bigHunk(half, half))[0]!;
    const before = recentHighlightChars();
    expect(FileSyntax.for(group)!.compute(0)).toBeNull();
    // Nothing of it is kept.
    expect(recentHighlightChars()).toBe(before);
  });

  it("keeps what it remembers under a size budget, however many versions arrive", () => {
    for (let version = 0; version < 40; version += 1) {
      const group = groupUnifiedDiff(bigHunk(600, 600, 60, `x${version}`))[0]!;
      expect(FileSyntax.for(group)!.compute(0)).not.toBeNull();
      expect(recentHighlightChars()).toBeLessThanOrEqual(
        RECENT_HIGHLIGHT_CHARS,
      );
    }
    // The newest version is still there to recall.
    const latest = FileSyntax.for(
      groupUnifiedDiff(bigHunk(600, 600, 60, "x39"))[0]!,
    )!;
    expect(latest.recall(0)).toBe(true);
  });

  it("finds each hunk's lines, and computes each hunk once", () => {
    const group = groupUnifiedDiff(`${DIFF}\n@@ -40,1 +40,1 @@\n-a\n+b`)[0]!;
    expect(hunkSpans(group)).toEqual([
      { start: 0, end: 6 },
      { start: 6, end: 9 },
    ]);
    const syntax = FileSyntax.for(group)!;
    const first = syntax.compute(1);
    expect(syntax.compute(1)).toBe(first);
  });
});

describe("diffLanguage", () => {
  it("reads a path the way the output viewer does, plus the diff's extras", () => {
    expect(diffLanguage("crates/server/src/lib.rs")).toBe("rust");
    expect(diffLanguage("ui/src/Diff.tsx")).toBe("typescript");
    expect(diffLanguage("docs/code-mode.md")).toBe("markdown");
    expect(diffLanguage("Cargo.lock")).toBeNull();
    expect(diffLanguage("Dockerfile")).toBe("bash");
  });
});

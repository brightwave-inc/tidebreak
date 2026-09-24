import { describe, expect, it } from "vitest";

import { groupUnifiedDiff } from "../unifiedDiff";
import {
  anchorRows,
  placeComment,
  quoteRows,
  type CommentAnchor,
} from "./commentAnchor";
import { buildDiffFileModel, type DiffRow } from "./diffModel";

function rowsOf(lines: readonly string[], ignoreWhitespace = false) {
  const group = groupUnifiedDiff(
    ["diff --git a/a.ts b/a.ts", ...lines].join("\n"),
  )[0]!;
  return buildDiffFileModel(group, { ignoreWhitespace }).rows;
}

function lastRowOf(rows: readonly DiffRow[], text: string): number {
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    if (rows[index]!.text === text) return index;
  }
  throw new Error(`no row reads ${text}`);
}

function rowOf(rows: readonly DiffRow[], text: string): number {
  const index = rows.findIndex((row) => row.text === text);
  if (index < 0) throw new Error(`no row reads ${text}`);
  return index;
}

/** The anchor a comment written on the rows reading `from` to `to` keeps. */
function anchorOn(
  rows: readonly DiffRow[],
  from: string,
  to = from,
): CommentAnchor {
  return anchorRows(rows, rowOf(rows, from), rowOf(rows, to));
}

const WRITTEN = [
  "@@ -8,5 +8,6 @@",
  " const i = 1;",
  " const j = 2;",
  "-const K = 3;",
  "+const K = 30;",
  "+const L = 4;",
  " const m = 5;",
];

describe("placeComment", () => {
  it("finds the lines where they were", () => {
    const rows = rowsOf(WRITTEN);
    const placed = placeComment(rows, anchorOn(rows, "const K = 30;"));
    expect(placed).toMatchObject({
      kind: "placed",
      start: rowOf(rows, "const K = 30;"),
      lines: [{ kind: "add", newNo: 10, text: "const K = 30;" }],
    });
  });

  it("follows the lines when the agent adds one above them", () => {
    const before = rowsOf(WRITTEN);
    const anchor = anchorOn(before, "const K = 30;");
    const after = rowsOf([
      "@@ -8,5 +8,7 @@",
      " const i = 1;",
      "+const inserted = 0;",
      " const j = 2;",
      "-const K = 3;",
      "+const K = 30;",
      "+const L = 4;",
      " const m = 5;",
    ]);
    const placed = placeComment(after, anchor);
    // Line 10 is `const j = 2;` now; the comment stays on its own line.
    expect(placed).toMatchObject({
      kind: "placed",
      start: rowOf(after, "const K = 30;"),
      end: rowOf(after, "const K = 30;"),
      lines: [{ kind: "add", newNo: 11, text: "const K = 30;" }],
      span: { lines: "11", oldLines: null },
    });
  });

  it("marks the comment outdated when its lines change", () => {
    const before = rowsOf(WRITTEN);
    const anchor = anchorOn(before, "const K = 30;");
    const after = rowsOf([
      "@@ -8,5 +8,6 @@",
      " const i = 1;",
      " const j = 2;",
      "-const K = 3;",
      "+const K = 300;",
      "+const L = 4;",
      " const m = 5;",
    ]);
    expect(placeComment(after, anchor)).toEqual({ kind: "outdated" });
  });

  it("tells repeated lines apart by the code around them", () => {
    const lines = [
      "@@ -1,9 +1,9 @@",
      " function a() {",
      "-  return 1;",
      "+  return 2;",
      " }",
      " function b() {",
      "-  return 1;",
      "+  return 2;",
      " }",
    ];
    const before = rowsOf(lines);
    const second = lastRowOf(before, "  return 2;");
    const anchor = anchorRows(before, second, second);
    // The agent adds a line at the top, so every number moves down one.
    const after = rowsOf(["@@ -1,9 +1,10 @@", "+// header", ...lines.slice(1)]);
    const placed = placeComment(after, anchor);
    expect(placed).toMatchObject({
      kind: "placed",
      start: lastRowOf(after, "  return 2;"),
    });
  });

  it("is outdated when repeated lines have nothing around them to tell them apart", () => {
    const rows = rowsOf(["@@ -1,2 +1,4 @@", "+}", " a();", "+}", " b();"]);
    const anchor: CommentAnchor = {
      lines: quoteRows(rows, rowOf(rows, "}"), rowOf(rows, "}")),
    };
    expect(placeComment(rows, anchor)).toEqual({ kind: "outdated" });
  });

  it("finds a whitespace change whichever way whitespace is shown", () => {
    // A real change beside it keeps the hunk when whitespace is hidden.
    const lines = [
      "@@ -1,3 +1,3 @@",
      "-open();",
      "+Open();",
      "-\tlayout();",
      "+  layout();",
      " close();",
    ];
    const shown = rowsOf(lines);
    const hidden = rowsOf(lines, true);
    // Written on the added line with whitespace shown, read with it hidden.
    expect(placeComment(hidden, anchorOn(shown, "  layout();"))).toMatchObject({
      kind: "placed",
      start: rowOf(hidden, "  layout();"),
    });
    // Written on the pair with whitespace hidden, read with it shown.
    expect(placeComment(shown, anchorOn(hidden, "  layout();"))).toMatchObject({
      kind: "placed",
      start: rowOf(shown, "  layout();"),
    });
  });
});

describe("anchorRows", () => {
  it("keeps at most 200 quoted lines and the whole range's span", () => {
    const body = Array.from({ length: 250 }, (_, index) => `+line ${index};`);
    const rows = rowsOf([`@@ -0,0 +1,250 @@`, ...body]);
    const anchor = anchorRows(rows, 1, 250);
    expect(anchor.lines).toHaveLength(200);
    expect(anchor.unquoted).toBe(50);
    expect(anchor.span).toEqual({ lines: "1-250", oldLines: null });
    const placed = placeComment(rows, anchor);
    expect(placed).toMatchObject({
      kind: "placed",
      start: 1,
      end: 250,
      span: { lines: "1-250" },
    });
  });
});

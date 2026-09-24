import { describe, expect, it } from "vitest";

import { groupUnifiedDiff } from "../unifiedDiff";
import {
  alignSequences,
  buildDiffFileModel,
  diffRows,
  ignoreWhitespaceChanges,
  splitRows,
  withWordEmphasis,
  wordDiff,
  type DiffRow,
} from "./diffModel";

function rowsOf(diff: string): DiffRow[] {
  return diffRows(groupUnifiedDiff(diff)[0]!);
}

/** The text a set of ranges covers, for reading assertions at a glance. */
function covered(
  text: string,
  ranges: readonly { start: number; end: number }[],
) {
  return ranges.map((range) => text.slice(range.start, range.end));
}

describe("wordDiff", () => {
  it("marks only the token that changed", () => {
    const before = "const MAX_QUEUED = 10;";
    const after = "const MAX_QUEUED = 20;";
    const diff = wordDiff(before, after)!;
    expect(covered(before, diff.before)).toEqual(["10"]);
    expect(covered(after, diff.after)).toEqual(["20"]);
  });

  it("marks separate edits separately, and an insertion on one side only", () => {
    const before = "send(queue, text, now())";
    const after = "send(queue, message, later(), true)";
    const diff = wordDiff(before, after)!;
    expect(covered(before, diff.before)).toEqual(["text", "now"]);
    expect(covered(after, diff.after)).toEqual(["message", "later", ", true"]);
  });

  it("bridges the space between two changed words into one edit", () => {
    const before = "return old value;";
    const after = "return new thing;";
    const diff = wordDiff(before, after)!;
    expect(covered(after, diff.after)).toEqual(["new thing"]);
  });

  it("marks changed indentation when the words are the same", () => {
    const diff = wordDiff("  layout();", "    layout();")!;
    expect(diff.before).toEqual([{ start: 0, end: 2 }]);
    expect(diff.after).toEqual([{ start: 0, end: 4 }]);
  });

  it("leaves lines too different to compare with their tint alone", () => {
    expect(wordDiff("return a;", "throw new Error(b);")).toBeNull();
    expect(wordDiff("same", "same")).toBeNull();
    expect(wordDiff("x".repeat(1_001), "y".repeat(1_001))).toBeNull();
  });
});

describe("withWordEmphasis", () => {
  it("pairs each removed line with the added line in the same place", () => {
    const rows = withWordEmphasis(
      rowsOf(
        [
          "diff --git a/a.ts b/a.ts",
          "@@ -1,3 +1,3 @@",
          "-const a = 1;",
          "-const b = 2;",
          "+const a = 10;",
          "+const b = 20;",
          " done();",
        ].join("\n"),
      ),
    );
    const removedA = rows.find((row) => row.text === "const a = 1;")!;
    const addedB = rows.find((row) => row.text === "const b = 20;")!;
    expect(covered(removedA.text, removedA.emphasis!)).toEqual(["1"]);
    expect(covered(addedB.text, addedB.emphasis!)).toEqual(["20"]);
    expect(
      rows.find((row) => row.kind === "context")?.emphasis,
    ).toBeUndefined();
  });
});

describe("ignoreWhitespaceChanges", () => {
  const REINDENT = [
    "diff --git a/layout.tsx b/layout.tsx",
    "@@ -20,5 +20,7 @@ export function Layout() {",
    "   const [open, setOpen] = useState(true);",
    "-  useEffect(() => {",
    "-    listen();",
    "-  }, []);",
    "+  if (inspector) {",
    "+    useEffect(() => {",
    "+      listen();",
    "+    }, []);",
    "+  }",
    "   return <Panel />;",
    "@@ -61,3 +63,3 @@",
    " function onResize() {",
    "-\tlayoutPanels();",
    "+  layoutPanels();",
    " }",
  ].join("\n");

  it("turns lines that only moved in or out into context, as git diff -w does", () => {
    const rows = ignoreWhitespaceChanges(rowsOf(REINDENT));
    const changed = rows.filter(
      (row) => row.kind === "add" || row.kind === "del",
    );
    expect(changed.map((row) => row.text)).toEqual([
      "  if (inspector) {",
      "  }",
    ]);
    const moved = rows.find((row) => row.text === "    useEffect(() => {")!;
    expect(moved).toMatchObject({
      kind: "context",
      oldNo: 21,
      newNo: 22,
      old: { text: "  useEffect(() => {" },
    });
  });

  it("drops a hunk whose only changes were whitespace", () => {
    const rows = ignoreWhitespaceChanges(rowsOf(REINDENT));
    expect(rows.filter((row) => row.kind === "hunk")).toHaveLength(1);
    expect(rows.some((row) => row.text.includes("layoutPanels"))).toBe(false);
  });

  it("says when hiding whitespace left nothing to show", () => {
    const group = groupUnifiedDiff(
      [
        "diff --git a/a.ts b/a.ts",
        "@@ -1,1 +1,1 @@",
        "-\tlayoutPanels();",
        "+  layoutPanels();",
      ].join("\n"),
    )[0]!;
    expect(
      buildDiffFileModel(group, { ignoreWhitespace: true }).onlyWhitespace,
    ).toBe(true);
    expect(
      buildDiffFileModel(group, { ignoreWhitespace: false }).onlyWhitespace,
    ).toBe(false);
  });

  it("treats only spaces, tabs, and carriage returns as whitespace, as git does", () => {
    const rows = ignoreWhitespaceChanges(
      rowsOf(
        [
          "diff --git a/a.ts b/a.ts",
          "@@ -1,4 +1,4 @@",
          "-const\u00a0a = 1;",
          "-\ufeffconst b = 2;",
          "-const c = 3;\r",
          "-\tconst d = 4;",
          "+const a = 1;",
          "+const b = 2;",
          "+const c = 3;",
          "+  const d = 4;",
        ].join("\n"),
      ),
    );
    // A no-break space and a byte-order mark are content to `git diff -w`.
    expect(
      rows
        .filter((row) => row.kind === "add" || row.kind === "del")
        .map((row) => row.text),
    ).toEqual([
      "const\u00a0a = 1;",
      "\ufeffconst b = 2;",
      "const a = 1;",
      "const b = 2;",
    ]);
    expect(
      rows.filter((row) => row.kind === "context").map((row) => row.text),
    ).toEqual(["const c = 3;", "  const d = 4;"]);
  });

  it("hides a change that only adds a final newline, as git diff -w does", () => {
    const group = groupUnifiedDiff(
      [
        "diff --git a/a.ts b/a.ts",
        "@@ -1,2 +1,2 @@",
        " first();",
        "-last();",
        "\\ No newline at end of file",
        "+last();",
      ].join("\n"),
    )[0]!;
    const model = buildDiffFileModel(group, { ignoreWhitespace: true });
    expect(model.onlyWhitespace).toBe(true);
    expect(model.rows).toEqual([]);
  });

  it("keeps a real change beside a final newline, and drops the newline's note", () => {
    const rows = ignoreWhitespaceChanges(
      rowsOf(
        [
          "diff --git a/a.ts b/a.ts",
          "@@ -1,3 +1,3 @@",
          "-first();",
          "+First();",
          " middle();",
          "-last();",
          "\\ No newline at end of file",
          "+last();",
        ].join("\n"),
      ),
    );
    expect(rows.map((row) => [row.kind, row.text])).toEqual([
      ["hunk", "@@ -1,3 +1,3 @@"],
      ["del", "first();"],
      ["add", "First();"],
      ["context", "middle();"],
      ["context", "last();"],
    ]);
    // The pair keeps its old line even though the text is the same, so the
    // old side of the view draws it from the old side's syntax.
    expect(rows.at(-1)).toMatchObject({
      oldNo: 3,
      newNo: 3,
      old: { text: "last();", source: 4 },
      source: 6,
    });
  });

  it("counts the lines a hunk hides because only their whitespace changed", () => {
    const group = groupUnifiedDiff(REINDENT)[0]!;
    expect([
      ...buildDiffFileModel(group, { ignoreWhitespace: true }).hiddenWhitespace,
    ]).toEqual([[0, 3]]);
    expect(
      buildDiffFileModel(group, { ignoreWhitespace: false }).hiddenWhitespace
        .size,
    ).toBe(0);
  });
});

describe("splitRows", () => {
  it("pairs removed with added lines in order and leaves a blank where a side runs out", () => {
    const rows = rowsOf(
      [
        "diff --git a/a.ts b/a.ts",
        "@@ -1,4 +1,5 @@",
        " keep();",
        "-one();",
        "-two();",
        "+uno();",
        "+dos();",
        "+tres();",
        " end();",
      ].join("\n"),
    );
    const text = (index: number | null) =>
      index === null ? null : rows[index]!.text;
    const split = splitRows(rows).map((entry) =>
      entry.kind === "full"
        ? rows[entry.row]!.kind
        : [text(entry.left), text(entry.right)],
    );
    expect(split).toEqual([
      "hunk",
      ["keep();", "keep();"],
      ["one();", "uno();"],
      ["two();", "dos();"],
      [null, "tres();"],
      ["end();", "end();"],
    ]);
  });
});

describe("alignSequences", () => {
  it("matches common ends in linear time and the middle by longest common run", () => {
    expect(alignSequences(["a", "b", "c", "d"], ["a", "x", "c", "d"])).toEqual([
      [0, 0],
      [2, 2],
      [3, 3],
    ]);
    expect(alignSequences(["p", "q", "r"], ["q", "r", "s"])).toEqual([
      [1, 0],
      [2, 1],
    ]);
  });

  it("gives up on a middle past its cap rather than go quadratic", () => {
    const before = Array.from({ length: 600 }, (_, index) => `a${index}`);
    const after = Array.from({ length: 600 }, (_, index) => `b${index}`);
    expect(alignSequences(before, after, 1_000)).toEqual([]);
  });
});

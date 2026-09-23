import { describe, expect, it } from "vitest";

import {
  diffHunks,
  groupUnifiedDiff,
  parseDiffGitLine,
  unquoteCStyle,
} from "./unifiedDiff";

describe("groupUnifiedDiff", () => {
  it("keeps a deleted SQL comment that git prints as --- keep", () => {
    const [group] = groupUnifiedDiff(`diff --git a/q.sql b/q.sql
--- a/q.sql
+++ b/q.sql
@@ -1,2 +1,1 @@
--- keep
 SELECT 1
`);
    const kinds = group?.lines
      .filter((line) => line.kind !== "meta")
      .map((line) => [line.kind, line.text]);
    expect(kinds).toEqual([
      ["hunk", "@@ -1,2 +1,1 @@"],
      ["del", "--- keep"],
      ["context", " SELECT 1"],
    ]);
    expect(group?.lines.find((line) => line.kind === "del")?.oldNo).toBe(1);
    expect(group?.lines.find((line) => line.kind === "context")?.oldNo).toBe(2);
  });

  it("keeps a deleted YAML document marker printed as ----", () => {
    const [group] = groupUnifiedDiff(`diff --git a/a.yaml b/a.yaml
--- a/a.yaml
+++ b/a.yaml
@@ -1,2 +1,1 @@
----
 body
`);
    expect(
      group?.lines
        .filter((line) => line.kind !== "meta")
        .map((line) => line.kind),
    ).toEqual(["hunk", "del", "context"]);
    expect(group?.lines.find((line) => line.kind === "del")?.text).toBe("----");
  });

  it("classifies ++x and +++ as added lines inside a hunk", () => {
    const [group] = groupUnifiedDiff(`diff --git a/a.md b/a.md
--- a/a.md
+++ b/a.md
@@ -1,1 +1,3 @@
 keep
++x
+++
`);
    expect(
      group?.lines
        .filter((line) => line.kind === "add")
        .map((line) => line.text),
    ).toEqual(["++x", "+++"]);
  });

  it("treats the no-newline marker as meta without shifting line numbers", () => {
    const [group] = groupUnifiedDiff(`diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
`);
    const body = group?.lines.filter(
      (line) => line.kind !== "meta" || line.text.startsWith("\\"),
    );
    expect(body?.map((line) => line.kind)).toEqual([
      "hunk",
      "del",
      "meta",
      "add",
      "meta",
    ]);
    expect(body?.[1]).toEqual({
      kind: "del",
      oldNo: 1,
      newNo: null,
      text: "-old",
    });
    expect(body?.[3]).toEqual({
      kind: "add",
      oldNo: null,
      newNo: 1,
      text: "+new",
    });
  });

  it("parses quoted non-ASCII paths on the diff --git line", () => {
    const quoted = '"a/caf\\303\\251.txt"';
    const groups = groupUnifiedDiff(
      `diff --git ${quoted} ${quoted.replace("a/", "b/")}\n+ok\n`,
    );
    expect(groups[0]?.path).toBe("café.txt");
  });
});

describe("parseDiffGitLine", () => {
  it("accepts paths without a/ b/ prefixes", () => {
    expect(parseDiffGitLine("diff --git f.sql f.sql")).toBe("f.sql");
  });
});

describe("unquoteCStyle", () => {
  it("decodes octal bytes", () => {
    expect(unquoteCStyle('"caf\\303\\251"')?.value).toBe("café");
  });
});

describe("diffHunks", () => {
  const header = ["diff --git a/a.txt b/a.txt", "--- a/a.txt", "+++ b/a.txt"];

  it("splits a file at its hunk headers, keeping the no-newline marker", () => {
    const [group] = groupUnifiedDiff(
      [
        ...header,
        "@@ -1,2 +1,2 @@",
        " one",
        "-two",
        "+2",
        "@@ -9 +9 @@",
        "-nine",
        "+9",
        "\\ No newline at end of file",
        "",
      ].join("\n"),
    );
    const hunks = group ? diffHunks(group) : [];
    expect(hunks.map((hunk) => [hunk.index, hunk.complete])).toEqual([
      [0, true],
      [1, true],
    ]);
    expect(hunks[1]?.text).toBe(
      "@@ -9 +9 @@\n-nine\n+9\n\\ No newline at end of file",
    );
    expect(hunks[1]).toMatchObject({ newStart: 9, newCount: 1 });
  });

  it("marks a hunk the diff ends inside as incomplete", () => {
    const [group] = groupUnifiedDiff(
      [...header, "@@ -1,4 +1,4 @@", " one", "-two", ""].join("\n"),
    );
    expect(group ? diffHunks(group)[0]?.complete : null).toBe(false);
  });
});

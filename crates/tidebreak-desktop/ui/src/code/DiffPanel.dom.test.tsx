// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  DIFF_COLLAPSE_LINE_THRESHOLD,
  DiffPanel,
  groupUnifiedDiff,
} from "./DiffPanel";

afterEach(() => {
  cleanup();
});

const DIFF = `diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-old
+new
`;

describe("DiffPanel", () => {
  it("renders gutters, a grouped unified diff, and a truncation notice", async () => {
    const client = {
      getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
        diff: DIFF,
        truncated: true,
        stat: { files: 1, insertions: 1, deletions: 1, truncated: true },
        turn_id: "turn-1",
        file: "src/lib.rs",
      }),
    };
    render(
      <DiffPanel
        client={client}
        workspaceId="ws-1"
        turnId="turn-1"
        turnLabel="Turn 4"
        file="src/lib.rs"
      />,
    );
    expect(
      await screen.findByRole("heading", { name: "src/lib.rs" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("Turn 4")).not.toBeInTheDocument();
    expect(screen.getByText("+new")).toBeInTheDocument();

    const added = screen.getByText("+new").parentElement;
    expect(added?.querySelector('[data-diff-gutter="old"]')?.textContent).toBe(
      "",
    );
    expect(added?.querySelector('[data-diff-gutter="new"]')?.textContent).toBe(
      "1",
    );
    expect(added?.querySelector('[data-diff-gutter="new"]')).toHaveClass(
      "select-none",
    );

    const removed = screen.getByText("-old").parentElement;
    expect(removed?.querySelector('[data-diff-gutter="old"]')?.textContent).toBe(
      "1",
    );
    expect(removed?.querySelector('[data-diff-gutter="new"]')?.textContent).toBe(
      "",
    );

    expect(
      screen.getByText("This diff was truncated. Open a single file for the rest."),
    ).toBeInTheDocument();
    expect(client.getCodeWorkspaceDiff).toHaveBeenCalledWith("ws-1", {
      turn: "turn-1",
      file: "src/lib.rs",
    });
  });

  it("collapses a long file behind Show diff", async () => {
    const body = Array.from(
      { length: DIFF_COLLAPSE_LINE_THRESHOLD + 1 },
      (_, index) => `+line ${index}`,
    ).join("\n");
    const client = {
      getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
        diff: `diff --git a/big.ts b/big.ts\n--- a/big.ts\n+++ b/big.ts\n@@ -1,1 +1,401 @@\n${body}\n`,
        truncated: false,
        stat: { files: 1, insertions: 401, deletions: 0, truncated: false },
      }),
    };
    render(<DiffPanel client={client} workspaceId="ws-1" />);
    expect(await screen.findByText("big.ts")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show diff" })).toBeInTheDocument();
    expect(screen.queryByText("+line 0")).not.toBeInTheDocument();

    await userEvent.setup().click(screen.getByRole("button", { name: "Show diff" }));
    expect(screen.getByText("+line 0")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Show diff" }),
    ).not.toBeInTheDocument();
  });
});

describe("groupUnifiedDiff", () => {
  it("splits a multi-file unified diff by path", () => {
    const groups = groupUnifiedDiff(
      "diff --git a/a.txt b/a.txt\n+one\ndiff --git a/b.txt b/b.txt\n+two\n",
    );
    expect(groups.map((group) => group.path)).toEqual(["a.txt", "b.txt"]);
  });

  it("assigns old and new line numbers from a hunk header", () => {
    const [group] = groupUnifiedDiff(`diff --git a/a.ts b/a.ts
--- a/a.ts
+++ b/a.ts
@@ -10,3 +10,4 @@
 context
-removed
+added
+also
 context
`);
    expect(group?.lines.filter((line) => line.kind !== "meta")).toEqual([
      { kind: "hunk", oldNo: null, newNo: null, text: "@@ -10,3 +10,4 @@" },
      { kind: "context", oldNo: 10, newNo: 10, text: " context" },
      { kind: "del", oldNo: 11, newNo: null, text: "-removed" },
      { kind: "add", oldNo: null, newNo: 11, text: "+added" },
      { kind: "add", oldNo: null, newNo: 12, text: "+also" },
      { kind: "context", oldNo: 12, newNo: 13, text: " context" },
    ]);
  });

  it("treats rename and file-mode lines as meta without line numbers", () => {
    const [group] = groupUnifiedDiff(`diff --git a/old.txt b/new.txt
similarity index 90%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1 +1 @@
-old
+new
`);
    expect(group?.path).toBe("new.txt");
    const meta = group?.lines.filter((line) => line.kind === "meta") ?? [];
    expect(meta.map((line) => line.text)).toEqual([
      "similarity index 90%",
      "rename from old.txt",
      "rename to new.txt",
      "--- a/old.txt",
      "+++ b/new.txt",
    ]);
    expect(meta.every((line) => line.oldNo === null && line.newNo === null)).toBe(
      true,
    );
    expect(group?.lines.find((line) => line.kind === "del")).toEqual({
      kind: "del",
      oldNo: 1,
      newNo: null,
      text: "-old",
    });
    expect(group?.lines.find((line) => line.kind === "add")).toEqual({
      kind: "add",
      oldNo: null,
      newNo: 1,
      text: "+new",
    });
  });
});

// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
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
    expect(await screen.findByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.queryByText("Turn 4")).not.toBeInTheDocument();
    expect(screen.getByText("+new")).toBeInTheDocument();
    expect(screen.queryByText("--- a/src/lib.rs")).not.toBeInTheDocument();
    expect(screen.queryByText("+++ b/src/lib.rs")).not.toBeInTheDocument();

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
    expect(added).toHaveClass("bg-success-background/55");

    const removed = screen.getByText("-old").parentElement;
    expect(
      removed?.querySelector('[data-diff-gutter="old"]')?.textContent,
    ).toBe("1");
    expect(
      removed?.querySelector('[data-diff-gutter="new"]')?.textContent,
    ).toBe("");
    expect(removed).toHaveClass("bg-critical-background/55");

    expect(
      screen.getByText(
        "This diff was truncated. Open a single file for the rest.",
      ),
    ).toBeInTheDocument();
    const summary = screen.getByLabelText(
      "1 file, 1 addition, 1 deletion, truncated",
    );
    expect(within(summary).getByText("· truncated")).toHaveClass(
      "text-warning-foreground",
    );
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
    // The expander names the file it belongs to: a screen reader listing a
    // multi-file diff's controls would otherwise get a column of "Show diff".
    expect(
      screen.getByRole("button", { name: "Show diff for big.ts" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("+line 0")).not.toBeInTheDocument();

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Show diff for big.ts" }));
    expect(screen.getByText("+line 0")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /^Show diff/ }),
    ).not.toBeInTheDocument();
  });

  it("keeps the file disclosure and its Open control side by side", async () => {
    const client = {
      getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
        diff: DIFF,
        truncated: false,
        stat: { files: 1, insertions: 1, deletions: 1, truncated: false },
      }),
    };
    const onOpenFile = vi.fn();
    render(
      <DiffPanel client={client} workspaceId="ws-1" onOpenFile={onOpenFile} />,
    );

    // A control nested inside another control is reachable by neither.
    const disclosure = await screen.findByRole("button", {
      name: "src/lib.rs",
    });
    const open = screen.getByRole("button", { name: "Open src/lib.rs" });
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
    expect(disclosure.contains(open)).toBe(false);

    await userEvent.setup().click(open);
    expect(onOpenFile).toHaveBeenCalledWith("src/lib.rs");
    // Opening the file must not also collapse the section under the reader.
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
  });

  it("says which scope came back empty", async () => {
    const client = {
      getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
        diff: "",
        truncated: false,
        stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
      }),
    };
    const { rerender } = render(
      <DiffPanel
        client={client}
        workspaceId="ws-1"
        turnId="turn-1"
        turnLabel="Turn 4"
      />,
    );
    expect(
      await screen.findByText("Turn 4 changed no files."),
    ).toBeInTheDocument();

    rerender(<DiffPanel client={client} workspaceId="ws-1" />);
    expect(
      await screen.findByText("The worktree matches its base branch."),
    ).toBeInTheDocument();
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
    expect(
      meta.every((line) => line.oldNo === null && line.newNo === null),
    ).toBe(true);
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

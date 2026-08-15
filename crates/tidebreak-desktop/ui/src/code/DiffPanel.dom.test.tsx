// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DiffPanel, groupUnifiedDiff } from "./DiffPanel";

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
  it("renders a grouped unified diff and a truncation notice", async () => {
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
        file="src/lib.rs"
      />,
    );
    expect(await screen.findByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByText("+new")).toBeInTheDocument();
    expect(
      screen.getByText("This diff was truncated. Open a single file for the rest."),
    ).toBeInTheDocument();
    expect(client.getCodeWorkspaceDiff).toHaveBeenCalledWith("ws-1", {
      turn: "turn-1",
      file: "src/lib.rs",
    });
  });
});

describe("groupUnifiedDiff", () => {
  it("splits a multi-file unified diff by path", () => {
    const groups = groupUnifiedDiff(
      "diff --git a/a.txt b/a.txt\n+one\ndiff --git a/b.txt b/b.txt\n+two\n",
    );
    expect(groups.map((group) => group.path)).toEqual(["a.txt", "b.txt"]);
  });
});

// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DiffOverview } from "./DiffOverview";

afterEach(() => {
  cleanup();
});

describe("DiffOverview", () => {
  it("lists file states without fetching or rendering patch contents", async () => {
    const client = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [
          {
            path: "src/new.ts",
            kind: "added",
            insertions: 8,
            deletions: 0,
          },
          {
            path: "src/old-name.ts",
            previous_path: "src/older-name.ts",
            kind: "renamed",
            insertions: 1,
            deletions: 1,
          },
        ],
        truncated: false,
        stat: { files: 2, insertions: 9, deletions: 1, truncated: false },
      }),
    };
    const onOpenFile = vi.fn();

    render(
      <DiffOverview
        client={client}
        workspaceId="ws-1"
        turnId="turn-1"
        turnLabel="Turn 4"
        onOpenFile={onOpenFile}
      />,
    );

    expect(await screen.findByText("new.ts")).toBeInTheDocument();
    expect(screen.getByText("old-name.ts")).toBeInTheDocument();
    expect(screen.getByText("src")).toBeInTheDocument();
    expect(screen.queryByText("src/new.ts")).not.toBeInTheDocument();
    expect(screen.getByText("Turn 4")).toBeInTheDocument();
    expect(screen.queryByText(/^@@/)).not.toBeInTheDocument();
    expect(client.listCodeWorkspaceFiles).toHaveBeenCalledWith(
      "ws-1",
      "turn-1",
    );

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: /Added src\/new\.ts/ }));
    expect(onOpenFile).toHaveBeenCalledWith("src/new.ts");
  });

  it("compacts singleton folder paths and collapses a hierarchy", async () => {
    const client = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [
          {
            path: "crates/tidebreak-desktop/ui/src/code/CodeInspector.tsx",
            kind: "modified",
            insertions: 12,
            deletions: 4,
          },
          {
            path: "crates/tidebreak-desktop/ui/src/stories/CodeInspector.stories.tsx",
            kind: "added",
            insertions: 30,
            deletions: 0,
          },
        ],
        truncated: false,
        stat: { files: 2, insertions: 42, deletions: 4, truncated: false },
      }),
    };
    render(
      <DiffOverview client={client} workspaceId="ws-1" onOpenFile={vi.fn()} />,
    );

    const root = await screen.findByRole("button", {
      name: "Collapse crates/tidebreak-desktop/ui/src",
    });
    expect(
      screen.getByText("crates/tidebreak-desktop/ui/src"),
    ).toBeInTheDocument();
    expect(screen.getByText("CodeInspector.tsx")).toBeInTheDocument();
    await userEvent.setup().click(root);
    expect(screen.queryByText("CodeInspector.tsx")).not.toBeInTheDocument();
  });

  it("marks the selected changed file and explains an empty workspace", async () => {
    const changed = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [
          {
            path: "src/lib.rs",
            kind: "modified",
            insertions: 1,
            deletions: 2,
          },
        ],
        truncated: false,
        stat: { files: 1, insertions: 1, deletions: 2, truncated: false },
      }),
    };
    const { rerender } = render(
      <DiffOverview
        client={changed}
        workspaceId="ws-1"
        selected="src/lib.rs"
        onOpenFile={vi.fn()}
      />,
    );

    expect(
      await screen.findByRole("button", { name: /Modified src\/lib\.rs/ }),
    ).toHaveAttribute("aria-current", "page");

    const clean = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [],
        truncated: false,
        stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
      }),
    };
    rerender(
      <DiffOverview client={clean} workspaceId="ws-2" onOpenFile={vi.fn()} />,
    );
    expect(
      await screen.findByText("The worktree matches its base branch."),
    ).toBeInTheDocument();
  });
});

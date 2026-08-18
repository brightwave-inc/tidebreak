// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FilesPanel } from "./FilesPanel";

afterEach(() => {
  cleanup();
});

describe("FilesPanel", () => {
  it("lists changed files and opens a file in the diff panel", async () => {
    const onOpenFile = vi.fn();
    const client = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [
          {
            path: "src/lib.rs",
            kind: "modified",
            insertions: 3,
            deletions: 1,
          },
          {
            path: "new.txt",
            kind: "added",
            insertions: 1,
            deletions: 0,
          },
        ],
        truncated: false,
        stat: { files: 2, insertions: 4, deletions: 1, truncated: false },
      }),
    };
    render(
      <FilesPanel
        client={client}
        workspaceId="ws-1"
        turnId="turn-1"
        onOpenFile={onOpenFile}
      />,
    );
    expect(await screen.findByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByText("new.txt")).toBeInTheDocument();
    expect(screen.getByLabelText("modified")).toHaveTextContent("M");
    expect(screen.getByLabelText("added")).toHaveTextContent("A");
    expect(screen.getByText("+3")).toHaveClass("text-success");
    expect(screen.getByText("−1")).toHaveClass("text-critical");
    expect(screen.getByText("+1")).toHaveClass("text-success");

    const selected = screen.getByRole("button", { name: /src\/lib\.rs/ });
    expect(selected).not.toHaveAttribute("aria-current");
    await userEvent.setup().click(screen.getByText("src/lib.rs"));
    expect(onOpenFile).toHaveBeenCalledWith("src/lib.rs");
    expect(client.listCodeWorkspaceFiles).toHaveBeenCalledWith("ws-1", "turn-1");
  });

  it("marks the selected row with aria-current", async () => {
    const client = {
      listCodeWorkspaceFiles: vi.fn().mockResolvedValue({
        files: [
          {
            path: "src/lib.rs",
            kind: "modified",
            insertions: 3,
            deletions: 1,
          },
        ],
        truncated: false,
        stat: { files: 1, insertions: 3, deletions: 1, truncated: false },
      }),
    };
    render(
      <FilesPanel
        client={client}
        workspaceId="ws-1"
        selected="src/lib.rs"
        onOpenFile={vi.fn()}
      />,
    );
    expect(await screen.findByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /src\/lib\.rs/ })).toHaveAttribute(
      "aria-current",
      "true",
    );
  });
});

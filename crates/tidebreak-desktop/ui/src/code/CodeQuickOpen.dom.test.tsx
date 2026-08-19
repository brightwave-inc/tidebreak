// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeQuickOpen, rankQuickOpenPaths } from "./CodeQuickOpen";

afterEach(cleanup);

describe("CodeQuickOpen", () => {
  it("ranks fuzzy filename matches ahead of looser matches", () => {
    expect(
      rankQuickOpenPaths(
        ["src/CodeWorkspacePage.tsx", "src/workspace.ts", "docs/code.md"],
        "cwp",
      ),
    ).toEqual(["src/CodeWorkspacePage.tsx"]);
    expect(
      rankQuickOpenPaths(["src/index.ts", "tests/index.ts", "src/lib.ts"], "index"),
    ).toEqual(["src/index.ts", "tests/index.ts"]);
  });

  it("opens in the center on Cmd+P and supports arrow-and-enter selection", async () => {
    const onOpenFile = vi.fn();
    const client = {
      listCodeWorkspaceTree: vi.fn(async () => ({
        paths: ["README.md", "src/main.rs", "src/model.rs"],
        truncated: false,
      })),
    };
    render(
      <CodeQuickOpen
        client={client}
        workspaceId="ws-1"
        contentRevision={0}
        onOpenFile={onOpenFile}
      />,
    );

    fireEvent.keyDown(window, { key: "p", metaKey: true });
    const input = await screen.findByRole("textbox", { name: "Search files by name" });
    expect(input).toHaveFocus();
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledWith("ws-1", {
      limit: 5000,
    });

    await userEvent.setup().type(input, "m");
    await waitFor(() => expect(screen.getByRole("button", { name: "src/main.rs" })).toBeInTheDocument());
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onOpenFile).toHaveBeenCalledWith("src/model.rs");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("opens when the visible new-tab control increments its request", async () => {
    const client = {
      listCodeWorkspaceTree: vi.fn(async () => ({
        paths: ["src/main.rs"],
        truncated: false,
      })),
    };
    const { rerender } = render(
      <CodeQuickOpen
        client={client}
        workspaceId="ws-1"
        contentRevision={0}
        onOpenFile={vi.fn()}
        openRequest={0}
      />,
    );

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    rerender(
      <CodeQuickOpen
        client={client}
        workspaceId="ws-1"
        contentRevision={0}
        onOpenFile={vi.fn()}
        openRequest={1}
      />,
    );

    expect(
      await screen.findByRole("textbox", { name: "Search files by name" }),
    ).toHaveFocus();
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledTimes(1);
  });
});

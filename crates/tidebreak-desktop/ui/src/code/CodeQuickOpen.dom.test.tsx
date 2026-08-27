// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeQuickOpen, rankQuickOpenPaths } from "./CodeQuickOpen";

afterEach(cleanup);

type QuickOpenProps = ComponentProps<typeof CodeQuickOpen>;

/**
 * Render the picker with a request counter the test can bump.
 *
 * The chords that reach it live in the shell keymap, so the component's only
 * entry point is this counter — the one Open file… increments and the
 * one the keymap raises. `open()` stands in for either.
 */
function mountQuickOpen(props: Omit<QuickOpenProps, "openRequest">) {
  let current = props;
  let openRequest = 0;
  const { rerender } = render(
    <CodeQuickOpen {...current} openRequest={openRequest} />,
  );
  const draw = () =>
    rerender(<CodeQuickOpen {...current} openRequest={openRequest} />);
  return {
    open: () => {
      openRequest += 1;
      draw();
    },
    /** Change what the picker is looking at, without reopening it. */
    update: (next: Partial<QuickOpenProps>) => {
      current = { ...current, ...next };
      draw();
    },
  };
}

describe("CodeQuickOpen", () => {
  it("ranks fuzzy filename matches ahead of looser matches", () => {
    expect(
      rankQuickOpenPaths(
        ["src/CodeWorkspacePage.tsx", "src/workspace.ts", "docs/code.md"],
        "cwp",
      ),
    ).toEqual(["src/CodeWorkspacePage.tsx"]);
    expect(
      rankQuickOpenPaths(
        ["src/index.ts", "tests/index.ts", "src/lib.ts"],
        "index",
      ),
    ).toEqual(["src/index.ts", "tests/index.ts"]);
  });

  it("opens on request, then picks with the arrows and enter", async () => {
    const onOpenFile = vi.fn();
    const client = {
      listCodeWorkspaceTree: vi.fn(async () => ({
        paths: ["README.md", "src/main.rs", "src/model.rs"],
        truncated: false,
      })),
    };
    const ui = mountQuickOpen({
      client,
      workspaceId: "ws-1",
      contentRevision: 0,
      onOpenFile,
    });

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    ui.open();
    const input = await screen.findByRole("combobox", {
      name: "Search files by name",
    });
    expect(input).toHaveFocus();
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledWith("ws-1", {
      limit: 5000,
    });

    await userEvent.setup().type(input, "m");
    await waitFor(() =>
      expect(
        screen.getByRole("option", { name: "src/main.rs" }),
      ).toBeInTheDocument(),
    );
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onOpenFile).toHaveBeenCalledWith("src/model.rs");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("reuses the file list until the worktree revision changes", async () => {
    const user = userEvent.setup();
    const client = {
      listCodeWorkspaceTree: vi.fn(async () => ({
        paths: ["src/main.rs"],
        truncated: false,
      })),
    };
    const ui = mountQuickOpen({
      client,
      workspaceId: "ws-1",
      contentRevision: 0,
      onOpenFile: vi.fn(),
    });

    ui.open();
    expect(
      await screen.findByRole("option", { name: "src/main.rs" }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");

    ui.open();
    expect(
      await screen.findByRole("option", { name: "src/main.rs" }),
    ).toBeInTheDocument();
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledTimes(1);
    await user.keyboard("{Escape}");

    ui.update({ contentRevision: 1 });
    ui.open();
    await waitFor(() =>
      expect(client.listCodeWorkspaceTree).toHaveBeenCalledTimes(2),
    );
  });

  it("drops filenames when the workspace changes", async () => {
    let resolveSecond:
      | ((value: { paths: string[]; truncated: boolean }) => void)
      | undefined;
    const client = {
      listCodeWorkspaceTree: vi
        .fn()
        .mockResolvedValueOnce({ paths: ["old.ts"], truncated: false })
        .mockImplementationOnce(
          () =>
            new Promise<{ paths: string[]; truncated: boolean }>((resolve) => {
              resolveSecond = resolve;
            }),
        ),
    };
    const ui = mountQuickOpen({
      client,
      workspaceId: "ws-1",
      contentRevision: 0,
      onOpenFile: vi.fn(),
    });

    ui.open();
    expect(
      await screen.findByRole("option", { name: "old.ts" }),
    ).toBeInTheDocument();

    ui.update({ workspaceId: "ws-2" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    ui.open();
    expect(
      screen.queryByRole("option", { name: "old.ts" }),
    ).not.toBeInTheDocument();
    resolveSecond?.({ paths: ["new.ts"], truncated: false });
    expect(
      await screen.findByRole("option", { name: "new.ts" }),
    ).toBeInTheDocument();
  });
});

// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "../../api/client";
import { DiffPanel } from "../DiffPanel";
import { groupUnifiedDiff } from "../unifiedDiff";
import { diffRows } from "./diffModel";
import { useDiffPreferences } from "./diffPreferences";
import { DIFF_CHUNK_ROWS, DiffView } from "./DiffView";
import { usePendingReviewStore } from "./pendingReview";
import { loadLanguage } from "./syntaxHighlight";
import {
  fileDiff,
  longFileDiff,
  QUEUE_DIFF,
  QUEUE_PATH,
} from "../../stories/diffFixtures";

beforeAll(async () => {
  await loadLanguage("typescript");
});

afterEach(() => {
  cleanup();
  usePendingReviewStore.setState({ byWorkspace: {}, sending: {} });
  useDiffPreferences.getState().setLayout("unified");
  window.localStorage.clear();
});

type PanelClient = Pick<
  ApiClient,
  "getCodeWorkspaceDiff" | "listCodeWorkspaceFiles"
>;

function clientFor(diff: string): PanelClient {
  return {
    getCodeWorkspaceDiff: vi.fn(async () => ({
      diff,
      truncated: false,
      stat: { files: 1, insertions: 1, deletions: 1, truncated: false },
    })),
    listCodeWorkspaceFiles: vi.fn(async () => ({
      files: groupUnifiedDiff(diff).map((group) => ({
        path: group.path,
        kind: "modified" as const,
        insertions: 1,
        deletions: 1,
      })),
      truncated: false,
      stat: { files: 1, insertions: 1, deletions: 1, truncated: false },
    })),
  };
}

const SMALL = fileDiff("src/queue.ts", [
  {
    oldStart: 1,
    newStart: 1,
    lines: [
      " const first = 1;",
      "-const limit = 10;",
      "+const limit = 20;",
      "+const extra = 3;",
      " const last = 4;",
    ],
  },
]);

async function renderPanel(
  diff = SMALL,
  props: Partial<Parameters<typeof DiffPanel>[0]> = {},
) {
  render(
    <DiffPanel
      client={clientFor(diff)}
      workspaceId="ws-1"
      file={
        groupUnifiedDiff(diff).length === 1
          ? groupUnifiedDiff(diff)[0]!.path
          : undefined
      }
      {...props}
    />,
  );
  await screen.findAllByRole("button", { name: /^Comment on/ });
}

function comments() {
  return usePendingReviewStore.getState().byWorkspace["ws-1"] ?? [];
}

describe("one diff view for the workspace and the pull request", () => {
  it("draws a pull request's hunks with the same gutters and type scale, without comments", () => {
    const patch = SMALL.split("\n").slice(4).join("\n");
    const group = {
      path: "src/queue.ts",
      lines: groupUnifiedDiff(patch).flatMap((part) => part.lines),
    };
    const { container } = render(
      <DiffView group={group} layout="unified" ignoreWhitespace={false} />,
    );
    const view = container.querySelector("[data-diff-view]")!;
    expect(view).toHaveClass("text-md", "font-mono");
    // Nothing inside takes focus, so the region does, and it has a name.
    expect(
      screen.getByRole("region", { name: "Changes to src/queue.ts" }),
    ).toBe(view);
    const added = container.querySelector('[data-kind="add"]')!;
    expect(added.querySelector('[data-diff-gutter="new"]')?.textContent).toBe(
      "2",
    );
    expect(screen.queryByRole("button", { name: /^Comment on/ })).toBeNull();
  });

  it("draws the workspace diff with the same view, taking comments", async () => {
    await renderPanel();
    const view = document.querySelector("[data-diff-view]")!;
    expect(view).toHaveClass("text-md", "font-mono");
    expect(
      screen.getByRole("button", { name: "Comment on deleted line 2" }),
    ).toBeInTheDocument();
  });
});

describe("line comments", () => {
  it("opens an editor under a clicked line and keeps the comment in the pending review", async () => {
    const user = userEvent.setup();
    await renderPanel();

    await user.click(screen.getByRole("button", { name: "Comment on line 3" }));
    const editor = await screen.findByRole("textbox", {
      name: "Comment on line 3",
    });
    await user.type(editor, "Name this for what it counts.");
    await user.click(screen.getByRole("button", { name: "Add comment" }));

    expect(comments()).toEqual([
      expect.objectContaining({
        path: "src/queue.ts",
        author: { kind: "person" },
        body: "Name this for what it counts.",
        lines: [
          { kind: "add", oldNo: null, newNo: 3, text: "const extra = 3;" },
        ],
      }),
    ]);
    expect(screen.getByText("Goes with your next message")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: /^Comment on/ })).toBeNull();
  });

  it("takes a range dragged across line numbers, within one hunk", async () => {
    await renderPanel();
    const from = screen.getByRole("button", {
      name: "Comment on deleted line 2",
    });
    const to = screen.getByRole("button", { name: "Comment on line 3" });

    fireEvent.pointerDown(from, { button: 0 });
    fireEvent.pointerOver(to);
    act(() => {
      window.dispatchEvent(new Event("pointerup"));
    });

    const editor = await screen.findByRole("textbox", {
      name: "Comment on lines 2–3 and deleted line 2",
    });
    fireEvent.change(editor, { target: { value: "One change, three lines." } });
    fireEvent.keyDown(editor, { key: "Enter", metaKey: true });
    expect(comments()[0]?.lines.map((line) => line.kind)).toEqual([
      "del",
      "add",
      "add",
    ]);
  });

  it("walks line numbers from the keyboard and comments on a selected range", async () => {
    const user = userEvent.setup();
    await renderPanel();
    const first = screen.getByRole("button", { name: "Comment on line 1" });
    // One tab stop for the whole diff, on the first line.
    expect(first).toHaveAttribute("tabindex", "0");
    expect(
      screen.getByRole("button", { name: "Comment on line 3" }),
    ).toHaveAttribute("tabindex", "-1");

    first.focus();
    await user.keyboard("{ArrowDown}");
    expect(
      screen.getByRole("button", { name: "Comment on deleted line 2" }),
    ).toHaveFocus();
    await user.keyboard("{Shift>}{ArrowDown}{/Shift}");
    expect(
      screen.getByRole("button", { name: "Comment on line 2" }),
    ).toHaveFocus();
    await user.keyboard("{Enter}");

    const editor = await screen.findByRole("textbox", {
      name: "Comment on line 2 and deleted line 2",
    });
    expect(editor).toHaveFocus();
    // The range runs from the removed line 2 to the added line 2.
    expect(
      document.querySelectorAll("[data-diff-view] [data-selected]"),
    ).toHaveLength(2);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: /^Comment on/ })).toBeNull();
  });

  it("asks before deleting a comment", async () => {
    const user = userEvent.setup();
    await renderPanel();
    await user.click(screen.getByRole("button", { name: "Comment on line 3" }));
    await user.type(
      await screen.findByRole("textbox", { name: "Comment on line 3" }),
      "Drop this.",
    );
    await user.click(screen.getByRole("button", { name: "Add comment" }));

    await user.click(
      screen.getByRole("button", { name: "Delete the comment on line 3" }),
    );
    const dialog = await screen.findByRole("alertdialog");
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(comments()).toEqual([]));
  });

  it("offers no line comments where the panel cannot send them", async () => {
    render(
      <DiffPanel
        client={clientFor(SMALL)}
        workspaceId="ws-1"
        file="src/queue.ts"
        comments={false}
      />,
    );
    expect(
      await screen.findByRole("region", { name: "Changes to src/queue.ts" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Comment on/ })).toBeNull();
  });
});

describe("split view", () => {
  it("puts each removed line beside the added line it pairs with", async () => {
    useDiffPreferences.getState().setLayout("split");
    await renderPanel();
    await waitFor(() =>
      expect(document.querySelector('[data-diff-view="split"]')).not.toBeNull(),
    );
    const removed = screen.getByRole("button", {
      name: "Comment on deleted line 2",
    });
    const pair = removed
      .closest("[data-display]")!
      .parentElement!.closest(".grid")!;
    const cells = [...pair.children];
    expect(cells.map((cell) => cell.getAttribute("data-kind"))).toEqual([
      "del",
      "add",
    ]);
    expect(cells[1]?.textContent).toContain("const limit = 20;");
    // The unpaired addition sits beside a blank cell.
    const extra = screen.getByRole("button", { name: "Comment on line 3" });
    const row = extra.closest(".grid")!;
    expect(row.children[0]).toHaveAttribute("data-kind", "empty");
  });

  it("remembers the reader's choice", async () => {
    const user = userEvent.setup();
    await renderPanel();
    await user.click(screen.getByRole("button", { name: "Split view" }));
    expect(useDiffPreferences.getState().layout).toBe("split");
    expect(window.localStorage.getItem("tidebreak.code-diff-layout")).toBe(
      "split",
    );
  });
});

describe("keys", () => {
  const TWO_FILES = [
    fileDiff("src/a.ts", [
      { oldStart: 1, newStart: 1, lines: ["-a = 1;", "+a = 2;"] },
    ]),
    fileDiff("src/b.ts", [
      { oldStart: 1, newStart: 1, lines: ["-b = 1;", "+b = 2;"] },
    ]),
  ].join("\n");

  it("moves between files with J and K", async () => {
    const user = userEvent.setup();
    await renderPanel(TWO_FILES);
    const first = screen.getByRole("button", { name: "src/a.ts" });
    const second = screen.getByRole("button", { name: "src/b.ts" });
    first.focus();
    await user.keyboard("j");
    expect(second).toHaveFocus();
    await user.keyboard("k");
    expect(first).toHaveFocus();
    await user.keyboard("]");
    expect(second).toHaveFocus();
  });

  it("steps a one-file diff to the next changed file", async () => {
    const user = userEvent.setup();
    const onStepFile = vi.fn();
    render(
      <DiffPanel
        client={{
          ...clientFor(SMALL),
          listCodeWorkspaceFiles: vi.fn(async () => ({
            files: ["src/z.ts", "src/queue.ts", "src/lib/inner.ts"].map(
              (path) => ({
                path,
                kind: "modified" as const,
                insertions: 1,
                deletions: 1,
              }),
            ),
            truncated: false,
            stat: { files: 3, insertions: 3, deletions: 3, truncated: false },
          })),
        }}
        workspaceId="ws-1"
        file="src/queue.ts"
        onStepFile={onStepFile}
      />,
    );
    (await screen.findByRole("button", { name: "Comment on line 1" })).focus();
    await user.keyboard("j");
    // Changes lists folders first: src/lib/inner.ts, then src/queue.ts, then src/z.ts.
    await waitFor(() => expect(onStepFile).toHaveBeenCalledWith("src/z.ts"));
    await user.keyboard("k");
    await waitFor(() =>
      expect(onStepFile).toHaveBeenLastCalledWith("src/lib/inner.ts"),
    );
  });

  it("hides whitespace changes with W, and leaves typing alone", async () => {
    const user = userEvent.setup();
    await renderPanel();
    const toggle = screen.getByRole("button", {
      name: "Hide whitespace changes",
    });
    screen.getByRole("button", { name: "Comment on line 1" }).focus();
    await user.keyboard("w");
    expect(toggle).toHaveAttribute("aria-pressed", "true");

    await user.click(screen.getByRole("button", { name: "Comment on line 3" }));
    const editor = await screen.findByRole("textbox", {
      name: "Comment on line 3",
    });
    await user.type(editor, "wjk");
    expect(editor).toHaveValue("wjk");
    expect(toggle).toHaveAttribute("aria-pressed", "true");
  });
});

describe("a long diff", () => {
  // The browser story `Code/Diff review/Very long file` times the whole
  // 5,000 lines; jsdom is too slow to time anything, so this pins the shape
  // that keeps each frame short: one chunk first, then a bounded step.
  it("draws its first chunk at once and the rest a few chunks a frame", async () => {
    const group = groupUnifiedDiff(longFileDiff(5_000))[0]!;
    const { container } = render(
      <DiffView group={group} layout="unified" ignoreWhitespace={false} />,
    );
    const rows = () => container.querySelectorAll("[data-row]").length;
    expect(rows()).toBe(DIFF_CHUNK_ROWS);
    await waitFor(() => expect(rows()).toBeGreaterThan(DIFF_CHUNK_ROWS));
    expect(rows()).toBeLessThanOrEqual(DIFF_CHUNK_ROWS * 3);
  });

  it("keeps every row through a refresh while an agent works", async () => {
    const diff = longFileDiff(600);
    const { container, rerender } = render(
      <DiffView
        group={groupUnifiedDiff(diff)[0]!}
        layout="unified"
        ignoreWhitespace={false}
      />,
    );
    const rows = () => container.querySelectorAll("[data-row]").length;
    const total = diffRows(groupUnifiedDiff(diff)[0]!).length;
    await waitFor(() => expect(rows()).toBe(total));
    // A refetch hands over a new group for the same file.
    rerender(
      <DiffView
        group={groupUnifiedDiff(diff)[0]!}
        layout="unified"
        ignoreWhitespace={false}
      />,
    );
    expect(rows()).toBe(total);
  });
});

describe("syntax color through a refresh", () => {
  it("keeps colors on the render a refetch causes", async () => {
    const group = () => groupUnifiedDiff(QUEUE_DIFF)[0]!;
    const { container, rerender } = render(
      <DiffView group={group()} layout="unified" ignoreWhitespace={false} />,
    );
    await waitFor(() =>
      expect(container.querySelector(".text-syntax-keyword")).not.toBeNull(),
    );
    rerender(
      <DiffView group={group()} layout="unified" ignoreWhitespace={false} />,
    );
    // No idle wait: unchanged hunks come back from what was just highlighted.
    expect(container.querySelector(".text-syntax-keyword")).not.toBeNull();
  });
});

describe("the queue fixture", () => {
  it("colors TypeScript once its grammar is loaded", async () => {
    await renderPanel(QUEUE_DIFF, { file: QUEUE_PATH });
    await waitFor(() =>
      expect(document.querySelector(".text-syntax-keyword")).not.toBeNull(),
    );
    expect(document.querySelector("mark")?.textContent).toBe(", one per turn");
  });
});

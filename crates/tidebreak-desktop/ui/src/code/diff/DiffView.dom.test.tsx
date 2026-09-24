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
import { messageWithReviewComments } from "./reviewComments";
import { loadLanguage } from "./syntaxHighlight";
import { useWorkspaceDiffReview } from "./useWorkspaceDiffReview";
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

describe("comments follow their code through a refresh", () => {
  // The agent keeps working while the reader reviews: first it adds a line
  // above the commented one, then it rewrites the commented line itself.
  const hunk = (lines: readonly string[]) =>
    fileDiff("src/limits.ts", [{ oldStart: 8, newStart: 8, lines }]);
  const WRITTEN = hunk([
    " const i = 1;",
    " const j = 2;",
    "-const K = 3;",
    "+const K = 30;",
    "+const L = 4;",
    " const m = 5;",
  ]);
  const LINE_ADDED_ABOVE = hunk([
    " const i = 1;",
    "+const inserted = 0;",
    " const j = 2;",
    "-const K = 3;",
    "+const K = 30;",
    "+const L = 4;",
    " const m = 5;",
  ]);
  const LINE_REWRITTEN = hunk([
    " const i = 1;",
    "+const inserted = 0;",
    " const j = 2;",
    "-const K = 3;",
    "+const K = 300;",
    "+const L = 4;",
    " const m = 5;",
  ]);

  function ReviewedDiff({ diff }: { diff: string }) {
    const reviewFor = useWorkspaceDiffReview({
      workspaceId: "ws-1",
      turnId: undefined,
      onDelete: (id) => usePendingReviewStore.getState().remove("ws-1", id),
    });
    const group = groupUnifiedDiff(diff)[0]!;
    return (
      <DiffView
        group={group}
        layout="unified"
        ignoreWhitespace={false}
        review={reviewFor?.(group.path)}
      />
    );
  }

  /** The text of the diff row a comment or editor sits under. */
  function rowAbove(element: HTMLElement): string {
    const frame = element.closest<HTMLElement>("[data-diff-comment]")!;
    let row = frame.previousElementSibling;
    while (row && !row.matches("[data-row]")) row = row.previousElementSibling;
    return row?.querySelector("[data-diff-code]")?.textContent ?? "";
  }

  it("keeps an open comment's text, and its lines, when a line lands above them", async () => {
    const user = userEvent.setup();
    const { rerender } = render(<ReviewedDiff diff={WRITTEN} />);
    await user.click(
      screen.getByRole("button", { name: "Comment on line 10" }),
    );
    await user.type(
      screen.getByRole("textbox", { name: "Comment on line 10" }),
      "Keep K small.",
    );

    rerender(<ReviewedDiff diff={LINE_ADDED_ABOVE} />);
    const editor = screen.getByRole("textbox", { name: "Comment on line 11" });
    expect(editor).toHaveValue("Keep K small.");
    expect(rowAbove(editor)).toBe("const K = 30;");
    // The picked line stays picked, on its new number.
    const picked = document.querySelectorAll(
      "[data-diff-view] [data-selected]",
    );
    expect([...picked].map((row) => row.textContent)).toEqual([
      expect.stringContaining("const K = 30;"),
    ]);

    fireEvent.keyDown(editor, { key: "Enter", metaKey: true });
    expect(comments()).toEqual([
      expect.objectContaining({
        body: "Keep K small.",
        lines: [{ kind: "add", oldNo: null, newNo: 11, text: "const K = 30;" }],
      }),
    ]);
    expect(comments()[0]?.outdated).toBeUndefined();
  });

  it("moves a saved comment with its line, and marks it outdated once the line changes", async () => {
    const user = userEvent.setup();
    const { rerender } = render(<ReviewedDiff diff={WRITTEN} />);
    await user.click(
      screen.getByRole("button", { name: "Comment on line 10" }),
    );
    await user.type(
      screen.getByRole("textbox", { name: "Comment on line 10" }),
      "Keep K small.",
    );
    await user.click(screen.getByRole("button", { name: "Add comment" }));

    rerender(<ReviewedDiff diff={LINE_ADDED_ABOVE} />);
    const card = await screen.findByRole("article", { name: "Line 11" });
    expect(rowAbove(card)).toBe("const K = 30;");
    // The review, and so the message, names the line where it is now.
    await waitFor(() => expect(comments()[0]?.lines[0]?.newNo).toBe(11));
    expect(messageWithReviewComments("", comments())).toContain('lines="11"');

    rerender(<ReviewedDiff diff={LINE_REWRITTEN} />);
    const outdated = await screen.findByRole("article", { name: "Line 11" });
    expect(outdated.closest("[data-diff-outdated]")).not.toBeNull();
    expect(within(outdated).getByText("Outdated")).toBeVisible();
    // It shows the line it was written on, not whatever sits there now.
    expect(outdated).toHaveTextContent("const K = 30;");
    await waitFor(() => expect(comments()[0]?.outdated).toBe(true));
    expect(messageWithReviewComments("", comments())).toContain(
      'outdated="true"',
    );
  });

  it("settles when two views of the file briefly disagree about where it is", async () => {
    usePendingReviewStore.getState().add("ws-1", {
      id: "c-k",
      author: { kind: "person" },
      path: "src/limits.ts",
      lines: [{ kind: "add", oldNo: null, newNo: 10, text: "const K = 30;" }],
      context: {
        before: ["const K = 3;", "const j = 2;", "const i = 1;"],
        after: ["const L = 4;", "const m = 5;"],
      },
      body: "Keep K small.",
      createdAt: "2026-09-24T10:00:00.000Z",
    });
    // One view has the refreshed diff, the other not yet. Each records its
    // own answer once rather than overwriting the other's in a loop.
    render(
      <>
        <ReviewedDiff diff={WRITTEN} />
        <ReviewedDiff diff={LINE_ADDED_ABOVE} />
      </>,
    );
    expect(
      await screen.findByRole("article", { name: "Line 10" }),
    ).toBeVisible();
    expect(
      await screen.findByRole("article", { name: "Line 11" }),
    ).toBeVisible();
    await waitFor(() => expect(comments()[0]?.lines[0]?.newNo).toBe(11));
  });

  it("keeps what was typed when the lines change while the editor is open", async () => {
    const user = userEvent.setup();
    const { rerender } = render(<ReviewedDiff diff={WRITTEN} />);
    await user.click(
      screen.getByRole("button", { name: "Comment on line 10" }),
    );
    await user.type(
      screen.getByRole("textbox", { name: "Comment on line 10" }),
      "Keep K small.",
    );

    rerender(<ReviewedDiff diff={LINE_REWRITTEN} />);
    const editor = screen.getByRole("textbox", { name: "Comment on line 10" });
    expect(editor).toHaveValue("Keep K small.");
    expect(editor.closest("[data-diff-outdated]")).not.toBeNull();
    fireEvent.keyDown(editor, { key: "Enter", metaKey: true });
    expect(comments()).toEqual([
      expect.objectContaining({
        body: "Keep K small.",
        outdated: true,
        lines: [{ kind: "add", oldNo: null, newNo: 10, text: "const K = 30;" }],
      }),
    ]);
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

  it("leaves keys pressed in the delete dialog to the dialog", async () => {
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
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    cancel.focus();
    await user.keyboard("wjk][[");
    // The dialog hides the page behind it from the accessibility tree.
    expect(
      screen.getByRole("button", {
        name: "Hide whitespace changes",
        hidden: true,
      }),
    ).toHaveAttribute("aria-pressed", "false");
    expect(cancel).toHaveFocus();
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

/**
 * The view's animation frames, run when the test says so. A long diff then
 * mounts on the test's schedule rather than the clock's, so a slow machine
 * changes how long the test takes and never what it sees.
 */
function manualFrames() {
  let queue: Array<{ id: number; run: FrameRequestCallback }> = [];
  let last = 0;
  const request = vi
    .spyOn(window, "requestAnimationFrame")
    .mockImplementation((run) => {
      last += 1;
      queue.push({ id: last, run });
      return last;
    });
  const cancel = vi
    .spyOn(window, "cancelAnimationFrame")
    .mockImplementation((id) => {
      queue = queue.filter((frame) => frame.id !== id);
    });
  return {
    pending: () => queue.length,
    /** Run every frame asked for so far, and what they render. */
    next() {
      const due = queue;
      queue = [];
      act(() => {
        for (const frame of due) frame.run(performance.now());
      });
    },
    restore() {
      request.mockRestore();
      cancel.mockRestore();
    },
  };
}

describe("a long diff", () => {
  // The browser story `Code/Diff review/Very long file` times the whole
  // 5,000 lines; jsdom is too slow to time anything, so this pins the shape
  // that keeps each frame short: one chunk first, then one chunk a frame.
  it("draws its first chunk at once and the rest one chunk a frame", () => {
    const frames = manualFrames();
    try {
      const group = groupUnifiedDiff(longFileDiff(5_000))[0]!;
      const { container } = render(
        <DiffView group={group} layout="unified" ignoreWhitespace={false} />,
      );
      const rows = () => container.querySelectorAll("[data-row]").length;
      expect(rows()).toBe(DIFF_CHUNK_ROWS);
      frames.next();
      expect(rows()).toBe(DIFF_CHUNK_ROWS * 2);
      frames.next();
      expect(rows()).toBe(DIFF_CHUNK_ROWS * 3);
    } finally {
      frames.restore();
    }
  });

  it("keeps every row through a refresh while an agent works", () => {
    const frames = manualFrames();
    try {
      const diff = longFileDiff(600);
      const view = () => (
        <DiffView
          group={groupUnifiedDiff(diff)[0]!}
          layout="unified"
          ignoreWhitespace={false}
        />
      );
      const { container, rerender } = render(view());
      const rows = () => container.querySelectorAll("[data-row]").length;
      const total = diffRows(groupUnifiedDiff(diff)[0]!).length;
      expect(total).toBeGreaterThan(DIFF_CHUNK_ROWS * 4);

      // A refetch hands over a new group for the same file. Halfway through
      // mounting, it keeps what was mounted and carries on from there.
      frames.next();
      frames.next();
      rerender(view());
      expect(rows()).toBe(DIFF_CHUNK_ROWS * 3);

      while (frames.pending() > 0) frames.next();
      expect(rows()).toBe(total);
      // Once everything is mounted, a refetch mounts nothing again.
      rerender(view());
      expect(rows()).toBe(total);
      expect(frames.pending()).toBe(0);
    } finally {
      frames.restore();
    }
  });
});

describe("syntax on each side of a whitespace pair", () => {
  // The old side wraps the paired line in a block comment and the new side
  // does not. A tab became one space, so the two lines are the same length
  // and either side's colors would fit the other's text.
  const PAIR = [
    "diff --git a/src/pair.ts b/src/pair.ts",
    "@@ -1,3 +1,3 @@",
    "-/* start",
    "-\tlet value = 1;",
    "- end */",
    "+// start",
    "+ let value = 1;",
    "+// end",
  ].join("\n");

  function pairedCells(container: HTMLElement) {
    return [
      ...container.querySelectorAll<HTMLElement>(
        '[data-kind="context"] [data-diff-code]',
      ),
    ];
  }

  it("colors the unified line from the side its text comes from", () => {
    const { container } = render(
      <DiffView
        group={groupUnifiedDiff(PAIR)[0]!}
        layout="unified"
        ignoreWhitespace
      />,
    );
    const [line] = pairedCells(container);
    expect(line?.textContent).toBe(" let value = 1;");
    expect(line?.querySelector(".text-syntax-keyword")?.textContent).toBe(
      "let",
    );
    expect(line?.querySelector(".text-syntax-comment")).toBeNull();
  });

  it("colors each side of the split line from its own side", () => {
    const { container } = render(
      <DiffView
        group={groupUnifiedDiff(PAIR)[0]!}
        layout="split"
        ignoreWhitespace
      />,
    );
    const [left, right] = pairedCells(container);
    expect(left?.textContent).toBe("\tlet value = 1;");
    expect(left?.querySelector(".text-syntax-comment")?.textContent).toBe(
      "\tlet value = 1;",
    );
    expect(right?.textContent).toBe(" let value = 1;");
    expect(right?.querySelector(".text-syntax-keyword")?.textContent).toBe(
      "let",
    );
  });

  it("colors both sides of a line whose only change was a final newline", () => {
    const diff = [
      "diff --git a/src/end.ts b/src/end.ts",
      "@@ -1,2 +1,2 @@",
      "-first();",
      "-return last;",
      "\\ No newline at end of file",
      "+First();",
      "+return last;",
    ].join("\n");
    const { container } = render(
      <DiffView
        group={groupUnifiedDiff(diff)[0]!}
        layout="split"
        ignoreWhitespace
      />,
    );
    const [left, right] = pairedCells(container);
    for (const cell of [left, right]) {
      expect(cell?.textContent).toBe("return last;");
      expect(cell?.querySelector(".text-syntax-keyword")?.textContent).toBe(
        "return",
      );
    }
    expect(container.textContent).not.toContain("No newline");
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

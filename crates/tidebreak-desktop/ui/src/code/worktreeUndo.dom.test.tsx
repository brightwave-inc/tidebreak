// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError } from "../api/client";
import type { CodeCheckpointRestorePreview } from "../api/types";
import { CommitBox, commitFailure } from "./CommitBox";
import { DiffOverviewContent } from "./DiffOverview";
import { DiffPanel } from "./DiffPanel";
import { CheckpointRestoreRow, TurnReviewCard } from "./TurnReviewCard";
import {
  restoreConfirmation,
  revertFileConfirmation,
  useWorktreeUndo,
  type WorktreeUndoClient,
} from "./worktreeUndo";

afterEach(cleanup);

/** Two hunks, far apart, in one file. */
const TWO_HUNKS = [
  "diff --git a/src/lib.rs b/src/lib.rs",
  "index 1111111..2222222 100644",
  "--- a/src/lib.rs",
  "+++ b/src/lib.rs",
  "@@ -1,3 +1,3 @@",
  " fn one() {}",
  "-fn two() {}",
  "+fn two() { todo!() }",
  " fn three() {}",
  "@@ -20,2 +20,3 @@ impl Parser {",
  " fn twenty() {}",
  "+fn twenty_one() {}",
  " fn twenty_two() {}",
  "",
].join("\n");

function diffClient(diff: string) {
  return {
    getCodeWorkspaceDiff: vi.fn().mockResolvedValue({
      diff,
      truncated: false,
      stat: { files: 1, insertions: 2, deletions: 1, truncated: false },
      file: "src/lib.rs",
    }),
  };
}

describe("reverting from the diff", () => {
  it("sends the hunk exactly as shown, by its position in the file", async () => {
    const onRevertHunk = vi.fn().mockResolvedValue(true);
    render(
      <DiffPanel
        client={diffClient(TWO_HUNKS)}
        workspaceId="ws-1"
        file="src/lib.rs"
        revert={{ onRevertFile: vi.fn(), onRevertHunk }}
      />,
    );

    await userEvent.click(
      await screen.findByRole("button", {
        name: "Revert the change at lines 20 to 22 of src/lib.rs",
      }),
    );

    expect(onRevertHunk).toHaveBeenCalledWith(
      { path: "src/lib.rs", turnId: undefined, kind: "modified" },
      expect.objectContaining({
        index: 1,
        newStart: 20,
        newCount: 3,
        text: [
          "@@ -20,2 +20,3 @@ impl Parser {",
          " fn twenty() {}",
          "+fn twenty_one() {}",
          " fn twenty_two() {}",
        ].join("\n"),
      }),
    );
  });

  it("offers no revert for a hunk the size cap cut short", async () => {
    // The second hunk promises three new lines and the diff ends after one.
    const cut = TWO_HUNKS.split("\n").slice(0, 12).join("\n");
    render(
      <DiffPanel
        client={diffClient(cut)}
        workspaceId="ws-1"
        file="src/lib.rs"
        revert={{ onRevertFile: vi.fn(), onRevertHunk: vi.fn() }}
      />,
    );
    await screen.findByRole("button", {
      name: "Revert the change at lines 1 to 3 of src/lib.rs",
    });
    expect(
      screen.queryByRole("button", { name: /lines 20 to 22/ }),
    ).not.toBeInTheDocument();
  });

  it("marks a turn's reverted hunk instead of offering it again", async () => {
    const onRevertHunk = vi.fn().mockResolvedValue(true);
    render(
      <DiffPanel
        client={diffClient(TWO_HUNKS)}
        workspaceId="ws-1"
        turnId="turn-2"
        revert={{ onRevertFile: vi.fn(), onRevertHunk }}
      />,
    );
    await userEvent.click(
      await screen.findByRole("button", {
        name: "Revert the change at lines 1 to 3 of src/lib.rs",
      }),
    );
    expect(onRevertHunk.mock.calls[0]?.[0]).toEqual({
      path: "src/lib.rs",
      turnId: "turn-2",
      kind: "modified",
    });
    expect(await screen.findByText("Reverted")).toBeVisible();
    expect(
      screen.queryByRole("button", { name: /lines 1 to 3/ }),
    ).not.toBeInTheDocument();
  });

  it("keeps the controls but turns them off while a turn runs", async () => {
    const onRevertFile = vi.fn();
    render(
      <DiffPanel
        client={diffClient(TWO_HUNKS)}
        workspaceId="ws-1"
        file="src/lib.rs"
        revert={{
          onRevertFile,
          onRevertHunk: vi.fn(),
          unavailableReason: "Wait for the turn to finish.",
        }}
      />,
    );
    const button = await screen.findByRole("button", {
      name: "Revert src/lib.rs",
    });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("title", "Wait for the turn to finish.");
  });
});

describe("the changed-file actions", () => {
  const files = {
    files: [
      {
        path: "src/lib.rs",
        kind: "modified" as const,
        insertions: 3,
        deletions: 1,
        uncommitted: true,
      },
      {
        path: "src/committed.rs",
        kind: "modified" as const,
        insertions: 1,
        deletions: 0,
      },
    ],
    truncated: false,
    stat: { files: 2, insertions: 4, deletions: 1, truncated: false },
    worktree_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
  };

  it("offers discard only where there is something uncommitted to lose", async () => {
    const onDiscard = vi.fn();
    const onRevertFile = vi.fn();
    render(
      <DiffOverviewContent
        resource={{ data: files, error: null, refreshing: false }}
        onOpenFile={vi.fn()}
        actions={{ onRevertFile, onDiscard }}
      />,
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Actions for src/committed.rs" }),
    );
    let menu = await screen.findByRole("menu");
    expect(
      within(menu).queryByRole("menuitem", {
        name: "Discard uncommitted changes",
      }),
    ).not.toBeInTheDocument();
    await userEvent.click(
      within(menu).getByRole("menuitem", { name: "Revert to base branch" }),
    );
    expect(onRevertFile).toHaveBeenCalledWith({
      path: "src/committed.rs",
      turnId: undefined,
      kind: "modified",
      previousPath: undefined,
    });

    await userEvent.click(
      screen.getByRole("button", { name: "Actions for src/lib.rs" }),
    );
    menu = await screen.findByRole("menu");
    await userEvent.click(
      within(menu).getByRole("menuitem", {
        name: "Discard uncommitted changes",
      }),
    );
    expect(onDiscard).toHaveBeenCalledWith(files.files[0], files.worktree_tree);
  });

  it("puts the commit box above the workspace's list, never a turn's", () => {
    const { rerender } = render(
      <DiffOverviewContent
        resource={{ data: files, error: null, refreshing: false }}
        onOpenFile={vi.fn()}
        commit={() => <p>commit box</p>}
      />,
    );
    expect(screen.getByText("commit box")).toBeVisible();
    rerender(
      <DiffOverviewContent
        resource={{ data: files, error: null, refreshing: false }}
        turnId="turn-2"
        onOpenFile={vi.fn()}
        commit={() => <p>commit box</p>}
      />,
    );
    expect(screen.queryByText("commit box")).not.toBeInTheDocument();
  });
});

describe("restoring from the transcript", () => {
  const turn = {
    kind: "turn_boundary" as const,
    id: "boundary-2",
    turnId: "turn-2",
    status: "completed" as const,
    durationMs: 12_000,
    usage: null,
    error: null,
    diffstat: { files: 2, insertions: 12, deletions: 3, truncated: false },
  };

  it("offers the restore from the turn's own menu", async () => {
    const onRestoreBeforeTurn = vi.fn();
    render(
      <TurnReviewCard turn={turn} onRestoreBeforeTurn={onRestoreBeforeTurn} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Turn actions" }));
    await userEvent.click(
      await screen.findByRole("menuitem", {
        name: "Restore to before this turn",
      }),
    );
    expect(onRestoreBeforeTurn).toHaveBeenCalledWith("turn-2");
  });

  it("says why the restore is off while a turn runs", async () => {
    render(
      <TurnReviewCard
        turn={turn}
        onRestoreBeforeTurn={vi.fn()}
        undoUnavailableReason="Wait for the turn to finish."
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Turn actions" }));
    const item = await screen.findByRole("menuitem", {
      name: "Restore to before this turn",
    });
    expect(item).toHaveAttribute("aria-disabled", "true");
    expect(item).toHaveAccessibleDescription("Wait for the turn to finish.");
    expect(screen.getByText("Wait for the turn to finish.")).toBeVisible();
  });

  it("lets a keyboard reach the turned-off restore and does nothing on it", async () => {
    const onRestoreBeforeTurn = vi.fn();
    render(
      <TurnReviewCard
        turn={turn}
        onRestoreBeforeTurn={onRestoreBeforeTurn}
        undoUnavailableReason="Wait for the turn to finish."
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Turn actions" }));
    const item = await screen.findByRole("menuitem", {
      name: "Restore to before this turn",
    });
    await userEvent.keyboard("{ArrowDown}");
    expect(item).toHaveFocus();
    await userEvent.keyboard("{Enter}");
    expect(onRestoreBeforeTurn).not.toHaveBeenCalled();
  });

  it("offers to undo a restore from its row", async () => {
    const onUndo = vi.fn();
    render(
      <CheckpointRestoreRow
        restore={{
          kind: "restore",
          id: "restore:r-1",
          restoreId: "r-1",
          target: { kind: "before_turn", turn_id: "turn-2" },
          turnOrdinal: 2,
          diffstat: {
            files: 3,
            insertions: 4,
            deletions: 40,
            truncated: false,
          },
          status: "completed",
          error: null,
        }}
        onUndo={onUndo}
      />,
    );
    expect(screen.getByText("Restored to before turn 2")).toBeVisible();
    await userEvent.click(
      screen.getByRole("button", { name: "Undo the restore" }),
    );
    expect(onUndo).toHaveBeenCalledWith("r-1");
  });

  it("names what the restore loses and how many more there are", () => {
    const preview: CodeCheckpointRestorePreview = {
      target: { kind: "before_turn", turn_id: "turn-2" },
      session_id: "session-1",
      files: Array.from({ length: 8 }, (_, index) => ({
        path: `src/file-${index}.rs`,
        kind: "modified" as const,
        insertions: 1,
        deletions: 1,
      })),
      truncated: false,
      stat: { files: 8, insertions: 8, deletions: 8, truncated: false },
      current_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
      blocked: [],
      affected_turns: [],
    };
    const options = restoreConfirmation(preview, { turnOrdinal: 2 });
    expect(options.title).toBe("Restore to before turn 2?");
    expect(options.destructive).toBe(true);
    render(<p>{options.description}</p>);
    expect(screen.getByText("src/file-0.rs")).toBeVisible();
    expect(screen.queryByText("src/file-6.rs")).not.toBeInTheDocument();
    expect(screen.getByText("and 2 more files")).toBeVisible();
    expect(screen.getByText(/including edits you made by hand/)).toBeVisible();
  });
});

describe("the commit box", () => {
  it("commits with the message and clears it", async () => {
    const onCommit = vi.fn().mockResolvedValue({
      sha: "cec166ffc1d2e3f4",
      message: "Fix the parser",
      stat: { files: 1, insertions: 1, deletions: 0, truncated: false },
    });
    render(<CommitBox dirty changedFiles={2} onCommit={onCommit} />);
    const field = screen.getByRole("textbox", { name: "Commit message" });
    await userEvent.type(field, "Fix the parser");
    await userEvent.click(screen.getByRole("button", { name: "Commit" }));
    expect(onCommit).toHaveBeenCalledWith("Fix the parser");
    expect(field).toHaveValue("");
  });

  it("shows what a refusing hook printed", async () => {
    const onCommit = vi
      .fn()
      .mockRejectedValue(
        new HttpError(
          409,
          "409: Git refused the commit.\n\nlint: 2 errors in src/lib.rs",
          "commit_rejected",
        ),
      );
    render(<CommitBox dirty onCommit={onCommit} />);
    await userEvent.click(screen.getByRole("button", { name: "Commit" }));
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Git refused the commit.")).toBeVisible();
    expect(
      within(alert).getByText("lint: 2 errors in src/lib.rs"),
    ).toBeVisible();
  });

  it("has nothing to do on a clean worktree", () => {
    render(<CommitBox dirty={false} onCommit={vi.fn()} />);
    expect(screen.getByRole("button", { name: "Commit" })).toBeDisabled();
    expect(screen.getByText("No changes to commit.")).toBeVisible();
  });

  it("reads a refusal with no output as one sentence", () => {
    expect(
      commitFailure(
        new HttpError(
          409,
          "409: Git refused the commit. A hook may have stopped it without saying why.",
          "commit_rejected",
        ),
      ),
    ).toEqual({
      title:
        "Git refused the commit. A hook may have stopped it without saying why.",
    });
  });
});

/** Runs one undo flow from a button, with the dialog the page renders. */
function UndoHarness({
  client,
  run,
}: {
  client: WorktreeUndoClient;
  run: (undo: ReturnType<typeof useWorktreeUndo>) => unknown;
}) {
  const undo = useWorktreeUndo({ client, workspaceId: "ws-1" });
  return (
    <>
      <button type="button" onClick={() => void run(undo)}>
        Go
      </button>
      {undo.dialog}
    </>
  );
}

function undoClient(overrides: Partial<WorktreeUndoClient> = {}) {
  return {
    previewCodeCheckpointRestore: vi.fn(),
    restoreCodeCheckpoint: vi.fn(),
    revertCodeWorkspaceChange: vi.fn(),
    discardCodeWorkspaceChanges: vi.fn().mockResolvedValue({ paths: [] }),
    ...overrides,
  } as unknown as WorktreeUndoClient & {
    discardCodeWorkspaceChanges: ReturnType<typeof vi.fn>;
    restoreCodeCheckpoint: ReturnType<typeof vi.fn>;
  };
}

const PREVIEW: CodeCheckpointRestorePreview = {
  target: { kind: "before_turn", turn_id: "turn-2" },
  session_id: "session-1",
  files: [
    { path: "src/lib.rs", kind: "modified", insertions: 1, deletions: 1 },
  ],
  truncated: false,
  stat: { files: 1, insertions: 1, deletions: 1, truncated: false },
  current_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
  blocked: [],
  affected_turns: [],
};

describe("the undo flows", () => {
  it("discards a renamed file by the path the list shows", async () => {
    // The list names the file by its new path. Its old path may be a
    // committed rename, which has nothing uncommitted to discard.
    const client = undoClient();
    render(
      <UndoHarness
        client={client}
        run={(undo) =>
          undo.discard(
            {
              path: "notes.md",
              kind: "renamed",
              previous_path: "notes.txt",
            },
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
          )
        }
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Go" }));
    await userEvent.click(
      within(await screen.findByRole("alertdialog")).getByRole("button", {
        name: "Discard",
      }),
    );
    expect(client.discardCodeWorkspaceChanges).toHaveBeenCalledWith(
      "ws-1",
      ["notes.md"],
      "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
    );
  });

  it("names the files in the way and never restores over them", async () => {
    const client = undoClient({
      previewCodeCheckpointRestore: vi
        .fn()
        .mockResolvedValue({ ...PREVIEW, blocked: [".env"] }),
    });
    render(
      <UndoHarness
        client={client}
        run={(undo) => undo.restore({ kind: "before_turn", turn_id: "turn-2" })}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Go" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText(".env")).toBeVisible();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Close" }),
    );
    expect(client.restoreCodeCheckpoint).not.toHaveBeenCalled();
  });

  it("names other agents' turns and says the agent still remembers", () => {
    const options = restoreConfirmation(
      {
        ...PREVIEW,
        affected_turns: [
          {
            session_id: "session-2",
            turn_id: "turn-9",
            ordinal: 4,
            harness_kind: "codex",
          },
        ],
      },
      { turnOrdinal: 2 },
    );
    render(<p>{options.description}</p>);
    expect(
      screen.getByText(/It also undoes a turn by other agents/),
    ).toBeVisible();
    expect(screen.getByText("Codex CLI, turn 4")).toBeVisible();
    expect(
      screen.getByText(/The agent still remembers the turns this undoes/),
    ).toBeVisible();
  });
});

describe("a restore row", () => {
  const row = {
    kind: "restore" as const,
    id: "restore:r-1",
    restoreId: "r-1",
    target: { kind: "before_turn" as const, turn_id: "turn-2" },
    turnOrdinal: 2,
    diffstat: { files: 3, insertions: 4, deletions: 40, truncated: false },
    error: null,
  };

  it("offers no undo for a restore that changed nothing", () => {
    render(
      <CheckpointRestoreRow
        restore={{ ...row, status: "failed", error: "git refused" }}
        onUndo={vi.fn()}
      />,
    );
    expect(
      screen.getByText("Could not restore. Nothing changed."),
    ).toBeVisible();
    expect(screen.getByText("git refused")).toBeVisible();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("keeps the way back for a restore that stopped partway", async () => {
    const onUndo = vi.fn();
    render(
      <CheckpointRestoreRow
        restore={{ ...row, status: "partial", error: "disk full" }}
        onUndo={onUndo}
      />,
    );
    expect(screen.getByText("The restore stopped partway")).toBeVisible();
    await userEvent.click(
      screen.getByRole("button", {
        name: "Put back the files the restore replaced",
      }),
    );
    expect(onUndo).toHaveBeenCalledWith("r-1");
  });
});

describe("reverting a whole file from the diff", () => {
  it("says an added file is deleted, because the diff says it is new", async () => {
    const onRevertFile = vi.fn().mockResolvedValue(true);
    const added = [
      "diff --git a/src/new.rs b/src/new.rs",
      "new file mode 100644",
      "index 0000000..2222222",
      "--- /dev/null",
      "+++ b/src/new.rs",
      "@@ -0,0 +1 @@",
      "+fn new() {}",
      "",
    ].join("\n");
    render(
      <DiffPanel
        client={diffClient(added)}
        workspaceId="ws-1"
        file="src/new.rs"
        revert={{ onRevertFile, onRevertHunk: vi.fn() }}
      />,
    );
    await userEvent.click(
      await screen.findByRole("button", { name: "Revert src/new.rs" }),
    );
    expect(onRevertFile).toHaveBeenCalledWith(
      expect.objectContaining({ path: "src/new.rs", kind: "added" }),
    );
    render(
      <p>
        {
          revertFileConfirmation({ path: "src/new.rs", kind: "added" })
            .description
        }
      </p>,
    );
    expect(screen.getByText(/so reverting it deletes it/)).toBeVisible();
  });
});

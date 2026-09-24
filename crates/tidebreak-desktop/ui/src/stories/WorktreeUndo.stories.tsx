import type { Meta, StoryObj } from "@storybook/react-vite";
import { type ReactNode, useEffect, useMemo } from "react";
import { expect, fn, userEvent, within } from "storybook/test";

import { HttpError } from "@/api/client";
import type { CodeCheckpointRestorePreview } from "@/api/types";
import { useConfirm, type ConfirmOptions } from "@/components/ConfirmDialog";
import { Toaster } from "@/components/ui/sonner";
import { CommitBox } from "@/code/CommitBox";
import { CodeTranscript } from "@/code/CodeTranscript";
import type { CodeTranscriptItem } from "@/code/CodeSessionReducer";
import { DiffOverviewContent } from "@/code/DiffOverview";
import { DiffPanel } from "@/code/DiffPanel";
import {
  discardConfirmation,
  restoreBlockedNotice,
  restoreConfirmation,
  revertHunkConfirmation,
  TURN_RUNNING_REASON,
  useWorktreeUndo,
  type WorktreeUndoClient,
} from "@/code/worktreeUndo";
import { diffHunks, groupUnifiedDiff } from "@/code/unifiedDiff";
import type { CheckpointRestoreStatus } from "@/generated/wire";

/**
 * Undo in the worktree, and Source control's commit box: every state a
 * reader meets before and after changing files by hand.
 */
const meta = {
  title: "Code/Undo and source control",
  parameters: { layout: "padded" },
} satisfies Meta;

export default meta;
type Story = StoryObj<typeof meta>;

const turnTwo: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "user-turn-2",
    turnId: "turn-2",
    text: "Split the parser into a lexer and a parser module.",
    createdAt: "2026-09-23T15:12:00.000Z",
  },
  {
    kind: "assistant",
    id: "assistant-turn-2",
    turnId: "turn-2",
    parentCallId: null,
    text: "The lexer now lives in `src/lexer.rs`, and `src/parser.rs` reads its tokens. The old tests pass against the new split.",
    streaming: false,
  },
  {
    kind: "turn_boundary",
    id: "boundary-turn-2",
    turnId: "turn-2",
    status: "completed",
    durationMs: 94_000,
    usage: null,
    error: null,
    diffstat: { files: 3, insertions: 182, deletions: 41, truncated: false },
  },
];

function TranscriptFrame({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-[360px] w-full max-w-3xl flex-col overflow-hidden rounded-lg border bg-background">
      <div className="message-view">{children}</div>
    </div>
  );
}

async function openTurnMenu(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  await userEvent.click(canvas.getByRole("button", { name: "Turn actions" }));
  await within(document.body).findByRole("menu");
}

/** The restore sits in the turn's own menu, beside forking. */
export const RestoreOffered: Story = {
  render: () => (
    <TranscriptFrame>
      <CodeTranscript
        items={turnTwo}
        onForkFromTurn={fn()}
        onRestoreBeforeTurn={fn()}
      />
    </TranscriptFrame>
  ),
  play: async ({ canvasElement }) => {
    await openTurnMenu(canvasElement);
    await expect(
      within(document.body).getByRole("menuitem", {
        name: "Restore to before this turn",
      }),
    ).toBeVisible();
  },
};

/** A running turn holds the worktree: the restore stays, turned off, with why. */
export const RestoreRefusedWhileATurnRuns: Story = {
  render: () => (
    <TranscriptFrame>
      <CodeTranscript
        items={turnTwo}
        busy
        onForkFromTurn={fn()}
        onRestoreBeforeTurn={fn()}
        undoUnavailableReason={TURN_RUNNING_REASON}
      />
    </TranscriptFrame>
  ),
  play: async ({ canvasElement }) => {
    await openTurnMenu(canvasElement);
    await expect(
      within(document.body).getByText(TURN_RUNNING_REASON),
    ).toBeVisible();
  },
};

const RESTORE_PREVIEW: CodeCheckpointRestorePreview = {
  target: { kind: "before_turn", turn_id: "turn-2" },
  session_id: "session-1",
  files: [
    {
      path: "src/lexer.rs",
      kind: "added",
      insertions: 121,
      deletions: 0,
    },
    {
      path: "src/parser.rs",
      kind: "modified",
      insertions: 58,
      deletions: 41,
    },
    {
      path: "tests/parser_split.rs",
      kind: "added",
      insertions: 3,
      deletions: 0,
    },
    {
      path: "notes/scratch.md",
      kind: "added",
      insertions: 12,
      deletions: 0,
    },
    {
      path: "src/old_tokenizer.rs",
      kind: "deleted",
      insertions: 0,
      deletions: 66,
    },
    {
      path: "Cargo.toml",
      kind: "modified",
      insertions: 1,
      deletions: 0,
    },
    {
      path: "src/lib.rs",
      kind: "modified",
      insertions: 2,
      deletions: 1,
    },
  ],
  truncated: false,
  stat: { files: 9, insertions: 197, deletions: 108, truncated: false },
  current_tree: "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
  blocked: [],
  affected_turns: [],
};

/** Opens one confirmation on mount, the way the page's shared dialog does. */
function OpenConfirmation({ options }: { options: ConfirmOptions }) {
  const { confirm, dialog } = useConfirm();
  useEffect(() => {
    void confirm(options);
  }, [confirm, options]);
  return dialog;
}

/** The losses are named before anything moves, hand edits included. */
export const RestoreConfirm: Story = {
  render: () => (
    <OpenConfirmation
      options={restoreConfirmation(RESTORE_PREVIEW, { turnOrdinal: 2 })}
    />
  ),
  play: async () => {
    await expect(
      await within(document.body).findByRole("alertdialog"),
    ).toBeVisible();
  },
};

const restoredItems: CodeTranscriptItem[] = [
  ...turnTwo,
  {
    kind: "restore",
    id: "restore:restore-1",
    restoreId: "restore-1",
    target: { kind: "before_turn", turn_id: "turn-2" },
    turnOrdinal: 2,
    diffstat: { files: 9, insertions: 108, deletions: 197, truncated: false },
    status: "completed",
    error: null,
  },
];

/** The restore stays in the conversation, with its own way back. */
export const RestoredNotice: Story = {
  render: () => (
    <TranscriptFrame>
      <CodeTranscript
        items={restoredItems}
        onForkFromTurn={fn()}
        onRestoreBeforeTurn={fn()}
        onUndoRestore={fn()}
      />
    </TranscriptFrame>
  ),
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getByText("Restored to before turn 2"),
    ).toBeVisible();
  },
};

const CHANGES = {
  files: [
    {
      path: "src/lexer.rs",
      kind: "added" as const,
      insertions: 121,
      deletions: 0,
      uncommitted: true,
    },
    {
      path: "src/parser.rs",
      kind: "modified" as const,
      insertions: 58,
      deletions: 41,
      uncommitted: true,
    },
    {
      path: "Cargo.toml",
      kind: "modified" as const,
      insertions: 1,
      deletions: 0,
    },
  ],
  truncated: false,
  stat: { files: 3, insertions: 180, deletions: 41, truncated: false },
};

const committed = async () => ({
  sha: "cec166ffc1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6",
  message: "Split the parser into a lexer and a parser module",
  stat: { files: 2, insertions: 179, deletions: 41, truncated: false },
});

function SourceControlFrame({ commit }: { commit: ReactNode }) {
  return (
    <div className="flex h-[420px] w-full max-w-sm flex-col overflow-hidden rounded-lg border bg-background">
      <DiffOverviewContent
        resource={{ data: CHANGES, error: null, refreshing: false }}
        onOpenFile={fn()}
        actions={{ onRevertFile: fn(), onDiscard: fn() }}
        commit={() => commit}
      />
    </div>
  );
}

/** Nothing typed yet: the placeholder names the message an empty commit uses. */
export const CommitBoxEmpty: Story = {
  render: () => (
    <SourceControlFrame
      commit={
        <CommitBox
          dirty
          changedFiles={2}
          suggestedMessage={"first change\n\n2 files changed"}
          onCommit={committed}
        />
      }
    />
  ),
};

/** A message typed and ready to commit. */
export const CommitBoxWithMessage: Story = {
  render: () => (
    <SourceControlFrame
      commit={
        <CommitBox
          dirty
          changedFiles={2}
          suggestedMessage="first change"
          onCommit={committed}
        />
      }
    />
  ),
  play: async ({ canvasElement }) => {
    await userEvent.type(
      within(canvasElement).getByRole("textbox", { name: "Commit message" }),
      "Split the parser into a lexer and a parser module",
    );
  },
};

/** A hook said no: its own words stay on screen until the next try. */
export const CommitFailed: Story = {
  render: () => (
    <SourceControlFrame
      commit={
        <CommitBox
          dirty
          changedFiles={2}
          suggestedMessage="first change"
          onCommit={async () => {
            throw new HttpError(
              409,
              "409: Git refused the commit.\n\nrunning cargo clippy...\nerror: unused variable: `token`\n --> src/lexer.rs:48:9\nhusky - pre-commit hook exited with code 1 (error)",
              "commit_rejected",
            );
          }}
        />
      }
    />
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(
      canvas.getByRole("textbox", { name: "Commit message" }),
      "Split the parser",
    );
    await userEvent.click(canvas.getByRole("button", { name: "Commit" }));
    await expect(await canvas.findByRole("alert")).toBeVisible();
  },
};

/** A running turn holds the worktree, so the box waits with it. */
export const CommitWhileATurnRuns: Story = {
  render: () => (
    <SourceControlFrame
      commit={
        <CommitBox
          dirty
          changedFiles={2}
          unavailableReason={TURN_RUNNING_REASON}
          onCommit={committed}
        />
      }
    />
  ),
};

/** Discard names the file and says it cannot be undone. */
export const DiscardConfirm: Story = {
  render: () => (
    <OpenConfirmation
      options={discardConfirmation({
        path: "src/parser.rs",
        kind: "modified",
      })}
    />
  ),
  play: async () => {
    await expect(
      await within(document.body).findByRole("alertdialog"),
    ).toBeVisible();
  },
};

/** The file's actions: revert to the base branch, or discard what is uncommitted. */
export const FileActionsMenu: Story = {
  render: () => <SourceControlFrame commit={null} />,
  play: async ({ canvasElement }) => {
    await userEvent.click(
      within(canvasElement).getByRole("button", {
        name: "Actions for src/parser.rs",
      }),
    );
    await expect(
      await within(document.body).findByRole("menuitem", {
        name: "Discard uncommitted changes",
      }),
    ).toBeVisible();
  },
};

const HUNK_DIFF = [
  "diff --git a/src/parser.rs b/src/parser.rs",
  "index 1111111..2222222 100644",
  "--- a/src/parser.rs",
  "+++ b/src/parser.rs",
  "@@ -1,5 +1,6 @@",
  " use crate::lexer::Token;",
  "+use crate::lexer::Lexer;",
  " ",
  " pub struct Parser {",
  "-    input: String,",
  "+    tokens: Vec<Token>,",
  " }",
  "@@ -40,3 +41,4 @@ impl Parser {",
  "     fn peek(&self) -> Option<&Token> {",
  "-        self.input.chars().next()",
  "+        self.tokens.first()",
  "+        // The lexer already dropped whitespace.",
  "     }",
  "",
].join("\n");

const hunkClient = {
  getCodeWorkspaceDiff: async () => ({
    diff: HUNK_DIFF,
    truncated: false,
    stat: { files: 1, insertions: 4, deletions: 2, truncated: false },
    file: "src/parser.rs",
  }),
};

/** Each whole hunk carries its own Revert beside its header. */
export const RevertHunk: Story = {
  render: () => (
    <div className="flex h-[420px] w-full max-w-3xl flex-col overflow-hidden rounded-lg border bg-background">
      <DiffPanel
        client={hunkClient}
        workspaceId="workspace-storybook"
        file="src/parser.rs"
        onOpenFile={fn()}
        revert={{
          onRevertFile: fn(async () => true),
          onRevertHunk: fn(async () => true),
        }}
      />
    </div>
  ),
  play: async ({ canvasElement }) => {
    await expect(
      await within(canvasElement).findByRole("button", {
        name: "Revert the change at lines 41 to 44 of src/parser.rs",
      }),
    ).toBeVisible();
  },
};

const [hunkGroup] = groupUnifiedDiff(HUNK_DIFF);

/** Reverting one hunk names the lines that go back. */
export const RevertHunkConfirm: Story = {
  render: () => (
    <OpenConfirmation
      options={revertHunkConfirmation(
        { path: "src/parser.rs" },
        diffHunks(hunkGroup!)[1]!,
      )}
    />
  ),
  play: async () => {
    await expect(
      await within(document.body).findByRole("alertdialog"),
    ).toBeVisible();
  },
};

/** Another agent's turns in the same workspace are undone too, by name. */
export const RestoreConfirmOtherAgents: Story = {
  render: () => (
    <OpenConfirmation
      options={restoreConfirmation(
        {
          ...RESTORE_PREVIEW,
          affected_turns: [
            {
              session_id: "session-2",
              turn_id: "turn-9",
              ordinal: 4,
              harness_kind: "codex",
            },
            {
              session_id: "session-3",
              turn_id: "turn-12",
              ordinal: 1,
              harness_kind: "grok",
            },
          ],
        },
        { turnOrdinal: 2 },
      )}
    />
  ),
  play: async () => {
    await expect(
      await within(document.body).findByRole("alertdialog"),
    ).toBeVisible();
  },
};

/**
 * Files no checkpoint holds stand where the restore would write, ignored
 * ones mostly. The restore names them and waits instead of losing them.
 */
export const RestoreBlocked: Story = {
  render: () => (
    <OpenConfirmation
      options={restoreBlockedNotice({
        blocked: [".env", "utils/settings.local"],
      })}
    />
  ),
  play: async () => {
    await expect(
      await within(document.body).findByRole("alertdialog"),
    ).toBeVisible();
  },
};

function restoreRow(
  status: CheckpointRestoreStatus,
  error: string | null,
): CodeTranscriptItem {
  return {
    kind: "restore",
    id: `restore:${status}`,
    restoreId: "restore-1",
    target: { kind: "before_turn", turn_id: "turn-2" },
    turnOrdinal: 2,
    diffstat: { files: 9, insertions: 108, deletions: 197, truncated: false },
    status,
    error,
  };
}

/** The restore stopped partway: the row keeps the way back to every file. */
export const RestoreStoppedPartway: Story = {
  render: () => (
    <TranscriptFrame>
      <CodeTranscript
        items={[
          ...turnTwo,
          restoreRow(
            "partial",
            "Could not write src/lexer.rs: No space left on device.",
          ),
        ]}
        onForkFromTurn={fn()}
        onRestoreBeforeTurn={fn()}
        onUndoRestore={fn()}
      />
    </TranscriptFrame>
  ),
};

/**
 * The restore stopped and put back every file it moved, so nothing changed.
 * The row keeps Undo, so a wrong check never strands a file.
 */
export const RestoreFailed: Story = {
  render: () => (
    <TranscriptFrame>
      <CodeTranscript
        items={[
          ...turnTwo,
          restoreRow(
            "failed",
            "src/lexer.rs changed after Tidebreak checked it.",
          ),
        ]}
        onForkFromTurn={fn()}
        onRestoreBeforeTurn={fn()}
        onUndoRestore={fn()}
      />
    </TranscriptFrame>
  ),
};

/** A client whose every undo call fails the way the server refuses it. */
function refusingClient(refusal: HttpError): WorktreeUndoClient {
  return {
    previewCodeCheckpointRestore: async () => RESTORE_PREVIEW,
    restoreCodeCheckpoint: async () => {
      throw refusal;
    },
    revertCodeWorkspaceChange: async () => {
      throw refusal;
    },
    discardCodeWorkspaceChanges: async () => {
      throw refusal;
    },
  } as unknown as WorktreeUndoClient;
}

/** A turn's diff wired to the real revert flow, and the toast it raises. */
function RevertFlow({ refusal }: { refusal: HttpError }) {
  const client = useMemo(() => refusingClient(refusal), [refusal]);
  const undo = useWorktreeUndo({ client, workspaceId: "workspace-storybook" });
  return (
    <>
      <div className="flex h-[420px] w-full max-w-3xl flex-col overflow-hidden rounded-lg border bg-background">
        <DiffPanel
          client={hunkClient}
          workspaceId="workspace-storybook"
          turnId="turn-2"
          turnLabel="Turn 2"
          revert={{
            onRevertFile: undo.revertFile,
            onRevertHunk: undo.revertHunk,
          }}
        />
      </div>
      {undo.dialog}
      <Toaster richColors duration={Number.POSITIVE_INFINITY} />
    </>
  );
}

async function revertTheSecondHunk(canvasElement: HTMLElement) {
  await userEvent.click(
    await within(canvasElement).findByRole("button", {
      name: "Revert the change at lines 41 to 44 of src/parser.rs",
    }),
  );
  const dialog = await within(document.body).findByRole("alertdialog");
  await userEvent.click(
    within(dialog).getByRole("button", { name: "Revert change" }),
  );
}

const REVERT_CONFLICT = new HttpError(
  409,
  "409: This change no longer matches the file, so nothing was reverted. The file changed after the diff you reviewed.",
  "revert_conflict",
);

/** A later edit overlaps the turn's change, so nothing is reverted. */
export const RevertConflict: Story = {
  render: () => <RevertFlow refusal={REVERT_CONFLICT} />,
  play: async ({ canvasElement }) => {
    await revertTheSecondHunk(canvasElement);
    await expect(
      await within(document.body).findByText(/nothing was reverted/),
    ).toBeVisible();
  },
};

const DIFF_CHANGED = new HttpError(
  409,
  "409: This change is no longer in the diff. Review the diff again.",
  "diff_changed",
);

/** The hunk on screen is no longer the file's, so the revert asks for a fresh look. */
export const DiffChanged: Story = {
  render: () => <RevertFlow refusal={DIFF_CHANGED} />,
  play: async ({ canvasElement }) => {
    await revertTheSecondHunk(canvasElement);
    await expect(
      await within(document.body).findByText(/The diff changed since/),
    ).toBeVisible();
  },
};

const WORKTREE_CHANGED = new HttpError(
  409,
  "409: The workspace changed after you reviewed this restore. Review it again.",
  "worktree_changed",
);

/** The workspace moved after the person confirmed, so nothing moved. */
function RestoreFlow() {
  const client = useMemo(() => refusingClient(WORKTREE_CHANGED), []);
  const undo = useWorktreeUndo({ client, workspaceId: "workspace-storybook" });
  return (
    <>
      <TranscriptFrame>
        <CodeTranscript
          items={turnTwo}
          onForkFromTurn={fn()}
          onRestoreBeforeTurn={(turnId) =>
            void undo.restore(
              { kind: "before_turn", turn_id: turnId },
              { turnOrdinal: 2 },
            )
          }
        />
      </TranscriptFrame>
      {undo.dialog}
      <Toaster richColors duration={Number.POSITIVE_INFINITY} />
    </>
  );
}

export const WorktreeChanged: Story = {
  render: () => <RestoreFlow />,
  play: async ({ canvasElement }) => {
    await openTurnMenu(canvasElement);
    await userEvent.click(
      within(document.body).getByRole("menuitem", {
        name: "Restore to before this turn",
      }),
    );
    const dialog = await within(document.body).findByRole("alertdialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Restore" }),
    );
    await expect(
      await within(document.body).findByText(
        "The workspace changed while you were reviewing the restore.",
      ),
    ).toBeVisible();
  },
};

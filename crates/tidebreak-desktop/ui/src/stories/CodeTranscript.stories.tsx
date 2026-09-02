import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { CodeTranscript } from "@/code/CodeTranscript";
import type { CodeTranscriptItem } from "@/code/CodeSessionReducer";

const items: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "user-turn-1",
    turnId: "turn-1",
    text: "Keep recent transcript stores in memory when I switch workspaces.",
    createdAt: "2026-08-25T16:45:00.000Z",
  },
  {
    kind: "assistant",
    id: "assistant-progress-1",
    turnId: "turn-1",
    parentCallId: null,
    text: "I’m tracing where the workspace registry releases transcript stores.",
    streaming: false,
  },
  {
    kind: "tool",
    id: "tool-read-registry",
    turnId: "turn-1",
    callId: "call-read-registry",
    parentCallId: null,
    name: "Read",
    detail: {
      kind: "file_read",
      path: "crates/tidebreak-desktop/ui/src/code/CodeSessionRegistry.ts",
    },
    status: "succeeded",
    preview: "",
    startedAt: null,
    durationMs: 400,
  },
  {
    kind: "assistant",
    id: "assistant-progress-2",
    turnId: "turn-1",
    parentCallId: null,
    text: "The registry drops the store on the last unmount, so a quick return has to hydrate from disk.",
    streaming: false,
  },
  {
    kind: "tool",
    id: "tool-test-registry",
    turnId: "turn-1",
    callId: "call-test-registry",
    parentCallId: null,
    name: "Bash",
    detail: {
      kind: "command",
      cmd: "pnpm vitest run src/code/CodeSessionRegistry.test.ts",
      cwd: "/repo/crates/tidebreak-desktop/ui",
    },
    status: "succeeded",
    preview: "60 tests passed",
    startedAt: null,
    durationMs: 2_300,
  },
  {
    kind: "assistant",
    id: "assistant-final",
    turnId: "turn-1",
    parentCallId: null,
    text: "Workspace switching now reuses recent transcript stores and reconnects from the last event sequence.",
    streaming: false,
  },
  {
    kind: "turn_boundary",
    id: "boundary-turn-1",
    turnId: "turn-1",
    status: "completed",
    durationMs: 58_000,
    usage: null,
    error: null,
    diffstat: { files: 4, insertions: 226, deletions: 29, truncated: false },
  },
];

/**
 * A code turn keeps its progress messages around tool phases, but only the
 * final assistant message owns the turn-level copy action.
 */
const meta = {
  title: "Code/Transcript",
  component: CodeTranscript,
  parameters: { layout: "fullscreen" },
  args: { items },
  decorators: [
    (Story) => (
      <div className="bg-page-background h-screen">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CodeTranscript>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ProgressUpdates: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getAllByRole("button", { name: "Copy" })).toHaveLength(
      1,
    );
  },
};

const rewrittenItems: CodeTranscriptItem[] = items.map((item) =>
  item.kind === "assistant" && item.id === "assistant-final"
    ? {
        ...item,
        rewrite:
          "Workspace switching reuses recent transcript stores. It reconnects from the last event sequence.",
        rewriteState: "rewritten",
      }
    : item,
);

/** Off: the original closing message, no recap. */
export const RewriteOff: Story = {
  args: { items },
};

/** Rewriting: the original stays visible while the recap is written. */
export const RewriteRewriting: Story = {
  args: {
    items: items.map((item) =>
      item.kind === "assistant" && item.id === "assistant-final"
        ? { ...item, rewriteState: "rewriting" }
        : item,
    ),
  },
};

/** Rewritten: original stays, recap sits under it. */
export const RewriteRewritten: Story = {
  args: { items: rewrittenItems },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Recap")).toBeInTheDocument();
  },
};

/** Failed: the original stands, and the transcript says so. */
export const RewriteFailed: Story = {
  args: {
    items: items.map((item) =>
      item.kind === "assistant" && item.id === "assistant-final"
        ? { ...item, rewriteState: "failed" }
        : item,
    ),
  },
};

const failedTurn: CodeTranscriptItem[] = [
  items[0],
  {
    kind: "assistant",
    id: "assistant-failed",
    turnId: "turn-1",
    parentCallId: null,
    text: "Loading the registry now.",
    streaming: false,
  },
  {
    kind: "turn_boundary",
    id: "boundary-failed",
    turnId: "turn-1",
    status: "failed",
    durationMs: 4_000,
    usage: null,
    error:
      "claude exited with status 1: ENOENT: no such file or directory, open 'crates/tidebreak-desktop/ui/src/code/CodeSessionRegistry.ts'",
    diffstat: null,
  },
];

/**
 * A failed turn is the one outcome that must never be silent, and the way
 * out of it sits on the failure: File an issue hands the session to Uneff me.
 */
export const FailedTurnFilesIssue: Story = {
  args: { items: failedTurn, onFileIssue: fn() },
  play: async ({ args, canvasElement }) => {
    const canvas = within(canvasElement);
    const alert = await canvas.findByRole("alert");
    await expect(alert).toHaveTextContent("Turn failed");
    await userEvent.click(
      within(alert).getByRole("button", { name: "File an issue" }),
    );
    await expect(args.onFileIssue).toHaveBeenCalledTimes(1);
  },
};

const engineError: CodeTranscriptItem[] = [
  items[0],
  {
    kind: "notice",
    id: "notice-error",
    level: "error",
    message:
      "The engine could not reach the model gateway: 502 Bad Gateway after 3 retries.",
  },
];

/** An engine error carries the same way out. Warnings and asides do not. */
export const EngineErrorFilesIssue: Story = {
  args: { items: engineError, onFileIssue: fn() },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const alert = await canvas.findByRole("alert");
    await expect(
      within(alert).getByRole("button", { name: "File an issue" }),
    ).toBeVisible();
  },
};

const pastedReport = JSON.stringify(
  {
    session: { id: "sess-1", harness_kind: "claude_code" },
    turns: Array.from({ length: 6 }, (_, index) => ({
      id: `turn-${index + 1}`,
      status: index === 5 ? "failed" : "completed",
    })),
    events: Array.from({ length: 40 }, (_, index) => ({
      seq: index,
      type: index % 2 ? "tool_started" : "tool_finished",
    })),
  },
  null,
  2,
);

const pastedTurn: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "user-uneff",
    turnId: "turn-uneff",
    text: `The user hit a problem in Tidebreak Code and asked for help.\n\nStart by asking the user what went wrong and what they want.\n\nThe debug report follows as pasted text.\n\n<pasted_text>\n${pastedReport}\n</pasted_text>`,
    createdAt: "2026-09-02T15:10:00.000Z",
  },
  {
    kind: "assistant",
    id: "assistant-uneff",
    turnId: "turn-uneff",
    parentCallId: null,
    text: "Before I dig in: what went wrong, and would you like an issue filed or a fix opened as a pull request?",
    streaming: false,
  },
];

/**
 * A long paste goes out folded behind a chip, and comes back folded: the
 * Uneff me first turn carries its whole debug report without showing it.
 */
export const PastedTextFolded: Story = {
  args: { items: pastedTurn },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const toggle = await canvas.findByRole("button", {
      name: /Pasted text/,
    });
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(canvas.queryByText(/"harness_kind"/)).toBeNull();
    await userEvent.click(toggle);
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await expect(canvas.getByText(/"harness_kind"/)).toBeVisible();
  },
};

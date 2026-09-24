import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import { CodeTranscript } from "@/code/CodeTranscript";
import type { CodeTranscriptItem } from "@/code/CodeSessionReducer";
import { messageWithReviewComments } from "@/code/diff/reviewComments";

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

export const ProgressUpdates: Story = {};

/**
 * One shared session driven by four actors: the owner on the desktop, a
 * teammate whose channel sent a display name, a teammate whose channel sent
 * none and renders as the generic "Slack user", and a trigger firing under
 * the owner's key. A turn with no actor is the owner's, and carries no name,
 * which is what every unshared session looks like. Raw principal keys never
 * render.
 */
export const SharedSessionActors: Story = {
  args: {
    items: [
      {
        kind: "user",
        id: "actor-owner",
        turnId: "turn-owner",
        text: "Trace where the session access rows resolve.",
        createdAt: "2026-09-04T09:00:00.000Z",
      },
      {
        kind: "assistant",
        id: "actor-owner-reply",
        turnId: "turn-owner",
        parentCallId: null,
        text: "Resolution happens once, in `ScopedCode::session_access`.",
        streaming: false,
      },
      {
        kind: "user",
        id: "actor-ines",
        turnId: "turn-ines",
        text: "Keep the display name the channel sent on the turn.",
        createdAt: "2026-09-04T09:02:00.000Z",
        actorLabel: "Ines Okafor",
      },
      {
        kind: "assistant",
        id: "actor-ines-reply",
        turnId: "turn-ines",
        parentCallId: null,
        text: "The grant supplies the channel identity; the adapter supplies the name.",
        streaming: false,
      },
      {
        kind: "user",
        id: "actor-nameless-channel",
        turnId: "turn-nameless-channel",
        text: "Ship it once the checks settle.",
        createdAt: "2026-09-04T09:03:00.000Z",
        actorLabel: "Slack user",
      },
      {
        kind: "assistant",
        id: "actor-nameless-channel-reply",
        turnId: "turn-nameless-channel",
        parentCallId: null,
        text: "Will do. The channel sent no display name, so the origin renders generically.",
        streaming: false,
      },
      {
        kind: "user",
        id: "actor-trigger",
        turnId: "turn-trigger",
        text: "Checks failed on this pull request. Take a look.",
        createdAt: "2026-09-04T09:04:00.000Z",
        actorLabel: "Trigger: checks failed",
      },
    ],
  },
};

/** Internal trigger delivery stays out of the transcript after the turn runs. */
export const TriggerFireNoticeHidden: Story = {
  args: {
    items: [
      {
        kind: "assistant",
        id: "assistant-trigger-result",
        turnId: "turn-trigger",
        parentCallId: null,
        text: "I addressed the requested changes and reran the focused tests.",
        streaming: false,
      },
      {
        kind: "notice",
        id: "trigger-fire",
        level: "info",
        message:
          "trigger d655b722-50c4-4560-b09b-991a56182771 fired: changes requested on #3105",
      },
      {
        kind: "turn_boundary",
        id: "boundary-trigger",
        turnId: "turn-trigger",
        status: "completed",
        durationMs: 130_000,
        usage: null,
        error: null,
        diffstat: {
          files: 1,
          insertions: 5,
          deletions: 5,
          truncated: false,
        },
      },
    ],
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText(/addressed the requested changes/),
    ).toBeInTheDocument();
    await expect(
      canvas.queryByText(/trigger d655b722/),
    ).not.toBeInTheDocument();
  },
};

/** A fallback recap stays quiet beneath the turn seam. */
export const SessionRecap: Story = {
  args: {
    items,
    recap:
      "Workspace switching reuses recent transcript stores. Next: verify the reconnect path under rapid workspace changes.",
  },
};

/** A turn with no closing message keeps the same quiet boundary recap. */
export const SessionRecapWithoutClosingMessage: Story = {
  args: {
    items: [
      items[0],
      {
        kind: "turn_boundary",
        id: "boundary-interrupted",
        turnId: "turn-1",
        status: "interrupted",
        durationMs: 18_000,
        usage: null,
        error: null,
        diffstat: null,
      },
    ],
    recap: "The interrupted turn left the transcript store unchanged.",
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

/** Rewritten: original stays, and the recap moves to the quiet turn seam. */
export const RewriteRewritten: Story = {
  args: { items: rewrittenItems },
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
};

const reviewTurn: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "user-review",
    turnId: "turn-review",
    text: messageWithReviewComments("Fix these, then run the queue tests.", [
      {
        id: "r1",
        author: { kind: "person" },
        path: "crates/tidebreak-desktop/ui/src/code/sessionQueue.ts",
        lines: [
          { kind: "del", oldNo: 18, newNo: null, text: "  text: string;" },
          { kind: "add", oldNo: null, newNo: 18, text: "  message: string;" },
        ],
        body: "Keep the old field name. The server journals it as `text`.",
        createdAt: "2026-09-24T10:00:00.000Z",
      },
      {
        id: "r2",
        author: { kind: "person" },
        path: "crates/tidebreak-desktop/ui/src/code/sessionQueue.ts",
        lines: [
          {
            kind: "del",
            oldNo: 22,
            newNo: null,
            text: "const MAX_QUEUED = 10;",
          },
        ],
        body: "Why double the limit? The tray was designed around ten rows.",
        createdAt: "2026-09-24T10:01:00.000Z",
      },
      {
        id: "r3",
        author: { kind: "person" },
        path: "crates/tidebreak-server/src/code/queue.rs",
        lines: [
          { kind: "add", oldNo: null, newNo: 88, text: "    MAX_QUEUED_TURNS" },
        ],
        body: "Match the UI's limit here.",
        createdAt: "2026-09-24T10:02:00.000Z",
      },
      {
        id: "r4",
        author: { kind: "person" },
        path: "crates/tidebreak-server/src/code/queue.rs",
        lines: [{ kind: "context", oldNo: 90, newNo: 91, text: "    }" }],
        body: "And add a test for the refusal.",
        createdAt: "2026-09-24T10:03:00.000Z",
      },
    ]),
    createdAt: "2026-09-24T10:05:00.000Z",
  },
  {
    kind: "assistant",
    id: "assistant-review",
    turnId: "turn-review",
    parentCallId: null,
    text: "I'll keep `text`, put the limit back to ten on both sides, and add the refusal test.",
    streaming: false,
  },
];

/**
 * Diff comments went out with the message. The transcript lists them one
 * line each and keeps the quoted lines behind the disclosure.
 */
export const ReviewCommentsFolded: Story = {
  args: { items: reviewTurn },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("4 comments on 2 files"),
    ).resolves.toBeVisible();
  },
};

/** Opened, each comment shows the lines it quoted. */
export const ReviewCommentsOpen: Story = {
  args: { items: reviewTurn },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: /Review comments/ }),
    );
    await expect(
      canvas.findByText("-const MAX_QUEUED = 10;"),
    ).resolves.toBeVisible();
  },
};

const steerWithReviewTurn: CodeTranscriptItem[] = [
  {
    kind: "user",
    id: "user-steer-review",
    turnId: "turn-steer-review",
    text: "Move the queue limit into one constant both sides read.",
    createdAt: "2026-09-24T10:05:00.000Z",
  },
  {
    kind: "assistant",
    id: "assistant-steer-review",
    turnId: "turn-steer-review",
    parentCallId: null,
    text: "I'll start with the server's constant and point the UI at it.",
    streaming: false,
  },
  {
    kind: "steer",
    id: "steer-review",
    turnId: "turn-steer-review",
    text: messageWithReviewComments("Before you go on, look at these two.", [
      {
        id: "s1",
        author: { kind: "person" },
        path: "crates/tidebreak-server/src/code/queue.rs",
        lines: [
          {
            kind: "add",
            oldNo: null,
            newNo: 12,
            text: "pub const MAX_QUEUED_TURNS: usize = 20;",
          },
        ],
        body: "Ten, not twenty. The tray was designed around ten rows.",
        createdAt: "2026-09-24T10:06:00.000Z",
      },
      {
        id: "s2",
        author: { kind: "person" },
        path: "crates/tidebreak-desktop/ui/src/code/sessionQueue.ts",
        lines: [
          {
            kind: "del",
            oldNo: 22,
            newNo: null,
            text: "const MAX_QUEUED = 10;",
          },
        ],
        body: "Keep this one until the server sends its limit.",
        createdAt: "2026-09-24T10:06:30.000Z",
      },
    ]),
  },
];

/**
 * Comments added to a steer fold into the same block as a message's,
 * above the note that the turn was steered.
 */
export const ReviewCommentsInASteer: Story = {
  args: { items: steerWithReviewTurn },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("2 comments on 2 files"),
    ).resolves.toBeVisible();
    await expect(canvas.findByText("Steered mid-turn")).resolves.toBeVisible();
    await expect(canvas.queryByText(/review_comments/)).toBeNull();
  },
};

export const Notices: Story = {
  args: {
    onFileIssue: fn(),
    items: [
      {
        kind: "notice",
        id: "notice-info",
        level: "info",
        message: "Session resumed.",
      },
      {
        kind: "notice",
        id: "notice-warning",
        level: "warning",
        message:
          "The provider is busy. The session will retry when capacity is available.",
      },
      {
        kind: "notice",
        id: "notice-error",
        level: "error",
        message:
          "The connection closed before the response completed. Check the connection and try again.",
      },
      {
        kind: "notice",
        id: "notice-credential-refused",
        level: "warning",
        message:
          "Push refused: this external connection has no live gateway delegation; reconnect it from Slack. Reconnect this session from Slack; a newer connect or a revoke ended the one it used.",
      },
      {
        kind: "notice",
        id: "notice-path",
        level: "error",
        message:
          "Unable to read /workspace/" +
          "long-directory-name/".repeat(12) +
          "output.log",
      },
    ],
  },
  play: async ({ canvasElement }) => {
    const notices = Array.from(
      canvasElement.querySelectorAll<HTMLElement>(
        ".message-notice, .message-turn-failure, .notice-surface",
      ),
    );
    await expect(notices.length).toBeGreaterThan(2);
    const first = notices[0].getBoundingClientRect();
    for (const notice of notices) {
      const rect = notice.getBoundingClientRect();
      await expect(Math.abs(rect.width - first.width)).toBeLessThan(1);
      await expect(Math.abs(rect.left - first.left)).toBeLessThan(1);
      await expect(notice.scrollWidth).toBeLessThanOrEqual(notice.clientWidth);
    }
  },
};

const groupedTools: CodeTranscriptItem[] = [
  {
    kind: "tool",
    id: "group-command",
    turnId: "group-turn",
    callId: "group-command",
    parentCallId: null,
    name: "Bash",
    detail: {
      kind: "command",
      cmd: "pnpm vitest run src/code/CodeCatalogStore.test.ts --reporter=verbose",
      cwd: "/workspace/crates/tidebreak-desktop/ui",
    },
    status: "succeeded",
    preview: "All tests passed",
    startedAt: null,
    durationMs: 2_000,
  },
  {
    kind: "tool",
    id: "group-edit",
    turnId: "group-turn",
    callId: "group-edit",
    parentCallId: null,
    name: "Edit",
    detail: {
      kind: "file_edit",
      path: "/workspace/crates/tidebreak-desktop/ui/src/code/CodeCatalogStore.ts",
    },
    status: "succeeded",
    preview: "Updated the workspace catalog.",
    startedAt: null,
    durationMs: 200,
  },
  {
    kind: "tool",
    id: "group-failed",
    turnId: "group-turn",
    callId: "group-failed",
    parentCallId: null,
    name: "Bash",
    detail: { kind: "command", cmd: "pnpm test", cwd: "/workspace" },
    status: "failed",
    preview: "One test failed.",
    startedAt: null,
    durationMs: 1_000,
  },
];

/** Nested icons remain inside the group border at compact transcript widths. */
export const ExpandedToolGroup: Story = {
  args: { items: groupedTools },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole("button", { name: /and 2 more/ }));
    const icons = canvasElement.querySelectorAll<HTMLElement>(
      ".tool-card-shell .tool-card-shell .tool-card-icon",
    );
    await expect(icons.length).toBe(3);
    for (const icon of icons) {
      const row = icon.closest("section")!;
      const body = row.parentElement!;
      const iconRect = icon.getBoundingClientRect();
      const bodyRect = body.getBoundingClientRect();
      await expect(iconRect.left).toBeGreaterThanOrEqual(bodyRect.left + 1);
      await expect(iconRect.right).toBeLessThanOrEqual(bodyRect.right);
      await expect(body.scrollWidth).toBeLessThanOrEqual(body.clientWidth);
      const title = icon.nextElementSibling!;
      const metadata = title.nextElementSibling!;
      await expect(title.getBoundingClientRect().right).toBeLessThanOrEqual(
        metadata.getBoundingClientRect().left,
      );
      await expect(getComputedStyle(title.firstElementChild!).overflowX).toBe(
        "hidden",
      );
    }
  },
};

export const ExpandedRunningToolGroup: Story = {
  ...ExpandedToolGroup,
  args: {
    items: groupedTools.map((item) =>
      item.id === "group-failed" && item.kind === "tool"
        ? { ...item, status: "running", durationMs: null }
        : item,
    ),
  },
};

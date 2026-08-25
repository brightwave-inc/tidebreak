import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, within } from "storybook/test";

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

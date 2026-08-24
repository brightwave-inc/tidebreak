import type { Meta, StoryObj } from "@storybook/react-vite";

import { CodeActivityGroup } from "@/code/CodeTranscript";
import type { CodeTranscriptItem } from "@/code/CodeSessionReducer";

type CodeToolItem = Extract<CodeTranscriptItem, { kind: "tool" }>;

function call(
  id: string,
  detail: CodeToolItem["detail"],
  overrides: Partial<CodeToolItem> = {},
): CodeToolItem {
  return {
    kind: "tool",
    id,
    turnId: "t1",
    callId: id,
    parentCallId: null,
    name: "Bash",
    detail,
    status: "succeeded",
    preview: "",
    startedAt: null,
    durationMs: 600,
    ...overrides,
  };
}

const SETTLED: CodeToolItem[] = [
  call(
    "g1",
    { kind: "search", query: "Fenced|reap|pid.reuse|start_time" },
    {
      name: "Grep",
      preview: "6 matches",
      durationMs: 400,
    },
  ),
  call(
    "g2",
    { kind: "file_read", path: "docs/code-mode.md" },
    { name: "Read" },
  ),
  call(
    "g3",
    { kind: "search", query: "pid_reuse|OrphanAlive|probe_pid" },
    { name: "Grep", preview: "2 matches", durationMs: 300 },
  ),
  call(
    "g4",
    { kind: "file_read", path: "crates/tidebreak-server/src/code/recovery.rs" },
    { name: "Read", durationMs: 900 },
  ),
];

/**
 * A run of tool calls behind one line, and the reason the code transcript no
 * longer reads as a log. The states worth looking at are the ones that decide
 * whether the reader has to open it: a live run naming the call still going, a
 * settled one totalling the phase, and a run holding a failure. Every state
 * stays folded until the reader asks for its rows.
 */
const meta = {
  title: "Code/Activity group",
  component: CodeActivityGroup,
  args: {
    tools: SETTLED,
    signature: "settled",
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CodeActivityGroup>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Four calls, one line, and the run's total time. */
export const Settled: Story = {};

/**
 * While the phase is live the line names the call still running, so a
 * supervisor sees what the engine is doing without opening anything.
 */
export const Running: Story = {
  args: {
    tools: [
      ...SETTLED.slice(0, 3),
      call(
        "g4",
        {
          kind: "command",
          cmd: "cargo test -p tidebreak-server code::recovery",
          cwd: "/repo",
        },
        { status: "running", durationMs: null },
      ),
    ],
    signature: "running",
  },
};

/** A failure marks the folded group without opening its rows. */
export const HoldsAFailure: Story = {
  args: {
    tools: [
      ...SETTLED.slice(0, 3),
      call(
        "g4",
        { kind: "command", cmd: "cargo check -p tidebreak-core", cwd: "/repo" },
        {
          status: "failed",
          durationMs: 4_400,
          preview:
            "error[E0308]: mismatched types\n  --> src/lib.rs:41:9\n   |\n41 |         Ok(())\n   |         ^^^^^^ expected `Turn`, found `()`\n\nexit code 1",
        },
      ),
    ],
    signature: "failed",
  },
};

/** The smallest run that groups at all: two calls, one line. */
export const TwoCalls: Story = {
  args: {
    tools: SETTLED.slice(0, 2),
    signature: "pair",
  },
};

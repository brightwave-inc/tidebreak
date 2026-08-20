import type { Meta, StoryObj } from "@storybook/react-vite";

import { CodeToolCard } from "@/code/CodeTranscript";

/**
 * The code transcript's boxless tool line: verb, mono subject, and trailing
 * meta on one row. The states that matter are the ones that stress that row —
 * a worktree-prefixed command long enough to force middle truncation, a
 * running call streaming its tail, and the failed line that opens itself.
 */
const meta = {
  title: "Code/Tool line",
  component: CodeToolCard,
  args: {
    name: "Bash",
    detail: { kind: "command", cmd: "cargo test -p tidebreak-core", cwd: "/repo" },
    status: "succeeded",
    preview: "",
    startedAt: "2026-08-15T12:00:00.000Z",
    durationMs: 1_800,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CodeToolCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Succeeded: Story = {};

/**
 * Headless engines lead every command with `cd <worktree> && `. The line
 * drops that prefix and middle-truncates what is left, so the verb, the
 * command's head and tail, and the trailing meta all stay on one row.
 */
export const LongWorktreeCommand: Story = {
  args: {
    detail: {
      kind: "command",
      cmd: 'cd "/Users/me/Library/Application Support/io.example.dev/code/worktrees/3d751509-02de-4f33" && pnpm vitest run src/code/CodeTranscript.dom.test.tsx --reporter=dot',
      cwd: "/repo",
    },
    durationMs: 6_100,
  },
};

/** A running call streams the last lines of output without growing the row. */
export const Running: Story = {
  args: {
    status: "running",
    durationMs: null,
    preview: "Compiling tidebreak-core v0.4.2\nCompiling tidebreak-harness v0.4.2\n",
  },
};

/** Failure opens itself: the output and exit code are the point. */
export const Failed: Story = {
  args: {
    status: "failed",
    durationMs: 4_400,
    preview:
      "error[E0308]: mismatched types\n  --> src/lib.rs:41:9\n   |\n41 |         Ok(())\n   |         ^^^^^^ expected `Turn`, found `()`\n\nexit code 1",
  },
};

export const Denied: Story = {
  args: {
    status: "denied",
    durationMs: null,
    preview: "The reader denied this command.",
  },
};

export const FileRead: Story = {
  args: {
    name: "Read",
    detail: { kind: "file_read", path: "crates/tidebreak-desktop/ui/src/code/CodeTranscript.tsx" },
    durationMs: 300,
  },
};

export const Search: Story = {
  args: {
    name: "Grep",
    detail: { kind: "search", query: "humanizeShellCommand" },
    preview: "src/code/CodeTranscript.tsx:651",
    durationMs: 500,
  },
};

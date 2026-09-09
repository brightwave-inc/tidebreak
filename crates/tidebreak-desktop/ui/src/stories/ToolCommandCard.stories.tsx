import type { Meta, StoryObj } from "@storybook/react-vite";

import { ToolCommandCard } from "@/ToolCallCard";

/**
 * Chat journal command rows: grounded title, quiet success, bounded running
 * tail, and a failed row that surfaces the error excerpt while keeping the
 * exact command on expansion. Framed in `.messages-column` so the icon gutter
 * and reading width match the live transcript.
 */
const meta = {
  title: "Conversation/Tool command",
  component: ToolCommandCard,
  args: {
    name: "exec",
    status: "completed" as const,
    preview: {
      tool: "exec" as const,
      command: "pnpm",
      args: ["exec", "biome", "check", "src/stories"],
      cwd: "crates/tidebreak-desktop/ui",
      files: [],
    },
    result: {
      tool: "exec" as const,
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      stdout: "Checked 12 files in 41ms. No fixes applied.\n",
      stderr: "",
      backend: "local" as const,
    },
  },
  decorators: [
    (Story) => (
      <div className="messages bg-background min-h-[28rem]">
        <div className="messages-column">
          <p className="text-md text-foreground leading-relaxed">
            Assistant prose shares this reading column. Tool titles should start
            on the same left edge; long commands must not widen it.
          </p>
          <Story />
        </div>
      </div>
    ),
  ],
  parameters: {
    layout: "fullscreen",
  },
} satisfies Meta<typeof ToolCommandCard>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Successful settled row: one quiet line plus an optional short fact. */
export const Succeeded: Story = {};

/**
 * Model narration leads when present; the literal argv stays on the command
 * tab and is what approvals still show.
 */
export const Narrated: Story = {
  args: {
    preview: {
      tool: "exec",
      command: "cargo",
      args: ["test", "-p", "tidebreak-core", "--", "preview"],
      cwd: ".",
      files: [],
      summary: "Running the core preview tests",
    },
    result: {
      tool: "exec",
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      stdout: "test result: ok. 12 passed; 0 failed\n",
      stderr: "",
      backend: "local",
    },
  },
};

/** Live command: open with a height-bounded output tail. */
export const Running: Story = {
  args: {
    status: "running",
    preview: {
      tool: "exec",
      command: "cargo",
      args: ["test", "--workspace"],
      cwd: ".",
      files: [],
      summary: "Running the workspace tests",
    },
    result: {
      tool: "exec",
      exitCode: null,
      timedOut: false,
      outputTruncated: false,
      stdout: Array.from(
        { length: 20 },
        (_, index) => `Compiling crate_${index} v0.4.2`,
      ).join("\n"),
      stderr: "",
      backend: "local",
    },
  },
};

/** Failure opens with the error excerpt on the row and full streams inside. */
export const Failed: Story = {
  args: {
    status: "failed",
    preview: {
      tool: "exec",
      command: "python3",
      args: ["scripts/render_deck.py", "reports/q3.pptx"],
      cwd: "checkout",
      files: ["reports/q3.pptx"],
    },
    result: {
      tool: "exec",
      exitCode: 1,
      timedOut: false,
      outputTruncated: false,
      stdout: "",
      stderr:
        "Error: Cannot find module 'pptxgenjs'\n    at Object.<anonymous> (scripts/render_deck.py:12)\n",
      backend: "local",
    },
  },
};

/**
 * A command long enough to force middle truncation must not grow the column
 * past the prose edge.
 */
export const LongCommand: Story = {
  args: {
    status: "completed",
    preview: {
      tool: "exec",
      command: "pnpm",
      args: [
        "--dir",
        "crates/tidebreak-desktop/ui",
        "exec",
        "vitest",
        "run",
        "src/ToolCallCard.dom.test.tsx",
        "src/ToolOutputPreview.dom.test.tsx",
        "src/ToolPreview.test.ts",
        "--reporter=dot",
      ],
      cwd: ".",
      files: [],
    },
    result: {
      tool: "exec",
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      stdout: "Tests  14 passed (14)\n",
      stderr: "",
      backend: "local",
    },
  },
};

/** Narrow journal width: truncation and gutter alignment under stress. */
export const Narrow: Story = {
  decorators: [
    (Story) => (
      <div
        className="messages bg-background min-h-[24rem]"
        style={{ width: 420 }}
      >
        <div className="messages-column">
          <p className="text-md text-foreground leading-relaxed">
            Narrow pane: titles still align with prose.
          </p>
          <Story />
        </div>
      </div>
    ),
  ],
  args: {
    ...LongCommand.args,
  },
};

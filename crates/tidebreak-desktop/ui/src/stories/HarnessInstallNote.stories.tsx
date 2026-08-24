import type { Meta, StoryObj } from "@storybook/react-vite";

import { HarnessInstallNote } from "@/code/HarnessInstallNote";

/**
 * The download line under the engine picker in New workspace.
 *
 * A pinned engine this machine has never fetched is a 37-297MB npm install.
 * It runs ahead of create now, so the dialog says what is happening instead
 * of stalling on the Create button.
 */
const meta = {
  title: "Code/Harness install note",
  component: HarnessInstallNote,
  args: {
    install: {
      kind: "claude_code",
      version: "2.1.234",
      phase: "installing",
      done: false,
    },
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-sm pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof HarnessInstallNote>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The download is running. */
export const Downloading: Story = {};

/** The download failed; the reason stays on screen rather than in a toast. */
export const Failed: Story = {
  args: {
    install: {
      kind: "codex",
      version: "0.147.0",
      phase: "failed",
      done: true,
      error: "npm install @openai/codex@0.147.0 timed out",
    },
  },
};

/** Engines already on disk render nothing — the picker already shows them. */
export const Ready: Story = {
  args: {
    install: {
      kind: "claude_code",
      version: "2.1.234",
      phase: "ready",
      done: true,
    },
  },
};

/** Nothing was ever started for this engine. */
export const Absent: Story = {
  args: { install: undefined },
};

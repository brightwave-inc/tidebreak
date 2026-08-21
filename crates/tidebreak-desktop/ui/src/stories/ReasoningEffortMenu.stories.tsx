import type { Meta, StoryObj } from "@storybook/react-vite";

import type { ReasoningEffort } from "@/api/types";
import { ReasoningEffortMenu } from "@/code/CodeComposer";

/**
 * The code composer's reasoning-effort control.
 *
 * The ladder is per engine and per model, not one global scale, so each story
 * below is a real one: Codex reaches `ultra`, Claude Code's top rung is
 * ultracode, grok stops at `xhigh`, and opencode offers nothing at all.
 *
 * The top rung of whatever ladder is on offer wears the violet treatment. It
 * is the one level that changes what a turn costs and how long it runs by more
 * than a step, so the control says so while it is selected. Which level that
 * is differs by engine, which is why the treatment keys on position rather
 * than on a level name.
 */
const meta = {
  title: "Composer/Reasoning effort",
  component: ReasoningEffortMenu,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ReasoningEffortMenu>;

export default meta;
type Story = StoryObj<typeof meta>;

/** What Codex 0.147 advertises for most catalog rows. */
const CODEX: ReasoningEffort[] = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

/** `claude --effort`, plus ultracode as the rung above it. */
const CLAUDE_CODE: ReasoningEffort[] = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

/** `grok --reasoning-effort`, which tops out below `max`. */
const GROK: ReasoningEffort[] = ["low", "medium", "high", "xhigh"];

/** Nothing chosen: the engine's own default is in force. */
export const Default: Story = {
  args: { levels: CODEX, value: null, onChange: () => undefined },
};

/** A middle rung reads as an ordinary selected control. */
export const Mid: Story = {
  args: { levels: CODEX, value: "high", onChange: () => undefined },
};

/** Codex's own top rung, which it spells `ultra`. */
export const UltraOnCodex: Story = {
  args: { levels: CODEX, value: "ultra", onChange: () => undefined },
};

/** The same rung on Claude Code, where it composes ultracode. */
export const UltraOnClaudeCode: Story = {
  args: { levels: CLAUDE_CODE, value: "ultra", onChange: () => undefined },
};

/**
 * Grok stops at `xhigh`, so that is its top rung and it takes the treatment.
 * Nothing hard-codes `ultra`.
 */
export const TopOfAShorterLadder: Story = {
  args: { levels: GROK, value: "xhigh", onChange: () => undefined },
};

/** Below the top on a short ladder: no treatment. */
export const MidOfAShorterLadder: Story = {
  args: { levels: GROK, value: "medium", onChange: () => undefined },
};

/** Mid-turn: the level is fixed until the turn ends. */
export const Locked: Story = {
  args: {
    levels: CODEX,
    value: "ultra",
    disabled: true,
    onChange: () => undefined,
  },
};

/**
 * An engine with no effort control renders nothing, rather than an empty menu
 * the reader can open and find nothing in.
 */
export const Unsupported: Story = {
  args: { levels: [], value: null, onChange: () => undefined },
};

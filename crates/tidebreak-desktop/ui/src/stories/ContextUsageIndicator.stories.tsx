import type { Meta, StoryObj } from "@storybook/react-vite";

import { ContextUsageIndicator } from "@/ContextUsageIndicator";
import {
  contextUsageCritical,
  contextUsageNoReading,
  contextUsageNormal,
  contextUsageUnmetered,
  contextUsageWarning,
} from "./fixtures";

/**
 * The composer's context ring.
 *
 * Every story shares one turn's spend — 258,738 prompt-side tokens across six
 * model calls — and varies only the prompt left resident at the end. That is
 * the point: the ring must move with the resident prompt and ignore the sum,
 * which would clamp all five of these to a red 100%.
 *
 * Hover any of them for the split: the context line, then the turn's spend.
 */
const meta = {
  title: "Composer/Context ring",
  component: ContextUsageIndicator,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ContextUsageIndicator>;

export default meta;
type Story = StoryObj<typeof meta>;

/** 22% of a 200k window, from a turn that spent nearly six times that. */
export const Normal: Story = {
  args: contextUsageNormal,
};

/** 79%: the ring takes the warning colour from the trigger, not the arc. */
export const Warning: Story = {
  args: contextUsageWarning,
};

/** 96%: the only state that should ever read destructive. */
export const Critical: Story = {
  args: contextUsageCritical,
};

/** No published window. Tokens are still honest without a denominator. */
export const Unmetered: Story = {
  args: contextUsageUnmetered,
};

/** The engine published nothing usable: an empty track, and no percent. */
export const NoReading: Story = {
  args: contextUsageNoReading,
};

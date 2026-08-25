import type { Meta, StoryObj } from "@storybook/react-vite";

import { AttentionBadge } from "@/code/AttentionBadge";
import {
  attentionDoneUnreviewed,
  attentionFenced,
  attentionManual,
  attentionNeedsYou,
  attentionStalled,
  attentionWorking,
} from "./fixtures";

/**
 * The server-computed attention vocabulary, as list surfaces wear it. Working
 * deliberately renders no full pill; every state stays tellable at a glance
 * in the rail's compact mark without relying on color alone.
 */
const meta = {
  title: "Code/Attention badge",
  component: AttentionBadge,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 pt-8 pl-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof AttentionBadge>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Renders nothing on purpose: quiet is the default. */
export const Working: Story = {
  args: { attention: attentionWorking },
};

export const NeedsYou: Story = {
  args: { attention: attentionNeedsYou },
};

export const Stalled: Story = {
  args: { attention: attentionStalled },
};

export const DoneUnreviewed: Story = {
  args: { attention: attentionDoneUnreviewed },
};

export const Fenced: Story = {
  args: { attention: attentionFenced },
};

export const ManualPin: Story = {
  args: { attention: attentionManual },
};

/** The rail, tab, and card marks for every state, side by side. */
export const CompactMarks: Story = {
  args: { attention: attentionNeedsYou, compact: true },
  render: () => (
    <>
      <AttentionBadge attention={attentionWorking} compact />
      <AttentionBadge attention={attentionNeedsYou} compact />
      <AttentionBadge attention={attentionStalled} compact />
      <AttentionBadge attention={attentionDoneUnreviewed} compact />
      <AttentionBadge attention={attentionFenced} compact />
      <AttentionBadge attention={attentionManual} compact />
    </>
  ),
};

import type { Meta, StoryObj } from "@storybook/react-vite";

import { ThinkingAccordion } from "@/ThinkingAccordion";

/**
 * The model's reasoning for one step, behind a disclosure. While the
 * reasoning is still arriving the label says Thinking and shimmers; once the
 * answer starts it says Thought and sits still.
 */
const meta = {
  title: "Conversation/Thinking accordion",
  component: ThinkingAccordion,
  args: {
    text: "I am comparing the dense tool phase with the compact transcript and checking which details should stay collapsed.",
    streaming: true,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-prose pt-12">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ThinkingAccordion>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Reasoning is still arriving, so the line is open and the label shimmers. */
export const Streaming: Story = {};

/** The answer has started. The line is closed and the label is still. */
export const Settled: Story = {
  args: { streaming: false },
};

/**
 * Code mode pauses to think between tool calls. The label still shimmers, but
 * the body stays closed so stacked thoughts do not fill the viewport.
 */
export const FoldedWhileStreaming: Story = {
  args: { expandWhileStreaming: false },
};

import type { Meta, StoryObj } from "@storybook/react-vite";

import { MemoryProposalChip } from "@/code/MemoryProposalChip";

const meta = {
  title: "Code/Memory proposal chip",
  component: MemoryProposalChip,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 pt-8 pl-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof MemoryProposalChip>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Quiet when the session has no pending proposals. */
export const None: Story = {
  args: { count: 0 },
};

export const One: Story = {
  args: { count: 1 },
};

export const Many: Story = {
  args: { count: 4 },
};

import type { Meta, StoryObj } from "@storybook/react-vite";

import { PanelLoading } from "@/components/PanelLoading";

const meta = {
  title: "Foundations/Panel loading",
  component: PanelLoading,
  args: {
    variant: "list",
    label: "Loading…",
    rows: 6,
  },
} satisfies Meta<typeof PanelLoading>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ListRows: Story = {};

export const ViewerSpinner: Story = {
  args: { variant: "viewer" },
  decorators: [
    (Story) => (
      <div className="h-64 rounded-xl border border-border-subtle bg-background">
        <Story />
      </div>
    ),
  ],
};

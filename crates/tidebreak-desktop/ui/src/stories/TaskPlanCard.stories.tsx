import type { Meta, StoryObj } from "@storybook/react-vite";

import { TaskPlanCard } from "@/TaskPlanCard";
import { taskPlan } from "./fixtures";

const meta = {
  title: "Conversation/Task plan",
  component: TaskPlanCard,
  args: {
    plan: taskPlan,
    live: true,
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl pt-12">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof TaskPlanCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Live: Story = {};

export const Settled: Story = {
  args: { live: false },
};

export const LongContent: Story = {
  args: {
    plan: {
      ...taskPlan,
      steps: [
        ...taskPlan.steps,
        {
          content:
            "Check that an unusually long step wraps inside a compact conversation pane without pushing the status glyph or completion count out of view",
          status: "pending",
        },
      ],
    },
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};

import type { Meta, StoryObj } from "@storybook/react-vite";
import { userEvent, within } from "storybook/test";

import { ToolActivityGroup, type ToolActivity } from "@/ToolActivityGroup";

const runningSearch: ToolActivity = {
  id: "search",
  name: "web_search",
  status: "running",
  preview: {
    tool: "search",
    query: "title: Conversation/ OR title: Composer/",
  },
};

const settledSearch: ToolActivity = {
  ...runningSearch,
  status: "completed",
};

const runningRead: ToolActivity = {
  id: "read",
  name: "read_file",
  status: "running",
};

/**
 * One phase of tool work behind a single line. While the phase is live the
 * label shimmers; a settled phase is quiet. `animate` is off so the label is
 * complete and only the shimmer moves.
 */
const meta = {
  title: "Conversation/Tool activity",
  component: ToolActivityGroup,
  args: {
    groupIndex: 0,
    animate: false,
    activities: [runningSearch],
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-prose pt-12">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ToolActivityGroup>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The phase line names the live call and shimmers. */
export const Running: Story = {};

/** The phase has settled. The label is still and the wording is past tense. */
export const Settled: Story = {
  args: { activities: [settledSearch] },
};

/** Expanded rows keep the shimmer on the call that is still running. */
export const RunningExpanded: Story = {
  args: { activities: [runningSearch, runningRead] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("button"));
  },
};

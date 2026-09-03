import type { Meta, StoryObj } from "@storybook/react-vite";
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

const browserWorkflow: ToolActivity[] = [
  {
    id: "browser-list",
    name: "browser_list",
    status: "completed",
  },
  {
    id: "browser-navigate",
    name: "browser_navigate",
    status: "completed",
  },
  {
    id: "browser-snapshot",
    name: "browser_snapshot",
    status: "completed",
  },
  {
    id: "browser-act",
    name: "browser_act",
    status: "completed",
  },
  {
    id: "browser-screenshot",
    name: "browser_screenshot",
    status: "completed",
  },
  {
    id: "browser-wait",
    name: "browser_wait",
    status: "running",
  },
];

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
};

/** Browser work keeps each step legible instead of collapsing to generic tools. */
export const BrowserWorkflow: Story = {
  args: { activities: browserWorkflow },
};

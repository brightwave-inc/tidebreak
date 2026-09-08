import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import { WorkspaceLessSessionRow } from "../code/WorkspaceLessSessionRow";
import {
  idleCompleteDigest,
  needsYouDigest,
  runningDigest,
  stalledDigest,
} from "./fixtures";

const meta = {
  title: "Code/Conversation without workspace",
  component: WorkspaceLessSessionRow,
  args: {
    digest: {
      ...runningDigest,
      workspace: null,
      harness_kind: "internal",
      title: "Research product feedback",
    },
    onOpen: fn(),
  },
  decorators: [
    (Story) => (
      <div className="w-72 max-w-full bg-sidebar p-2">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WorkspaceLessSessionRow>;
export default meta;
type Story = StoryObj<typeof meta>;
export const Running: Story = {
  play: async ({ canvasElement, args }) => {
    await userEvent.click(within(canvasElement).getByRole("button"));
    await expect(args.onOpen).toHaveBeenCalledWith(args.digest.session);
  },
};
export const Completed: Story = {
  args: {
    digest: {
      ...idleCompleteDigest,
      workspace: null,
      title: "Research product feedback",
    },
  },
};
export const NeedsYou: Story = {
  args: {
    digest: { ...needsYouDigest, workspace: null, title: "Review the draft" },
  },
};
export const Failed: Story = {
  args: {
    digest: {
      ...stalledDigest,
      workspace: null,
      title: "Research product feedback",
    },
  },
};
export const LongTitle: Story = {
  args: {
    digest: {
      ...runningDigest,
      workspace: null,
      title:
        "Compare the customer feedback across every release and identify the most common requests",
    },
  },
};
export const Untitled: Story = {
  args: { digest: { ...runningDigest, workspace: null, title: "" } },
};

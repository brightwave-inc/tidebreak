import type { Meta, StoryObj } from "@storybook/react-vite";

import {
  resetWorkflowPromptStore,
  useWorkflowPromptStore,
} from "@/code/workflowPrompts";
import { QuickActionsPanel } from "@/settings/QuickActionsPanel";

/**
 * The prompts workspace actions send into chat. Defaults are the shipped
 * wording; a customized Create PR is the state a reader who has edited one
 * field actually sees, including a live Reset.
 */
const meta = {
  title: "Settings/Quick actions",
  component: QuickActionsPanel,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => {
      resetWorkflowPromptStore();
      return <Story />;
    },
  ],
} satisfies Meta<typeof QuickActionsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Defaults: Story = {};

export const CustomCreatePr: Story = {
  render: () => {
    useWorkflowPromptStore
      .getState()
      .setPrompt(
        "compose_pr",
        [
          "Review the diff, write a focused commit, push, and open a pull",
          "request against {base}. Keep the description short. Do not merge.",
        ].join(" "),
      );
    return <QuickActionsPanel />;
  },
};

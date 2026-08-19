import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { UserQuestionsCard } from "@/UserQuestionsCard";
import { userQuestions } from "./fixtures";

const meta = {
  title: "Conversation/User questions",
  component: UserQuestionsCard,
  args: {
    request: userQuestions,
    working: false,
    error: undefined,
    onAnswer: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof UserQuestionsCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MultipleQuestions: Story = {};

export const SendingFailed: Story = {
  args: {
    working: true,
    error: "The answer could not be sent. Your selections are still here.",
  },
};

export const CompactPane: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};

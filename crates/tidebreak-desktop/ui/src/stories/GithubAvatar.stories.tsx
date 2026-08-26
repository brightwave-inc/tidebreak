import type { Meta, StoryObj } from "@storybook/react-vite";

import { GithubAvatar } from "@/code/GithubAvatar";

const meta = {
  title: "Code/GitHub avatar",
  component: GithubAvatar,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof GithubAvatar>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Image: Story = {
  args: { login: "github" },
};

export const Initials: Story = {
  args: { login: "mira-chen", url: "https://invalid.example/missing.png" },
};

export const MissingLogin: Story = {
  args: { login: undefined },
};

export const Sizes: Story = {
  args: { login: "github" },
  render: () => (
    <div className="flex items-center gap-3">
      <GithubAvatar login="github" className="size-4" />
      <GithubAvatar login="github" className="size-5" />
      <GithubAvatar login="github" className="size-7" />
    </div>
  ),
};

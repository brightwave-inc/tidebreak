import type { Meta, StoryObj } from "@storybook/react-vite";
import { ReviewPrototype } from "./ReviewPages";

const meta = {
  title: "Code/Review proposal/Pages",
  component: ReviewPrototype,
  parameters: { layout: "fullscreen" },
  args: { scenario: "files" },
  render: (args) => <ReviewPrototype key={args.scenario} {...args} />,
} satisfies Meta<typeof ReviewPrototype>;
export default meta;
type Story = StoryObj<typeof meta>;

export const PullRequestInbox: Story = { args: { scenario: "inbox" } };
export const InboxLoading: Story = { args: { scenario: "inbox-loading" } };
export const InboxEmpty: Story = { args: { scenario: "inbox-empty" } };
export const RefreshFailed: Story = { args: { scenario: "inbox-failed" } };
export const ReviewFiles: Story = { args: { scenario: "files" } };
export const FilesLoading: Story = { args: { scenario: "files-loading" } };
export const ReviewDiscussion: Story = { args: { scenario: "discussion" } };
export const ReviewChecks: Story = { args: { scenario: "checks" } };
export const ReadyToMerge: Story = { args: { scenario: "ready" } };
export const SourceControl: Story = { args: { scenario: "source" } };
export const StagedChanges: Story = { args: { scenario: "staged" } };
export const CleanWorkingTree: Story = { args: { scenario: "clean" } };
export const MergeConflict: Story = { args: { scenario: "conflict" } };

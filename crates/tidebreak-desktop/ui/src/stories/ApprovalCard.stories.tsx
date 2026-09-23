import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ApprovalCard } from "@/ApprovalCard";
import { FolderAccessCard } from "@/FolderAccessCard";
import { OutputWritebackCard } from "@/OutputWritebackCard";

import { execPreview } from "./fixtures";
import { toolApprovalPresentation } from "@/ToolCallCard";

const meta = {
  title: "Conversation/Approval",
  component: ApprovalCard,
  args: {
    callId: "call-storybook",
    summary:
      "This command can access the network and the staged files listed below.",
    preview: execPreview,
    canApprove: true,
    canRemember: true,
    grantRungs: ["exact_action", "whole_tool"],
    deciding: false,
    onDecide: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ApprovalCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NetworkedCommand: Story = {};

export const QuestionFocused: Story = {
  parameters: {
    docs: {
      description: {
        story:
          "On mount the card focuses the question, not a choice. Hover only highlights.",
      },
    },
  },
};

export const RepositoryWork: Story = {
  args: {
    ...toolApprovalPresentation("code_session_may_run_repository_agent"),
    grantRungs: [],
    preview: {
      tool: "code_session",
      operation: "create",
      target: "example/repository",
      task: "Inspect the failing test and report the cause.",
      harness: "codex",
      model: "example-model",
    },
  },
};

export const AutoModeJudging: Story = {
  args: { autoJudging: true },
};

export const ProjectGrant: Story = {
  args: { grantScope: "project" },
};

export const DecidingWithError: Story = {
  args: {
    deciding: true,
    error: "The decision could not be saved. The command has not run.",
  },
};

export const FolderAccess = {
  render: () => (
    <FolderAccessCard
      request={{
        callId: "call-folder",
        turnId: "turn-1",
        reason: "The agent wants to read files in a connected folder.",
        folderHint: "documents",
        claimedByDesktop: false,
      }}
      nativeHost
      nativeBusy={false}
      working={false}
      error={undefined}
      onDecision={fn()}
      onCancel={fn()}
    />
  ),
};

export const OutputWriteback = {
  render: () => (
    <OutputWritebackCard
      request={{
        callId: "call-write",
        turnId: "turn-1",
        mode: "create",
        claimedByDesktop: false,
      }}
      nativeHost
      working={false}
      error={undefined}
      onDecision={fn()}
      onCancel={fn()}
    />
  ),
};

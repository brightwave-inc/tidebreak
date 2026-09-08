import type { Meta, StoryObj } from "@storybook/react-vite";

import type { CodeConnectPage } from "@/api";
import { WorkspaceApprovalView } from "@/WorkspaceApprovalRoute";

const acme: CodeConnectPage = {
  channel_kind: "slack",
  display_name: "tidebreak-slack",
  workspace_name: "Acme Corp",
  state: "pending",
  csrf: "story-csrf",
  expires_at: "2026-09-08T12:15:00Z",
};

const meta = {
  title: "Code/Workspace grant approval",
  component: WorkspaceApprovalView,
  args: {
    page: acme,
    phase: "ready",
    error: null,
    onApprove: () => {},
    onRetry: () => {},
  },
} satisfies Meta<typeof WorkspaceApprovalView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ApproveWorkspace: Story = {};

export const Loading: Story = {
  args: { page: null, phase: "loading" },
};

export const Approving: Story = {
  args: { phase: "approving" },
};

export const ApprovedAwaitingAdapter: Story = {
  args: { phase: "approved" },
};

export const LinkNoLongerValid: Story = {
  args: { page: null, phase: "invalid" },
};

export const OpenFailed: Story = {
  args: { page: null, phase: "unavailable" },
};

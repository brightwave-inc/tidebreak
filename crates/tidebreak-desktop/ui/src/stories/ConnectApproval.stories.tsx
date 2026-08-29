import type { Meta, StoryObj } from "@storybook/react-vite";

import type { CodeConnectPage } from "@/api";
import { ConnectApprovalView } from "@/ConnectApprovalRoute";

const casey: CodeConnectPage = {
  channel_kind: "slack",
  display_name: "Casey Nakamura",
  workspace_name: "Acme Corp",
  state: "pending",
  csrf: "story-csrf",
  expires_at: "2026-08-28T12:15:00Z",
};

/**
 * The connect approval a channel's connect card links to. The states that
 * matter: the question itself, the approved hand-back to the channel for
 * its closing confirm, and the dead link a forwarded or expired nonce
 * renders.
 */
const meta = {
  title: "Code/Connect approval",
  component: ConnectApprovalView,
  args: {
    page: casey,
    phase: "ready",
    error: null,
    onApprove: () => {},
  },
} satisfies Meta<typeof ConnectApprovalView>;

export default meta;
type Story = StoryObj<typeof meta>;

/** "Is this you?" — the identity shown is exactly what a grant would bind. */
export const IsThisYou: Story = {};

/** Approved: nothing is linked until the channel-side confirm lands. */
export const ApprovedAwaitingChannelConfirm: Story = {
  args: { phase: "approved" },
};

/** A used, expired, or forwarded-and-consumed link renders its refusal. */
export const LinkNoLongerValid: Story = {
  args: { page: null, phase: "invalid" },
};

/** The approve POST failed; the question stays and the error is spoken. */
export const ApproveFailed: Story = {
  args: { error: "The machine could not be reached. Try again." },
};

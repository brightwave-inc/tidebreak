import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { HostedSignIn } from "@/HostedSignIn";

/**
 * A browser tab served by a hosted machine, before it holds a session.
 *
 * The machine serves the same renderer the desktop app runs, but a tab
 * cannot sign itself in: its bearer comes from the reader's Model Gateway
 * console, once. These are the screens between opening the address and
 * arriving signed in, and the one that follows an hour later.
 */
const meta = {
  title: "Shell/Hosted browser session",
  component: HostedSignIn,
  parameters: { layout: "fullscreen" },
  args: {
    reason: "no_session",
    machineUrl: "https://tidebreak.example.com",
    discovery: {
      mode: "gateway",
      gateway_url: "https://gateway.example.com",
      resource: "tidebreak",
    },
    onRetry: fn(),
  },
} satisfies Meta<typeof HostedSignIn>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Someone opened the machine's address directly, or reloaded a tab that had
 * a session. The page names the console that can send them back signed in.
 */
export const SignInRequired: Story = {};

/**
 * The bearer a tab held stopped being accepted. After an hour that is simply
 * its lifetime, and the copy says so before it says where to go.
 */
export const SessionEnded: Story = {
  args: { reason: "session_ended" },
};

/**
 * A machine on static tokens asks for the administrator-provided token and
 * keeps it only in this tab's memory.
 */
export const StaticToken: Story = {
  args: { discovery: { mode: "static_token" } },
};

export const Oidc: Story = {
  args: {
    discovery: {
      mode: "oidc",
      issuer_name: "login.example.com",
      start_url: "/auth/oidc/start",
    },
  },
};

/**
 * The console's link was followed too late, or twice. The route lands the
 * page anyway, and the page says what a link is good for before sending the
 * reader back for another.
 */
export const HandoffExpired: Story = {
  args: { reason: "handoff_failed", failure: "expired" },
};

/**
 * The machine could not reach the gateway to exchange the code. Nothing
 * about the reader's account is in question, and the copy says so.
 */
export const HandoffUnavailable: Story = {
  args: { reason: "handoff_failed", failure: "unavailable" },
};

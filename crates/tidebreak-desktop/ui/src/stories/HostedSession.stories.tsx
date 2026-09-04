import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { HostedSignIn } from "@/HostedSignIn";

/**
 * A browser tab served by a hosted machine, before it holds a session.
 *
 * The machine serves the same renderer the desktop app runs, and how a tab
 * signs in is the machine's to say: a gateway machine sends the reader to
 * the console that mints its bearers, a token-file machine takes a pasted
 * token, and an OIDC machine starts the flow on the machine itself. These
 * are the screens between opening the address and arriving signed in, and
 * the one that follows an hour later.
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
 * A machine on a token file takes the token its administrator handed out.
 * The tab holds it in memory alone, and forgets it on reload.
 */
export const StaticToken: Story = {
  args: {
    discovery: { mode: "static_token" },
    onToken: fn(async () => true),
  },
};

/**
 * The refusal a wrong token gets. It stays on this screen with the field
 * still filled, because the next thing the reader does is fix the token.
 */
export const StaticTokenRefused: Story = {
  args: {
    discovery: { mode: "static_token" },
    onToken: fn(async () => false),
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.type(canvas.getByLabelText("Token"), "not-the-token");
    await userEvent.click(canvas.getByRole("button", { name: "Sign in" }));
    await expect(await canvas.findByRole("alert")).toBeVisible();
  },
};

/**
 * An OIDC machine offers one button, named for the issuer the operator
 * configured. The flow starts and finishes on the machine.
 */
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
 * The machine could not reach the provider that signs the reader in. Nothing
 * about their account is in question, and the copy says so.
 */
export const HandoffUnavailable: Story = {
  args: { reason: "handoff_failed", failure: "unavailable" },
};

import type { Meta, StoryObj } from "@storybook/react-vite";
import type { McpServerInfo } from "@/api";
import {
  McpOAuthControl,
  McpTierChip,
  mcpServerStatus,
} from "@/settings/McpPanel";
import { SettingsSection, SettingsStatus } from "@/settings/primitives";
import {
  mcpOauthAccessDenied,
  mcpOauthAuthorizing,
  mcpOauthConnected,
  mcpOauthExpired,
  mcpOauthNotConnected,
  mcpOauthRegistrationRefused,
  mcpOauthTimedOut,
  mcpOauthUnsupported,
  mcpSignInServer,
} from "./fixtures";

type Row = { title: string; server: McpServerInfo; busy?: boolean };

/** Every state a remote server's sign-in can be in, as its settings row
 * shows it: the status line says what is going on, and the action under it
 * is the one next step. */
const rows: Row[] = [
  {
    title: "Sign in required",
    server: mcpSignInServer(mcpOauthNotConnected),
  },
  {
    title: "Starting the sign-in",
    server: mcpSignInServer(mcpOauthNotConnected),
    busy: true,
  },
  {
    title: "Waiting for the browser",
    server: mcpSignInServer(mcpOauthAuthorizing),
  },
  { title: "Signed in", server: mcpSignInServer(mcpOauthConnected) },
  {
    title: "Sign-in timed out",
    server: mcpSignInServer(mcpOauthTimedOut),
  },
  {
    title: "Registration refused",
    server: mcpSignInServer(mcpOauthRegistrationRefused),
  },
  { title: "Sign-in expired", server: mcpSignInServer(mcpOauthExpired) },
  { title: "Sign-in denied", server: mcpSignInServer(mcpOauthAccessDenied) },
  {
    title: "Sign-in not supported",
    server: mcpSignInServer(mcpOauthUnsupported, {
      name: "legacy_docs_with_a_long_name",
      url: "https://docs.example.test/mcp",
    }),
  },
];

function SignInRow({ row }: { row: Row }) {
  const status = row.server.oauth_status;
  return (
    <SettingsSection title={row.title}>
      <SettingsStatus {...mcpServerStatus(row.server)} />
      <McpTierChip curated={row.server.curated} />
      {status ? <McpOAuthControl status={status} busy={row.busy} /> : null}
    </SettingsSection>
  );
}

function OauthStatesShowcase({ only }: { only?: string }) {
  const shown = only ? rows.filter((row) => row.title === only) : rows;
  return (
    <div className="mx-auto flex max-w-xl flex-col gap-10 p-6">
      {shown.map((row) => (
        <SignInRow key={row.title} row={row} />
      ))}
    </div>
  );
}

const meta = {
  title: "Settings/MCP OAuth",
  component: OauthStatesShowcase,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof OauthStatesShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every sign-in state, top to bottom. */
export const States: Story = {};

/** A server that asks for a sign-in, before anyone connects it. */
export const SignInRequired: Story = { args: { only: "Sign in required" } };

/** The browser has the sign-in page; the row can open it again. */
export const WaitingForBrowser: Story = {
  args: { only: "Waiting for the browser" },
};

/** Signed in and connected. */
export const SignedIn: Story = { args: { only: "Signed in" } };

/** The server refused to register Tidebreak, so the sign-in never began. */
export const RegistrationRefused: Story = {
  args: { only: "Registration refused" },
};

/** The server asks for a sign-in Tidebreak cannot complete. */
export const NotSupported: Story = { args: { only: "Sign-in not supported" } };

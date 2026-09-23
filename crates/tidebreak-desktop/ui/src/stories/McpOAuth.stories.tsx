import type { Meta, StoryObj } from "@storybook/react-vite";
import type { McpServerInfo } from "@/api";
import { McpServerSummary } from "@/settings/McpPanel";
import { SettingsSection } from "@/settings/primitives";
import {
  mcpOauthAccessDenied,
  mcpOauthAuthorizing,
  mcpOauthConnected,
  mcpOauthExpired,
  mcpOauthNotConnected,
  mcpOauthRegistrationRefused,
  mcpOauthServiceDown,
  mcpOauthTimedOut,
  mcpOauthUnsupported,
  mcpSignInServer,
  mcpSignInServiceDownDiagnostic,
} from "./fixtures";

type Row = {
  title: string;
  server: McpServerInfo;
  busy?: boolean;
  /** The window is attached to another machine. */
  remote?: boolean;
};

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
  {
    title: "Connecting after sign-in",
    server: mcpSignInServer(mcpOauthConnected, {
      health: "reconnecting",
      tool_count: 0,
    }),
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
  {
    title: "Sign-in service down",
    server: mcpSignInServer(mcpOauthServiceDown, {
      diagnostic: mcpSignInServiceDownDiagnostic,
    }),
  },
  { title: "Sign-in expired", server: mcpSignInServer(mcpOauthExpired) },
  { title: "Sign-in denied", server: mcpSignInServer(mcpOauthAccessDenied) },
  {
    title: "Attached to another machine",
    server: mcpSignInServer(mcpOauthTimedOut),
    remote: true,
  },
  {
    title: "Sign-in not supported",
    server: mcpSignInServer(mcpOauthUnsupported, {
      name: "legacy_docs_with_a_long_name",
      url: "https://docs.example.test/mcp",
    }),
  },
];

function SignInRow({ row }: { row: Row }) {
  return (
    <SettingsSection title={row.title}>
      <McpServerSummary
        server={row.server}
        busy={row.busy}
        remote={row.remote}
      />
    </SettingsSection>
  );
}

function OauthStatesShowcase({ only }: { only?: string }) {
  const shown = only ? rows.filter((row) => row.title === only) : rows;
  return (
    <div className="mx-auto flex max-w-xl flex-col gap-10">
      {shown.map((row) => (
        <SignInRow key={row.title} row={row} />
      ))}
    </div>
  );
}

const meta = {
  title: "Settings/MCP OAuth",
  component: OauthStatesShowcase,
  // Padded, not fullscreen: the fullscreen surface clips at the viewport,
  // and the full list of states is taller than one.
  parameters: { layout: "padded" },
} satisfies Meta<typeof OauthStatesShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every sign-in state, top to bottom. */
export const States: Story = {};

/** A server that asks for a sign-in, before anyone connects it. The row
 * names the sign-in service Connect opens. */
export const SignInRequired: Story = { args: { only: "Sign in required" } };

/** The browser has the sign-in page; the row can open it again or cancel. */
export const WaitingForBrowser: Story = {
  args: { only: "Waiting for the browser" },
};

/** Back from the browser: signed in, and loading the server's tools. */
export const ConnectingAfterSignIn: Story = {
  args: { only: "Connecting after sign-in" },
};

/** Signed in and connected. */
export const SignedIn: Story = { args: { only: "Signed in" } };

/** The server refused to register Tidebreak, so the sign-in never began. */
export const RegistrationRefused: Story = {
  args: { only: "Registration refused" },
};

/** The sign-in service did not answer. Temporary: Tidebreak keeps trying. */
export const SignInServiceDown: Story = {
  args: { only: "Sign-in service down" },
};

/** Attached to another machine, the sign-in has to finish on that machine. */
export const AttachedToAnotherMachine: Story = {
  args: { only: "Attached to another machine" },
};

/** The server asks for a sign-in Tidebreak cannot complete. */
export const NotSupported: Story = { args: { only: "Sign-in not supported" } };

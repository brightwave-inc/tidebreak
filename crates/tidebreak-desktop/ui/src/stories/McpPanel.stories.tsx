import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ApiClient, GatewayStatus, McpServerInfo } from "@/api";
import { McpPanel } from "@/settings/McpPanel";
import {
  mcpDirectoryServer,
  mcpDirectoryServers,
  mcpOauthAuthorizing,
  mcpOauthNotConnected,
  mcpSignInDiagnostic,
  mcpSignInServer,
} from "./fixtures";

const signedOut: GatewayStatus = {
  base_url: "http://127.0.0.1:28081",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

function stdioServer(overrides: Partial<McpServerInfo> = {}): McpServerInfo {
  return {
    name: "beeper",
    command: "npx",
    args: ["-y", "@beeper/mcp-remote"],
    env: ["ACCESS_TOKEN"],
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    oauth: false,
    gateway_endpoint: null,
    request_timeout_ms: 60_000,
    enabled: true,
    plugin: null,
    health: "healthy",
    tool_count: 2,
    diagnostic: null,
    curated: null,
    resolved_command: "/opt/homebrew/bin/npx",
    ...overrides,
  };
}

function stubClient(servers: McpServerInfo[]): ApiClient {
  const listing = { servers };
  return {
    listMcpServers: async () => listing,
    putMcpServers: async () => listing,
    reconnectMcpServer: async () => listing,
    // The story's page stays put: the stub answers as the server would, and
    // the listing keeps showing the state the story is about.
    connectMcpServer: async () => mcpOauthAuthorizing,
    cancelMcpServerConnect: async () => mcpOauthNotConnected,
    disconnectMcpServer: async () => mcpOauthNotConnected,
    getGatewayStatus: async () => signedOut,
    getGatewayApps: async () => ({ supported: true, apps: [] }),
    getMcpDirectory: async () => ({ servers: mcpDirectoryServers }),
    addMcpDirectoryServer: async (id: string) => ({ name: id, servers }),
  } as unknown as ApiClient;
}

const meta = {
  title: "Settings/MCP servers",
  component: McpPanel,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof McpPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { client: stubClient([]) },
};

export const LoadFailure: Story = {
  args: {
    client: {
      ...stubClient([]),
      listMcpServers: async () => {
        throw new Error("MCP servers could not be loaded.");
      },
    } as ApiClient,
  },
};

export const ManyServers: Story = {
  args: {
    client: stubClient(
      Array.from({ length: 8 }, (_, index) =>
        stdioServer({
          name: `server_${index + 1}`,
          health: index === 3 ? "degraded" : "healthy",
          diagnostic:
            index === 3 ? "The process exited before it answered." : null,
        }),
      ),
    ),
  },
};

export const StdioResolvedCommand: Story = {
  args: { client: stubClient([stdioServer()]) },
};

export const StdioCommandNotFound: Story = {
  args: {
    client: stubClient([
      stdioServer({
        health: "degraded",
        tool_count: 0,
        resolved_command: undefined,
        diagnostic:
          'Command not found: "npx" is not on the host PATH. Searched: /usr/bin, /bin.',
      }),
    ]),
  },
};

export const StdioLaunchFailure: Story = {
  args: {
    client: stubClient([
      stdioServer({
        health: "degraded",
        tool_count: 0,
        resolved_command: "/opt/homebrew/bin/npx",
        diagnostic:
          "Process failed to launch: exit code 1. First stderr line: npx: command failed.",
      }),
    ]),
  },
};

export const StdioProtocolFailure: Story = {
  args: {
    client: stubClient([
      stdioServer({
        health: "degraded",
        tool_count: 0,
        resolved_command: "/opt/homebrew/bin/npx",
        diagnostic:
          "Protocol negotiation failed (the process answered, but not with MCP JSON-RPC).",
      }),
    ]),
  },
};

/** A remote server imported without the OAuth flag that asks for a sign-in:
 * Connect, not an authentication failure. Padded so the row below the
 * import section scrolls into view. */
export const RemoteServerNeedsSignIn: Story = {
  args: { client: stubClient([mcpSignInServer(mcpOauthNotConnected)]) },
  parameters: { layout: "padded" },
};

/** The same server while the person finishes signing in in the browser. */
export const RemoteServerWaitingForSignIn: Story = {
  args: { client: stubClient([mcpSignInServer(mcpOauthAuthorizing)]) },
  parameters: { layout: "padded" },
};

const linear = mcpDirectoryServers.find((server) => server.id === "linear");
if (linear === undefined) throw new Error("the directory fixture lists Linear");

/** Right after Tidebreak starts, a saved server is still making its first
 * connection. It reads as connecting, not as an unsaved row, and the panel
 * reads the list again until it is up. */
export const SavedServerConnecting: Story = {
  args: {
    client: stubClient([
      mcpDirectoryServer(linear, { health: "initializing", tool_count: 0 }),
    ]),
  },
  parameters: { layout: "padded" },
};

/** Linear, just added from the directory: the directory marks it added, and
 * its row asks for the sign-in the server wants, on the host Connect opens. */
export const AddedServerNeedsSignIn: Story = {
  args: {
    client: stubClient([
      mcpDirectoryServer(linear, {
        health: "degraded",
        tool_count: 0,
        diagnostic: mcpSignInDiagnostic,
        oauth_status: {
          state: "not_connected",
          sign_in_host: "mcp.linear.app",
        },
      }),
    ]),
  },
  parameters: { layout: "padded" },
};

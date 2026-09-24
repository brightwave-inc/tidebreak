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
    } as unknown as ApiClient,
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

/** A saved remote server, healthy, with the authentication and headers a
 * story sets. */
function remoteServer(overrides: Partial<McpServerInfo> = {}): McpServerInfo {
  return {
    name: "docs",
    command: null,
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: "https://mcp.example.com/mcp",
    bearer_token_env: null,
    oauth: false,
    gateway_endpoint: null,
    request_timeout_ms: 60_000,
    enabled: true,
    plugin: null,
    health: "healthy",
    tool_count: 6,
    diagnostic: null,
    curated: null,
    ...overrides,
  };
}

/** Authentication: None. Tidebreak sends no credential; a server that asks
 * for a sign-in would offer Connect after the save. */
export const HttpAuthenticationNone: Story = {
  args: { client: stubClient([remoteServer()]) },
  parameters: { layout: "padded" },
};

/** Authentication: a bearer token stored in the OS credential store, with
 * two custom headers. The fields say a value is stored and never show it. */
export const HttpAuthenticationStoredToken: Story = {
  args: {
    client: stubClient([
      remoteServer({
        bearer_token_stored: true,
        headers: ["X-Api-Key", "X-Workspace-Routing-Tenant-Identifier"],
        stored_credentials: {
          bearer: true,
          headers: ["X-Api-Key", "X-Workspace-Routing-Tenant-Identifier"],
        },
      }),
    ]),
  },
  parameters: { layout: "padded" },
};

/** Authentication: a bearer token read from a variable in the environment
 * Tidebreak started with. */
export const HttpAuthenticationVariable: Story = {
  args: {
    client: stubClient([remoteServer({ bearer_token_env: "DOCS_TOKEN" })]),
  },
  parameters: { layout: "padded" },
};

/** Authentication: OAuth, chosen in the editor. The saved flag offers
 * Connect before the server has answered. */
export const HttpAuthenticationOAuth: Story = {
  args: {
    client: stubClient([
      remoteServer({
        oauth: true,
        health: "degraded",
        tool_count: 0,
        diagnostic: mcpSignInDiagnostic,
        oauth_status: { state: "not_connected", sign_in_host: "example.com" },
      }),
    ]),
  },
  parameters: { layout: "padded" },
};

/** A stored token this computer does not hold, as after an import from
 * another computer: the row names the field to fill in. */
export const HttpStoredTokenMissing: Story = {
  args: {
    client: stubClient([
      remoteServer({
        bearer_token_stored: true,
        stored_credentials: { bearer: false, headers: [] },
        health: "degraded",
        tool_count: 0,
        diagnostic:
          "Not stored: this server's bearer token is not in the credential store on this computer. Enter it under Authentication, then save.",
      }),
    ]),
  },
  parameters: { layout: "padded" },
};

/** The server refused its stored bearer token and offers an OAuth sign-in
 * instead: Use OAuth switches it and opens the sign-in page. */
export const UseOAuthOffer: Story = {
  args: {
    client: stubClient([
      remoteServer({
        name: "vercel",
        url: "https://mcp.vercel.com",
        bearer_token_stored: true,
        stored_credentials: { bearer: true, headers: [] },
        health: "degraded",
        tool_count: 0,
        diagnostic:
          "This server did not accept the bearer token. It offers an OAuth sign-in on vercel.com instead: select Use OAuth to sign in with your browser, or correct the token.",
        oauth_status: { state: "available", sign_in_host: "vercel.com" },
      }),
    ]),
  },
  parameters: { layout: "padded" },
};

/** A local server lists HOME and PATH as forwarded by default above the
 * names it forwards itself. */
export const StdioForwardedDefaults: Story = {
  args: {
    client: stubClient([stdioServer({ env_from: ["BEEPER_WORKSPACE"] })]),
  },
  parameters: { layout: "padded" },
};

/** A local server that forwards its own PATH: the default row for PATH
 * gives way, and HOME stays. */
export const StdioForwardedDefaultsReplaced: Story = {
  args: {
    client: stubClient([stdioServer({ env_from: ["PATH"] })]),
  },
  parameters: { layout: "padded" },
};

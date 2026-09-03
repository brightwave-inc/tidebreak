import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ApiClient, GatewayStatus, McpServerInfo } from "@/api";
import { McpPanel } from "@/settings/McpPanel";

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
    getGatewayStatus: async () => signedOut,
    getGatewayApps: async () => ({ supported: true, apps: [] }),
  } as unknown as ApiClient;
}

const meta = {
  title: "Settings/MCP servers",
  component: McpPanel,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof McpPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

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

// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, McpServersInfo } from "../api";
import { McpPanel } from "./McpPanel";

const healthy: McpServersInfo = {
  servers: [
    {
      name: "private_docs",
      command: "/opt/mcp/docs",
      args: ["--stdio"],
      env: { LOG_LEVEL: "info" },
      env_from: ["PRIVATE_DOCS_TOKEN"],
      cwd: "/tmp/docs",
      url: null,
      bearer_token_env: null,
      gateway_endpoint: null,
      request_timeout_ms: 60_000,
      enabled: true,
      health: "healthy",
      tool_count: 2,
      diagnostic: null,
    },
  ],
};

function api(result = healthy) {
  return {
    listMcpServers: vi.fn().mockResolvedValue(result),
    putMcpServers: vi.fn().mockResolvedValue(result),
    reconnectMcpServer: vi.fn().mockResolvedValue(result),
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("McpPanel", () => {
  it("shows bounded health and never asks for a credential value", async () => {
    render(<McpPanel client={api()} />);

    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(screen.getByText("2 tools available to new turns.")).toBeInTheDocument();
    expect(screen.getByDisplayValue("PRIVATE_DOCS_TOKEN")).toBeInTheDocument();
    expect(screen.queryByLabelText(/API key/i)).not.toBeInTheDocument();
    expect(screen.getByText(/values are resolved in the host process/i)).toBeInTheDocument();
  });

  it("sends argv and selected environment names as typed arrays", async () => {
    const client = api({ servers: [] });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    const namespace = screen.getByRole("textbox", { name: /^Namespace/ });
    await user.clear(namespace);
    await user.type(namespace, "documents_server");
    expect(namespace).toHaveValue("documents_server");
    await user.type(
      screen.getByPlaceholderText("/absolute/path/to/server"),
      "/opt/mcp/docs",
    );
    await user.click(screen.getByRole("button", { name: "Add argument" }));
    await user.type(screen.getByLabelText("Arguments 1"), "--stdio");
    await user.click(
      screen.getByRole("button", { name: "Add variable name" }),
    );
    await user.type(
      screen.getByLabelText("Forward environment names 1"),
      "DOCS_TOKEN",
    );
    await user.click(screen.getByRole("button", { name: "Save and verify" }));

    await waitFor(() =>
      expect(client.putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({
          command: "/opt/mcp/docs",
          name: "documents_server",
          args: ["--stdio"],
          env_from: ["DOCS_TOKEN"],
        }),
      ]),
    );
  });

  it("reconnects by namespace and replaces the displayed health snapshot", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText("Healthy");
    await user.click(
      screen.getByRole("button", { name: /Reconnect and refresh tools/ }),
    );
    await waitFor(() =>
      expect(client.reconnectMcpServer).toHaveBeenCalledWith("private_docs"),
    );

    await user.type(
      screen.getByPlaceholderText("/absolute/path/to/server"),
      "-edited",
    );
    expect(
      screen.queryByRole("button", { name: /Reconnect and refresh tools/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/Save and verify changes before reconnecting/),
    ).toBeInTheDocument();
  });

  it("switches a server to HTTP and sends a url definition without process fields", async () => {
    const client = api({ servers: [] });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(
      screen.getByRole("radio", { name: "Remote endpoint (HTTP)" }),
    );

    // Process-only editors leave the form with the transport.
    expect(
      screen.queryByPlaceholderText("/absolute/path/to/server"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add variable name" }),
    ).not.toBeInTheDocument();

    await user.type(
      screen.getByPlaceholderText("https://gateway.example/mcp/tools"),
      "http://127.0.0.1:28081/mcp/tools",
    );
    await user.type(
      screen.getByPlaceholderText("GATEWAY_TOKEN"),
      "MY_GATEWAY_TOKEN",
    );
    await user.click(screen.getByRole("button", { name: "Save and verify" }));

    await waitFor(() =>
      expect(client.putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({
          command: null,
          args: [],
          env: {},
          env_from: [],
          cwd: null,
          url: "http://127.0.0.1:28081/mcp/tools",
          bearer_token_env: "MY_GATEWAY_TOKEN",
          gateway_endpoint: null,
        }),
      ]),
    );
  });

  it("returning to stdio clears the http fields", async () => {
    const client = api({ servers: [] });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(
      screen.getByRole("radio", { name: "Remote endpoint (HTTP)" }),
    );
    await user.type(
      screen.getByPlaceholderText("https://gateway.example/mcp/tools"),
      "http://127.0.0.1/mcp",
    );
    await user.click(
      screen.getByRole("radio", { name: "Local process (stdio)" }),
    );
    await user.type(
      screen.getByPlaceholderText("/absolute/path/to/server"),
      "/opt/mcp/docs",
    );
    await user.click(screen.getByRole("button", { name: "Save and verify" }));

    await waitFor(() =>
      expect(client.putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({
          command: "/opt/mcp/docs",
          url: null,
          bearer_token_env: null,
          gateway_endpoint: null,
        }),
      ]),
    );
  });

  it("shows health for an http server without asking for a token value", async () => {
    render(
      <McpPanel
        client={api({
          servers: [
            {
              ...healthy.servers[0],
              name: "gateway",
              command: null,
              args: [],
              env: {},
              env_from: [],
              cwd: null,
              url: "http://127.0.0.1:28081/mcp/tools",
              bearer_token_env: "GATEWAY_TOKEN",
              gateway_endpoint: null,
              tool_count: 1,
            },
          ],
        })}
      />,
    );

    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(screen.getByText("1 tool available to new turns.")).toBeInTheDocument();
    expect(
      screen.getByDisplayValue("http://127.0.0.1:28081/mcp/tools"),
    ).toBeInTheDocument();
    // Only the variable name is ever displayed.
    expect(screen.getByDisplayValue("GATEWAY_TOKEN")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Remote endpoint (HTTP)" })).toBeChecked();
  });

  it("surfaces secret-free degraded diagnostics", async () => {
    render(
      <McpPanel
        client={api({
          servers: [
            {
              ...healthy.servers[0],
              health: "degraded",
              tool_count: 0,
              diagnostic:
                'required parent environment variable "DOCS_TOKEN" is not set',
            },
          ],
        })}
      />,
    );

    expect(await screen.findByText("Needs attention")).toBeInTheDocument();
    expect(screen.getByText(/DOCS_TOKEN/)).toBeInTheDocument();
  });

  it("is read-only on a managed profile, keeping mounts and their reasons visible", async () => {
    const client = api({
      servers: [
        {
          ...healthy.servers[0],
          name: "legacy_docs",
          health: "disabled",
          tool_count: 0,
          diagnostic:
            "Disabled by managed policy. Gateway-managed MCP endpoints remain available.",
        },
        {
          name: "tools",
          command: null,
          args: [],
          env: {},
          env_from: [],
          cwd: null,
          url: null,
          bearer_token_env: null,
          gateway_endpoint: "tools",
          request_timeout_ms: 60_000,
          enabled: true,
          health: "healthy",
          tool_count: 3,
          diagnostic: null,
        },
      ],
    });
    render(<McpPanel client={client} managed />);

    // The gateway mount and its health stay visible; the locked manual server
    // stays listed with the server's own reason rather than disappearing.
    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(screen.getByText("3 tools available to new turns.")).toBeInTheDocument();
    expect(screen.getByText(/Disabled by managed policy/)).toBeInTheDocument();

    // Nothing manual is editable: no fields, and no way to add or save one.
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Save and verify" }),
    ).not.toBeInTheDocument();
  });
});

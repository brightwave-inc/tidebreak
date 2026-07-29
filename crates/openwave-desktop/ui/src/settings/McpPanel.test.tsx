// @vitest-environment jsdom
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApiClient,
  GatewayStatus,
  McpServerInfo,
  McpServersInfo,
} from "../api";
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

const signedOut: GatewayStatus = {
  configured: true,
  enabled: true,
  base_url: "http://127.0.0.1:28081",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

const signedIn: GatewayStatus = { ...signedOut, signed_in: true, model_count: 2 };

/** A configured gateway mount as `listMcpServers` reports it. */
function gatewayMount(
  slug: string,
  overrides: Partial<McpServerInfo> = {},
): McpServerInfo {
  return {
    name: slug,
    command: null,
    args: [],
    env: {},
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: slug,
    request_timeout_ms: 60_000,
    enabled: true,
    health: "healthy",
    tool_count: 3,
    diagnostic: null,
    ...overrides,
  };
}

const incidentApps = {
  supported: true,
  apps: [
    {
      id: "app-1",
      name: "Incident API",
      app_kind: "rest_api",
      enabled: true,
      mcp_endpoint_slugs: ["example-security-tools"],
    },
  ],
};

/** No gateway session unless a test signs one in, so the gateway endpoints
 * section is absent by default — as it is on an unpaired profile. */
function api(
  result = healthy,
  overrides: Partial<Record<keyof ApiClient, unknown>> = {},
) {
  return {
    listMcpServers: vi.fn().mockResolvedValue(result),
    putMcpServers: vi.fn().mockResolvedValue(result),
    reconnectMcpServer: vi.fn().mockResolvedValue(result),
    getGatewayStatus: vi.fn().mockResolvedValue(signedOut),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    ...overrides,
  } as unknown as ApiClient;
}

/** The row a mount toggle belongs to, for assertions that would otherwise
 * also match the same server's card further down the page. */
function mountRow(slug: string): HTMLElement {
  const row = screen
    .getByRole("switch", { name: `Mount ${slug}` })
    .closest("li");
  if (!row) throw new Error(`no mount row for ${slug}`);
  return row;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
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
    expect(
      screen.getAllByText("3 tools available to new turns.").length,
    ).toBeGreaterThan(0);
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

  it("shows no gateway endpoints section on an unpaired profile", async () => {
    render(<McpPanel client={api()} />);

    await screen.findByText("Healthy");
    expect(screen.queryByText("Gateway endpoints")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /^Mount / }),
    ).not.toBeInTheDocument();
  });

  it("lists configured mounts signed out, toggles off, pointing at sign-in", async () => {
    render(
      <McpPanel
        client={api({ servers: [gatewayMount("example-security-tools")] })}
      />,
    );

    const toggle = await screen.findByRole("switch", {
      name: "Mount example-security-tools",
    });
    expect(toggle).toBeChecked();
    expect(toggle).toBeDisabled();
    expect(
      screen.getByText(/Sign in to the Model Gateway to mount or unmount/),
    ).toBeInTheDocument();
    // Signed out, no entitlements were read, so nothing claims a revocation.
    expect(screen.queryByText(/No longer granted/)).not.toBeInTheDocument();
  });

  it("mounts a gateway endpoint with a session-bound definition", async () => {
    const putMcpServers = vi.fn().mockResolvedValue({
      servers: [gatewayMount("example-security-tools")],
    });
    const client = api(
      { servers: [] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
        putMcpServers,
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(
      await screen.findByRole("switch", {
        name: "Mount example-security-tools",
      }),
    );
    await waitFor(() =>
      expect(putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({
          name: "example-security-tools",
          gateway_endpoint: "example-security-tools",
          url: null,
          bearer_token_env: null,
        }),
      ]),
    );
    // The saved mount reports its health inline, on its own row.
    await waitFor(() =>
      expect(
        within(mountRow("example-security-tools")).getByText(
          /3 tools available/,
        ),
      ).toBeInTheDocument(),
    );
  });

  it("derives a mount name that fits the namespace limit for long slugs", async () => {
    const longSlug = "a-very-long-endpoint-slug-that-exceeds-the-name-limit";
    const putMcpServers = vi.fn().mockResolvedValue({ servers: [] });
    const client = api(
      { servers: [] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue({
          supported: true,
          apps: [{ ...incidentApps.apps[0], mcp_endpoint_slugs: [longSlug] }],
        }),
        putMcpServers,
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(
      await screen.findByRole("switch", { name: `Mount ${longSlug}` }),
    );
    await waitFor(() =>
      expect(putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({
          name: longSlug.slice(0, 32),
          gateway_endpoint: longSlug,
        }),
      ]),
    );
  });

  it("keeps a row and unmount toggle for a mount whose entitlement was revoked", async () => {
    const revoked = gatewayMount("revoked-tools", {
      health: "reconnecting",
      tool_count: 0,
    });
    const putMcpServers = vi.fn().mockResolvedValue({ servers: [] });
    const client = api(
      { servers: [revoked] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        // No entitled app references the mounted slug any more.
        getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
        putMcpServers,
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    // The configured mount keeps its row, with an explanation instead of a
    // health line once the entitlements land.
    await waitFor(() =>
      expect(
        within(mountRow("revoked-tools")).getByText(
          /No longer granted to your teams/,
        ),
      ).toBeInTheDocument(),
    );
    const row = mountRow("revoked-tools");
    expect(within(row).queryByText(/Connecting/)).not.toBeInTheDocument();

    // And the toggle still unmounts it.
    const toggle = within(row).getByRole("switch");
    expect(toggle).toBeChecked();
    await user.click(toggle);
    await waitFor(() => expect(putMcpServers).toHaveBeenCalledWith([]));
  });

  it("surfaces a failed server-list fetch as a retryable error, not dead toggles", async () => {
    const listMcpServers = vi
      .fn()
      .mockRejectedValue(new Error("mcp backend unavailable"));
    const client = api(
      { servers: [] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
        listMcpServers,
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    // The failure is visible and carries the underlying message, and the
    // disabled toggle is explained rather than passing unknown off as
    // unmounted.
    expect(
      await screen.findByText(
        /Couldn't read the MCP server list: mcp backend unavailable/,
      ),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("switch", {
        name: "Mount example-security-tools",
      }),
    ).toBeDisabled();
    expect(
      within(mountRow("example-security-tools")).getByText(
        /Mount state unknown/,
      ),
    ).toBeInTheDocument();

    listMcpServers.mockResolvedValue({ servers: [] });
    await user.click(screen.getByRole("button", { name: /Retry/ }));
    await waitFor(() =>
      expect(
        screen.queryByText(/mcp backend unavailable/),
      ).not.toBeInTheDocument(),
    );
    expect(
      screen.getByRole("switch", { name: "Mount example-security-tools" }),
    ).toBeEnabled();
  });

  it("keeps last-known rows and live toggles when a later refresh fails", async () => {
    const listMcpServers = vi
      .fn()
      .mockResolvedValue({ servers: [gatewayMount("example-security-tools")] });
    const client = api(
      { servers: [] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
        listMcpServers,
      },
    );
    vi.useFakeTimers();
    render(<McpPanel client={client} />);
    await act(async () => {});
    expect(
      within(mountRow("example-security-tools")).getByText(/3 tools available/),
    ).toBeInTheDocument();

    // The 15s refresh fails: the error appears, but the last-known row keeps
    // its health line and a usable toggle instead of resetting to unknown.
    listMcpServers.mockRejectedValue(new Error("mcp backend unavailable"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_100);
    });
    expect(
      screen.getByText(
        /Couldn't read the MCP server list: mcp backend unavailable/,
      ),
    ).toBeInTheDocument();
    const row = mountRow("example-security-tools");
    expect(within(row).getByText(/3 tools available/)).toBeInTheDocument();
    const toggle = within(row).getByRole("switch");
    expect(toggle).toBeChecked();
    expect(toggle).toBeEnabled();
  });

  it("still shows configured mounts when entitlements cannot be read", async () => {
    const client = api(
      { servers: [gatewayMount("example-security-tools")] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi
          .fn()
          .mockRejectedValue(new Error("gateway unreachable")),
      },
    );
    render(<McpPanel client={client} />);

    // The row survives the apps failure, labeled unknown-entitlements — never
    // misreported as revoked.
    expect(
      await screen.findByText(/Couldn't read your entitlements/),
    ).toBeInTheDocument();
    const row = mountRow("example-security-tools");
    expect(within(row).queryByText(/No longer granted/)).not.toBeInTheDocument();
    expect(within(row).getByRole("switch")).toBeChecked();
  });

  it("mounts on a managed profile, round-tripping a manual server unchanged", async () => {
    const legacy: McpServerInfo = {
      ...healthy.servers[0],
      name: "legacy_docs",
      health: "disabled",
      tool_count: 0,
      diagnostic:
        "Disabled by managed policy. Gateway-managed MCP endpoints remain available.",
    };
    const putMcpServers = vi.fn().mockResolvedValue({
      servers: [legacy, gatewayMount("example-security-tools")],
    });
    const client = api(
      { servers: [legacy] },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
        putMcpServers,
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} managed />);

    // Manual servers are read-only under managed policy, but a gateway mount
    // is exactly the write the server admits — and admission depends on the
    // inert manual definition arriving unchanged, so pin it exactly.
    const toggle = await screen.findByRole("switch", {
      name: "Mount example-security-tools",
    });
    expect(toggle).toBeEnabled();
    await user.click(toggle);
    await waitFor(() =>
      expect(putMcpServers).toHaveBeenCalledWith([
        {
          name: "legacy_docs",
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
        },
        expect.objectContaining({
          gateway_endpoint: "example-security-tools",
        }),
      ]),
    );
  });

  it("keeps a mount made during unsaved edits through the next save", async () => {
    const mount = gatewayMount("example-security-tools");
    const putMcpServers = vi
      .fn()
      .mockResolvedValue({ servers: [...healthy.servers, mount] });
    const client = api(healthy, {
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
      putMcpServers,
    });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    // An unsaved manual edit first, so the draft is dirty when the mount lands.
    await user.type(
      await screen.findByPlaceholderText("/absolute/path/to/server"),
      "-edited",
    );
    await user.click(
      await screen.findByRole("switch", {
        name: "Mount example-security-tools",
      }),
    );
    // The mount write is rebuilt from the saved configuration: nobody's
    // toggle persists an unsaved edit.
    await waitFor(() =>
      expect(putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({ command: "/opt/mcp/docs" }),
        expect.objectContaining({
          gateway_endpoint: "example-security-tools",
        }),
      ]),
    );

    // Saving the dirty draft carries the mount instead of silently
    // reverting it.
    await user.click(screen.getByRole("button", { name: "Save and verify" }));
    await waitFor(() => expect(putMcpServers).toHaveBeenCalledTimes(2));
    expect(putMcpServers).toHaveBeenLastCalledWith([
      expect.objectContaining({ command: "/opt/mcp/docs-edited" }),
      expect.objectContaining({
        gateway_endpoint: "example-security-tools",
      }),
    ]);
  });
});

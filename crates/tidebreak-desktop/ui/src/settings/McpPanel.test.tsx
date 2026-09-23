// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  HttpError,
  type ApiClient,
  type GatewayStatus,
  type McpServerInfo,
  type McpServersInfo,
} from "../api";
import { McpPanel } from "./McpPanel";
import { setAttachedRemotely } from "@/host";
import { mcpDirectoryServer, mcpDirectoryServers } from "../stories/fixtures";

const { openInBrowser, toast } = vi.hoisted(() => ({
  openInBrowser: vi.fn(async (_url: string) => {}),
  toast: { success: vi.fn(), message: vi.fn(), error: vi.fn() },
}));
vi.mock("@/openInBrowser", () => ({ openInBrowser }));
vi.mock("sonner", () => ({ toast }));

const healthy: McpServersInfo = {
  servers: [
    {
      name: "private_docs",
      command: "/opt/mcp/docs",
      args: ["--stdio"],
      env: ["LOG_LEVEL"],
      env_from: ["PRIVATE_DOCS_TOKEN"],
      cwd: "/tmp/docs",
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
      resolved_command: "/opt/mcp/docs",
    },
  ],
};

const signedOut: GatewayStatus = {
  base_url: "http://127.0.0.1:28081",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

const signedIn: GatewayStatus = {
  ...signedOut,
  signed_in: true,
  model_count: 2,
};

/** A configured gateway mount as `listMcpServers` reports it. */
function gatewayMount(
  slug: string,
  overrides: Partial<McpServerInfo> = {},
): McpServerInfo {
  return {
    name: slug,
    command: null,
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    oauth: false,
    gateway_endpoint: slug,
    request_timeout_ms: 60_000,
    enabled: true,
    plugin: null,
    health: "healthy",
    tool_count: 3,
    diagnostic: null,
    curated: null,
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
      used_by_app_count: 0,
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
    connectMcpServer: vi.fn().mockResolvedValue(result),
    cancelMcpServerConnect: vi.fn().mockResolvedValue(result),
    disconnectMcpServer: vi.fn().mockResolvedValue(result),
    getGatewayStatus: vi.fn().mockResolvedValue(signedOut),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    getMcpDirectory: vi
      .fn()
      .mockResolvedValue({ servers: mcpDirectoryServers }),
    addMcpDirectoryServer: vi.fn(),
    ...overrides,
  } as unknown as ApiClient;
}

/** The row a mount toggle belongs to, for assertions that would otherwise
 * also match the same server's card further down the page. */
function mountRow(slug: string): HTMLElement {
  const row = screen
    .getByRole("switch", { name: `Connect ${slug}` })
    .closest("li");
  if (!row) throw new Error(`no mount row for ${slug}`);
  return row;
}

afterEach(() => {
  setAttachedRemotely(false);
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("McpPanel", () => {
  it("shows bounded health and never asks for a credential value", async () => {
    render(<McpPanel client={api()} />);

    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(
      screen.getByText("2 tools available to new turns."),
    ).toBeInTheDocument();
    expect(screen.getByDisplayValue("PRIVATE_DOCS_TOKEN")).toBeInTheDocument();
    expect(screen.queryByLabelText(/API key/i)).not.toBeInTheDocument();
    expect(
      screen.getByText(
        /Tidebreak reads their values from the process environment/i,
      ),
    ).toBeInTheDocument();
  });

  it("shows the resolved stdio executable path after verify", async () => {
    render(
      <McpPanel
        client={api({
          servers: [
            {
              ...healthy.servers[0],
              command: "npx",
              resolved_command: "/opt/homebrew/bin/npx",
            },
          ],
        })}
      />,
    );
    expect(
      await screen.findByText(
        (_, node) =>
          node?.tagName === "P" &&
          node.textContent === "Resolved npx to /opt/homebrew/bin/npx",
      ),
    ).toBeInTheDocument();
  });

  it("imports a JSON file into the editor and reports skipped servers", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText("Healthy");
    const fileName =
      "mcp-configuration-with-a-filename-that-must-wrap-without-overflowing.json";
    const secretName =
      "CALENDAR_TOKEN_WITH_A_LONG_UNBROKEN_IDENTIFIER_FOR_A_COMPACT_PANEL";
    const file = new File(
      [
        JSON.stringify({
          mcpServers: {
            private_docs: { command: "duplicate" },
            "bad.name": { command: "invalid-name" },
            calendar: {
              command: "npx",
              args: ["-y", "@example/calendar-mcp"],
              env: { [secretName]: "do-not-retain" },
            },
            remote: { url: "https://mcp.example.test/tools" },
          },
        }),
      ],
      fileName,
      { type: "application/json" },
    );
    if (typeof file.text !== "function") {
      Object.defineProperty(file, "text", {
        value: vi
          .fn()
          .mockResolvedValue(
            await file
              .arrayBuffer()
              .then((value) => new TextDecoder().decode(value)),
          ),
      });
    }
    await user.upload(screen.getByLabelText("Import MCP configuration"), file);

    const importSection = screen
      .getByRole("heading", { name: "Import configuration" })
      .closest("section");
    if (!importSection) throw new Error("import section missing");
    const status = await within(importSection).findByRole("status");
    expect(status).toHaveTextContent(
      `2 servers added to the editor from ${fileName}. 2 entries were skipped.`,
    );
    expect(status.closest("[aria-live]")).toBeNull();
    expect(status).not.toContainElement(
      within(importSection).getByRole("list", {
        name: "Environment values to enter",
      }),
    );
    expect(within(status).getByText(fileName)).toHaveClass("break-all");
    const skipped = within(
      screen.getByRole("list", { name: "Skipped MCP servers" }),
    )
      .getAllByRole("listitem")
      .map((item) => item.textContent);
    expect(skipped).toEqual([
      "private_docs: mcpServers.private_docs: Namespace already exists. Choose a different name.",
      'bad.name: mcpServers["bad.name"]: Name a server with 1–32 ASCII letters, numbers, underscores, or hyphens. "bad.name" contains a period. A valid value looks like docs or remote-tools.',
    ]);
    expect(screen.getByRole("heading", { name: "calendar" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "remote" })).toBeVisible();
    expect(screen.getByText(secretName)).toHaveClass("break-all");
    expect(
      within(
        screen.getByRole("list", { name: "Skipped MCP servers" }),
      ).getByText("private_docs"),
    ).toHaveClass("break-all");
    expect(screen.queryByText("do-not-retain")).not.toBeInTheDocument();
    expect(client.putMcpServers).not.toHaveBeenCalled();

    const calendarSection = screen
      .getByRole("heading", { name: "calendar" })
      .closest("section");
    if (!calendarSection) throw new Error("calendar section missing");
    expect(
      within(calendarSection).getByLabelText("Environment value 1"),
    ).toHaveValue("");
    expect(screen.getByLabelText("MCP JSON")).toHaveValue(await file.text());
  });

  it("keeps pasted JSON when import fails and lists field-level skips", async () => {
    const client = api({ servers: [] });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    const json = `{
  "mcpServers": {
    "docs": { "command": "npx" },
    "bad name": { "command": "npx" }
  }
}`;
    fireEvent.change(screen.getByLabelText("MCP JSON"), {
      target: { value: json },
    });
    await user.click(
      screen.getByRole("button", { name: "Import pasted JSON" }),
    );

    expect(screen.getByLabelText("MCP JSON")).toHaveValue(json);
    expect(await screen.findByRole("heading", { name: "docs" })).toBeVisible();
    const skipped = within(
      screen.getByRole("list", { name: "Skipped MCP servers" }),
    )
      .getAllByRole("listitem")
      .map((item) => item.textContent);
    expect(skipped).toEqual([
      'bad name: mcpServers["bad name"]: Name a server with 1–32 ASCII letters, numbers, underscores, or hyphens. "bad name" contains a space. A valid value looks like docs or remote-tools.',
    ]);
  });

  it("keeps pasted JSON and names the character when comments make it invalid", async () => {
    const client = api({ servers: [] });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    const json = `{
  // comment
  "mcpServers": { "docs": { "command": "npx" } }
}`;
    fireEvent.change(screen.getByLabelText("MCP JSON"), {
      target: { value: json },
    });
    await user.click(
      screen.getByRole("button", { name: "Import pasted JSON" }),
    );

    expect(screen.getByLabelText("MCP JSON")).toHaveValue(json);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Could not import pasted JSON: JSON at character \d+: this JSON uses a comment/,
    );
  });

  it("shows the server's field-level reason when save rejects a server", async () => {
    const client = api(
      { servers: [] },
      {
        putMcpServers: vi
          .fn()
          .mockRejectedValue(
            new HttpError(
              400,
              '400: invalid external MCP server "docs": must configure exactly one of command, url, or gateway endpoint',
            ),
          ),
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText(/No MCP servers configured/);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Save and verify" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      'invalid external MCP server "docs": must configure exactly one of command, url, or gateway endpoint',
    );
  });

  it("shows an existing environment value as blank and keeps it unless retyped", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await screen.findByText("Healthy");
    // The server returns names only, so the value field starts empty and is
    // password-typed — people put credentials here whatever the label says.
    const value = screen.getByLabelText("Environment value 1");
    expect(value).toHaveValue("");
    expect(value).toHaveAttribute("type", "password");

    // Saving without touching it sends no value for that name, which is what
    // tells the server to keep the stored one.
    await user.click(screen.getByRole("button", { name: "Save and verify" }));
    await waitFor(() =>
      expect(client.putMcpServers).toHaveBeenCalledWith([
        expect.objectContaining({ env: ["LOG_LEVEL"] }),
      ]),
    );
    expect(
      vi.mocked(client.putMcpServers).mock.calls[0][0][0].env_values ?? {},
    ).toEqual({});

    await user.type(value, "rotated-secret");
    await user.click(screen.getByRole("button", { name: "Save and verify" }));
    await waitFor(() =>
      expect(client.putMcpServers).toHaveBeenLastCalledWith([
        expect.objectContaining({
          env: ["LOG_LEVEL"],
          env_values: { LOG_LEVEL: "rotated-secret" },
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
              env: [],
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
    expect(
      screen.getByText("1 tool available to new turns."),
    ).toBeInTheDocument();
    expect(
      screen.getByDisplayValue("http://127.0.0.1:28081/mcp/tools"),
    ).toBeInTheDocument();
    // Only the variable name is ever displayed.
    expect(screen.getByDisplayValue("GATEWAY_TOKEN")).toBeInTheDocument();
    expect(
      screen.getByRole("radio", { name: "Remote endpoint (HTTP)" }),
    ).toBeChecked();
    expect(
      screen.getByText(/Export it in the shell you start Tidebreak from/),
    ).toBeInTheDocument();
  });

  it("shows the full classified verify failure", async () => {
    const message = `DNS resolution failed (mcp.example.invalid). Bearer-token environment variable "GATEWAY_TOKEN" is not set in the environment Tidebreak reads. Export it in the shell you start Tidebreak from, then restart Tidebreak.`;
    const client = api(
      { servers: [] },
      {
        putMcpServers: vi.fn().mockRejectedValue(new Error(message)),
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);
    await screen.findByText(/No MCP servers configured/);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Save and verify" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(message);
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

  it("managed: compact endpoint rows only — no editor, headings, or tool counts", async () => {
    const client = api(
      {
        servers: [
          {
            ...healthy.servers[0],
            name: "legacy_docs",
            health: "disabled",
            tool_count: 0,
            diagnostic:
              "Disabled by managed policy. Gateway-managed MCP endpoints remain available.",
          },
          gatewayMount("example-security-tools"),
        ],
      },
      {
        getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
        getGatewayApps: vi.fn().mockResolvedValue(incidentApps),
      },
    );
    render(<McpPanel client={client} managed />);

    // One row per endpoint: mount toggle, health chip, the apps it serves,
    // and a reconnect action — the whole transport story in one line.
    await screen.findByRole("switch", {
      name: "Connect example-security-tools",
    });
    const row = mountRow("example-security-tools");
    expect(within(row).getByText("Healthy")).toBeInTheDocument();
    // Apps load after the session check, so the switch can appear before
    // this label. Wait for it instead of assuming the two fetches finish
    // in lockstep.
    expect(
      await within(row).findByText(/serves: Incident API/),
    ).toBeInTheDocument();
    expect(
      within(row).getByRole("button", { name: /Reconnect/ }),
    ).toBeInTheDocument();

    // Tool availability and the locked manual server belong to the app
    // entries on the Connected apps page, not this view — and nothing here
    // is a heading, a per-endpoint card, or an editor.
    expect(
      screen.queryByText(/available to new turns/),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    expect(screen.queryByText("legacy_docs")).not.toBeInTheDocument();
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
      screen.queryByRole("switch", { name: /^Connect / }),
    ).not.toBeInTheDocument();
  });

  it("lists configured mounts signed out, toggles off, pointing at sign-in", async () => {
    render(
      <McpPanel
        client={api({ servers: [gatewayMount("example-security-tools")] })}
      />,
    );

    const toggle = await screen.findByRole("switch", {
      name: "Connect example-security-tools",
    });
    expect(toggle).toBeChecked();
    expect(toggle).toBeDisabled();
    expect(
      screen.getByText(/Sign in to the Model Gateway to connect or disconnect/),
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
        name: "Connect example-security-tools",
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
      await screen.findByRole("switch", { name: `Connect ${longSlug}` }),
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
        getGatewayApps: vi
          .fn()
          .mockResolvedValue({ supported: true, apps: [] }),
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
        /Could not read the MCP server list: mcp backend unavailable/,
      ),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("switch", {
        name: "Connect example-security-tools",
      }),
    ).toBeDisabled();
    expect(
      within(mountRow("example-security-tools")).getByText(
        /Connection state unknown/,
      ),
    ).toBeInTheDocument();

    listMcpServers.mockResolvedValue({ servers: [] });
    await user.click(screen.getByRole("button", { name: /Try again/ }));
    await waitFor(() =>
      expect(
        screen.queryByText(/mcp backend unavailable/),
      ).not.toBeInTheDocument(),
    );
    expect(
      screen.getByRole("switch", { name: "Connect example-security-tools" }),
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
        /Could not read the MCP server list: mcp backend unavailable/,
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
      await screen.findByText(/Could not read your entitlements/),
    ).toBeInTheDocument();
    const row = mountRow("example-security-tools");
    expect(
      within(row).queryByText(/No longer granted/),
    ).not.toBeInTheDocument();
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
      name: "Connect example-security-tools",
    });
    expect(toggle).toBeEnabled();
    await user.click(toggle);
    await waitFor(() =>
      expect(putMcpServers).toHaveBeenCalledWith([
        {
          name: "legacy_docs",
          command: "/opt/mcp/docs",
          args: ["--stdio"],
          env: ["LOG_LEVEL"],
          env_from: ["PRIVATE_DOCS_TOKEN"],
          cwd: "/tmp/docs",
          url: null,
          bearer_token_env: null,
          oauth: false,
          gateway_endpoint: null,
          request_timeout_ms: 60_000,
          enabled: true,
          plugin: null,
        },
        expect.objectContaining({
          gateway_endpoint: "example-security-tools",
        }),
      ]),
    );
  });

  it("a background refresh landing mid-edit keeps the unsaved draft", async () => {
    // The mount write disables the form while it flies, so the in-flight
    // completion a reader can actually race against is the background list
    // read. It captures a render from before the edit; only the dirty ref —
    // not that render's `dirty` — can tell it the draft must survive.
    const listMcpServers = vi.fn().mockResolvedValue(healthy);
    const client = api(healthy, {
      listMcpServers,
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    // Real time may pass (userEvent needs it); only the 15s cadence is
    // driven explicitly.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);
    await act(async () => {});
    expect(screen.getByPlaceholderText("/absolute/path/to/server")).toHaveValue(
      "/opt/mcp/docs",
    );

    // The next cadence read hangs; the reader edits while it is in flight.
    let resolveRead!: (value: McpServersInfo) => void;
    listMcpServers.mockImplementationOnce(
      () =>
        new Promise<McpServersInfo>((resolve) => {
          resolveRead = resolve;
        }),
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_100);
    });
    await user.type(
      screen.getByPlaceholderText("/absolute/path/to/server"),
      "-edited",
    );

    // The read's snapshot predates the edit; landing, it must not undo it.
    resolveRead(healthy);
    await act(async () => {});
    expect(screen.getByPlaceholderText("/absolute/path/to/server")).toHaveValue(
      "/opt/mcp/docs-edited",
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
        name: "Connect example-security-tools",
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

/** An HTTP server as an import saves it, with no OAuth flag, after it
 * answered the handshake by asking for a sign-in. */
function signInServer(overrides: Partial<McpServerInfo> = {}): McpServerInfo {
  return {
    name: "vercel",
    command: null,
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: "https://mcp.vercel.com",
    bearer_token_env: null,
    oauth: false,
    gateway_endpoint: null,
    request_timeout_ms: 60_000,
    enabled: true,
    plugin: null,
    health: "degraded",
    tool_count: 0,
    diagnostic:
      "This server needs you to sign in. Select Connect to sign in with your browser.",
    curated: null,
    oauth_status: { state: "not_connected" },
    ...overrides,
  };
}

describe("McpPanel OAuth sign-in", () => {
  it("offers Connect for an imported server that asks for a sign-in", async () => {
    render(<McpPanel client={api({ servers: [signInServer()] })} />);

    expect(await screen.findByText("Sign in required")).toBeInTheDocument();
    expect(screen.getByText(/needs you to sign in/)).toBeInTheDocument();
    // Not a failed connection: a sign-in is the next step.
    expect(screen.queryByText("Needs attention")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("opens the sign-in page on this computer and follows it until the server connects", async () => {
    const page = "https://vercel.com/oauth/authorize?client_id=tidebreak-1";
    const waiting = signInServer({
      oauth_status: {
        state: "authorizing",
        pending_authorization_url: page,
        sign_in_host: "vercel.com",
      },
    });
    // The browser came back: the session is stored and the server is
    // reconnecting to load its tools.
    const reconnecting = signInServer({
      health: "reconnecting",
      diagnostic: null,
      oauth_status: { state: "connected", sign_in_host: "vercel.com" },
    });
    const connected = signInServer({
      health: "healthy",
      tool_count: 4,
      diagnostic: null,
      oauth_status: { state: "connected", sign_in_host: "vercel.com" },
    });
    const listMcpServers = vi
      .fn()
      .mockResolvedValueOnce({ servers: [signInServer()] })
      .mockResolvedValueOnce({ servers: [waiting] })
      .mockResolvedValueOnce({ servers: [reconnecting] })
      .mockResolvedValue({ servers: [connected] });
    const connectMcpServer = vi.fn().mockResolvedValue({
      state: "authorizing",
      pending_authorization_url: page,
      sign_in_host: "vercel.com",
    });
    const client = api(
      { servers: [signInServer()] },
      { listMcpServers, connectMcpServer },
    );
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(await screen.findByRole("button", { name: "Connect" }));
    expect(connectMcpServer).toHaveBeenCalledWith("vercel");
    await waitFor(() => expect(openInBrowser).toHaveBeenCalledWith(page));
    expect(await screen.findByText("Waiting for sign-in")).toBeInTheDocument();
    expect(
      screen.getByText(/Finish signing in on vercel.com/),
    ).toBeInTheDocument();

    // A closed tab is not a dead end.
    await user.click(
      screen.getByRole("button", { name: "Reopen sign-in page" }),
    );
    expect(openInBrowser).toHaveBeenCalledTimes(2);
    expect(openInBrowser).toHaveBeenLastCalledWith(page);

    // Back from the browser, the server reconnects. It reads as signed in
    // and connecting, and offers no second sign-in.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    expect(await screen.findByText("Connecting")).toBeInTheDocument();
    expect(screen.getByText("Signed in with vercel.com")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Reconnect$|Connect$/ }),
    ).not.toBeInTheDocument();
    expect(toast.success).not.toHaveBeenCalled();

    // The panel keeps reading through the reconnect, and says when it lands.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(
      screen.getByText("4 tools available to new turns."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Disconnect" }),
    ).toBeInTheDocument();
    expect(toast.success).toHaveBeenCalledTimes(1);
    expect(toast.success).toHaveBeenCalledWith("Connected vercel");
  });

  it("names the sign-in service before Connect", async () => {
    render(
      <McpPanel
        client={api({
          servers: [
            signInServer({
              oauth_status: {
                state: "not_connected",
                sign_in_host: "vercel.com",
              },
            }),
          ],
        })}
      />,
    );
    expect(
      await screen.findByRole("button", { name: "Connect" }),
    ).toBeEnabled();
    expect(screen.getByText("Opens vercel.com")).toBeInTheDocument();
  });

  it("cancels a sign-in that waits on the browser", async () => {
    const page = "https://vercel.com/oauth/authorize?client_id=tidebreak-1";
    const cancelMcpServerConnect = vi
      .fn()
      .mockResolvedValue({ state: "not_connected" });
    const listMcpServers = vi
      .fn()
      .mockResolvedValueOnce({
        servers: [
          signInServer({
            oauth_status: {
              state: "authorizing",
              pending_authorization_url: page,
            },
          }),
        ],
      })
      .mockResolvedValue({ servers: [signInServer()] });
    const client = api(
      { servers: [signInServer()] },
      { listMcpServers, cancelMcpServerConnect },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(cancelMcpServerConnect).toHaveBeenCalledWith("vercel");
    expect(await screen.findByText("Sign in required")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
    expect(
      screen.queryByRole("button", { name: "Reopen sign-in page" }),
    ).not.toBeInTheDocument();
  });

  it("says a sign-in finishes on the other machine when attached to one", async () => {
    setAttachedRemotely(true);
    render(
      <McpPanel
        client={api({
          servers: [
            signInServer({
              oauth_status: {
                state: "not_connected",
                error:
                  "The sign-in timed out before you finished it. Select Connect to try again.",
              },
            }),
          ],
        })}
      />,
    );
    expect(await screen.findByText("Sign in required")).toBeInTheDocument();
    expect(
      screen.getByText(/has to finish in a browser on that machine/),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Select Connect to try again/),
    ).not.toBeInTheDocument();
  });

  it("says why a sign-in stopped and offers the next step", async () => {
    render(
      <McpPanel
        client={api({
          servers: [
            signInServer({
              name: "denied",
              oauth_status: {
                state: "access_denied",
                error:
                  "The sign-in was canceled or denied. Select Try again to start over.",
              },
            }),
            signInServer({
              name: "late",
              oauth_status: {
                state: "not_connected",
                error:
                  "The sign-in timed out before you finished it. Select Connect to try again.",
              },
            }),
            signInServer({
              name: "legacy",
              diagnostic:
                "This server asks you to sign in, but Tidebreak cannot complete its sign-in. Its sign-in service does not let new apps register (no dynamic client registration), and Tidebreak has no client ID for it. If the server offers access tokens, set a bearer token variable instead.",
              oauth_status: {
                state: "unsupported",
                error:
                  "Its sign-in service does not let new apps register (no dynamic client registration), and Tidebreak has no client ID for it.",
              },
            }),
          ],
        })}
      />,
    );

    const denied = await screen.findByRole("region", { name: "denied" });
    expect(within(denied).getByText("Sign-in denied")).toBeInTheDocument();
    expect(within(denied).getByText(/canceled or denied/)).toBeInTheDocument();
    expect(
      within(denied).getByRole("button", { name: "Try again" }),
    ).toBeEnabled();

    const late = screen.getByRole("region", { name: "late" });
    expect(within(late).getByText("Sign in required")).toBeInTheDocument();
    expect(within(late).getByText(/timed out/)).toBeInTheDocument();
    expect(within(late).getByRole("button", { name: "Connect" })).toBeEnabled();

    // A sign-in Tidebreak cannot run offers no button that cannot work.
    const legacy = screen.getByRole("region", { name: "legacy" });
    expect(
      within(legacy).getByText("Sign-in not supported"),
    ).toBeInTheDocument();
    expect(
      within(legacy).getByText(/set a bearer token variable instead/),
    ).toBeInTheDocument();
    expect(
      within(legacy).queryByRole("button", { name: /Connect|Try again/ }),
    ).not.toBeInTheDocument();
  });

  it("shows a Connect that could not start on the server's row", async () => {
    const refused =
      "The server refused to register Tidebreak for sign-in. It may allow only apps it has approved.";
    const listMcpServers = vi
      .fn()
      .mockResolvedValueOnce({ servers: [signInServer()] })
      .mockResolvedValue({
        servers: [
          signInServer({
            oauth_status: { state: "not_connected", error: refused },
          }),
        ],
      });
    const client = api(
      { servers: [signInServer()] },
      {
        listMcpServers,
        connectMcpServer: vi
          .fn()
          .mockResolvedValue({ state: "not_connected", error: refused }),
      },
    );
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(await screen.findByRole("button", { name: "Connect" }));
    expect(await screen.findByText(refused)).toBeInTheDocument();
    expect(openInBrowser).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("never sends the OAuth status back as part of a definition", async () => {
    const putMcpServers = vi
      .fn()
      .mockResolvedValue({ servers: [signInServer()] });
    const client = api({ servers: [signInServer()] }, { putMcpServers });
    const user = userEvent.setup();
    render(<McpPanel client={client} />);

    await user.click(
      await screen.findByRole("button", { name: "Save and verify" }),
    );
    await waitFor(() => expect(putMcpServers).toHaveBeenCalledTimes(1));
    const [sent] = putMcpServers.mock.calls[0][0] as Record<string, unknown>[];
    expect(sent).not.toHaveProperty("oauth_status");
    expect(sent).not.toHaveProperty("health");
    expect(sent).toMatchObject({
      name: "vercel",
      url: "https://mcp.vercel.com",
      oauth: false,
    });
  });
});

/** The directory entry with this id, from the shared fixture. */
function directoryEntry(id: string) {
  const entry = mcpDirectoryServers.find((server) => server.id === id);
  if (!entry) throw new Error(`no directory fixture ${id}`);
  return entry;
}

/** The directory row that names `name`. */
async function directoryRow(name: string): Promise<HTMLElement> {
  const list = await screen.findByRole("list", { name: "MCP directory" });
  const row = within(list).getByText(name).closest("li");
  if (!row) throw new Error(`no directory row for ${name}`);
  return row;
}

describe("McpPanel directory", () => {
  it("lists servers with how each signs in, and searches them", async () => {
    const user = userEvent.setup();
    render(<McpPanel client={api({ servers: [] })} />);

    const github = await directoryRow("GitHub");
    expect(
      within(github).getByText("GITHUB_PERSONAL_ACCESS_TOKEN"),
    ).toBeInTheDocument();
    expect(within(github).getByText("api.githubcopilot.com")).toHaveClass(
      "font-mono",
    );
    expect(
      within(await directoryRow("Linear")).getByText(
        /Sign in with your browser/,
      ),
    ).toBeInTheDocument();
    expect(
      within(await directoryRow("Cloudflare Docs")).getByText(/No sign-in/),
    ).toBeInTheDocument();
    // No entry claims a tier the curated list did not grant.
    expect(screen.queryByText("Tested")).not.toBeInTheDocument();

    const search = screen.getByRole("searchbox", {
      name: "Search the MCP directory",
    });
    await user.type(search, "issues");
    const list = screen.getByRole("list", { name: "MCP directory" });
    expect(within(list).getByText("Linear")).toBeInTheDocument();
    expect(within(list).getByText("Atlassian")).toBeInTheDocument();
    expect(within(list).queryByText("Stripe")).not.toBeInTheDocument();

    await user.clear(search);
    await user.type(search, "no such server");
    expect(
      screen.queryByRole("list", { name: "MCP directory" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/No server in the directory matches “no such server”/),
    ).toBeInTheDocument();
  });

  it("marks a server already configured at the same address as added", async () => {
    const linear = directoryEntry("linear");
    render(
      <McpPanel
        client={api({
          servers: [
            mcpDirectoryServer(linear, {
              name: "work_linear",
              url: `${linear.url}/`,
            }),
          ],
        })}
      />,
    );

    const row = await directoryRow("Linear");
    expect(within(row).getByText("Added")).toBeInTheDocument();
    expect(
      within(row).queryByRole("button", { name: "Add Linear" }),
    ).not.toBeInTheDocument();
    expect(
      within(await directoryRow("Notion")).getByRole("button", {
        name: "Add Notion",
      }),
    ).toBeEnabled();
  });

  it("adds a server and starts its sign-in when it asks for one", async () => {
    const linear = directoryEntry("linear");
    const page = "https://mcp.linear.app/authorize?client_id=tidebreak-1";
    const needsSignIn = mcpDirectoryServer(linear, {
      health: "degraded",
      tool_count: 0,
      diagnostic:
        "This server needs you to sign in. Select Connect to sign in with your browser.",
      oauth_status: { state: "not_connected", sign_in_host: "mcp.linear.app" },
    });
    let finishAdd: (value: unknown) => void = () => {};
    const addMcpDirectoryServer = vi.fn(
      () =>
        new Promise((resolve) => {
          finishAdd = resolve;
        }),
    );
    const connectMcpServer = vi.fn().mockResolvedValue({
      state: "authorizing",
      pending_authorization_url: page,
      sign_in_host: "mcp.linear.app",
    });
    const user = userEvent.setup();
    render(
      <McpPanel
        client={api(
          { servers: [] },
          {
            addMcpDirectoryServer,
            connectMcpServer,
            listMcpServers: vi
              .fn()
              .mockResolvedValueOnce({ servers: [] })
              .mockResolvedValue({ servers: [needsSignIn] }),
          },
        )}
      />,
    );

    await user.click(
      within(await directoryRow("Linear")).getByRole("button", {
        name: "Add Linear",
      }),
    );
    expect(addMcpDirectoryServer).toHaveBeenCalledWith("linear");
    // While the add runs, its row says so and no other add can start.
    const adding = within(await directoryRow("Linear")).getByRole("button", {
      name: "Add Linear",
    });
    expect(adding).toHaveTextContent("Adding…");
    expect(adding).toBeDisabled();
    expect(
      within(await directoryRow("Notion")).getByRole("button", {
        name: "Add Notion",
      }),
    ).toBeDisabled();

    await act(async () => {
      finishAdd({ name: "linear", servers: [needsSignIn] });
    });
    await waitFor(() =>
      expect(connectMcpServer).toHaveBeenCalledWith("linear"),
    );
    await waitFor(() => expect(openInBrowser).toHaveBeenCalledWith(page));
    expect(toast.message).toHaveBeenCalledWith(
      "Finish signing in to linear in your browser",
    );
    expect(
      within(await directoryRow("Linear")).getByText("Added"),
    ).toBeInTheDocument();
  });

  it("adds a server that needs no sign-in without opening a browser", async () => {
    const docs = directoryEntry("cloudflare_docs");
    const addMcpDirectoryServer = vi.fn().mockResolvedValue({
      name: "cloudflare_docs",
      servers: [mcpDirectoryServer(docs, { tool_count: 2 })],
    });
    const connectMcpServer = vi.fn();
    const user = userEvent.setup();
    render(
      <McpPanel
        client={api(
          { servers: [] },
          { addMcpDirectoryServer, connectMcpServer },
        )}
      />,
    );

    await user.click(
      within(await directoryRow("Cloudflare Docs")).getByRole("button", {
        name: "Add Cloudflare Docs",
      }),
    );
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith("Added Cloudflare Docs"),
    );
    expect(connectMcpServer).not.toHaveBeenCalled();
    expect(openInBrowser).not.toHaveBeenCalled();
    expect(
      await screen.findByText("2 tools available to new turns."),
    ).toBeInTheDocument();
  });

  it("leaves the sign-in to the row when attached to another machine", async () => {
    setAttachedRemotely(true);
    const linear = directoryEntry("linear");
    const needsSignIn = mcpDirectoryServer(linear, {
      health: "degraded",
      tool_count: 0,
      oauth_status: { state: "not_connected", sign_in_host: "mcp.linear.app" },
    });
    const connectMcpServer = vi.fn();
    const user = userEvent.setup();
    render(
      <McpPanel
        client={api(
          { servers: [] },
          {
            connectMcpServer,
            addMcpDirectoryServer: vi
              .fn()
              .mockResolvedValue({ name: "linear", servers: [needsSignIn] }),
          },
        )}
      />,
    );

    await user.click(
      within(await directoryRow("Linear")).getByRole("button", {
        name: "Add Linear",
      }),
    );
    expect(await screen.findByText("Sign in required")).toBeInTheDocument();
    expect(connectMcpServer).not.toHaveBeenCalled();
    expect(openInBrowser).not.toHaveBeenCalled();
  });

  it("says why an add failed beside the directory", async () => {
    const user = userEvent.setup();
    render(
      <McpPanel
        client={api(
          { servers: [] },
          {
            addMcpDirectoryServer: vi
              .fn()
              .mockRejectedValue(
                new HttpError(
                  400,
                  "400: This server asks you to sign in, but Tidebreak cannot complete its sign-in.",
                ),
              ),
          },
        )}
      />,
    );

    await user.click(
      within(await directoryRow("Stripe")).getByRole("button", {
        name: "Add Stripe",
      }),
    );
    // The status prefix comes off, so the reason reads as a sentence.
    expect(
      await screen.findByText(
        "Could not add Stripe: This server asks you to sign in, but Tidebreak cannot complete its sign-in.",
      ),
    ).toBeInTheDocument();
    expect(
      within(await directoryRow("Stripe")).getByRole("button", {
        name: "Add Stripe",
      }),
    ).toBeEnabled();
  });

  it("keeps unsaved edits when a directory add lands", async () => {
    const docs = directoryEntry("cloudflare_docs");
    const putMcpServers = vi.fn().mockResolvedValue(healthy);
    const addMcpDirectoryServer = vi.fn().mockResolvedValue({
      name: "cloudflare_docs",
      servers: [...healthy.servers, mcpDirectoryServer(docs)],
    });
    const user = userEvent.setup();
    render(
      <McpPanel
        client={api(healthy, { addMcpDirectoryServer, putMcpServers })}
      />,
    );

    const namespace = await screen.findByDisplayValue("private_docs");
    await user.clear(namespace);
    await user.type(namespace, "renamed_docs");
    await user.click(
      within(await directoryRow("Cloudflare Docs")).getByRole("button", {
        name: "Add Cloudflare Docs",
      }),
    );
    await waitFor(() => expect(addMcpDirectoryServer).toHaveBeenCalled());
    expect(await screen.findByDisplayValue("cloudflare_docs")).toBeVisible();
    expect(screen.getByDisplayValue("renamed_docs")).toBeVisible();

    // The next save keeps both the edit and the added server.
    await user.click(screen.getByRole("button", { name: "Save and verify" }));
    await waitFor(() => expect(putMcpServers).toHaveBeenCalledTimes(1));
    const names = (putMcpServers.mock.calls[0][0] as { name: string }[]).map(
      (server) => server.name,
    );
    expect(names).toEqual(["renamed_docs", "cloudflare_docs"]);
  });
});

describe("McpPanel after Tidebreak starts", () => {
  it("says a saved server is connecting and follows it until it is up", async () => {
    const linear = directoryEntry("linear");
    const connecting = mcpDirectoryServer(linear, {
      health: "initializing",
      tool_count: 0,
    });
    const up = mcpDirectoryServer(linear, { tool_count: 5 });
    const listMcpServers = vi
      .fn()
      .mockResolvedValueOnce({ servers: [connecting] })
      .mockResolvedValue({ servers: [up] });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    render(
      <McpPanel client={api({ servers: [connecting] }, { listMcpServers })} />,
    );

    expect(await screen.findByText("Connecting")).toBeInTheDocument();
    expect(
      screen.getByText(/Tidebreak is connecting to this server/),
    ).toBeInTheDocument();
    // A saved server is not an unsaved row: it never asks to be saved.
    expect(screen.queryByText("Not verified")).not.toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(
      screen.getByText("5 tools available to new turns."),
    ).toBeInTheDocument();
    const reads = listMcpServers.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(6_000);
    });
    expect(listMcpServers).toHaveBeenCalledTimes(reads);
  });

  it("still calls a new, unsaved row not verified", async () => {
    const user = userEvent.setup();
    render(<McpPanel client={api({ servers: [] })} />);

    await user.click(await screen.findByRole("button", { name: "Add server" }));
    expect(screen.getByText("Not verified")).toBeInTheDocument();
    expect(screen.queryByText("Connecting")).not.toBeInTheDocument();
  });
});

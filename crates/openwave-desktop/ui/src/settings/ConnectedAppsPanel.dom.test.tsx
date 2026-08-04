// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ConnectedAppsInfo, McpServerInfo } from "../api";
import { ConnectedAppsPanel } from "./ConnectedAppsPanel";

const SECRET = "sk-rest-value-hunter2";

/** One configured manual server, so the demoted editor has a row to show. */
const docsServer: McpServerInfo = {
  name: "docs",
  command: "/usr/local/bin/docs-mcp",
  args: [],
  env: {},
  env_from: [],
  cwd: null,
  url: null,
  bearer_token_env: null,
  gateway_endpoint: null,
  request_timeout_ms: 60_000,
  enabled: true,
  health: "healthy",
  tool_count: 3,
  diagnostic: null,
};

/** The app-first listing: a gateway-backed record (org apps ride the
 * `primary` endpoint), a local record, and a REST entry. */
const listing: ConnectedAppsInfo = {
  apps: [
    {
      kind: "mcp_server",
      id: "00000000-0000-4000-8000-000000000000",
      name: "primary",
      health: "healthy",
      tool_count: 2,
      tools: ["sentry__proxy_api", "linear__proxy_api"],
      diagnostic: null,
      gateway_endpoint: "primary",
      gateway_apps: ["Sentry (org)", "Linear (org)"],
    },
    {
      kind: "mcp_server",
      id: "11111111-1111-4111-8111-111111111111",
      name: "docs",
      health: "healthy",
      tool_count: 3,
      tools: ["search", "fetch", "list_sources"],
      diagnostic: null,
      gateway_endpoint: null,
      gateway_apps: [],
    },
    {
      kind: "rest_api",
      id: "22222222-2222-4222-8222-222222222222",
      name: "Sentry",
      base_url: "https://api.sentry.example/v2",
      operation_count: 2,
      document_sha256: "ab".repeat(32),
      credential_status: "configured",
      placement: "bearer",
      updated_at: "2026-08-03T00:00:00Z",
    },
  ],
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    listConnectedApps: vi.fn().mockResolvedValue(listing),
    putRestConnectedApp: vi.fn().mockResolvedValue(listing),
    deleteRestConnectedApp: vi.fn().mockResolvedValue(undefined),
    // The demoted MCP editor reads its own server list and gateway session.
    listMcpServers: vi.fn().mockResolvedValue({ servers: [docsServer] }),
    getGatewayStatus: vi.fn().mockResolvedValue({ signed_in: false }),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    ...overrides,
  } as unknown as ApiClient;
}

/** A signed-in gateway session whose org apps ride the `primary` endpoint,
 * for tests that open the Advanced disclosure's endpoint toggles. */
function signedInOverrides() {
  return {
    getGatewayStatus: vi
      .fn()
      .mockResolvedValue({ signed_in: true, model_count: 1, sign_in: { state: "idle" } }),
    getGatewayApps: vi.fn().mockResolvedValue({
      supported: true,
      apps: [
        {
          id: "app-1",
          name: "Sentry (org)",
          app_kind: "mcp_server",
          enabled: true,
          mcp_endpoint_slugs: ["primary"],
        },
      ],
    }),
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("ConnectedAppsPanel", () => {
  it("leads with apps: org-app names for gateway entries, record names for local ones", async () => {
    render(<ConnectedAppsPanel client={api()} managed={false} />);

    // The gateway-backed entry leads with the organization's app names, not
    // its endpoint slug; the local record is its own app.
    expect(
      await screen.findByText("Sentry (org), Linear (org)"),
    ).toBeInTheDocument();
    expect(screen.getByText("docs")).toBeInTheDocument();
    expect(screen.queryByText("primary")).not.toBeInTheDocument();

    // REST entries are first-class app entries in the same list.
    expect(screen.getByText("Sentry")).toBeInTheDocument();
    expect(
      screen.getByText(
        /api\.sentry\.example\/v2 · 2 operations · Credential configured/,
      ),
    ).toBeInTheDocument();
  });

  it("expands a collapsed accordion to monospace tool names", async () => {
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={api()} managed={false} />);

    await screen.findByText("docs");
    // Collapsed by default: no tool names anywhere.
    expect(screen.queryByText("search")).not.toBeInTheDocument();
    expect(screen.queryByText("sentry__proxy_api")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "3 tools" }));
    const tool = screen.getByText("search");
    expect(tool).toHaveClass("font-mono");
    expect(screen.getByText("list_sources")).toBeInTheDocument();
    // Only the clicked entry expanded.
    expect(screen.queryByText("sentry__proxy_api")).not.toBeInTheDocument();
  });

  it("keeps endpoints and the manual editor inside the Advanced disclosure", async () => {
    const user = userEvent.setup();
    render(
      <ConnectedAppsPanel client={api(signedInOverrides())} managed={false} />,
    );

    await screen.findByText("docs");
    // Before expanding: no mount toggles, no editor, no endpoint section.
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Save and verify/ }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("Gateway endpoints")).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: /Advanced: transport & endpoints/ }),
    );
    // Every existing capability is reachable: the mount toggle, the manual
    // editor with its namespace field, and save.
    expect(
      await screen.findByRole("switch", { name: "Mount primary" }),
    ).toBeInTheDocument();
    expect(await screen.findByLabelText(/Namespace/)).toHaveValue("docs");
    expect(
      screen.getByRole("button", { name: /Save and verify/ }),
    ).toBeInTheDocument();
  });

  it("managed: keeps the notice and mount toggles but no edit affordances", async () => {
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={api(signedInOverrides())} managed />);

    expect(
      await screen.findByText(/Managed by your organization/),
    ).toBeInTheDocument();
    // Entries stay visible read-only; every REST write affordance is gone.
    expect(screen.getByText("Sentry")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Add REST API/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Edit/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /^Remove/ }),
    ).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: /Advanced: transport & endpoints/ }),
    );
    // The endpoint toggle — the one write managed policy admits — stays
    // live; nothing manual is editable.
    expect(
      await screen.findByRole("switch", { name: "Mount primary" }),
    ).toBeEnabled();
    await waitFor(() =>
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "Add server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Save and verify/ }),
    ).not.toBeInTheDocument();
  });

  it("submits the PUT shape from the create form and never renders the value back", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(
      await screen.findByRole("button", { name: /Add REST API/ }),
    );
    await user.type(screen.getByLabelText(/^Name$/), "Issues");
    await user.type(
      screen.getByLabelText(/Base URL/),
      "https://api.example.com/v2",
    );
    await user.type(
      screen.getByLabelText(/OpenAPI document/),
      '{{"openapi": "3.0.3"}',
    );
    await user.click(screen.getByRole("radio", { name: /Bearer token/ }));
    const value = screen.getByLabelText(/Credential value/);
    expect(value).toHaveAttribute("type", "password");
    await user.type(value, SECRET);
    await user.click(screen.getByRole("button", { name: /^Save$/ }));

    await waitFor(() =>
      expect(client.putRestConnectedApp).toHaveBeenCalledWith(
        expect.any(String),
        {
          name: "Issues",
          base_url: "https://api.example.com/v2",
          openapi_document: '{"openapi": "3.0.3"}',
          credential: { set: { value: SECRET, placement: "bearer" } },
        },
      ),
    );
    // After the save the listing re-renders from the server's projection;
    // the value the user typed exists nowhere in the document.
    expect(document.body.textContent).not.toContain(SECRET);
  });

  it("an edit with an untouched value keeps the stored credential", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(await screen.findByRole("button", { name: /Edit/ }));
    await user.type(
      screen.getByLabelText(/OpenAPI document/),
      '{{"openapi": "3.0.3"}',
    );
    await user.click(screen.getByRole("button", { name: /^Save$/ }));

    await waitFor(() =>
      expect(client.putRestConnectedApp).toHaveBeenCalledWith(
        "22222222-2222-4222-8222-222222222222",
        expect.objectContaining({ credential: "keep" }),
      ),
    );
  });
});

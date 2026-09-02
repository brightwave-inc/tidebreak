// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ConnectedAppsInfo, McpServerInfo } from "../api";
import { HttpError } from "../api";
import { ConnectedAppsPanel } from "./ConnectedAppsPanel";

const SECRET = "sk-rest-value-hunter2";

/** One configured manual server, so the editing surface has a row to show. */
const docsServer: McpServerInfo = {
  name: "docs",
  command: "/usr/local/bin/docs-mcp",
  args: [],
  env: [],
  env_from: [],
  cwd: null,
  url: null,
  bearer_token_env: null,
  gateway_endpoint: null,
  request_timeout_ms: 60_000,
  enabled: true,
  plugin: null,
  health: "healthy",
  tool_count: 3,
  diagnostic: null,
  curated: null,
};

/** The app-first listing: a gateway-backed record (org apps ride the
 * `primary` endpoint), a local record two mini-apps bind, and a REST entry
 * one mini-app binds. */
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
      curated: null,
      gateway_endpoint: "primary",
      gateway_apps: ["Sentry (org)", "Linear (org)"],
      used_by_app_count: 0,
    },
    {
      kind: "mcp_server",
      id: "11111111-1111-4111-8111-111111111111",
      name: "docs",
      health: "healthy",
      tool_count: 3,
      tools: ["search", "fetch", "list_documents"],
      diagnostic: null,
      curated: null,
      gateway_endpoint: null,
      gateway_apps: [],
      used_by_app_count: 2,
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
      used_by_app_count: 1,
    },
  ],
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    listConnectedApps: vi.fn().mockResolvedValue(listing),
    putRestConnectedApp: vi.fn().mockResolvedValue(listing),
    deleteRestConnectedApp: vi.fn().mockResolvedValue(undefined),
    reconnectMcpServer: vi.fn().mockResolvedValue({ servers: [docsServer] }),
    // The embedded MCP surface reads its own server list and gateway session.
    listMcpServers: vi.fn().mockResolvedValue({ servers: [docsServer] }),
    getGatewayStatus: vi.fn().mockResolvedValue({ signed_in: false }),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    ...overrides,
  } as unknown as ApiClient;
}

/** A signed-in gateway session whose org apps ride the `primary` endpoint,
 * for tests that exercise the endpoint rows. */
function signedInOverrides() {
  return {
    getGatewayStatus: vi.fn().mockResolvedValue({
      signed_in: true,
      model_count: 1,
      sign_in: { state: "idle" },
    }),
    getGatewayApps: vi.fn().mockResolvedValue({
      supported: true,
      apps: [
        {
          id: "app-1",
          name: "Sentry (org)",
          app_kind: "mcp_server",
          enabled: true,
          mcp_endpoint_slugs: ["primary"],
          used_by_app_count: 0,
        },
      ],
    }),
  };
}

/** The one list container the app entries live in. */
async function appsList(): Promise<HTMLElement> {
  return await screen.findByRole("list", { name: "Connected apps" });
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("ConnectedAppsPanel", () => {
  it("leads with apps: org-app names, health chips, and bound-app counts", async () => {
    render(<ConnectedAppsPanel client={api()} managed={false} />);

    // The gateway-backed entry leads with the organization's app names, not
    // its endpoint slug; the local record is its own app.
    const list = await appsList();
    expect(
      within(list).getByText("Sentry (org), Linear (org)"),
    ).toBeInTheDocument();
    expect(within(list).getByText("docs")).toBeInTheDocument();
    expect(within(list).queryByText("primary")).not.toBeInTheDocument();

    // REST entries are first-class app entries in the same list.
    expect(within(list).getByText("Sentry")).toBeInTheDocument();
    expect(
      within(list).getByText(
        /api\.sentry\.example\/v2 · 2 operations · Credential configured/,
      ),
    ).toBeInTheDocument();

    // Bound-app counts render per entry — and only when non-zero.
    expect(within(list).getByText("Used by 2 local apps")).toBeInTheDocument();
    expect(within(list).getByText("Used by 1 local app")).toBeInTheDocument();
    expect(screen.queryByText(/Used by 0/)).not.toBeInTheDocument();
  });

  it("expands a collapsed accordion to monospace tool names", async () => {
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={api()} managed={false} />);

    await appsList();
    // Collapsed by default: no tool names anywhere.
    expect(screen.queryByText("search")).not.toBeInTheDocument();
    expect(screen.queryByText("sentry__proxy_api")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "3 tools" }));
    const tool = screen.getByText("search");
    expect(tool).toHaveClass("font-mono");
    expect(screen.getByText("list_documents")).toBeInTheDocument();
    // Only the clicked entry expanded.
    expect(screen.queryByText("sentry__proxy_api")).not.toBeInTheDocument();
  });

  it("unmanaged: no Advanced section; the editor follows the apps list", async () => {
    render(
      <ConnectedAppsPanel client={api(signedInOverrides())} managed={false} />,
    );

    const list = await appsList();
    // No gateway indirection to hide: there is no Advanced disclosure, and
    // the endpoint toggles and the manual editor are directly reachable.
    expect(
      screen.queryByRole("button", { name: /Advanced: transport/ }),
    ).not.toBeInTheDocument();
    expect(
      await screen.findByRole("switch", { name: "Mount primary" }),
    ).toBeInTheDocument();
    expect(await screen.findByLabelText(/Namespace/)).toHaveValue("docs");
    const save = screen.getByRole("button", { name: /Save and verify/ });
    // The editing surface no longer leads: the apps list comes first.
    expect(
      list.compareDocumentPosition(save) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();

    // The reconnect action rides the entries themselves.
    expect(
      within(list).getAllByRole("button", { name: /Reconnect/ }).length,
    ).toBeGreaterThan(0);

    // The approval boundary is stated once, as the page footer.
    expect(screen.getAllByText(/existing approval boundary/)).toHaveLength(1);
  });

  it("managed: prose notice, once-only facts, and a collapsed Advanced", async () => {
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={api(signedInOverrides())} managed />);

    // The managed notice is one line of muted prose under the Apps heading —
    // not a card, and not an entry inside the list.
    const notice = await screen.findByText(
      /REST connected apps are managed by your organization's gateway/,
    );
    expect(notice.tagName).toBe("P");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    const list = await appsList();
    expect(list.contains(notice)).toBe(false);

    // Entries stay visible read-only; every REST write affordance is gone,
    // and no entry carries a reconnect action — that lives on the endpoint
    // row under Advanced.
    expect(within(list).getByText("Sentry")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Add REST API/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Edit/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /^Remove/ }),
    ).not.toBeInTheDocument();
    expect(
      within(list).queryByRole("button", { name: /Reconnect/ }),
    ).not.toBeInTheDocument();

    // Advanced is collapsed by default: no endpoint machinery on screen.
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: /Advanced: transport & endpoints/ }),
    );
    // Compact rows: the mount toggle (the one write managed policy admits)
    // and the serves-list, with no inner headings and nothing editable.
    expect(
      await screen.findByRole("switch", { name: "Mount primary" }),
    ).toBeEnabled();
    expect(screen.getByText(/serves: Sentry \(org\)/)).toBeInTheDocument();
    expect(screen.queryByText("Gateway endpoints")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "Add server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Save and verify/ }),
    ).not.toBeInTheDocument();

    // Every fact once: with Advanced open, the tool count still renders only
    // on the app entry, and the old status sentence is gone everywhere.
    expect(screen.getAllByText("2 tools")).toHaveLength(1);
    expect(
      screen.queryByText(/available to new turns/),
    ).not.toBeInTheDocument();
    expect(screen.getAllByText(/existing approval boundary/)).toHaveLength(1);
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
    await user.click(screen.getByRole("radio", { name: /Paste document/ }));
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

  it("fetches a spec by URL, picks operations, and saves under the hash pin", async () => {
    const preview = {
      document_sha256: "cd".repeat(32),
      operations: [
        {
          operation_id: "listOrganizations",
          method: "get",
          path: "/api/organizations/",
          summary: "List organizations.",
        },
        {
          operation_id: "deleteOrganization",
          method: "delete",
          path: "/api/organizations/{id}/",
          summary: null,
        },
      ],
      unlistable: 3,
      truncated: false,
    };
    const client = api({
      previewRestSpec: vi.fn().mockResolvedValue(preview),
    });
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(
      await screen.findByRole("button", { name: /Add REST API/ }),
    );
    await user.type(screen.getByLabelText(/^Name$/), "PostHog");
    await user.type(
      screen.getByLabelText(/Base URL/),
      "https://us.posthog.example",
    );
    // URL is the default source: no paste textarea on screen.
    expect(screen.queryByLabelText(/OpenAPI document/)).not.toBeInTheDocument();
    await user.type(
      screen.getByLabelText(/Document URL/),
      "https://us.posthog.example/api/schema/?format=json",
    );
    await user.click(screen.getByRole("button", { name: /Fetch operations/ }));

    // Everything under the catalog bound starts selected, and the picker is
    // honest about what it could not list.
    const picker = await screen.findByRole("list", { name: "Operations" });
    expect(
      within(picker).getByRole("checkbox", {
        name: /GET \/api\/organizations\//,
      }),
    ).toBeChecked();
    expect(
      screen.getByText(/2 of 2 selected · 3 unselectable/),
    ).toBeInTheDocument();

    // Drop the destructive one; the saved catalog is exactly the selection.
    await user.click(
      within(picker).getByRole("checkbox", {
        name: /DELETE \/api\/organizations\/\{id\}\//,
      }),
    );
    await user.click(screen.getByRole("button", { name: /^Save$/ }));

    await waitFor(() =>
      expect(client.putRestConnectedApp).toHaveBeenCalledWith(
        expect.any(String),
        {
          name: "PostHog",
          base_url: "https://us.posthog.example",
          openapi_document_url:
            "https://us.posthog.example/api/schema/?format=json",
          document_sha256: "cd".repeat(32),
          operation_ids: ["listOrganizations"],
          credential: "none",
        },
      ),
    );
  });

  it("discovery fills the document URL from a candidate and fetches operations", async () => {
    const preview = {
      document_sha256: "cd".repeat(32),
      operations: [
        {
          operation_id: "listOrganizations",
          method: "get",
          path: "/api/organizations/",
          summary: null,
        },
      ],
      unlistable: 0,
      truncated: false,
    };
    const client = api({
      discoverRestSpec: vi.fn().mockResolvedValue({
        candidates: [
          {
            url: "https://api.example.com/openapi.json",
            operation_count: 1,
            unsupported_reason: null,
          },
        ],
        tried: [
          "https://api.example.com/openapi.json",
          "https://api.example.com/swagger.json",
        ],
      }),
      previewRestSpec: vi.fn().mockResolvedValue(preview),
    });
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(
      await screen.findByRole("button", { name: /Add REST API/ }),
    );
    await user.type(
      screen.getByLabelText(/Base URL/),
      "https://api.example.com",
    );
    await user.click(
      screen.getByRole("button", { name: /Find the OpenAPI document/ }),
    );
    await user.click(
      await screen.findByRole("button", { name: /Use this document/ }),
    );

    await waitFor(() =>
      expect(client.previewRestSpec).toHaveBeenCalledWith({
        url: "https://api.example.com/openapi.json",
      }),
    );
    expect(screen.getByLabelText(/Document URL/)).toHaveValue(
      "https://api.example.com/openapi.json",
    );
    expect(
      await screen.findByRole("list", { name: "Operations" }),
    ).toBeInTheDocument();
  });

  it("discovery with no candidates lists the locations tried", async () => {
    const client = api({
      discoverRestSpec: vi.fn().mockResolvedValue({
        candidates: [],
        tried: [
          "https://api.example.com/openapi.json",
          "https://api.example.com/swagger.json",
        ],
      }),
    });
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(
      await screen.findByRole("button", { name: /Add REST API/ }),
    );
    await user.type(
      screen.getByLabelText(/Base URL/),
      "https://api.example.com",
    );
    await user.click(
      screen.getByRole("button", { name: /Find the OpenAPI document/ }),
    );

    expect(
      await screen.findByText(/No OpenAPI document turned up/),
    ).toBeInTheDocument();
    await user.click(screen.getByText("Locations tried"));
    expect(
      screen.getByText("https://api.example.com/openapi.json"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Paste this example/ }),
    ).toBeInTheDocument();
  });

  it("an edit with an untouched value keeps the stored credential", async () => {
    const client = api();
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(await screen.findByRole("button", { name: /Edit/ }));
    await user.click(screen.getByRole("radio", { name: /Paste document/ }));
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

  it("shows the server ingest message under the paste textarea, without a bare 400", async () => {
    const client = api({
      previewRestSpec: vi
        .fn()
        .mockRejectedValue(
          new HttpError(
            400,
            "400: JSON invalid JSON syntax at line 2, column 5. Check line 2, column 5",
            "openapi_ingest",
          ),
        ),
    });
    const user = userEvent.setup();
    render(<ConnectedAppsPanel client={client} managed={false} />);

    await user.click(
      await screen.findByRole("button", { name: /Add REST API/ }),
    );
    await user.click(screen.getByRole("radio", { name: /Paste document/ }));
    await user.type(screen.getByLabelText(/OpenAPI document/), '{{"openapi":');
    await user.click(screen.getByRole("button", { name: /Select operations/ }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "JSON invalid JSON syntax at line 2, column 5. Check line 2, column 5",
    );
    expect(alert).not.toHaveTextContent(/^400:/);
  });
});

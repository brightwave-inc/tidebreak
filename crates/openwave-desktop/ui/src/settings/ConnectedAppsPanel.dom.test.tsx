// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, ConnectedAppsInfo } from "../api";
import { ConnectedAppsPanel } from "./ConnectedAppsPanel";

const SECRET = "sk-rest-value-hunter2";

const bothKinds: ConnectedAppsInfo = {
  apps: [
    {
      kind: "mcp_server",
      id: "11111111-1111-4111-8111-111111111111",
      name: "docs",
      health: "healthy",
      tool_count: 3,
      diagnostic: null,
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
    listConnectedApps: vi.fn().mockResolvedValue(bothKinds),
    putRestConnectedApp: vi.fn().mockResolvedValue(bothKinds),
    deleteRestConnectedApp: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("ConnectedAppsPanel", () => {
  it("lists both kinds with per-kind detail and deep-links MCP editing", async () => {
    const client = api();
    const onOpenMcpSettings = vi.fn();
    const user = userEvent.setup();
    render(
      <ConnectedAppsPanel
        client={client}
        managed={false}
        onOpenMcpSettings={onOpenMcpSettings}
      />,
    );

    expect(await screen.findByText("docs")).toBeInTheDocument();
    expect(screen.getByText(/Healthy — 3 tools/)).toBeInTheDocument();
    expect(screen.getByText("Sentry")).toBeInTheDocument();
    expect(
      screen.getByText(/api\.sentry\.example\/v2 · 2 operations · Credential configured/),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Manage MCP servers/ }));
    expect(onOpenMcpSettings).toHaveBeenCalled();
  });

  it("managed: shows the read-only notice and no REST editing affordances", async () => {
    render(
      <ConnectedAppsPanel
        client={api()}
        managed
        onOpenMcpSettings={() => undefined}
      />,
    );

    expect(
      await screen.findByText(/Managed by your organization/),
    ).toBeInTheDocument();
    // Entries stay visible read-only; every write affordance is gone.
    expect(screen.getByText("Sentry")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Add REST API/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Edit/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Remove/ }),
    ).not.toBeInTheDocument();
  });

  it("submits the PUT shape from the create form and never renders the value back", async () => {
    const client = api();
    const user = userEvent.setup();
    render(
      <ConnectedAppsPanel
        client={client}
        managed={false}
        onOpenMcpSettings={() => undefined}
      />,
    );

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
    render(
      <ConnectedAppsPanel
        client={client}
        managed={false}
        onOpenMcpSettings={() => undefined}
      />,
    );

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

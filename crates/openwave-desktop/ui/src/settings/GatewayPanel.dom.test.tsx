// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, GatewayStatus } from "../api";
import { GatewayPanel } from "./GatewayPanel";

const signedOut: GatewayStatus = {
  configured: true,
  enabled: true,
  base_url: "http://127.0.0.1:28081",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

const signedIn: GatewayStatus = {
  ...signedOut,
  signed_in: true,
  account_hint: "abaas@example.test",
  installation_id: "install-1",
  model_count: 2,
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    getGatewayStatus: vi.fn().mockResolvedValue(signedOut),
    gatewaySignIn: vi
      .fn()
      .mockResolvedValue({ authorization_url: "http://gw/oauth/authorize?x=1" }),
    gatewaySignOut: vi.fn().mockResolvedValue(signedOut),
    syncGatewayModels: vi.fn().mockResolvedValue(signedIn),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    listMcpServers: vi.fn().mockResolvedValue({ servers: [] }),
    putMcpServers: vi.fn().mockResolvedValue({ servers: [] }),
    putProvider: vi.fn().mockResolvedValue({}),
    ...overrides,
  } as unknown as ApiClient;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("GatewayPanel", () => {
  it("connects through the browser and never asks for a credential", async () => {
    const client = api();
    const open = vi.fn();
    vi.stubGlobal("open", open);
    const user = userEvent.setup();
    render(<GatewayPanel client={client} onChanged={() => undefined} />);

    expect(await screen.findByText("Not signed in")).toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/API key/i)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Connect/ }));
    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
    expect(open).toHaveBeenCalledWith(
      "http://gw/oauth/authorize?x=1",
      "_blank",
      "noreferrer,noopener",
    );
    vi.unstubAllGlobals();
  });

  it("shows identity and entitlements when signed in, and disconnects", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    const onChanged = vi.fn();
    const user = userEvent.setup();
    render(<GatewayPanel client={client} onChanged={onChanged} />);

    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.getByText(/abaas@example\.test/)).toBeInTheDocument();
    expect(screen.getByText(/2 models entitled/)).toBeInTheDocument();
    expect(screen.getByText(/Installation install-1/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Refresh models/ }));
    await waitFor(() => expect(client.syncGatewayModels).toHaveBeenCalled());
    expect(onChanged).toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: /Disconnect/ }));
    await waitFor(() => expect(client.gatewaySignOut).toHaveBeenCalled());
  });

  it("saves the gateway URL as provider configuration", async () => {
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValue({ ...signedOut, configured: false, base_url: undefined }),
    });
    const user = userEvent.setup();
    render(<GatewayPanel client={client} onChanged={() => undefined} />);

    await screen.findByText("Not signed in");
    // Unconfigured, the connect affordance stays off.
    expect(screen.getByRole("button", { name: /Connect/ })).toBeDisabled();

    await user.type(
      screen.getByPlaceholderText("https://gateway.example"),
      "http://127.0.0.1:28081",
    );
    await user.click(screen.getByRole("button", { name: "Save gateway URL" }));
    await waitFor(() =>
      expect(client.putProvider).toHaveBeenCalledWith("model_gateway", {
        enabled: true,
        base_url: "http://127.0.0.1:28081",
      }),
    );
  });

  it("watches a pending browser sign-in and refreshes the catalog on completion", async () => {
    const getGatewayStatus = vi
      .fn()
      .mockResolvedValueOnce({
        ...signedOut,
        sign_in: { state: "pending", authorization_url: "http://gw/authorize" },
      })
      .mockResolvedValue(signedIn);
    const client = api({ getGatewayStatus });
    const onChanged = vi.fn();
    vi.useFakeTimers();
    render(<GatewayPanel client={client} onChanged={onChanged} />);
    // Flush the initial status load.
    await act(async () => {});

    const reopen = screen.getByRole("link", {
      name: /Open the sign-in page again/,
    });
    expect(reopen).toHaveAttribute("href", "http://gw/authorize");

    // The poll notices the background exchange completing.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    expect(screen.getByText("Signed in")).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalled();
  });

  it("lists entitled connected apps, and hides the section on an older gateway", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayApps: vi.fn().mockResolvedValue({
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
      }),
    });
    render(<GatewayPanel client={client} onChanged={() => undefined} />);

    expect(await screen.findByText("Incident API")).toBeInTheDocument();
    expect(screen.getByText(/via example-security-tools/)).toBeInTheDocument();

    cleanup();
    // A gateway that predates the JSON apps surface: no section, no error.
    const older = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayApps: vi
        .fn()
        .mockResolvedValue({ supported: false, apps: [] }),
    });
    render(<GatewayPanel client={older} onChanged={() => undefined} />);
    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.queryByText("Connected apps")).not.toBeInTheDocument();
  });

  it("mounts a gateway endpoint with a session-bound definition", async () => {
    const putMcpServers = vi.fn().mockResolvedValue({
      servers: [
        {
          name: "example-security-tools",
          command: null,
          args: [],
          env: {},
          env_from: [],
          cwd: null,
          url: null,
          bearer_token_env: null,
          gateway_endpoint: "example-security-tools",
          request_timeout_ms: 60_000,
          enabled: true,
          health: "healthy",
          tool_count: 3,
          diagnostic: null,
        },
      ],
    });
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayApps: vi.fn().mockResolvedValue({
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
      }),
      putMcpServers,
    });
    const user = userEvent.setup();
    render(<GatewayPanel client={client} onChanged={() => undefined} />);

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
    // The saved mount reports its health inline.
    expect(await screen.findByText(/3 tools available/)).toBeInTheDocument();
  });

  it("surfaces a failed sign-in with its bounded message", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedOut,
        sign_in: { state: "failed", message: "browser authorization timed out" },
      }),
    });
    render(<GatewayPanel client={client} onChanged={() => undefined} />);

    expect(
      await screen.findByText(/browser authorization timed out/),
    ).toBeInTheDocument();
  });
});

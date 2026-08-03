// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, GatewayStatus } from "../api";
import { GatewayPanel } from "./GatewayPanel";

const GATEWAY_URL = "http://127.0.0.1:28081/";

const signedOut: GatewayStatus = {
  base_url: GATEWAY_URL,
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
    putProvider: vi.fn().mockResolvedValue({}),
    ...overrides,
  } as unknown as ApiClient;
}

function managedPanel(
  client: ApiClient,
  {
    onChanged = () => undefined,
    onOpenConnectedApps = () => undefined,
  }: {
    onChanged?: () => void;
    onOpenConnectedApps?: () => void;
  } = {},
) {
  return (
    <GatewayPanel
      client={client}
      managed
      gatewayUrl={GATEWAY_URL}
      onChanged={onChanged}
      onOpenConnectedApps={onOpenConnectedApps}
    />
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("GatewayPanel", () => {
  it("unmanaged: renders the pairing signpost with no gateway configuration surface", async () => {
    const client = api();
    render(
      <GatewayPanel
        client={client}
        managed={false}
        gatewayUrl={null}
        onChanged={() => undefined}
        onOpenConnectedApps={() => undefined}
      />,
    );

    expect(
      screen.getByText(/not connected to a model gateway/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/gateway's own page/i)).toBeInTheDocument();
    // No URL field, no enable toggle, no connect flow — and no gateway
    // traffic at all: policy is the only way a profile becomes connected.
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Connect/ }),
    ).not.toBeInTheDocument();
    expect(client.getGatewayStatus).not.toHaveBeenCalled();
  });

  it("managed: shows the read-only policy origin and connects through the browser", async () => {
    const client = api();
    const open = vi.fn();
    vi.stubGlobal("open", open);
    const user = userEvent.setup();
    render(managedPanel(client));

    expect(await screen.findByText("Not signed in")).toBeInTheDocument();
    // The origin comes from policy and is display-only: no input to edit
    // it, no toggle to turn the gateway off, and no credential prompt.
    expect(screen.getByText(GATEWAY_URL)).toBeInTheDocument();
    expect(screen.getByText(/not editable here/i)).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/API key/i)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Connect/ }));
    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
    // Connecting is the sign-in flow alone — never a provider write.
    expect(client.putProvider).not.toHaveBeenCalled();
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
    render(managedPanel(client, { onChanged }));

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
    render(managedPanel(client, { onChanged }));
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
    render(managedPanel(client));

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
    render(managedPanel(older));
    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.queryByText("Connected apps")).not.toBeInTheDocument();
    // The route to mounting still shows: older gateways mount by slug too.
    expect(
      screen.getByRole("button", { name: /Mount endpoints in Connected apps/ }),
    ).toBeInTheDocument();
  });

  it("keeps connected apps informational and sends mounting to Connected apps", async () => {
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
    const openConnectedApps = vi.fn();
    const user = userEvent.setup();
    render(managedPanel(client, { onOpenConnectedApps: openConnectedApps }));

    expect(await screen.findByText("Incident API")).toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /^Mount / }),
    ).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: /Mount endpoints in Connected apps/ }),
    );
    expect(openConnectedApps).toHaveBeenCalled();
  });

  it("a policy that names no gateway cannot be connected to", async () => {
    // Misconfigured managed policy: there is no deployment to sign in
    // against, so the affordance stays off rather than failing on click.
    // The server derives status from the same policy, so it names no
    // origin either.
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValue({ ...signedOut, base_url: undefined }),
    });
    render(
      <GatewayPanel
        client={client}
        managed
        gatewayUrl={null}
        onChanged={() => undefined}
        onOpenConnectedApps={() => undefined}
      />,
    );

    expect(await screen.findByText("Not signed in")).toBeInTheDocument();
    expect(screen.getByText(/names no gateway/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Connect/ })).toBeDisabled();
  });

  it("surfaces a failed sign-in with its bounded message", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedOut,
        sign_in: { state: "failed", message: "browser authorization timed out" },
      }),
    });
    render(managedPanel(client));

    expect(
      await screen.findByText(/browser authorization timed out/),
    ).toBeInTheDocument();
  });
});

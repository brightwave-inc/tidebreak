// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, GatewayStatus, RemoteMachineState } from "../api";
import { GatewayPanel, type MachineControls } from "./GatewayPanel";

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
    gatewaySignIn: vi.fn().mockResolvedValue({
      authorization_url: "http://gw/oauth/authorize?x=1",
    }),
    gatewaySignOut: vi.fn().mockResolvedValue(signedOut),
    syncGatewayModels: vi.fn().mockResolvedValue(signedIn),
    getGatewayApps: vi.fn().mockResolvedValue({ supported: true, apps: [] }),
    getGatewayMachine: vi.fn().mockResolvedValue({}),
    putProvider: vi.fn().mockResolvedValue({}),
    ...overrides,
  } as unknown as ApiClient;
}

const local: RemoteMachineState = { attachment: "local", baseUrl: null };
const attached: RemoteMachineState = {
  attachment: "remote",
  baseUrl: "https://machine.example",
};

/** The native machine commands, stubbed: nothing here reaches a shell. */
function machineStub(
  state: RemoteMachineState = local,
): MachineControls & { attachWithGateway: ReturnType<typeof vi.fn> } {
  return {
    read: vi.fn().mockResolvedValue(state),
    attachWithGateway: vi.fn().mockResolvedValue(state),
    attachWithToken: vi.fn().mockResolvedValue(state),
    detach: vi.fn().mockResolvedValue(local),
    detachable: true,
    reattach: vi.fn(),
  };
}

function managedPanel(
  client: ApiClient,
  {
    onChanged = () => undefined,
    onOpenConnectedApps = () => undefined,
    machine = machineStub(),
  }: {
    onChanged?: () => void;
    onOpenConnectedApps?: () => void;
    machine?: MachineControls;
  } = {},
) {
  return (
    <GatewayPanel
      client={client}
      managed
      gatewayUrl={GATEWAY_URL}
      onChanged={onChanged}
      onOpenConnectedApps={onOpenConnectedApps}
      machine={machine}
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
        machine={machineStub()}
      />,
    );

    expect(
      screen.getByText(/not connected to a model gateway/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/gateway's own page/i)).toBeInTheDocument();
    // No gateway URL field, no enable toggle, no sign-in — and no gateway
    // traffic at all: policy is the only way a profile becomes connected,
    // and there is no gateway here to ask which machine it hosts.
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Connect" }),
    ).not.toBeInTheDocument();
    expect(client.getGatewayStatus).not.toHaveBeenCalled();
    expect(client.getGatewayMachine).not.toHaveBeenCalled();

    // The machine does show. Hiding it would leave a machine behind no
    // gateway — the standalone-token case — with no route in the app.
    expect(await screen.findByText("Working on this computer")).toBeVisible();
    expect(screen.getByLabelText(/Address/)).toHaveValue("");
    expect(
      screen.getByRole("button", { name: /Connect with Model Gateway/ }),
    ).toBeInTheDocument();
  });

  it("hosted machine: names the gateway and offers no session to manage", async () => {
    // Callers authenticate *to* this machine with their own gateway account.
    // It holds no session of its own, so a sign-in here would start a flow it
    // could never complete — and the panel must not read a status it has no
    // session behind either.
    const client = api();
    render(
      <GatewayPanel
        client={client}
        managed={false}
        gatewayUrl={null}
        hostedGatewayUrl="https://gateway.example/"
        onChanged={() => undefined}
        onOpenConnectedApps={() => undefined}
        machine={machineStub(attached)}
      />,
    );

    expect(screen.getByText("https://gateway.example/")).toBeInTheDocument();
    expect(
      screen.getByText(/authenticates you with your Model Gateway account/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/not connected to a model gateway/i),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Connect" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Sync/ }),
    ).not.toBeInTheDocument();
    expect(client.getGatewayStatus).not.toHaveBeenCalled();

    // The machine still says where the work runs, and how to come back.
    expect(
      await screen.findByText("Attached to a remote machine"),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /Work on this computer/ }),
    ).toBeInTheDocument();
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
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/API key/i)).not.toBeInTheDocument();
    // The one text field on the page is the machine address below. Nothing
    // edits the gateway origin.
    expect(screen.getAllByRole("textbox")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "Connect" }));
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

    await user.click(screen.getByRole("button", { name: /Sync with gateway/ }));
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

  it("starts over when a pending browser sign-in stalls", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedOut,
        sign_in: { state: "pending", authorization_url: "http://gw/stalled" },
      }),
    });
    const open = vi.fn();
    vi.stubGlobal("open", open);
    const user = userEvent.setup();
    render(managedPanel(client));

    expect(
      await screen.findByText(/Waiting for the browser/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start over" }));

    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
    expect(open).toHaveBeenCalledWith(
      "http://gw/oauth/authorize?x=1",
      "_blank",
      "noreferrer,noopener",
    );
    vi.unstubAllGlobals();
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
            used_by_app_count: 2,
          },
        ],
      }),
    });
    render(managedPanel(client));

    expect(await screen.findByText("Incident API")).toBeInTheDocument();
    expect(screen.getByText(/via example-security-tools/)).toBeInTheDocument();
    // What a revocation here would break: the local apps bound to this app.
    expect(screen.getByText("Used by 2 local apps")).toBeInTheDocument();

    cleanup();
    // A gateway that predates the JSON apps surface: no section, no error.
    const older = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayApps: vi.fn().mockResolvedValue({ supported: false, apps: [] }),
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
            used_by_app_count: 2,
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
    expect(screen.getByRole("button", { name: "Connect" })).toBeDisabled();
  });

  it("surfaces a failed sign-in with its bounded message", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedOut,
        sign_in: {
          state: "failed",
          message: "browser authorization timed out",
        },
      }),
    });
    render(managedPanel(client));

    expect(
      await screen.findByText(/browser authorization timed out/),
    ).toBeInTheDocument();
  });

  it("soft-warns about a gateway older than this Tidebreak, and stays quiet on a current one", async () => {
    // A synced snapshot with no member-catalog revision is the older-gateway
    // shape; the note is informational and nothing is disabled by it.
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    render(managedPanel(client));
    expect(
      await screen.findByText(/older than this Tidebreak/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Sync with gateway/ }),
    ).toBeEnabled();
    cleanup();

    const current = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValue({ ...signedIn, member_catalog: "v1" }),
    });
    render(managedPanel(current));
    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.queryByText(/older than this Tidebreak/)).toBeNull();
  });

  it("labels an entitled app the gateway reports as not ready", async () => {
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValue({ ...signedIn, member_catalog: "v1" }),
      getGatewayApps: vi.fn().mockResolvedValue({
        supported: true,
        apps: [
          {
            id: "app-1",
            name: "Tools",
            app_kind: "mcp_endpoint",
            enabled: true,
            mcp_endpoint_slugs: ["tools"],
            connection: "authorization_required",
            used_by_app_count: 0,
          },
          {
            id: "app-2",
            name: "Monitors",
            app_kind: "rest_api",
            enabled: true,
            mcp_endpoint_slugs: [],
            connection: "ready",
            used_by_app_count: 0,
          },
        ],
      }),
    });
    render(managedPanel(client));

    expect(
      await screen.findByText(/authorize this app at your gateway/),
    ).toBeInTheDocument();
    // A ready app carries no readiness copy at all.
    expect(screen.queryByText(/not ready at your gateway/)).toBeNull();
    expect(screen.getByText("Monitors")).toBeInTheDocument();
  });

  it("prefills the address with the machine the gateway offers", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayMachine: vi
        .fn()
        .mockResolvedValue({ url: "https://tidebreak.example.com" }),
    });
    const machine = machineStub();
    render(managedPanel(client, { machine }));

    await waitFor(() =>
      expect(screen.getByLabelText(/Address/)).toHaveValue(
        "https://tidebreak.example.com",
      ),
    );
    expect(screen.getByText(/Offered by your gateway/)).toBeInTheDocument();
    // Filling the field is the whole of it. Attaching moves the reader's
    // work to another machine, so it stays a deliberate click.
    expect(machine.attachWithGateway).not.toHaveBeenCalled();
    expect(screen.getByText("Working on this computer")).toBeInTheDocument();
  });

  it("leaves the address empty when the gateway offers no machine", async () => {
    // A gateway that hosts nothing, and one older than the field, answer the
    // same way. Absence is a state, not a failure.
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
      getGatewayMachine: vi.fn().mockRejectedValue(new Error("no such route")),
    });
    render(managedPanel(client));

    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.getByLabelText(/Address/)).toHaveValue("");
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("keeps the standalone token behind Advanced", async () => {
    // The only client path to a machine running on a static token file, so
    // it stays reachable — but it is not what a managed reader needs, so it
    // starts collapsed.
    const user = userEvent.setup();
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    render(managedPanel(client));

    expect(await screen.findByText("Signed in")).toBeInTheDocument();
    expect(screen.queryByLabelText(/Standalone token/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Advanced" }));
    expect(screen.getByLabelText(/Standalone token/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Connect with token/ }),
    ).toBeDisabled();
  });
});

// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, GatewayStatus, ManagedPolicy } from "./api";
import { ManagedGate } from "./ManagedGate";
import { useManagedPolicy } from "./managedPolicy";

// The policy stores its URL normalized with a trailing slash; the provider
// config carries what the user (or a convergence) saved, typically without.
// The fixtures differ deliberately so every match exercises normalization.
const managed: ManagedPolicy = {
  managed: true,
  gateway_url: "https://gateway.example/",
  source: "os",
  misconfigured: false,
};

const signedOut: GatewayStatus = {
  configured: true,
  enabled: true,
  base_url: "https://gateway.example",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

const signedIn: GatewayStatus = {
  ...signedOut,
  signed_in: true,
  account_hint: "abaas@example.test",
  model_count: 2,
};

function api(overrides: Partial<Record<keyof ApiClient, unknown>> = {}) {
  return {
    getPolicy: vi.fn().mockResolvedValue(managed),
    getGatewayStatus: vi.fn().mockResolvedValue(signedOut),
    gatewaySignIn: vi
      .fn()
      .mockResolvedValue({ authorization_url: "http://gw/oauth/authorize?x=1" }),
    putProvider: vi.fn().mockResolvedValue({}),
    ...overrides,
  } as unknown as ApiClient;
}

function mount(client: ApiClient) {
  return render(
    <ManagedGate client={client}>
      <p>the open product</p>
    </ManagedGate>,
  );
}

/** Stands in for the settings panels, which gate themselves on the policy the
 * gate publishes rather than fetching it again. */
function PolicyProbe() {
  const policy = useManagedPolicy();
  return <p>policy: {policy.managed ? "managed" : "unmanaged"}</p>;
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("ManagedGate", () => {
  it("managed and signed out: gates the app and starts the browser sign-in", async () => {
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValueOnce(signedOut)
        .mockResolvedValue({
          ...signedOut,
          sign_in: { state: "pending", authorization_url: "http://gw/authorize" },
        }),
    });
    const open = vi.fn();
    vi.stubGlobal("open", open);
    const user = userEvent.setup();
    mount(client);

    expect(await screen.findByText("Sign in to continue")).toBeInTheDocument();
    // The gateway identity is the policy's, shown but locked — no input.
    expect(screen.getByText("https://gateway.example/")).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Connect/ }));
    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
    // The config already points at the policy gateway (modulo the trailing
    // slash), so there is nothing to converge.
    expect(client.putProvider).not.toHaveBeenCalled();
    expect(open).toHaveBeenCalledWith(
      "http://gw/oauth/authorize?x=1",
      "_blank",
      "noreferrer,noopener",
    );
    // The reload after starting the flow reports it pending.
    expect(await screen.findByText(/Waiting for the browser/)).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /Open the sign-in page again/ }),
    ).toHaveAttribute("href", "http://gw/authorize");
  });

  it("managed and signed in: renders the app, and a sign-out brings the gate back", async () => {
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValueOnce(signedIn)
        .mockResolvedValue(signedOut),
    });
    vi.useFakeTimers();
    mount(client);
    // Flush the policy fetch, then the status fetch it triggers.
    await act(async () => {});
    await act(async () => {});
    expect(screen.getByText("the open product")).toBeInTheDocument();
    expect(screen.queryByText("Sign in to continue")).not.toBeInTheDocument();

    // The session watch notices the sign-out and lowers the gate again.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    expect(screen.getByText("Sign in to continue")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
  });

  it("pairing mid-session flips the app to managed without a restart", async () => {
    const unmanaged: ManagedPolicy = {
      managed: false,
      source: "unmanaged",
      misconfigured: false,
    };
    // The profile starts open, then a deep-link pairing provisions it and the
    // session already satisfies the new policy.
    const getPolicy = vi
      .fn()
      .mockResolvedValueOnce(unmanaged)
      .mockResolvedValue(managed);
    const client = api({
      getPolicy,
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    vi.useFakeTimers();
    render(
      <ManagedGate client={client}>
        <PolicyProbe />
      </ManagedGate>,
    );
    await act(async () => {});
    // Unmanaged: the app renders, and what it reads from the gate says so —
    // this is the value the Providers and MCP panels gate themselves on.
    expect(screen.getByText("policy: unmanaged")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    await act(async () => {});
    expect(screen.getByText("policy: managed")).toBeInTheDocument();
  });

  it("a mid-session sign-out on a newly managed profile raises the gate", async () => {
    const getPolicy = vi
      .fn()
      .mockResolvedValueOnce({
        managed: false,
        source: "unmanaged",
        misconfigured: false,
      } satisfies ManagedPolicy)
      .mockResolvedValue(managed);
    const client = api({
      getPolicy,
      getGatewayStatus: vi.fn().mockResolvedValue(signedOut),
    });
    vi.useFakeTimers();
    mount(client);
    await act(async () => {});
    expect(screen.getByText("the open product")).toBeInTheDocument();

    // Policy flips to managed with no session: the gate comes up on the same
    // watch tick rather than at the next launch.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    await act(async () => {});
    expect(screen.getByText("Sign in to continue")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
  });

  it("a session on the wrong gateway does not lift the gate", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedIn,
        base_url: "https://other.example",
      }),
    });
    mount(client);

    expect(await screen.findByText("Sign in to continue")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
    // What is shown is the device's managed gateway, not the stray config.
    expect(screen.getByText("https://gateway.example/")).toBeInTheDocument();
    expect(screen.queryByText(/other\.example/)).not.toBeInTheDocument();
  });

  it("connect on an unconfigured profile converges the provider to the policy gateway first", async () => {
    const client = api({
      getGatewayStatus: vi
        .fn()
        .mockResolvedValueOnce({
          ...signedOut,
          configured: false,
          base_url: undefined,
        })
        .mockResolvedValue(signedOut),
    });
    vi.stubGlobal("open", vi.fn());
    const user = userEvent.setup();
    mount(client);

    await screen.findByText("Sign in to continue");
    await user.click(screen.getByRole("button", { name: /Connect/ }));

    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
    expect(client.putProvider).toHaveBeenCalledWith("model_gateway", {
      enabled: true,
      base_url: "https://gateway.example/",
    });
    // Converge first, then sign in — the other order still refuses.
    const putOrder = vi.mocked(client.putProvider).mock.invocationCallOrder[0];
    const signInOrder = vi.mocked(client.gatewaySignIn).mock
      .invocationCallOrder[0];
    expect(putOrder).toBeLessThan(signInOrder);
  });

  it("unmanaged profiles see the app untouched and no gateway traffic", async () => {
    const client = api({
      getPolicy: vi
        .fn()
        .mockResolvedValue({ managed: false, source: "unmanaged" }),
    });
    mount(client);

    expect(await screen.findByText("the open product")).toBeInTheDocument();
    expect(screen.queryByText("Sign in to continue")).not.toBeInTheDocument();
    expect(client.getGatewayStatus).not.toHaveBeenCalled();
  });

  it("an error response from /policy blocks the app until a retry succeeds", async () => {
    // The server answered: the policy exists but cannot be read. Fail closed.
    const client = api({
      getPolicy: vi
        .fn()
        .mockRejectedValueOnce(new Error("500: managed policy unreadable"))
        .mockResolvedValue(managed),
    });
    const user = userEvent.setup();
    mount(client);

    expect(
      await screen.findByText(/Contact your administrator/),
    ).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
    expect(screen.queryByText("Sign in to continue")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Retry/ }));
    expect(await screen.findByText("Sign in to continue")).toBeInTheDocument();
  });

  it("a transport failure never opens the app: boot holds until the policy answers", async () => {
    // The server is not up yet. Rejected fetches and timeouts are not an
    // answer, so nothing may be concluded from them — least of all
    // "unmanaged". The read retries behind the boot screen until it lands.
    const getPolicy = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("fetch failed"))
      .mockRejectedValueOnce(new TypeError("fetch failed"))
      .mockResolvedValue(managed);
    const getGatewayStatus = vi
      .fn()
      .mockRejectedValueOnce(new Error("503: starting"))
      .mockResolvedValue(signedOut);
    const client = api({ getPolicy, getGatewayStatus });
    vi.useFakeTimers();
    mount(client);

    // First attempt fails at the transport level: still booting.
    await act(async () => {});
    expect(screen.getByText("starting…")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();

    // Through the first retry (1s backoff): still failing, still booting.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_100);
    });
    expect(screen.getByText("starting…")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();

    // The second retry (2s backoff) resolves managed. The first status fetch
    // fails too, so the gate rises carrying that error.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_100);
    });
    expect(screen.getByText("Sign in to continue")).toBeInTheDocument();
    expect(screen.getByText(/503: starting/)).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();

    // The watch's next tick recovers the status and clears the stale error.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_100);
    });
    expect(screen.queryByText(/503: starting/)).not.toBeInTheDocument();
    expect(screen.getByText("Sign in to continue")).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
  });

  it("a managed policy without a gateway URL blocks instead of lifting on any session", async () => {
    const client = api({
      getPolicy: vi.fn().mockResolvedValue({ managed: true, source: "os" }),
      getGatewayStatus: vi.fn().mockResolvedValue(signedIn),
    });
    mount(client);

    expect(
      await screen.findByText(/Contact your administrator/),
    ).toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();
  });

  it("surfaces a failed browser sign-in and offers to try again", async () => {
    const client = api({
      getGatewayStatus: vi.fn().mockResolvedValue({
        ...signedOut,
        sign_in: { state: "failed", message: "browser authorization timed out" },
      }),
    });
    mount(client);

    expect(
      await screen.findByText(/browser authorization timed out/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Connect/ })).toBeEnabled();
  });
});

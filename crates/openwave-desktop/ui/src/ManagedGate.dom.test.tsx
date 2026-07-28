// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiClient, GatewayStatus, ManagedPolicy } from "./api";
import { ManagedGate } from "./ManagedGate";

const managed: ManagedPolicy = {
  managed: true,
  gateway_url: "https://gateway.example",
  source: "os",
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

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.useRealTimers();
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
    // The gateway identity is shown but locked — no input to change it.
    expect(screen.getByText("https://gateway.example")).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByText("the open product")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Connect/ }));
    await waitFor(() => expect(client.gatewaySignIn).toHaveBeenCalled());
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
    vi.unstubAllGlobals();
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

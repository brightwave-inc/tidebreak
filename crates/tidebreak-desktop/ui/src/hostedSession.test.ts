// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

import { HostedSignInRequired, hostedServerInfo } from "./boot";
import {
  captureHandoffToken,
  handoffBearer,
  handoffFailure,
  hostedSession,
  resetHostedSessionForTests,
} from "./hostedSession";
import { remoteMachineState } from "./remoteMachine";

function fakeWindow(hash: string): Window & { replaced: string[] } {
  const replaced: string[] = [];
  return {
    location: { hash, pathname: "/", search: "" },
    history: {
      state: null,
      replaceState: (_state: unknown, _title: string, url: string) => {
        replaced.push(url);
      },
    },
    replaced,
  } as unknown as Window & { replaced: string[] };
}

function discovery(body: unknown, ok = true): typeof globalThis.fetch {
  return vi.fn(async () => ({
    ok,
    json: async () => body,
  })) as unknown as typeof globalThis.fetch;
}

afterEach(() => {
  resetHostedSessionForTests();
});

describe("the handoff fragment", () => {
  it("is taken into memory and cleared from the address before the router sees it", () => {
    const win = fakeWindow("#handoff=mg_at_abc.DEF-123~");
    captureHandoffToken(win);
    expect(handoffBearer()).toBe("mg_at_abc.DEF-123~");
    expect(win.replaced).toEqual(["/"]);
  });

  it("leaves a route fragment alone", () => {
    const win = fakeWindow("#/settings/machine");
    captureHandoffToken(win);
    expect(handoffBearer()).toBeNull();
    expect(win.replaced).toEqual([]);
  });

  it("refuses a fragment that is not a bare token", () => {
    const win = fakeWindow("#handoff=<script>alert(1)</script>");
    captureHandoffToken(win);
    expect(handoffBearer()).toBeNull();
  });

  it("keeps the landing route's failure reason and clears it from the address", () => {
    const win = fakeWindow("#handoff-failed=expired");
    captureHandoffToken(win);
    expect(handoffBearer()).toBeNull();
    expect(handoffFailure()).toBe("expired");
    expect(win.replaced).toEqual(["/"]);
  });

  it("ignores a failure reason it has no words for", () => {
    const win = fakeWindow("#handoff-failed=something-new");
    captureHandoffToken(win);
    expect(handoffFailure()).toBeNull();
    expect(win.replaced).toEqual([]);
  });
});

describe("the hosted boot branch", () => {
  it("is off in the dev server, whose pages are the bundle itself", async () => {
    const fetch = discovery({ mode: "gateway", gateway_url: "https://g" });
    await expect(
      hostedServerInfo({ origin: "http://localhost:1420", dev: true, fetch }),
    ).resolves.toBeNull();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("attaches remotely to its own origin with the bearer it was handed", async () => {
    const fetch = discovery({
      mode: "gateway",
      gateway_url: "https://gateway.example.com/",
      resource: "tidebreak:abc",
    });
    const info = await hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch,
      bearer: "mg_at_token",
    });
    expect(info).toEqual({
      baseUrl: "https://tidebreak.example.com",
      token: "mg_at_token",
      attachment: "remote",
      gatewayAuth: true,
    });
    expect(fetch).toHaveBeenCalledWith(
      "https://tidebreak.example.com/auth/discovery",
      expect.objectContaining({ cache: "no-store" }),
    );
    expect(hostedSession()).toEqual({
      baseUrl: "https://tidebreak.example.com",
      gatewayUrl: "https://gateway.example.com",
    });
    // The gate and the Machine panel read the attachment from here, and a
    // browser tab has no shell to ask.
    await expect(remoteMachineState()).resolves.toEqual({
      attachment: "remote",
      baseUrl: "https://tidebreak.example.com",
    });
  });

  it("asks for a sign-in, naming the console, when the page holds no bearer", async () => {
    const fetch = discovery({
      mode: "gateway",
      gateway_url: "https://gateway.example.com",
    });
    const attempt = hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch,
      bearer: null,
    });
    await expect(attempt).rejects.toBeInstanceOf(HostedSignInRequired);
    await attempt.catch((error: HostedSignInRequired) => {
      expect(error.gatewayUrl).toBe("https://gateway.example.com");
      expect(error.failure).toBeNull();
    });
  });

  it("carries the landing route's failure reason to the sign-in screen", async () => {
    const fetch = discovery({
      mode: "gateway",
      gateway_url: "https://gateway.example.com",
    });
    await expect(
      hostedServerInfo({
        origin: "https://tidebreak.example.com",
        dev: false,
        fetch,
        bearer: null,
        failure: "unavailable",
      }),
    ).rejects.toMatchObject({ failure: "unavailable" });
  });

  it("has no console to name for a machine on static tokens", async () => {
    const fetch = discovery({ mode: "static_token" });
    const attempt = hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch,
      bearer: null,
    });
    await expect(attempt).rejects.toMatchObject({ gatewayUrl: null });
  });

  it("is not a machine when the origin answers no discovery document", async () => {
    await expect(
      hostedServerInfo({
        origin: "https://static.example.com",
        dev: false,
        fetch: discovery("<!doctype html>", false),
        bearer: "mg_at_token",
      }),
    ).resolves.toBeNull();
    await expect(
      hostedServerInfo({
        origin: "https://static.example.com",
        dev: false,
        fetch: vi.fn(async () => {
          throw new TypeError("Load failed");
        }) as unknown as typeof globalThis.fetch,
        bearer: "mg_at_token",
      }),
    ).resolves.toBeNull();
    expect(hostedSession()).toBeNull();
    await expect(remoteMachineState()).resolves.toEqual({
      attachment: "local",
      baseUrl: null,
    });
  });
});

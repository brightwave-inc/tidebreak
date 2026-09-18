// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  HostedSignInRequired,
  acceptPastedToken,
  hostedServerInfo,
  resolveServerInfo,
} from "./boot";
import {
  HOME_DRAFT_KEY,
  hydrateComposerDraftFromHostedReentry,
  useComposerDrafts,
} from "./ComposerDrafts";
import {
  allowHostedReentryRetry,
  captureHandoffToken,
  consoleSignInUrl,
  forgetHostedBrowserSession,
  handoffBearer,
  handoffFailure,
  hostedSession,
  noteHostedSessionEstablished,
  oidcSignInUrl,
  reenterExpiredHostedSession,
  reenterReloadedHostedSession,
  resetHostedSessionForTests,
  stashComposerDraftForReentry,
  takeComposerDraftForReentry,
} from "./hostedSession";
import { remoteMachineState } from "./remoteMachine";

function fakeWindow(
  hash: string,
  pathname = "/",
  search = "",
): Window & { replaced: string[] } {
  const replaced: string[] = [];
  return {
    location: { hash, pathname, search },
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
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

describe("the handoff fragment", () => {
  it("is taken into memory and cleared from the address before the router sees it", () => {
    const win = fakeWindow("#handoff=mg_at_abc.DEF-123~");
    captureHandoffToken(win);
    expect(handoffBearer()).toBe("mg_at_abc.DEF-123~");
    expect(win.replaced).toEqual(["/"]);
  });

  it("restores a return route for the hash router while clearing the bearer", () => {
    const win = fakeWindow(
      "#handoff=mg_at_abc.DEF-123%7E&return_to=%2Fconnect%2Fnonce-1%3Fsource%3Dslack",
      "/tidebreak/",
    );
    captureHandoffToken(win);
    expect(handoffBearer()).toBe("mg_at_abc.DEF-123~");
    expect(win.replaced).toEqual(["/tidebreak/#/connect/nonce-1?source=slack"]);
  });

  it.each(["/code/s/session-1", "/code/w/workspace-1?task=session-2"])(
    "preserves the exact Slack destination %s through sign-in",
    (route) => {
      const win = fakeWindow(
        `#handoff=mg_at_abc&return_to=${encodeURIComponent(route)}`,
      );
      captureHandoffToken(win);
      expect(win.replaced).toEqual([`/#${route}`]);
      const restored = fakeWindow(`#${route}`);
      captureHandoffToken(restored);
      expect(restored.replaced).toEqual([]);
    },
  );

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
      discovery: {
        mode: "gateway",
        gateway_url: "https://gateway.example.com/",
        resource: "",
      },
    });
    expect(
      window.sessionStorage.getItem("tidebreak.hostedSessionContinuity"),
    ).toBe("1");
    expect(storedValues(window.sessionStorage)).not.toContain("mg_at_token");
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
      expect(error.discovery).toMatchObject({
        mode: "gateway",
        gateway_url: "https://gateway.example.com",
      });
      expect(error.failure).toBeNull();
    });
    expect(
      window.sessionStorage.getItem("tidebreak.hostedSessionContinuity"),
    ).toBeNull();
  });

  it.each(["network", "503", "429", "invalid JSON"])(
    "recovers from a transient %s discovery failure without losing the route",
    async (failure) => {
      const originalUrl = window.location.href;
      window.history.replaceState(null, "", "#/code/s/pending-question");
      try {
        const valid = {
          mode: "gateway",
          gateway_url: "https://gateway.example.com",
        };
        const fetch = vi
          .fn()
          .mockImplementationOnce(async () => {
            if (failure === "network") throw new TypeError("Load failed");
            if (failure === "invalid JSON") return new Response("<html>");
            return new Response(null, { status: Number(failure) });
          })
          .mockResolvedValueOnce(new Response(JSON.stringify(valid)));
        await expect(
          hostedServerInfo({
            origin: "https://tidebreak.example.com",
            dev: false,
            fetch,
            bearer: null,
          }),
        ).rejects.toBeInstanceOf(HostedSignInRequired);
        expect(fetch).toHaveBeenCalledTimes(2);
        expect(window.location.hash).toBe("#/code/s/pending-question");
        expect(hostedSession()?.discovery).toMatchObject(valid);
      } finally {
        window.history.replaceState(null, "", originalUrl);
      }
    },
  );

  it("bounds failed discovery retries and keeps development instructions out of hosted errors", async () => {
    const fetch = vi.fn().mockRejectedValue(new TypeError("Load failed"));
    vi.stubGlobal("fetch", fetch);
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_TIDEBREAK_URL", "");
    vi.stubEnv("VITE_TIDEBREAK_TOKEN", "");
    await expect(resolveServerInfo()).rejects.toThrow(
      `Could not reach Tidebreak at ${window.location.origin}. Try again.`,
    );
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(hostedSession()).toBeNull();
  });

  it("does not retry a missing discovery route", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response(null, { status: 404 }));
    await expect(hostedServerInfo({ dev: false, fetch })).resolves.toBeNull();
    expect(fetch).toHaveBeenCalledTimes(1);
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

  it("offers the token field, not a console, for a machine on a token file", async () => {
    const fetch = discovery({ mode: "static_token" });
    const attempt = hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch,
      bearer: null,
    });
    await expect(attempt).rejects.toMatchObject({
      discovery: { mode: "static_token" },
    });
    expect(hostedSession()).toMatchObject({
      baseUrl: "https://tidebreak.example.com",
      gatewayUrl: null,
      discovery: { mode: "static_token" },
    });
  });

  it("carries an OIDC machine's issuer and start URL to the sign-in screen", async () => {
    const attempt = hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch: discovery({
        mode: "oidc",
        issuer_name: "login.example.test",
        start_url: "https://tidebreak.example.com/auth/oidc/start",
      }),
      bearer: null,
    });
    await expect(attempt).rejects.toMatchObject({
      discovery: {
        mode: "oidc",
        issuer_name: "login.example.test",
        start_url: "https://tidebreak.example.com/auth/oidc/start",
      },
    });
    // An OIDC machine is standalone: there is no console to send anyone to.
    expect(hostedSession()).toMatchObject({ gatewayUrl: null });
  });

  it("is not a machine when its discovery document names a mode it cannot read", async () => {
    await expect(
      hostedServerInfo({
        origin: "https://tidebreak.example.com",
        dev: false,
        fetch: discovery({ mode: "oidc", issuer_name: "login.example.test" }),
        bearer: null,
      }),
    ).resolves.toBeNull();
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

  it("a later reload of a signed-in tab renews through the console with the route", async () => {
    const fetch = discovery({
      mode: "gateway",
      gateway_url: "https://gateway.example.com",
    });
    await hostedServerInfo({
      origin: "https://tidebreak.example.com",
      dev: false,
      fetch,
      bearer: "mg_at_token",
    });
    const win = navWindow("#/code/w/workspace-1?task=session-2");
    expect(reenterReloadedHostedSession(hostedSession(), null, win)).toBe(
      "redirect",
    );
    expect(win.location.href).toBe(
      "https://gateway.example.com/tidebreak?return_to=%2Fcode%2Fw%2Fworkspace-1%3Ftask%3Dsession-2",
    );
    expect(storedValues(window.sessionStorage)).not.toContain("mg_at_token");
  });
});

function navWindow(
  hash: string,
  origin = "https://machine.example.test",
): {
  location: { hash: string; href: string; origin: string };
} {
  let href = `${origin}/`;
  return {
    location: {
      hash,
      origin,
      get href() {
        return href;
      },
      set href(next: string) {
        href = next;
      },
    },
  };
}

function storedValues(storage: Storage): string[] {
  const values: string[] = [];
  for (let index = 0; index < storage.length; index++) {
    const key = storage.key(index);
    if (key) values.push(storage.getItem(key) ?? "");
  }
  return values;
}

function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() {
      return data.size;
    },
    clear() {
      data.clear();
    },
    getItem(key: string) {
      return data.get(key) ?? null;
    },
    key(index: number) {
      return [...data.keys()][index] ?? null;
    },
    removeItem(key: string) {
      data.delete(key);
    },
    setItem(key: string, value: string) {
      data.set(key, value);
    },
  };
}

describe("a pasted token", () => {
  function probe(ok: boolean): typeof globalThis.fetch {
    return vi.fn(async () => ({ ok })) as unknown as typeof globalThis.fetch;
  }

  it("is probed against the machine and then held in memory alone", async () => {
    const fetch = probe(true);
    await expect(
      acceptPastedToken("alice-token-one-padded-to-thirty-two", {
        origin: "https://tidebreak.example.com",
        fetch,
      }),
    ).resolves.toBe(true);
    expect(fetch).toHaveBeenCalledWith(
      "https://tidebreak.example.com/auth/token-sign-in",
      expect.objectContaining({
        method: "POST",
        cache: "no-store",
        headers: expect.objectContaining({
          authorization: "Bearer alice-token-one-padded-to-thirty-two",
        }),
      }),
    );
    // The same slot the hand-off bearer lands in, and nowhere else.
    expect(handoffBearer()).toBe("alice-token-one-padded-to-thirty-two");
    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
    expect(document.cookie).toBe("");
  });

  it("leaves the tab with no bearer when the machine refuses it", async () => {
    await expect(
      acceptPastedToken("wrong-token-padded-out-to-thirty-two", {
        origin: "https://tidebreak.example.com",
        fetch: probe(false),
      }),
    ).resolves.toBe(false);
    expect(handoffBearer()).toBeNull();
  });

  it("is not sent at all when it could not be a bearer", async () => {
    const fetch = probe(true);
    for (const pasted of ["", "has spaces", "line\nbreak", "a".repeat(513)]) {
      await expect(
        acceptPastedToken(pasted, {
          origin: "https://tidebreak.example.com",
          fetch,
        }),
      ).resolves.toBe(false);
    }
    expect(fetch).not.toHaveBeenCalled();
    expect(handoffBearer()).toBeNull();
  });

  it("is refused, not thrown, when the machine cannot be reached", async () => {
    await expect(
      acceptPastedToken("alice-token-one-padded-to-thirty-two", {
        origin: "https://tidebreak.example.com",
        fetch: vi.fn(async () => {
          throw new TypeError("network down");
        }) as unknown as typeof globalThis.fetch,
      }),
    ).resolves.toBe(false);
    expect(handoffBearer()).toBeNull();
  });
});

describe("consoleSignInUrl", () => {
  it("sends the reader to the console's Tidebreak page with this page as the return path", () => {
    const win = {
      location: {
        pathname: "/tidebreak/",
        search: "",
        hash: "#/connect/nonce-1?source=slack",
      },
    } as unknown as Window;
    expect(consoleSignInUrl("https://gateway.example.test/", win)).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fconnect%2Fnonce-1%3Fsource%3Dslack",
    );
  });

  it("asks for no return path from the root", () => {
    const win = {
      location: { pathname: "/tidebreak/", search: "", hash: "#/" },
    } as unknown as Window;
    expect(consoleSignInUrl("https://gateway.example.test", win)).toBe(
      "https://gateway.example.test/tidebreak",
    );
  });
});

/**
 * The same return-path contract, on the machine's own start route: a Slack
 * link to a session survives the trip through the issuer.
 */
describe("oidcSignInUrl", () => {
  function win(hash: string): { location: { origin: string; hash: string } } {
    return {
      location: { origin: "https://machine.example.test", hash },
    };
  }

  it("carries this page's hash route through the issuer", () => {
    expect(
      oidcSignInUrl("/auth/oidc/start", win("#/c/session-1?source=slack")),
    ).toBe(
      "https://machine.example.test/auth/oidc/start?return_to=%2Fc%2Fsession-1%3Fsource%3Dslack",
    );
  });

  it("asks for no return path from the root, and keeps an absolute start URL", () => {
    expect(
      oidcSignInUrl(
        "https://machine.example.test/tidebreak/auth/oidc/start",
        win("#/"),
      ),
    ).toBe("https://machine.example.test/tidebreak/auth/oidc/start");
  });
});

describe("reenterExpiredHostedSession", () => {
  it("navigates a gateway machine to the console with the current hash route", () => {
    const win = navWindow("#/c/chat-1");
    const outcome = reenterExpiredHostedSession(
      {
        baseUrl: "https://machine.example.test",
        gatewayUrl: "https://gateway.example.test",
      },
      win,
    );
    expect(outcome).toBe("redirect");
    expect(win.location.href).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fc%2Fchat-1",
    );
  });

  it("renders sign-in when a hand-off is refused again inside the loop window", () => {
    captureHandoffToken(fakeWindow("#handoff=mg_at_abc.DEF-123~"));
    const win = navWindow("#/c/chat-1");
    const outcome = reenterExpiredHostedSession(
      {
        baseUrl: "https://machine.example.test",
        gatewayUrl: "https://gateway.example.test",
      },
      win,
    );
    expect(outcome).toBe("sign_in");
    expect(win.location.href).toBe("https://machine.example.test/");
  });

  it("renders sign-in on a standalone machine", () => {
    const win = navWindow("#/c/chat-1");
    const outcome = reenterExpiredHostedSession(
      {
        baseUrl: "https://machine.example.test",
        gatewayUrl: null,
      },
      win,
    );
    expect(outcome).toBe("sign_in");
    expect(win.location.href).toBe("https://machine.example.test/");
  });

  it("records the attempt so a failed console return does not loop on reload", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterExpiredHostedSession(
        {
          baseUrl: "https://machine.example.test",
          gatewayUrl: "https://gateway.example.test",
        },
        navWindow("#/c/chat-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    const reload = navWindow("#/c/chat-1");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        reload,
        now + 1_000,
        storage,
      ),
    ).toBe("sign_in");
    expect(reload.location.href).toBe("https://machine.example.test/");
  });
});

const gatewayHosted = {
  baseUrl: "https://machine.example.test",
  gatewayUrl: "https://gateway.example.test",
  discovery: {
    mode: "gateway" as const,
    gateway_url: "https://gateway.example.test",
    resource: "tidebreak",
  },
};

const oidcHosted = {
  baseUrl: "https://machine.example.test",
  gatewayUrl: null,
  discovery: {
    mode: "oidc" as const,
    issuer_name: "login.example.test",
    start_url: "/auth/oidc/start",
  },
};

const staticHosted = {
  baseUrl: "https://machine.example.test",
  gatewayUrl: null,
  discovery: { mode: "static_token" as const },
};

describe("reenterReloadedHostedSession", () => {
  it("renews a gateway tab through the console, keeping the workspace and task query", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const win = navWindow("#/code/w/workspace-1?task=session-2");
    const outcome = reenterReloadedHostedSession(
      gatewayHosted,
      null,
      win,
      Date.now(),
      storage,
    );
    expect(outcome).toBe("redirect");
    expect(win.location.href).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fcode%2Fw%2Fworkspace-1%3Ftask%3Dsession-2",
    );
    expect(storedValues(storage)).not.toContain("mg_at_token");
    expect(storage.getItem("tidebreak.hostedSessionContinuity")).toBe("1");
  });

  it("renews an OIDC tab through the machine's start URL, keeping the hash route", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const win = navWindow("#/code/s/session-1?source=slack");
    const outcome = reenterReloadedHostedSession(
      oidcHosted,
      null,
      win,
      Date.now(),
      storage,
    );
    expect(outcome).toBe("redirect");
    expect(win.location.href).toBe(
      "https://machine.example.test/auth/oidc/start?return_to=%2Fcode%2Fs%2Fsession-1%3Fsource%3Dslack",
    );
  });

  it("leaves a first visit on sign-in so typing the address does not bounce", () => {
    const storage = memoryStorage();
    const win = navWindow("#/code/w/workspace-1?task=session-2");
    const outcome = reenterReloadedHostedSession(
      gatewayHosted,
      null,
      win,
      Date.now(),
      storage,
    );
    expect(outcome).toBe("sign_in");
    expect(win.location.href).toBe("https://machine.example.test/");
  });

  it("does not renew a static-token tab, whose contract forgets the bearer on reload", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const win = navWindow("#/c/chat-1");
    const outcome = reenterReloadedHostedSession(
      staticHosted,
      null,
      win,
      Date.now(),
      storage,
    );
    expect(outcome).toBe("sign_in");
    expect(win.location.href).toBe("https://machine.example.test/");
  });

  it("shows the hand-off failure instead of retrying an expired or revoked grant", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const win = navWindow("#/code/s/session-1");
    for (const failure of ["expired", "invalid", "unavailable"] as const) {
      const outcome = reenterReloadedHostedSession(
        gatewayHosted,
        failure,
        win,
        Date.now(),
        storage,
      );
      expect(outcome).toBe("sign_in");
    }
    expect(win.location.href).toBe("https://machine.example.test/");
  });

  it("does not silently sign back in after sign-out", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    forgetHostedBrowserSession(storage);
    const win = navWindow("#/code/s/session-1");
    const outcome = reenterReloadedHostedSession(
      gatewayHosted,
      null,
      win,
      Date.now(),
      storage,
    );
    expect(outcome).toBe("sign_in");
    expect(win.location.href).toBe("https://machine.example.test/");
    expect(handoffBearer()).toBeNull();
  });

  it("staying on the sign-in screen drops continuity so a later reload does not renew", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now + 1_000,
        storage,
      ),
    ).toBe("sign_in");
    forgetHostedBrowserSession(storage);
    const later = navWindow("#/code/s/session-1");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        later,
        now + 20_000,
        storage,
      ),
    ).toBe("sign_in");
    expect(later.location.href).toBe("https://machine.example.test/");
  });

  it("treats a missing gateway login as a loop instead of redirecting again", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    const first = navWindow("#/code/s/session-1");
    expect(
      reenterReloadedHostedSession(gatewayHosted, null, first, now, storage),
    ).toBe("redirect");
    const second = navWindow("#/code/s/session-1");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        second,
        now + 1_000,
        storage,
      ),
    ).toBe("sign_in");
    expect(second.location.href).toBe("https://machine.example.test/");
  });

  it("consumes one automatic renewal across a round trip longer than the loop window", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const first = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        first,
        1_000_000,
        storage,
      ),
    ).toBe("redirect");
    expect(first.location.href).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fcode%2Fw%2Fworkspace-1%3Ftask%3Dsession-2",
    );
    expect(storedValues(storage)).not.toContain("mg_at_token");
    const second = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        second,
        1_015_001,
        storage,
      ),
    ).toBe("sign_in");
    expect(second.location.href).toBe("https://machine.example.test/");
    const third = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        third,
        1_045_002,
        storage,
      ),
    ).toBe("sign_in");
    expect(third.location.href).toBe("https://machine.example.test/");
  });

  it("stays on sign-in across repeated reloads with no bearer", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    for (const later of [now + 1_000, now + 15_001, now + 120_000]) {
      const reload = navWindow("#/code/s/session-1");
      expect(
        reenterReloadedHostedSession(
          gatewayHosted,
          null,
          reload,
          later,
          storage,
        ),
      ).toBe("sign_in");
      expect(reload.location.href).toBe("https://machine.example.test/");
      expect(storedValues(storage)).not.toContain("mg_at_token");
    }
  });

  it("allows another renewal after a successful sign-in", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    noteHostedSessionEstablished(storage);
    const again = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        again,
        now + 45_002,
        storage,
      ),
    ).toBe("redirect");
    expect(again.location.href).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fcode%2Fw%2Fworkspace-1%3Ftask%3Dsession-2",
    );
    expect(storedValues(storage)).not.toContain("mg_at_token");
  });

  it("allows another renewal after an explicit retry", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now + 15_001,
        storage,
      ),
    ).toBe("sign_in");
    allowHostedReentryRetry(storage);
    const retry = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        retry,
        now + 45_002,
        storage,
      ),
    ).toBe("redirect");
    expect(retry.location.href).toBe(
      "https://gateway.example.test/tidebreak?return_to=%2Fcode%2Fw%2Fworkspace-1%3Ftask%3Dsession-2",
    );
  });

  it("does not renew after sign-out even after the loop window", () => {
    const storage = memoryStorage();
    noteHostedSessionEstablished(storage);
    const now = 1_000_000;
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        navWindow("#/code/s/session-1"),
        now,
        storage,
      ),
    ).toBe("redirect");
    forgetHostedBrowserSession(storage);
    const later = navWindow("#/code/w/workspace-1?task=session-2");
    expect(
      reenterReloadedHostedSession(
        gatewayHosted,
        null,
        later,
        now + 45_002,
        storage,
      ),
    ).toBe("sign_in");
    expect(later.location.href).toBe("https://machine.example.test/");
    expect(handoffBearer()).toBeNull();
    expect(storage.getItem("tidebreak.hostedSessionContinuity")).toBeNull();
  });

  it("keeps concurrent tabs independent: only the tab that held a session renews", () => {
    const tabA = memoryStorage();
    const tabB = memoryStorage();
    noteHostedSessionEstablished(tabA);
    const winA = navWindow("#/code/s/from-a");
    const winB = navWindow("#/code/s/from-b");
    expect(
      reenterReloadedHostedSession(gatewayHosted, null, winA, Date.now(), tabA),
    ).toBe("redirect");
    expect(
      reenterReloadedHostedSession(gatewayHosted, null, winB, Date.now(), tabB),
    ).toBe("sign_in");
    expect(winA.location.href).toContain("return_to=%2Fcode%2Fs%2Ffrom-a");
    expect(winB.location.href).toBe("https://machine.example.test/");
  });
});

describe("hosted re-entry composer draft", () => {
  afterEach(() => {
    useComposerDrafts.getState().clearDraft("chat-1");
    useComposerDrafts.getState().clearDraft(HOME_DRAFT_KEY);
  });

  it("survives the round trip and is deleted after it is read", () => {
    const storage = memoryStorage();
    const route = "/c/chat-1";
    stashComposerDraftForReentry(route, "unsent hello", storage);
    expect(storage.getItem(`tidebreak.hostedReentryDraft:${route}`)).toBe(
      "unsent hello",
    );
    expect(takeComposerDraftForReentry(route, storage)).toBe("unsent hello");
    expect(storage.getItem(`tidebreak.hostedReentryDraft:${route}`)).toBeNull();
    expect(takeComposerDraftForReentry(route, storage)).toBeNull();
  });

  it("hydrates the composer from storage once on boot", () => {
    const storage = memoryStorage();
    const route = "/c/chat-1";
    stashComposerDraftForReentry(route, "keep this", storage);
    resetHostedSessionForTests();
    const win = { location: { hash: `#${route}` } };
    hydrateComposerDraftFromHostedReentry(win, storage);
    expect(useComposerDrafts.getState().drafts["chat-1"]).toBe("keep this");
    expect(storage.getItem(`tidebreak.hostedReentryDraft:${route}`)).toBeNull();
  });
});

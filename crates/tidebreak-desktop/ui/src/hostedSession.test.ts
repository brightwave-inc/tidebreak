// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  HostedSignInRequired,
  acceptPastedToken,
  hostedServerInfo,
} from "./boot";
import {
  HOME_DRAFT_KEY,
  hydrateComposerDraftFromHostedReentry,
  useComposerDrafts,
} from "./ComposerDrafts";
import {
  captureHandoffToken,
  consoleSignInUrl,
  handoffBearer,
  handoffFailure,
  hostedSession,
  oidcSignInUrl,
  reenterExpiredHostedSession,
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
});

function navWindow(hash: string): {
  location: { hash: string; href: string };
} {
  let href = "https://machine.example.test/";
  return {
    location: {
      hash,
      get href() {
        return href;
      },
      set href(next: string) {
        href = next;
      },
    },
  };
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
      "https://tidebreak.example.com/models",
      expect.objectContaining({
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
  function win(hash: string): Pick<Window, "location"> {
    return {
      location: { origin: "https://machine.example.test", hash },
    } as unknown as Pick<Window, "location">;
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

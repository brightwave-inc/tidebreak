import { describe, expect, it, vi } from "vitest";
import { memoryStorage, SESSION_STORAGE_KEY } from "./storage";
import { SignedOutError, TokenStore, type TokenHttp } from "./tokenStore";
import type { PersistedSession, TokenResponse } from "./types";

function tokens(n: number): TokenResponse {
  return {
    access_token: `mg_at_${n}`,
    refresh_token: `mg_rt_${n}`,
    expires_in: 600,
    token_type: "Bearer",
  };
}

function session(refresh = "mg_rt_0"): PersistedSession {
  return {
    gatewayUrl: "https://gateway.example.test",
    refreshToken: refresh,
    accessTokens: [],
  };
}

describe("TokenStore", () => {
  it("serializes concurrent refreshes into one network call", async () => {
    let calls = 0;
    let release!: (value: { status: number; json: unknown }) => void;
    const gate = new Promise<{ status: number; json: unknown }>((resolve) => {
      release = resolve;
    });
    const http: TokenHttp = {
      postForm: vi.fn(async () => {
        calls += 1;
        return gate;
      }),
    };
    const store = new TokenStore(memoryStorage(), http);
    await store.replace(session());
    const first = store.getAccessToken("control");
    const second = store.getAccessToken("control");
    release({ status: 200, json: tokens(1) });
    expect(await first).toBe("mg_at_1");
    expect(await second).toBe("mg_at_1");
    expect(calls).toBe(1);
    expect(http.postForm).toHaveBeenCalledTimes(1);
  });

  it("caches per resource and mints separately", async () => {
    const http: TokenHttp = {
      postForm: vi.fn(async (_url, body) => ({
        status: 200,
        json: {
          ...tokens(body.resource === "control" ? 1 : 2),
        },
      })),
    };
    const store = new TokenStore(memoryStorage(), http);
    await store.replace(session());
    expect(await store.getAccessToken("control")).toBe("mg_at_1");
    expect(
      await store.getAccessToken(
        "tidebreak:3c6444cbec9b33f56b4ed0f1bf7015741c69cf7e516977c52975c6a0012a097b",
      ),
    ).toBe("mg_at_2");
    expect(await store.getAccessToken("control")).toBe("mg_at_1");
    expect(http.postForm).toHaveBeenCalledTimes(2);
  });

  it("signs out when the refresh family is revoked", async () => {
    const http: TokenHttp = {
      postForm: vi.fn(async () => ({ status: 401, json: { error: "invalid_grant" } })),
    };
    const store = new TokenStore(memoryStorage(), http);
    await store.replace(session());
    await expect(store.getAccessToken("control")).rejects.toBeInstanceOf(
      SignedOutError,
    );
    expect(store.snapshot()).toBeNull();
  });

  it("signing out during an in-flight refresh cannot resurrect the session", async () => {
    let release!: (value: { status: number; json: unknown }) => void;
    const gate = new Promise<{ status: number; json: unknown }>((resolve) => {
      release = resolve;
    });
    const http: TokenHttp = {
      postForm: vi.fn(async () => gate),
    };
    const storage = memoryStorage();
    const store = new TokenStore(storage, http);
    await store.replace(session());

    const minted = store.getAccessToken("control");
    const cleared = store.clear();
    release({ status: 200, json: tokens(1) });
    await minted;
    await cleared;

    expect(store.snapshot()).toBeNull();
    expect(await storage.getItem(SESSION_STORAGE_KEY)).toBeNull();
  });

  it("keeps access tokens out of persistent storage", async () => {
    const http: TokenHttp = {
      postForm: vi.fn(async () => ({ status: 200, json: tokens(1) })),
    };
    const storage = memoryStorage();
    const store = new TokenStore(storage, http);
    await store.replace(session());
    expect(await store.getAccessToken("control")).toBe("mg_at_1");

    const raw = await storage.getItem(SESSION_STORAGE_KEY);
    expect(raw).not.toBeNull();
    const persisted = JSON.parse(raw!) as PersistedSession;
    expect(persisted.accessTokens).toEqual([]);
    expect(persisted.refreshToken).toBe("mg_rt_1");

    // A stale token written by an older build is dropped on hydrate.
    await storage.setItem(
      SESSION_STORAGE_KEY,
      JSON.stringify({
        ...persisted,
        accessTokens: [
          {
            resource: "control",
            accessToken: "mg_at_stale",
            expiresAtMs: Date.now() + 600_000,
          },
        ],
      }),
    );
    const rehydrated = new TokenStore(storage, http);
    const hydrated = await rehydrated.hydrate();
    expect(hydrated?.accessTokens).toEqual([]);
  });
});

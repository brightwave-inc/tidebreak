import { describe, expect, it, vi } from "vitest";
import { memoryStorage } from "./storage";
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
});

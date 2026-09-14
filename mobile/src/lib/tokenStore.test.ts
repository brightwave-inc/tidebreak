import { describe, expect, it, vi } from "vitest";
import { connectionStorageKey, memoryStorage } from "./storage";
import {
  ResourceRefusedError,
  SignedOutError,
  TokenStore,
  type StoredConnection,
  type TokenHttp,
} from "./tokenStore";
import type { TokenResponse } from "./types";

const CONNECTION_ID = "gw_fixture";
const KEY = connectionStorageKey(CONNECTION_ID);

function tokens(n: number): TokenResponse {
  return {
    access_token: `mg_at_${n}`,
    refresh_token: `mg_rt_${n}`,
    expires_in: 600,
    token_type: "Bearer",
  };
}

function connection(refresh = "mg_rt_0"): StoredConnection {
  return {
    id: CONNECTION_ID,
    kind: "gateway",
    addedAt: "2026-01-01T00:00:00.000Z",
    gatewayUrl: "https://gateway.example.test",
    refreshToken: refresh,
    accessTokens: [],
  };
}

function store(http: TokenHttp, storage = memoryStorage()): TokenStore {
  return new TokenStore(storage, http, CONNECTION_ID);
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
    const tokenStore = store(http);
    await tokenStore.replace(connection());
    const first = tokenStore.getAccessToken("control");
    const second = tokenStore.getAccessToken("control");
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
    const tokenStore = store(http);
    await tokenStore.replace(connection());
    expect(await tokenStore.getAccessToken("control")).toBe("mg_at_1");
    expect(
      await tokenStore.getAccessToken(
        "tidebreak:3c6444cbec9b33f56b4ed0f1bf7015741c69cf7e516977c52975c6a0012a097b",
      ),
    ).toBe("mg_at_2");
    expect(await tokenStore.getAccessToken("control")).toBe("mg_at_1");
    expect(http.postForm).toHaveBeenCalledTimes(2);
  });

  it("signs out when the refresh family is revoked", async () => {
    const http: TokenHttp = {
      postForm: vi.fn(async () => ({ status: 401, json: { error: "invalid_grant" } })),
    };
    const tokenStore = store(http);
    await tokenStore.replace(connection());
    await expect(tokenStore.getAccessToken("control")).rejects.toBeInstanceOf(
      SignedOutError,
    );
    expect(tokenStore.snapshot()).toBeNull();
  });

  it("keeps the session when the gateway refuses a resource", async () => {
    // A gateway whose tidebreak-mobile client is not widened yet answers the
    // console resources with 400 invalid_resource — the same status a revoked
    // family uses. Reading that as a sign-out would throw the user out of a
    // working machine session (brightwave-inc/model-gateway#2044).
    const http: TokenHttp = {
      postForm: vi.fn(async (_url, body) =>
        body.resource === "control_plane"
          ? { status: 400, json: { error: "invalid_resource" } }
          : { status: 200, json: tokens(1) },
      ),
    };
    const tokenStore = store(http);
    await tokenStore.replace(connection());
    await expect(
      tokenStore.getAccessToken("control_plane"),
    ).rejects.toBeInstanceOf(ResourceRefusedError);
    expect(tokenStore.snapshot()).not.toBeNull();
    expect(await tokenStore.getAccessToken("control")).toBe("mg_at_1");
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
    const tokenStore = store(http, storage);
    await tokenStore.replace(connection());

    const minted = tokenStore.getAccessToken("control");
    const cleared = tokenStore.clear();
    release({ status: 200, json: tokens(1) });
    await minted;
    await cleared;

    expect(tokenStore.snapshot()).toBeNull();
    expect(await storage.getItem(KEY)).toBeNull();
  });

  it("keeps access tokens out of persistent storage", async () => {
    const http: TokenHttp = {
      postForm: vi.fn(async () => ({ status: 200, json: tokens(1) })),
    };
    const storage = memoryStorage();
    const tokenStore = store(http, storage);
    await tokenStore.replace(connection());
    expect(await tokenStore.getAccessToken("control")).toBe("mg_at_1");

    const raw = await storage.getItem(KEY);
    expect(raw).not.toBeNull();
    const persisted = JSON.parse(raw!) as StoredConnection;
    expect(persisted.accessTokens).toEqual([]);
    expect(persisted.refreshToken).toBe("mg_rt_1");

    // A stale token written by an older build is dropped on hydrate.
    await storage.setItem(
      KEY,
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
    const rehydrated = store(http, storage);
    const hydrated = await rehydrated.hydrate();
    expect(hydrated?.accessTokens).toEqual([]);
  });

  it("carries the granted scope through rotations", async () => {
    // Only the sign-in exchange answers about the session's authority; a
    // rotation's response describes the resource it minted. A scope that
    // decayed on the first refresh would silently downgrade a console session.
    const http: TokenHttp = {
      postForm: vi.fn(async () => ({
        status: 200,
        json: { ...tokens(1), scope: "openid profile offline_access" },
      })),
    };
    const storage = memoryStorage();
    const tokenStore = store(http, storage);
    await tokenStore.replace({
      ...connection(),
      grantedScope: "openid profile offline_access control_plane:read",
    });
    await tokenStore.getAccessToken("control");

    expect(tokenStore.snapshot()?.grantedScope).toBe(
      "openid profile offline_access control_plane:read",
    );
    const persisted = JSON.parse((await storage.getItem(KEY))!) as StoredConnection;
    expect(persisted.grantedScope).toBe(
      "openid profile offline_access control_plane:read",
    );
  });
});

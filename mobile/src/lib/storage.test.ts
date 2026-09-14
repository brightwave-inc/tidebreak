import { describe, expect, it, vi } from "vitest";
import {
  CONNECTION_INDEX_KEY,
  connectionStorageKey,
  storageForOs,
  type SecureStorage,
} from "./storage";
import { TokenStore, type TokenHttp } from "./tokenStore";

function throwingNative(): SecureStorage {
  const boom = async () => {
    throw new Error("expo-secure-store web stub");
  };
  return {
    getItem: vi.fn(boom),
    setItem: vi.fn(boom),
    deleteItem: vi.fn(boom),
  };
}

function webStore(native: SecureStorage): TokenStore {
  return new TokenStore(storageForOs("web", native), {
    postForm: vi.fn(),
  } as TokenHttp, "gw_fixture");
}

describe("storageForOs", () => {
  it("hydrates on web without calling the native stub", async () => {
    const native = throwingNative();
    await expect(webStore(native).hydrate()).resolves.toBeNull();
    expect(native.getItem).not.toHaveBeenCalled();
  });

  it("persists a connection in memory on web", async () => {
    const native = throwingNative();
    const store = webStore(native);
    await store.replace({
      id: "gw_fixture",
      kind: "gateway",
      addedAt: "2026-01-01T00:00:00.000Z",
      gatewayUrl: "https://gateway.example.test",
      refreshToken: "mg_rt_0",
      accessTokens: [],
    });
    expect(store.snapshot()?.refreshToken).toBe("mg_rt_0");
    expect(native.setItem).not.toHaveBeenCalled();
  });

  it("uses native storage off web", async () => {
    const native = throwingNative();
    native.getItem = vi.fn(async () => null);
    await expect(
      storageForOs("ios", native).getItem(CONNECTION_INDEX_KEY),
    ).resolves.toBeNull();
    expect(native.getItem).toHaveBeenCalledWith(CONNECTION_INDEX_KEY);
  });

  it("keeps each connection under its own key", () => {
    expect(connectionStorageKey("gw_one")).not.toBe(
      connectionStorageKey("gw_two"),
    );
    expect(connectionStorageKey("gw_one")).toMatch(/^[A-Za-z0-9._-]+$/);
  });
});

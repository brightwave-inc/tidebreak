import { describe, expect, it, vi } from "vitest";
import {
  SESSION_STORAGE_KEY,
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

describe("storageForOs", () => {
  it("hydrates on web without calling the native stub", async () => {
    const native = throwingNative();
    const store = new TokenStore(storageForOs("web", native), {
      postForm: vi.fn(),
    } as TokenHttp);
    await expect(store.hydrate()).resolves.toBeNull();
    expect(native.getItem).not.toHaveBeenCalled();
  });

  it("persists a session in memory on web", async () => {
    const native = throwingNative();
    const store = new TokenStore(storageForOs("web", native), {
      postForm: vi.fn(),
    } as TokenHttp);
    await store.replace({
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
      storageForOs("ios", native).getItem(SESSION_STORAGE_KEY),
    ).resolves.toBeNull();
    expect(native.getItem).toHaveBeenCalledWith(SESSION_STORAGE_KEY);
  });
});

import { describe, expect, it, vi } from "vitest";
import { ConnectionRegistry } from "./connectionRegistry";
import { gatewayConnectionId } from "./connections";
import {
  CONNECTION_INDEX_KEY,
  SESSION_STORAGE_KEY,
  connectionStorageKey,
  memoryStorage,
  type SecureStorage,
} from "./storage";
import type { TokenHttp } from "./tokenStore";
import type { PersistedSession } from "./types";

function silentHttp(): TokenHttp {
  return {
    postForm: vi.fn(async () => ({
      status: 200,
      json: {
        access_token: "mg_at_1",
        refresh_token: "mg_rt_next",
        expires_in: 600,
        token_type: "Bearer",
      },
    })),
  };
}

function registry(
  storage: SecureStorage,
  options: { http?: TokenHttp; installTimeMs?: number | null } = {},
): ConnectionRegistry {
  return new ConnectionRegistry({
    storage,
    http: options.http ?? silentHttp(),
    installTimeMs: async () => options.installTimeMs ?? 1_000,
    now: () => new Date("2026-01-01T00:00:00.000Z"),
  });
}

async function credential(
  storage: SecureStorage,
  id: string,
): Promise<{ refreshToken?: string } | null> {
  const raw = await storage.getItem(connectionStorageKey(id));
  return raw ? (JSON.parse(raw) as { refreshToken?: string }) : null;
}

describe("ConnectionRegistry", () => {
  it("holds two gateway pairings at once and switches between them", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();

    const first = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
      machinePrefillUrl: "https://machine-one.example.test",
    });
    await store.updateActive({
      machine: {
        baseUrl: "https://machine-one.example.test",
        resource: "tidebreak:one",
      },
    });
    const second = await store.addGateway({
      gatewayUrl: "https://two.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-two",
    });

    expect(store.list().map((item) => item.id)).toEqual([first.id, second.id]);
    // Pairing makes the newest connection active.
    expect(store.active()?.id).toBe(second.id);

    await store.setActive(first.id);
    // Switching switches the machine and the credential family with it.
    expect(store.active()?.machine?.resource).toBe("tidebreak:one");
    expect(store.activeTokens().connectionId).toBe(first.id);
    expect((await credential(storage, first.id))?.refreshToken).toBe(
      "mg_rt_one",
    );
    expect((await credential(storage, second.id))?.refreshToken).toBe(
      "mg_rt_two",
    );
  });

  it("keeps the connection list and the active one across a relaunch", async () => {
    const storage = memoryStorage();
    const first = registry(storage);
    await first.hydrate();
    await first.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });
    const second = await first.addGateway({
      gatewayUrl: "https://two.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-two",
    });
    await first.setActive(second.id);

    const relaunched = registry(storage);
    const snapshot = await relaunched.hydrate();
    expect(snapshot.connections).toHaveLength(2);
    expect(snapshot.activeId).toBe(second.id);
    expect(relaunched.active()?.gatewayUrl).toBe("https://two.example.test");
  });

  it("signs out of one connection and leaves the other signed in", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const first = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });
    const second = await store.addGateway({
      gatewayUrl: "https://two.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-two",
    });

    await store.remove(second.id);

    expect(store.list().map((item) => item.id)).toEqual([first.id]);
    expect(store.active()?.id).toBe(first.id);
    expect(await credential(storage, second.id)).toBeNull();
    expect((await credential(storage, first.id))?.refreshToken).toBe(
      "mg_rt_one",
    );
  });

  it("drops only the revoked connection when a refresh family dies", async () => {
    const storage = memoryStorage();
    const http: TokenHttp = {
      postForm: vi.fn(async (_url, body) =>
        body.refresh_token === "mg_rt_two"
          ? { status: 401, json: { error: "invalid_grant" } }
          : {
              status: 200,
              json: {
                access_token: "mg_at_1",
                refresh_token: "mg_rt_next",
                expires_in: 600,
                token_type: "Bearer",
              },
            },
      ),
    };
    const store = registry(storage, { http });
    await store.hydrate();
    const first = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });
    const second = await store.addGateway({
      gatewayUrl: "https://two.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-two",
    });

    await expect(
      store.tokensFor(second.id)!.getAccessToken("control"),
    ).rejects.toThrow();

    expect(store.list().map((item) => item.id)).toEqual([first.id]);
    expect(store.active()?.id).toBe(first.id);
    expect(await store.tokensFor(first.id)!.getAccessToken("control")).toBe(
      "mg_at_1",
    );
  });

  it("adopts the single-session blob an older build wrote", async () => {
    const storage = memoryStorage();
    const legacy: PersistedSession = {
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_legacy",
      installationId: "inst-one",
      machinePrefillUrl: "https://machine-one.example.test",
      machine: {
        baseUrl: "https://machine-one.example.test",
        resource: "tidebreak:one",
      },
      identity: { user_id: "user-1", display_name: "Paired person" },
      accessTokens: [],
    };
    await storage.setItem(SESSION_STORAGE_KEY, JSON.stringify(legacy));

    const snapshot = await registry(storage).hydrate();

    const id = gatewayConnectionId("https://one.example.test", "inst-one");
    expect(snapshot.activeId).toBe(id);
    const migrated = snapshot.connections[0];
    expect(migrated?.gatewayUrl).toBe("https://one.example.test");
    expect(migrated?.machine?.resource).toBe("tidebreak:one");
    expect(migrated?.identity?.user_id).toBe("user-1");
    // The rotating refresh token comes along, so the upgrade does not sign the
    // install out, and the old blob is gone.
    expect((await credential(storage, id))?.refreshToken).toBe("mg_rt_legacy");
    expect(await storage.getItem(SESSION_STORAGE_KEY)).toBeNull();
    // The summary the UI sees never carries the credential.
    expect(migrated && "refreshToken" in migrated).toBe(false);
  });

  it("migrates once and never resurrects the legacy blob", async () => {
    const storage = memoryStorage();
    await storage.setItem(
      SESSION_STORAGE_KEY,
      JSON.stringify({
        gatewayUrl: "https://one.example.test",
        refreshToken: "mg_rt_legacy",
        accessTokens: [],
      }),
    );
    await registry(storage).hydrate();
    const second = await registry(storage).hydrate();
    expect(second.connections).toHaveLength(1);
    expect(await storage.getItem(SESSION_STORAGE_KEY)).toBeNull();
  });

  it("discards an unreadable legacy blob instead of stranding it", async () => {
    const storage = memoryStorage();
    await storage.setItem(SESSION_STORAGE_KEY, "{not json");
    const snapshot = await registry(storage).hydrate();
    expect(snapshot.connections).toEqual([]);
    expect(await storage.getItem(SESSION_STORAGE_KEY)).toBeNull();
  });

  it("wipes credentials that survived an uninstall", async () => {
    // iOS Keychain entries outlive the app that wrote them; a reinstall must
    // not resume the previous install's session.
    const storage = memoryStorage();
    const first = registry(storage, { installTimeMs: 1_000 });
    await first.hydrate();
    const paired = await first.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });

    const reinstalled = registry(storage, { installTimeMs: 2_000 });
    const snapshot = await reinstalled.hydrate();

    expect(snapshot.connections).toEqual([]);
    expect(snapshot.activeId).toBeNull();
    expect(await credential(storage, paired.id)).toBeNull();
    // The new install records its own stamp, so the next launch keeps its work.
    const index = JSON.parse((await storage.getItem(CONNECTION_INDEX_KEY))!) as {
      installTimeMs?: number;
    };
    expect(index.installTimeMs).toBe(2_000);
  });

  it("keeps the session when the platform cannot report an install time", async () => {
    const storage = memoryStorage();
    const first = registry(storage, { installTimeMs: 1_000 });
    await first.hydrate();
    await first.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });

    const unknown = registry(storage, { installTimeMs: null });
    expect((await unknown.hydrate()).connections).toHaveLength(1);
  });

  it("re-pairing the same gateway refreshes its credential in place", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const first = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });
    const again = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-one",
      grantedScope: "openid profile offline_access control_plane:read",
    });

    expect(again.id).toBe(first.id);
    expect(store.list()).toHaveLength(1);
    expect((await credential(storage, first.id))?.refreshToken).toBe(
      "mg_rt_two",
    );
    expect(store.active()?.grantedScope).toBe(
      "openid profile offline_access control_plane:read",
    );
  });

  it("never lets a re-pair inherit the previous account's administrator role", async () => {
    // The id is derived from the installation, so signing in as somebody else
    // lands on the same record. A cached `isAdmin` that survived that would
    // show a member the administration group until the first usage read
    // corrected it.
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const paired = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
      grantedScope: "openid profile offline_access control_plane:read",
    });
    await store.updateActive({ isAdmin: true });
    expect(store.active()?.isAdmin).toBe(true);

    const again = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_two",
      installationId: "inst-one",
      grantedScope: "openid profile offline_access control_plane:read",
    });

    expect(again.id).toBe(paired.id);
    expect(store.active()?.isAdmin).toBeUndefined();
  });

  it("publishes every change to its listeners", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const seen: number[] = [];
    store.onChange((snapshot) => seen.push(snapshot.connections.length));
    const added = await store.addGateway({
      gatewayUrl: "https://one.example.test",
      refreshToken: "mg_rt_one",
      installationId: "inst-one",
    });
    await store.remove(added.id);
    expect(seen).toEqual([1, 0]);
  });
});

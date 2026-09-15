import { describe, expect, it, vi } from "vitest";
import { ConnectionRegistry } from "./connectionRegistry";
import { machineConnectionId } from "./connections";
import { MachineClient, MachineRequestError } from "./machine";
import { StaticTokenStore } from "./machineTokenStore";
import { connectionStorageKey, memoryStorage, type SecureStorage } from "./storage";
import { SignedOutError, type TokenHttp } from "./tokenStore";

const MACHINE = {
  baseUrl: "https://machine.example.test",
  resource: "tidebreak:abc",
};
const TOKEN = "a".repeat(64);

function silentHttp(): TokenHttp {
  return { postForm: vi.fn(async () => ({ status: 200, json: {} })) };
}

function registry(storage: SecureStorage): ConnectionRegistry {
  return new ConnectionRegistry({ storage, http: silentHttp() });
}

async function stored(
  storage: SecureStorage,
  id: string,
): Promise<Record<string, unknown> | null> {
  const raw = await storage.getItem(connectionStorageKey(id));
  return raw ? (JSON.parse(raw) as Record<string, unknown>) : null;
}

describe("StaticTokenStore", () => {
  it("answers only for the resource its own machine was attached as", async () => {
    const store = new StaticTokenStore(memoryStorage(), "mc_one");
    await store.replace({
      id: "mc_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
      staticToken: TOKEN,
    });
    expect(await store.getAccessToken(MACHINE.resource)).toBe(TOKEN);
    // A gateway-only surface that slipped past the section gate must not be
    // handed a roster token to send to a server that never issued one.
    await expect(store.getAccessToken("control")).rejects.toThrow(/no gateway/);
    await expect(store.getAccessToken("control_plane")).rejects.toThrow();
  });

  it("wipes itself on 401 and signals, and ignores every other status", async () => {
    const storage = memoryStorage();
    const store = new StaticTokenStore(storage, "mc_one");
    const signedOut = vi.fn();
    store.onSignedOut(signedOut);
    await store.replace({
      id: "mc_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
      staticToken: TOKEN,
    });

    // A 403 is an answer about a principal the machine still recognizes —
    // opening one admin-only surface must not sign a member out.
    store.reportUnauthorized(403);
    store.reportUnauthorized(500);
    expect(signedOut).not.toHaveBeenCalled();
    expect(await stored(storage, "mc_one")).not.toBeNull();

    store.reportUnauthorized(401);
    await vi.waitFor(() => expect(signedOut).toHaveBeenCalledTimes(1));
    expect(await stored(storage, "mc_one")).toBeNull();
    await expect(store.getAccessToken(MACHINE.resource)).rejects.toBeInstanceOf(
      SignedOutError,
    );
  });

  it("drops a record whose credential did not survive storage", async () => {
    const storage = memoryStorage();
    await storage.setItem(
      connectionStorageKey("mc_one"),
      JSON.stringify({ id: "mc_one", kind: "machine", machine: MACHINE }),
    );
    const store = new StaticTokenStore(storage, "mc_one");
    expect(await store.hydrate()).toBeNull();
    expect(await stored(storage, "mc_one")).toBeNull();
  });
});

describe("machine connections in the registry", () => {
  it("adds one, hydrates it back, and never publishes its token", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const added = await store.addMachine({ machine: MACHINE, staticToken: TOKEN });

    expect(added.id).toBe(machineConnectionId(MACHINE.baseUrl));
    expect(store.active()).toEqual(added);
    // The summary the UI mirrors into React state carries no credential.
    expect(added).not.toHaveProperty("staticToken");
    expect((await stored(storage, added.id))?.staticToken).toBe(TOKEN);

    const relaunched = registry(storage);
    const snapshot = await relaunched.hydrate();
    expect(snapshot.connections).toEqual([added]);
    expect(snapshot.activeId).toBe(added.id);
    expect(
      await relaunched.activeTokens().getAccessToken(MACHINE.resource),
    ).toBe(TOKEN);
  });

  it("replaces rather than stacks when the same machine is re-attached", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const first = await store.addMachine({ machine: MACHINE, staticToken: TOKEN });
    const rotated = "b".repeat(64);
    const again = await store.addMachine({
      machine: MACHINE,
      staticToken: rotated,
    });

    expect(again.id).toBe(first.id);
    expect(store.list()).toHaveLength(1);
    // The dead token is overwritten, not left beside its successor.
    expect((await stored(storage, first.id))?.staticToken).toBe(rotated);
  });

  it("holds a gateway and a machine side by side, and signs out only one", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const gateway = await store.addGateway({
      gatewayUrl: "https://gateway.example.test",
      refreshToken: "mg_rt_1",
      installationId: "inst-one",
    });
    const machine = await store.addMachine({
      machine: MACHINE,
      staticToken: TOKEN,
    });
    expect(store.list()).toHaveLength(2);

    await store.remove(machine.id);
    expect(store.list().map((entry) => entry.id)).toEqual([gateway.id]);
    expect(await stored(storage, machine.id)).toBeNull();
    // The gateway's rotating credential is untouched.
    expect((await stored(storage, gateway.id))?.refreshToken).toBe("mg_rt_1");
  });

  it("forgets exactly the machine whose token a 401 revoked", async () => {
    const storage = memoryStorage();
    const store = registry(storage);
    await store.hydrate();
    const gateway = await store.addGateway({
      gatewayUrl: "https://gateway.example.test",
      refreshToken: "mg_rt_1",
      installationId: "inst-one",
    });
    const machine = await store.addMachine({
      machine: MACHINE,
      staticToken: TOKEN,
    });

    const credentials = store.tokensFor(machine.id);
    expect(credentials).toBeInstanceOf(StaticTokenStore);
    (credentials as StaticTokenStore).reportUnauthorized(401);

    await vi.waitFor(() => expect(store.list()).toHaveLength(1));
    expect(store.list()[0]?.id).toBe(gateway.id);
    expect(store.active()?.id).toBe(gateway.id);
    expect(await stored(storage, machine.id)).toBeNull();
  });
});

describe("a machine answering 401 through MachineClient", () => {
  it("reports the status back to the credential that issued it", async () => {
    const storage = memoryStorage();
    const store = new StaticTokenStore(storage, "mc_one");
    await store.replace({
      id: "mc_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
      staticToken: TOKEN,
    });
    const client = new MachineClient({
      baseUrl: MACHINE.baseUrl,
      resource: MACHINE.resource,
      tokens: {
        getAccessToken: (resource) => store.getAccessToken(resource),
        reportStatus: (status) => store.reportUnauthorized(status),
      },
      fetchImpl: async () =>
        new Response(JSON.stringify({ kind: "unauthorized" }), { status: 401 }),
    });

    await expect(client.getJson("/sessions")).rejects.toBeInstanceOf(
      MachineRequestError,
    );
    await vi.waitFor(async () =>
      expect(await stored(storage, "mc_one")).toBeNull(),
    );
  });
});

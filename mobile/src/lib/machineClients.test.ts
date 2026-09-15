import { describe, expect, it, vi } from "vitest";
import { ConnectionRegistry } from "./connectionRegistry";
import { machineClientFor } from "./machineClients";
import { StaticTokenStore } from "./machineTokenStore";
import { connectionStorageKey, memoryStorage, type SecureStorage } from "./storage";
import type { TokenHttp } from "./tokenStore";

const MACHINE_A = {
  baseUrl: "https://a.example.test",
  resource: "tidebreak:aaa",
};
const MACHINE_B = {
  baseUrl: "https://b.example.test",
  resource: "tidebreak:bbb",
};
const TOKEN_A = "a".repeat(64);
const TOKEN_B = "b".repeat(64);

function silentHttp(): TokenHttp {
  return { postForm: vi.fn(async () => ({ status: 200, json: {} })) };
}

async function stored(
  storage: SecureStorage,
  id: string,
): Promise<Record<string, unknown> | null> {
  const raw = await storage.getItem(connectionStorageKey(id));
  return raw ? (JSON.parse(raw) as Record<string, unknown>) : null;
}

/** A transport that hands back a response only when the test releases it. */
function heldTransport() {
  let release!: (status: number) => void;
  const held = new Promise<number>((resolve) => {
    release = resolve;
  });
  return {
    release,
    fetchImpl: async () => {
      const status = await held;
      return new Response(JSON.stringify({ kind: "unauthorized" }), { status });
    },
  };
}

describe("a machine client is bound to the connection that built it", () => {
  it("signs out only the machine whose own request was refused", async () => {
    // The race Shipright found: machine A's poll is in flight, the user
    // switches to machine B, and A's 401 lands afterwards. Resolving the
    // consequence against whoever is active at *response* time wipes B — a
    // working roster token that was never on that wire.
    const storage = memoryStorage();
    const registry = new ConnectionRegistry({ storage, http: silentHttp() });
    await registry.hydrate();
    // B first, then A, so A is the active connection when its poll starts —
    // the state the user is actually in before they switch away.
    const b = await registry.addMachine({
      machine: MACHINE_B,
      staticToken: TOKEN_B,
    });
    const a = await registry.addMachine({
      machine: MACHINE_A,
      staticToken: TOKEN_A,
    });
    expect(registry.active()?.id).toBe(a.id);

    const transport = heldTransport();
    const clientA = machineClientFor(registry, a.id, MACHINE_A, {
      fetchImpl: transport.fetchImpl,
    });
    const inFlight = clientA.getJson("/approvals?state=pending");

    // The switch happens while A's request is still open.
    await registry.setActive(b.id);
    expect(registry.active()?.id).toBe(b.id);

    transport.release(401);
    await expect(inFlight).rejects.toThrow();

    await vi.waitFor(() => expect(registry.list()).toHaveLength(1));
    expect(registry.list()[0]?.id).toBe(b.id);
    expect(await stored(storage, a.id)).toBeNull();
    // B never saw a refusal, so its token is untouched and still mints.
    expect((await stored(storage, b.id))?.staticToken).toBe(TOKEN_B);
    expect(await registry.activeTokens().getAccessToken(MACHINE_B.resource)).toBe(
      TOKEN_B,
    );
  });

  it("mints from the connection that built it, not from whoever is active", async () => {
    // The same capture, on the request side. A client built for A asking the
    // active store for A's resource would be refused by B's store, so an
    // unbound client does not merely mint wrongly — it stops working.
    const storage = memoryStorage();
    const registry = new ConnectionRegistry({ storage, http: silentHttp() });
    await registry.hydrate();
    const a = await registry.addMachine({
      machine: MACHINE_A,
      staticToken: TOKEN_A,
    });
    await registry.addMachine({ machine: MACHINE_B, staticToken: TOKEN_B });
    expect(registry.active()?.machine?.baseUrl).toBe(MACHINE_B.baseUrl);

    const seen: string[] = [];
    const clientA = machineClientFor(registry, a.id, MACHINE_A, {
      fetchImpl: async (_url, init) => {
        seen.push(init.headers?.Authorization ?? "");
        return new Response("{}", { status: 200 });
      },
    });
    await clientA.getJson("/sessions");
    expect(seen).toEqual([`Bearer ${TOKEN_A}`]);
  });

  it("fails loudly once its connection is gone, rather than borrowing another", async () => {
    const storage = memoryStorage();
    const registry = new ConnectionRegistry({ storage, http: silentHttp() });
    await registry.hydrate();
    const a = await registry.addMachine({
      machine: MACHINE_A,
      staticToken: TOKEN_A,
    });
    await registry.addMachine({ machine: MACHINE_B, staticToken: TOKEN_B });
    const clientA = machineClientFor(registry, a.id, MACHINE_A, {
      fetchImpl: async () => new Response("{}", { status: 200 }),
    });
    await registry.remove(a.id);

    await expect(clientA.getJson("/sessions")).rejects.toThrow(
      /no longer signed in/i,
    );
  });

  it("leaves a gateway connection's credential alone on a machine 401", async () => {
    // A gateway machine's 401 is an expired minted token, not a revocation:
    // the refresh family answers for it, and only the token endpoint may end
    // that session.
    const storage = memoryStorage();
    const registry = new ConnectionRegistry({ storage, http: silentHttp() });
    await registry.hydrate();
    const gateway = await registry.addGateway({
      gatewayUrl: "https://gateway.example.test",
      refreshToken: "mg_rt_1",
      installationId: "inst-one",
    });
    await registry.updateActive({ machine: MACHINE_A });

    expect(registry.tokensFor(gateway.id)).not.toBeInstanceOf(StaticTokenStore);
    const client = machineClientFor(registry, gateway.id, MACHINE_A, {
      fetchImpl: async () => new Response("{}", { status: 401 }),
    });
    await expect(client.getJson("/sessions")).rejects.toThrow();
    expect(registry.list()).toHaveLength(1);
    expect((await stored(storage, gateway.id))?.refreshToken).toBe("mg_rt_1");
  });
});

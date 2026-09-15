import { describe, expect, it } from "vitest";
import {
  activeAfterRemoval,
  connectionDetail,
  consoleCacheScope,
  connectionLabel,
  gatewayConnectionId,
  isGatewayConnection,
  reinstalled,
  withConnectionId,
  type GatewayConnection,
} from "./connections";

function connection(over: Partial<GatewayConnection> = {}): GatewayConnection {
  return {
    id: "gw_one",
    kind: "gateway",
    addedAt: "2026-01-01T00:00:00.000Z",
    gatewayUrl: "https://gateway.example.test",
    ...over,
  };
}

describe("gatewayConnectionId", () => {
  it("is stable per installation so re-pairing replaces rather than stacks", () => {
    const first = gatewayConnectionId("https://gateway.example.test", "inst-1");
    const second = gatewayConnectionId("https://gateway.example.test/", "inst-1");
    expect(first).toBe(second);
    expect(
      gatewayConnectionId("https://gateway.example.test", "inst-2"),
    ).not.toBe(first);
  });

  it("falls back to the URL when the gateway named no installation", () => {
    const id = gatewayConnectionId("https://gateway.example.test");
    expect(id).not.toBe(gatewayConnectionId("https://other.example.test"));
    expect(id).toBe(gatewayConnectionId("https://gateway.example.test"));
  });

  it("only produces secure-store-safe keys", () => {
    expect(
      gatewayConnectionId("https://gateway.example.test", "inst/one two:three"),
    ).toMatch(/^[A-Za-z0-9._-]+$/);
  });
});

describe("reinstalled", () => {
  it("wipes only on a known-different install time", () => {
    expect(reinstalled(100, 200)).toBe(true);
    expect(reinstalled(100, 100)).toBe(false);
    // Unknown either way cannot tell an upgrade from a reinstall, and a wipe
    // on a guess signs a working session out.
    expect(reinstalled(undefined, 200)).toBe(false);
    expect(reinstalled(100, null)).toBe(false);
  });
});

describe("the connection directory", () => {
  it("adds without duplicating", () => {
    expect(withConnectionId(["a"], "b")).toEqual(["a", "b"]);
    expect(withConnectionId(["a", "b"], "b")).toEqual(["a", "b"]);
  });

  it("keeps the active connection when another is removed", () => {
    expect(activeAfterRemoval(["a", "b", "c"], "b", "a")).toBe("b");
  });

  it("falls back to the newest survivor when the active one goes", () => {
    expect(activeAfterRemoval(["a", "b", "c"], "c", "c")).toBe("b");
    expect(activeAfterRemoval(["a"], "a", "a")).toBeNull();
  });
});

describe("connection rendering", () => {
  it("labels a gateway by host and says what it attached", () => {
    expect(connectionLabel(connection())).toBe("gateway.example.test");
    expect(connectionDetail(connection())).toBe("No machine attached");
    expect(
      connectionDetail(
        connection({
          machine: {
            baseUrl: "https://machine.example.test",
            resource: "tidebreak:abc",
          },
        }),
      ),
    ).toBe("machine.example.test");
  });

  it("narrows the union by kind", () => {
    expect(isGatewayConnection(connection())).toBe(true);
    expect(
      isGatewayConnection({
        id: "m_one",
        kind: "machine",
        addedAt: "2026-01-01T00:00:00.000Z",
        machine: { baseUrl: "https://machine.example.test", resource: "t:1" },
      }),
    ).toBe(false);
    expect(isGatewayConnection(null)).toBe(false);
  });
});

describe("consoleCacheScope", () => {
  it("separates one sign-in from the next at the same deployment", () => {
    // The id names a deployment, so it is identical across the two; only the
    // generation says these are different sessions.
    expect(consoleCacheScope(connection({ pairingGeneration: 1 }))).not.toBe(
      consoleCacheScope(connection({ pairingGeneration: 2 })),
    );
  });

  it("is stable for one sign-in", () => {
    const paired = connection({ pairingGeneration: 3 });
    expect(consoleCacheScope(paired)).toBe(consoleCacheScope({ ...paired }));
    // Nothing a live session learns about itself may move the namespace, or a
    // read would be refetched every time the role or the machine is recorded.
    expect(consoleCacheScope({ ...paired, isAdmin: true })).toBe(
      consoleCacheScope(paired),
    );
  });

  it("gives an unpaired app and a record from an older build stable scopes", () => {
    expect(consoleCacheScope(null)).toBe("unpaired");
    // A record written before this field existed has no generation to read;
    // it scopes consistently rather than colliding with "unpaired".
    expect(consoleCacheScope(connection())).toBe("gw_one#0");
  });
});

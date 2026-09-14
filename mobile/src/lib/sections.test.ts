import { describe, expect, it } from "vitest";
import type { Connection, GatewayConnection } from "./connections";
import { landingRoute, sectionsFor } from "./sections";

const MACHINE = {
  baseUrl: "https://machine.example.test",
  resource: "tidebreak:abc",
};

function gateway(over: Partial<GatewayConnection> = {}): GatewayConnection {
  return {
    id: "gw_one",
    kind: "gateway",
    addedAt: "2026-01-01T00:00:00.000Z",
    gatewayUrl: "https://gateway.example.test",
    ...over,
  };
}

describe("sectionsFor", () => {
  it("shows nothing to supervise before a machine is attached", () => {
    expect(sectionsFor(gateway())).toEqual([]);
    expect(sectionsFor(null)).toEqual([]);
  });

  it("shows the machine surfaces once attached", () => {
    expect(sectionsFor(gateway({ machine: MACHINE }))).toEqual([
      "sessions",
      "delivery",
      "chats",
      "workspaces",
    ]);
  });

  it("shows the console only for a session that was granted it", () => {
    expect(
      sectionsFor(
        gateway({
          machine: MACHINE,
          grantedScope: "openid profile offline_access control_plane:read",
        }),
      ),
    ).toContain("console");
    // A pairing made against a gateway that does not offer the console to this
    // client carries no console scope, so the surface stays hidden instead of
    // failing on every request behind it.
    expect(sectionsFor(gateway({ machine: MACHINE }))).not.toContain("console");
  });

  it("never offers the console to a kind that has no gateway", () => {
    const direct: Connection = {
      id: "m_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
    };
    expect(sectionsFor(direct)).toEqual([
      "sessions",
      "delivery",
      "chats",
      "workspaces",
    ]);
  });
});

describe("landingRoute", () => {
  it("routes by what the connection has, not by how it signed in", () => {
    expect(landingRoute(null)).toBe("/pair");
    expect(landingRoute(gateway())).toBe("/attach");
    expect(landingRoute(gateway({ machine: MACHINE }))).toBe("/home");
  });
});

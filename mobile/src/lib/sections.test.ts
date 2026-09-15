import { describe, expect, it } from "vitest";
import type { Connection, GatewayConnection } from "./connections";
import {
  adminSectionsFor,
  administers,
  canAdminCancel,
  consoleSectionsFor,
  hasAdminSection,
  hasConsoleSection,
  landingRoute,
  sectionsFor,
} from "./sections";

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

describe("consoleSectionsFor", () => {
  const CONSOLE_SCOPE =
    "openid profile offline_access control_plane:read runtime:execute";

  it("offers the control-backed surfaces to any gateway pairing", () => {
    // `control` is in every tidebreak-mobile session's confinement — pairing
    // already mints it — so these answer with or without a console grant, and
    // hiding them would withhold surfaces that work.
    expect(consoleSectionsFor(gateway())).toEqual([
      "catalog",
      "subscriptions",
      "shared-apps",
    ]);
  });

  it("adds the control-plane surfaces only for a session granted them", () => {
    expect(consoleSectionsFor(gateway({ grantedScope: CONSOLE_SCOPE }))).toEqual(
      [
        "sandboxes",
        "activity",
        "limits",
        "catalog",
        "subscriptions",
        "shared-apps",
      ],
    );
    for (const section of ["sandboxes", "activity", "limits"] as const) {
      expect(hasConsoleSection(gateway(), section)).toBe(false);
    }
  });

  it("reads an unrecorded grant as no console authority, not as unknown", () => {
    // A credential migrated from a build that never recorded a grant carries
    // none, and unknown authority is not authority.
    expect(hasConsoleSection(gateway({}), "sandboxes")).toBe(false);
    expect(
      hasConsoleSection(
        gateway({ grantedScope: "openid profile offline_access" }),
        "activity",
      ),
    ).toBe(false);
  });

  it("does not confuse the runtime grant with console read", () => {
    // Steering is an affordance inside the sandbox detail, not a section; a
    // runtime-only grant must not light up the control-plane reads behind it.
    expect(
      consoleSectionsFor(
        gateway({
          grantedScope: "openid profile offline_access runtime:execute",
        }),
      ),
    ).not.toContain("sandboxes");
  });

  it("never offers the console to a kind that has no gateway", () => {
    const direct: Connection = {
      id: "m_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
    };
    expect(consoleSectionsFor(direct)).toEqual([]);
    expect(consoleSectionsFor(null)).toEqual([]);
  });
});

describe("landingRoute", () => {  it("routes by what the connection has, not by how it signed in", () => {
    expect(landingRoute(null)).toBe("/pair");
    expect(landingRoute(gateway())).toBe("/attach");
    expect(landingRoute(gateway({ machine: MACHINE }))).toBe("/home");
  });
});

describe("administers", () => {
  const READ = "openid profile offline_access control_plane:read";

  it("needs the role and the grant together", () => {
    expect(administers(gateway({ grantedScope: READ, isAdmin: true }))).toBe(
      true,
    );
    // The role without the console grant: this session never consented to the
    // control plane, so it cannot mint the resource these reads authenticate
    // with — administrator or not.
    expect(administers(gateway({ isAdmin: true }))).toBe(false);
    // The grant without the role is an ordinary member's console session.
    expect(administers(gateway({ grantedScope: READ }))).toBe(false);
  });

  it("reads an unlearned role as member", () => {
    // The failure direction that costs one refresh, rather than the one that
    // shows a member a wall of refusals.
    expect(administers(gateway({ grantedScope: READ, isAdmin: false }))).toBe(
      false,
    );
    expect(administers(null)).toBe(false);
  });

  it("never promotes a connection with no gateway behind it", () => {
    const direct: Connection = {
      id: "m_one",
      kind: "machine",
      addedAt: "2026-01-01T00:00:00.000Z",
      machine: MACHINE,
    };
    expect(administers(direct)).toBe(false);
    expect(canAdminCancel(direct)).toBe(false);
  });
});

describe("adminSectionsFor", () => {
  const ADMIN = {
    grantedScope: "openid profile offline_access control_plane:read",
    isAdmin: true,
  };

  it("offers the whole administration group to an administrator", () => {
    expect(adminSectionsFor(gateway(ADMIN))).toEqual([
      "admin-usage",
      "admin-models",
      "admin-people",
      "admin-teams",
      "admin-limits",
      "admin-guardrails",
      "admin-audit",
      "admin-configuration",
    ]);
    expect(hasAdminSection(gateway(ADMIN), "admin-audit")).toBe(true);
  });

  it("offers a member nothing at all", () => {
    // Not a thinner group: every read behind these screens is refused for a
    // member, so the group is absent rather than present-and-failing.
    expect(adminSectionsFor(gateway({ ...ADMIN, isAdmin: false }))).toEqual([]);
    expect(adminSectionsFor(gateway())).toEqual([]);
    expect(adminSectionsFor(null)).toEqual([]);
    expect(hasAdminSection(gateway(), "admin-audit")).toBe(false);
  });
});

describe("canAdminCancel", () => {
  const WRITE =
    "openid profile offline_access control_plane:read control_plane:write";

  it("needs the write scope on top of the role", () => {
    expect(canAdminCancel(gateway({ grantedScope: WRITE, isAdmin: true }))).toBe(
      true,
    );
    // A read-only console session is refused at the request, and a button
    // that can only fail is worse than one that is absent.
    expect(
      canAdminCancel(
        gateway({
          grantedScope: "openid profile offline_access control_plane:read",
          isAdmin: true,
        }),
      ),
    ).toBe(false);
  });

  it("is not something a member's write grant buys", () => {
    expect(canAdminCancel(gateway({ grantedScope: WRITE }))).toBe(false);
  });

  it("is a different verb from the owner cancel", () => {
    // Steering and the owner cancel ride `runtime:execute`; holding that alone
    // never reaches somebody else's run.
    expect(
      canAdminCancel(
        gateway({
          grantedScope: "openid profile offline_access runtime:execute",
          isAdmin: true,
        }),
      ),
    ).toBe(false);
  });
});

/**
 * Connections: what this phone is attached to, plural from day one.
 *
 * A connection is one thing the app talks to, with a `kind` that decides how
 * it authenticates and which surfaces it can render:
 *
 * - `gateway` — a Model Gateway pairing (OAuth authorization-code + PKCE). It
 *   carries the gateway URL, the installation it identifies, the machine
 *   attached through it, and the identity the session signed in as. The only
 *   kind implemented here.
 * - `machine` — a Tidebreak machine reached directly by URL with a static
 *   token, no gateway in the path (issue #3404). The shape exists so that
 *   slice adds a member to this union instead of re-cutting the store, and
 *   nothing in this slice constructs one.
 *
 * Several connections coexist; exactly one is active. Credentials never live
 * in this module: a connection's refresh token is written under its own
 * secure-store key by its own `TokenStore` (`tokenStore.ts`), so signing out
 * of one connection cannot touch another's.
 */

import { sha256Hex } from "./crypto";
import type { AttachedMachine, GatewayIdentity } from "./types";

export type ConnectionKind = "gateway" | "machine";

/** Fields every connection carries, whatever it connects to. */
type ConnectionBase = {
  /** Stable, secure-store-key-safe identifier. */
  id: string;
  kind: ConnectionKind;
  /** ISO-8601; the connection list sorts oldest-first by it. */
  addedAt: string;
  /** The Tidebreak machine this connection supervises, once attached. */
  machine?: AttachedMachine;
};

/** A Model Gateway pairing. */
export type GatewayConnection = ConnectionBase & {
  kind: "gateway";
  gatewayUrl: string;
  installationId?: string;
  /** `tidebreak_machine_url` from `/api/v1/meta`, the attach prefill. */
  machinePrefillUrl?: string;
  identity?: GatewayIdentity;
  /**
   * The scope the gateway granted this session, recorded at sign-in and
   * carried unchanged through every refresh rotation. It is what the app asks
   * about console authority — a rotation's response describes one resource,
   * never the session.
   */
  grantedScope?: string;
  /**
   * Whether this account administers the gateway, learned from the unfiltered
   * usage read's `scope` (`admin.ts`) and cached here so the administration
   * surfaces gate on first paint rather than resolving into view.
   *
   * A cached convenience, never an authority: every administrator read is
   * refused server-side for a member regardless of what this says. Undefined
   * means "not yet learned", which reads as member — the group appears when
   * the gateway confirms it, and signing out of the connection deletes the
   * record along with the answer.
   */
  isAdmin?: boolean;
  /**
   * How many times this connection has been signed in to, counting from one.
   *
   * The id is derived from the installation so that re-pairing one deployment
   * lands on one record rather than stacking live refresh families — which
   * means the id alone cannot tell two *accounts* apart. Signing in as
   * somebody else reuses it. This counter is what changes then, and
   * `consoleCacheScope` is how the read caches inherit that change.
   */
  pairingGeneration?: number;
};

/**
 * A machine reached directly, with no gateway in the path (#3404). Declared,
 * deliberately unimplemented: no code in this slice builds or hydrates one.
 */
export type MachineConnection = ConnectionBase & {
  kind: "machine";
  machine: AttachedMachine;
};

export type Connection = GatewayConnection | MachineConnection;

export function isGatewayConnection(
  connection: Connection | null | undefined,
): connection is GatewayConnection {
  return connection?.kind === "gateway";
}

/** Secure-store keys allow `[A-Za-z0-9._-]` only. */
function keySafe(value: string): string {
  return value.replace(/[^A-Za-z0-9._-]/g, "_");
}

/**
 * The id for a gateway pairing.
 *
 * Derived, not random: pairing the same deployment twice must land on the same
 * connection rather than accumulating duplicates that each hold a live refresh
 * family. The installation id is the deployment's own name for itself; without
 * one (an older gateway, or metadata that omitted it) the canonical URL stands
 * in, hashed so a long host cannot blow the key length.
 */
export function gatewayConnectionId(
  gatewayUrl: string,
  installationId?: string,
): string {
  const named = installationId?.trim();
  if (named) {
    return `gw_${keySafe(named)}`;
  }
  return `gw_${sha256Hex(gatewayUrl).slice(0, 32)}`;
}

/**
 * The namespace every cached gateway read is filed under.
 *
 * Not the connection id, and the difference is the point. The id names a
 * *deployment*, so signing in to one gateway as somebody else reuses it — and
 * a cache keyed on the id alone would serve the previous account's answers to
 * the next one, for as long as those entries live. That is not a privacy leak
 * on its own (every response was already read by the account that fetched it,
 * and the gateway re-authorizes every request) but it is a correctness one:
 * the administration surfaces are derived from a cached usage read's `scope`,
 * so a stale `installation` would light the whole administration group up for
 * an account the gateway had just answered `self` for.
 *
 * Folding the pairing generation in makes that structurally impossible rather
 * than something an eviction has to remember: a new sign-in cannot address the
 * previous one's entries at all, and they age out on their own.
 */
export function consoleCacheScope(connection: Connection | null): string {
  if (!connection) {
    return "unpaired";
  }
  return `${connection.id}#${
    isGatewayConnection(connection) ? (connection.pairingGeneration ?? 0) : 0
  }`;
}

/** The host a connection is shown as in a list. */
export function connectionLabel(connection: Connection): string {
  const url = isGatewayConnection(connection)
    ? connection.gatewayUrl
    : connection.machine.baseUrl;
  return url.replace(/^https?:\/\//, "").replace(/\/+$/, "");
}

/** The second line of a connection row: what it is attached to. */
export function connectionDetail(connection: Connection): string {
  if (!connection.machine) {
    return isGatewayConnection(connection) ? "No machine attached" : "Machine";
  }
  return connection.machine.baseUrl.replace(/^https?:\/\//, "");
}

/**
 * The directory of connections, without credentials. Persisted so the app
 * knows what exists and which is active before it reads any secret.
 */
export type ConnectionIndex = {
  version: 2;
  ids: string[];
  activeId: string | null;
  /**
   * When the running install was installed, in epoch milliseconds. iOS
   * Keychain entries survive an uninstall, so a differing value means these
   * credentials belong to a previous install and must be wiped.
   */
  installTimeMs?: number;
};

export function emptyIndex(installTimeMs?: number): ConnectionIndex {
  return {
    version: 2,
    ids: [],
    activeId: null,
    ...(installTimeMs === undefined ? {} : { installTimeMs }),
  };
}

/**
 * Whether stored credentials belong to a previous install of this app.
 *
 * Only a known-different install time answers yes. An unknown current time
 * (the web build reports none) or an index written before the app recorded
 * one cannot distinguish a reinstall from an upgrade, and wiping on a guess
 * would sign every user out of a working session.
 */
export function reinstalled(
  recordedMs: number | undefined,
  currentMs: number | null | undefined,
): boolean {
  if (recordedMs === undefined || currentMs === null || currentMs === undefined) {
    return false;
  }
  return recordedMs !== currentMs;
}

/** Add or replace one id, keeping insertion order stable for the rest. */
export function withConnectionId(ids: string[], id: string): string[] {
  return ids.includes(id) ? ids : [...ids, id];
}

/**
 * Which connection is active after `removed` goes away: the current one when
 * it survived, otherwise the newest remaining, otherwise none.
 */
export function activeAfterRemoval(
  ids: string[],
  activeId: string | null,
  removed: string,
): string | null {
  const remaining = ids.filter((id) => id !== removed);
  if (activeId && activeId !== removed && remaining.includes(activeId)) {
    return activeId;
  }
  return remaining[remaining.length - 1] ?? null;
}

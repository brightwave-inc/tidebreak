/**
 * Which surfaces one connection can show.
 *
 * Kind decides the shape: a `gateway` connection can reach the gateway console
 * once its session holds the authority for it, a `machine` connection (#3404)
 * never can — there is no gateway in its path. Attachment decides the rest:
 * every supervision surface needs a machine token, so none of them render
 * before attach.
 *
 * Kept pure and separate from the screens so the answer is one testable
 * function rather than a condition repeated per route, and so the design pass
 * (#3314) can restyle the navigation without relitigating what belongs in it.
 */

import { isGatewayConnection, type Connection } from "./connections";
import { grantsConsoleRead, grantsRuntimeExecute } from "./scope";

export type SectionId =
  | "sessions"
  | "delivery"
  | "chats"
  | "workspaces"
  | "console"
  | "runtime";

/**
 * The sections to render for one connection, in navigation order. An empty
 * list means there is nothing to supervise yet — the caller routes to attach
 * or pairing instead.
 */
export function sectionsFor(connection: Connection | null): SectionId[] {
  if (!connection) {
    return [];
  }
  const sections: SectionId[] = [];
  if (connection.machine) {
    sections.push("sessions", "delivery", "chats", "workspaces");
  }
  if (isGatewayConnection(connection)) {
    // Gated on the grant this session actually holds, not on the kind: a
    // pairing made against a gateway that does not offer the console to this
    // client carries no console scope, and the surface must not appear only
    // to fail on every request behind it.
    if (grantsConsoleRead(connection.grantedScope)) {
      sections.push("console");
    }
    if (grantsRuntimeExecute(connection.grantedScope)) {
      sections.push("runtime");
    }
  }
  return sections;
}

export function hasSection(
  connection: Connection | null,
  section: SectionId,
): boolean {
  return sectionsFor(connection).includes(section);
}

/** The gateway console's member surfaces, in navigation order. */
export type ConsoleSectionId =
  | "sandboxes"
  | "activity"
  | "limits"
  | "catalog"
  | "subscriptions"
  | "shared-apps";

/**
 * Which console surfaces one connection can show.
 *
 * Two authorities are at work, and conflating them would either hide working
 * surfaces or offer broken ones.
 *
 * The **`control`** resource is in every `tidebreak-mobile` session's
 * confinement — it is what pairing already mints — so the surfaces that ride
 * it answer for any gateway connection, console grant or not: the member
 * catalog, provider subscriptions, and shared apps.
 *
 * The **`control_plane`** resource is the widening this session may or may not
 * have consented to (`scope.ts`). Sandboxes, usage, and cost limits ride it,
 * so they appear only when the recorded grant carries `control_plane:read` —
 * the same seam `sectionsFor` uses for the `console` section, asked at the
 * finer grain the screens need.
 *
 * The runtime verbs are not a section: steering and cancel are affordances
 * *inside* the sandbox detail, gated there on `grantsRuntimeExecute`.
 */
export function consoleSectionsFor(
  connection: Connection | null,
): ConsoleSectionId[] {
  if (!isGatewayConnection(connection)) {
    return [];
  }
  const sections: ConsoleSectionId[] = [];
  if (grantsConsoleRead(connection.grantedScope)) {
    sections.push("sandboxes", "activity", "limits");
  }
  sections.push("catalog", "subscriptions", "shared-apps");
  return sections;
}

export function hasConsoleSection(
  connection: Connection | null,
  section: ConsoleSectionId,
): boolean {
  return consoleSectionsFor(connection).includes(section);
}

/** Where the app belongs when it opens with this connection active. */
export function landingRoute(connection: Connection | null): string {
  if (!connection) {
    return "/pair";
  }
  return connection.machine ? "/home" : "/attach";
}

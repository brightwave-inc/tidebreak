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

/** Where the app belongs when it opens with this connection active. */
export function landingRoute(connection: Connection | null): string {
  if (!connection) {
    return "/pair";
  }
  return connection.machine ? "/home" : "/attach";
}

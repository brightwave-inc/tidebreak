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
import {
  grantsConsoleRead,
  grantsConsoleWrite,
  grantsRuntimeExecute,
} from "./scope";

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

/** The gateway console's administration surfaces, in navigation order. */
export type AdminSectionId =
  | "admin-usage"
  | "admin-models"
  | "admin-people"
  | "admin-teams"
  | "admin-limits"
  | "admin-guardrails"
  | "admin-audit"
  | "admin-configuration";

/**
 * Which administration surfaces one connection can show.
 *
 * Two independent conditions, and both are needed. The **grant** is the same
 * `control_plane:read` seam the member surfaces use: a session that never
 * consented to the console cannot mint the resource these reads authenticate,
 * administrator or not. The **role** is `isAdmin`, cached on the connection
 * from the usage read's scope (`admin.ts`), and it is a product judgement
 * rather than a security one — the gateway refuses every read behind these
 * screens for a member regardless of what the phone believes.
 *
 * An unlearned role reads as member: the group appears once the gateway has
 * said so, which is the failure direction that costs a tap rather than the one
 * that shows a member a wall of refusals.
 *
 * The order is how an administrator asks about an installation: what is it
 * spending, what can it serve, who reaches it, what bounds it, what has it
 * refused, and how is it wired.
 */
export function adminSectionsFor(
  connection: Connection | null,
): AdminSectionId[] {
  if (!administers(connection)) {
    return [];
  }
  return [
    "admin-usage",
    "admin-models",
    "admin-people",
    "admin-teams",
    "admin-limits",
    "admin-guardrails",
    "admin-audit",
    "admin-configuration",
  ];
}

export function hasAdminSection(
  connection: Connection | null,
  section: AdminSectionId,
): boolean {
  return adminSectionsFor(connection).includes(section);
}

/**
 * Whether this connection may show the administrator affordances that live
 * *inside* the member screens — the sandbox fleet toggle and its owner
 * attribution, the activity account switcher, the console hand-offs whose
 * destination is an administrator-only console page.
 *
 * The same pair of conditions the administration group is gated on, asked
 * where there is no section to hide: these surfaces answer for a member too,
 * and only the extra controls on them are an administrator's.
 */
export function administers(connection: Connection | null): boolean {
  if (!isGatewayConnection(connection)) {
    return false;
  }
  return (
    grantsConsoleRead(connection.grantedScope) && connection.isAdmin === true
  );
}

/**
 * Whether this session may stop somebody else's run.
 *
 * The administrator cancel is a control-plane *write*, so it needs the write
 * scope on top of the role (mg ADR 0102) — a read-only console session is
 * refused at the request, and an affordance that can only fail is worse than
 * one that is absent. The owner cancel is a different verb on a different
 * resource and is gated by `grantsRuntimeExecute`, not by this.
 */
export function canAdminCancel(connection: Connection | null): boolean {
  if (!administers(connection)) {
    return false;
  }
  return grantsConsoleWrite(
    isGatewayConnection(connection) ? connection.grantedScope : undefined,
  );
}

/**
 * Why a surface is not on offer.
 *
 * Absence alone is a bad answer when a person knows the surface exists: a
 * gateway user who attaches a standalone machine, switches to it, and finds
 * the console gone should be able to learn that it is gone *because this
 * connection has no gateway*, not left to guess at a bug. Stable identifiers
 * rather than sentences, so the copy lives with the screens and the rule stays
 * one tested function.
 */
export type UnavailableReason =
  /** Nothing is connected at all. */
  | "no_connection"
  /** Connected, but no machine is attached yet. */
  | "no_machine"
  /**
   * This connection reaches a machine directly and has no gateway in its path,
   * so no gateway surface can ever appear on it (#3404).
   */
  | "no_gateway"
  /**
   * A gateway connection whose recorded grant does not carry the authority
   * this surface needs. Re-pairing against a widened gateway is the fix.
   */
  | "not_granted"
  /**
   * A gateway connection holding the grant, whose account the gateway has not
   * said administers the installation. Nothing the phone can fix.
   */
  | "not_admin";

/** Why `section` is absent for this connection, or null when it is present. */
export function sectionUnavailableReason(
  connection: Connection | null,
  section: SectionId,
): UnavailableReason | null {
  if (hasSection(connection, section)) {
    return null;
  }
  if (!connection) {
    return "no_connection";
  }
  if (section === "console" || section === "runtime") {
    return isGatewayConnection(connection) ? "not_granted" : "no_gateway";
  }
  return "no_machine";
}

/** Why `section` is absent from the console, or null when it is present. */
export function consoleSectionUnavailableReason(
  connection: Connection | null,
  section: ConsoleSectionId,
): UnavailableReason | null {
  if (hasConsoleSection(connection, section)) {
    return null;
  }
  if (!connection) {
    return "no_connection";
  }
  return isGatewayConnection(connection) ? "not_granted" : "no_gateway";
}

/**
 * Why an administration surface is absent, or null when it is present.
 *
 * The administration group is gated on two independent conditions
 * (`administers`), and they fail for different reasons with different
 * remedies: a session that never consented to the console can re-pair for it,
 * an account the gateway calls a member cannot do anything about that from a
 * phone, and a standalone machine has no gateway to administer at all. The
 * grant is reported first, because it is the one a user can act on.
 */
export function adminSectionUnavailableReason(
  connection: Connection | null,
  section: AdminSectionId,
): UnavailableReason | null {
  if (hasAdminSection(connection, section)) {
    return null;
  }
  if (!connection) {
    return "no_connection";
  }
  if (!isGatewayConnection(connection)) {
    return "no_gateway";
  }
  return grantsConsoleRead(connection.grantedScope)
    ? "not_admin"
    : "not_granted";
}

/**
 * Whether push notifications can be offered for this connection at all.
 *
 * Push is a gateway service (mg ADR 0093): the device address is held by an
 * installation and every notification is enqueued there. A standalone machine
 * has no such registry, so the settings surface, the registration reconcile,
 * and the tray-action router all skip one rather than calling routes that do
 * not exist.
 */
export function supportsPush(connection: Connection | null): boolean {
  return isGatewayConnection(connection);
}

/** Where the app belongs when it opens with this connection active. */
export function landingRoute(connection: Connection | null): string {
  if (!connection) {
    return "/pair";
  }
  return connection.machine ? "/home" : "/attach";
}

/**
 * The administration console's pure logic: who counts as an administrator, how
 * the audit ledger pages, and how a fleet row names its owner.
 *
 * Kept out of the screens because each answer is load-bearing and cheap to get
 * subtly wrong — a scope misread promotes a member, a cursor mishandled either
 * loops or silently truncates the ledger, and an owner line that guesses turns
 * somebody else's run into yours.
 */

import type { AuditEvent, AuditEventPage } from "./gatewayAdmin";

/**
 * Whether the signed-in account administers this gateway, from the unfiltered
 * usage read's `scope`.
 *
 * This is the only administrator signal the app is given: the gateway widens
 * that read to the whole installation for an administrator and narrows it to
 * the caller for everyone else, and says which it did. There is no capability
 * endpoint to ask instead.
 *
 * `undefined` for anything else — an absent field on an older gateway, or the
 * `filtered` scope a parameterised read answers with. Those describe a
 * different question, so they must neither promote nor demote what is already
 * cached: a filtered read landing first would otherwise flap the whole
 * administration group off and back on.
 */
export function adminFromUsageScope(
  scope: string | null | undefined,
): boolean | undefined {
  if (scope === "installation") {
    return true;
  }
  if (scope === "self") {
    return false;
  }
  return undefined;
}

/**
 * How many rows one ledger page asks for. Exported because the screen reports
 * what it loaded rather than implying a window nobody asked the gateway for.
 */
export const AUDIT_PAGE_SIZE = 50;

/**
 * How many directory rows one read asks for. Well inside the endpoint's 500
 * ceiling, and exported so a screen that got exactly this many can say it is
 * showing the first page rather than the directory.
 */
export const PEOPLE_PAGE_SIZE = 200;

/**
 * The cursor list after asking for another page.
 *
 * The ledger pages by keyset: each loaded page is its own query keyed by the
 * cursor that produced it, so appending is how "load more" adds a page instead
 * of refetching the ones already read. `null` leads the list — the first page
 * is the one fetched with no cursor.
 *
 * A null or already-loaded cursor is ignored rather than appended. The
 * duplicate guard matters: a gateway that echoed the cursor it was handed
 * would otherwise let every tap stack another query on the same key, which
 * renders the same page repeatedly and never ends.
 */
export function withNextAuditPage(
  cursors: readonly (string | null)[],
  next: string | null,
): (string | null)[] {
  if (next === null || cursors.includes(next)) {
    return [...cursors];
  }
  return [...cursors, next];
}

/**
 * A recorded action that did not take effect. `denied` is policy refusing and
 * `failed` is the operation breaking after it was accepted — different causes,
 * both of them what an administrator opened the ledger to find, so one
 * predicate keeps them together and the chips keep them apart.
 */
export function isAuditFailure(event: Pick<AuditEvent, "outcome">): boolean {
  return event.outcome !== "succeeded";
}

/**
 * Failures within the trailing window, across the pages loaded.
 *
 * Recency is `last_occurred_at`: identical actions collapse into one row, so a
 * row first seen last week that recurred an hour ago is something that
 * happened in the last day. An unparseable stamp is excluded rather than
 * counted at the epoch, which would report every malformed row as recent.
 */
export function recentAuditFailures(
  events: readonly AuditEvent[],
  windowMs: number,
  now: number = Date.now(),
): number {
  const since = now - windowMs;
  return events.filter((event) => {
    if (!isAuditFailure(event)) {
      return false;
    }
    const at = new Date(event.last_occurred_at || event.occurred_at).getTime();
    return Number.isFinite(at) && at >= since;
  }).length;
}

/** A row in the people directory, as far as owner attribution cares. */
export type DirectoryEntry = {
  id: string;
  display_name?: string | null;
  email?: string | null;
};

/** User id → the best name the directory has for it. */
export function ownerDirectory(
  people: readonly DirectoryEntry[],
): Map<string, string> {
  const names = new Map<string, string>();
  for (const person of people) {
    const name = person.display_name || person.email;
    if (name) {
      names.set(person.id, name);
    }
  }
  return names;
}

/**
 * How a fleet row names its owner, or `undefined` when the run is the reader's
 * own.
 *
 * Every row that is not the reader's says whose it is, even when the directory
 * cannot name them: the directory read is administrator-only and capped, so a
 * display name is best-effort while attribution is not. The id prefix is the
 * fallback — less use than a name, but it still tells two people's runs apart,
 * which is the misreading this line exists to prevent.
 *
 * Before the viewer's identity resolves there is no id to compare against, so
 * every row carries the label. That over-attributes nothing: the failure worth
 * avoiding is somebody else's run silently reading as the reader's, and an
 * owner line on one's own run is merely redundant.
 */
export function ownerLabel(
  ownerId: string,
  names: ReadonlyMap<string, string>,
  viewerId: string | undefined,
): string | undefined {
  if (ownerId === viewerId) {
    return undefined;
  }
  return names.get(ownerId) ?? `User ${ownerId.slice(0, 8)}`;
}

/** The cursor for the next page across the pages loaded, or null at the end. */
export function ledgerCursor(
  pages: readonly (AuditEventPage | undefined)[],
): string | null {
  return pages[pages.length - 1]?.next_cursor ?? null;
}

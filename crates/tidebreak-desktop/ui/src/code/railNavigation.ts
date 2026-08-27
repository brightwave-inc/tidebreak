import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore, workspaceDigests } from "./CodeUpdatesStore";
import { arrangeWorkspaces } from "./workspaceCards";

/**
 * Walking the workspace rail from the keyboard.
 *
 * The order is the rail's own — the same `arrangeWorkspaces` call the rail
 * renders, read from the same stores. Recomputing it here rather than caching
 * a list is what keeps the chord landing on the card the reader can see: sort
 * order, repo grouping, and archiving all move rows, and a remembered order
 * would send Cmd+Alt+Down somewhere the rail no longer draws.
 */

/**
 * The id a step lands on, or `null` when the rail has nothing to step to.
 *
 * Wraps at both ends, the way the tab chords do: a rail is a ring the reader
 * cycles, and stopping at the last card would make the chord feel broken
 * exactly when they are furthest from the top. From off the rail — the code
 * home or a delivery page — a step enters at the end it is walking towards.
 */
export function stepWorkspaceId(
  ids: readonly string[],
  current: string | undefined,
  delta: -1 | 1,
): string | null {
  if (ids.length === 0) return null;
  const position = current === undefined ? -1 : ids.indexOf(current);
  if (position < 0) return (delta === 1 ? ids[0] : ids[ids.length - 1]) ?? null;
  return ids[(position + delta + ids.length) % ids.length] ?? null;
}

/** The rail's workspaces in the order it draws them, groups flattened. */
export function railWorkspaceIds(): string[] {
  const { repos, workspaces } = useCodeCatalogStore.getState();
  const { sortMode } = useCodeUiStore.getState().railPrefs;
  const digests = workspaceDigests(useCodeUpdatesStore.getState());
  return arrangeWorkspaces(sortMode, repos, workspaces, digests).flatMap(
    (group) => group.workspaces.map((workspace) => workspace.id),
  );
}

/** The workspace a rail step lands on, or `null` when there is nowhere to go. */
export function stepRailWorkspace(
  current: string | undefined,
  delta: -1 | 1,
): string | null {
  return stepWorkspaceId(railWorkspaceIds(), current, delta);
}

/**
 * The workspace to open after `leftId` leaves the rail, or `null` for `/code`.
 *
 * Archiving the open workspace must not leave the reader on a page the rail
 * no longer draws. The next live card is the replacement; an empty rail falls
 * through to the code home.
 */
export function nextWorkspaceAfterLeaving(
  ids: readonly string[],
  leftId: string,
): string | null {
  const next = stepWorkspaceId(ids, leftId, 1);
  return next && next !== leftId ? next : null;
}

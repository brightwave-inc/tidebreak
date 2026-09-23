import type { DesktopUpdateState } from "./updates";

const UNKNOWN_UPDATE_VERSION = "unknown";

/**
 * What dismissing the update card remembers. A ready update keeps the bare
 * version, as it always has; an offered one is its own notice, so dismissing
 * "available" never hides the "ready" card for the same version later.
 */
export function updateNoticeKey(state: DesktopUpdateState): string {
  const version = state.version ?? UNKNOWN_UPDATE_VERSION;
  return state.status === "available" ? `available:${version}` : version;
}

/** The update card the shell shows, if any. */
export type UpdateCardChoice =
  | {
      kind: "progress";
      status: "checking" | "downloading";
      version: string | null;
    }
  | { kind: "up-to-date"; version: string | null }
  | { kind: "failed"; message: string }
  | { kind: "available"; version: string | null; error: string | null }
  | { kind: "ready"; version: string | null; error: string | null };

/**
 * Whether a download you started from the card is still worth following.
 * It is until it settles: ready to restart, or offered again after a failure.
 */
export function stillFollowing(status: DesktopUpdateState["status"]): boolean {
  return status === "checking" || status === "downloading";
}

/**
 * Pick the one update card to show. The states never overlap, because each
 * card belongs to a different update status.
 */
export function updateCardFor({
  state,
  explicitCheck,
  followingDownload,
  dismissedKey,
  appVersion,
}: {
  state: DesktopUpdateState;
  /** The check the native "Check for Updates…" item started, if any. */
  explicitCheck: "running" | "settled" | null;
  /** You started a download from the card, and it has not settled yet. */
  followingDownload: boolean;
  /** The notice you dismissed, as {@link updateNoticeKey} spells it. */
  dismissedKey: string | null;
  /** The version you are running, for the up-to-date result. */
  appVersion: string | null;
}): UpdateCardChoice | null {
  switch (state.status) {
    case "checking":
    case "downloading":
      return explicitCheck === "running" || followingDownload
        ? { kind: "progress", status: state.status, version: state.version }
        : null;
    case "idle":
      if (explicitCheck !== "settled") return null;
      return state.error
        ? { kind: "failed", message: state.error }
        : { kind: "up-to-date", version: appVersion };
    case "available":
    case "ready":
      if (dismissedKey === updateNoticeKey(state)) return null;
      return {
        kind: state.status,
        version: state.version,
        error: state.error,
      };
  }
}

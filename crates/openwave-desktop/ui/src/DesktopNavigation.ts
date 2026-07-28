import { useEffect, useState } from "react";
import { useRouter } from "@tanstack/react-router";
import type { HistoryLocation } from "@tanstack/react-router";

/** The move that produced a location, as the history reports it. */
type NavigationAction = "PUSH" | "REPLACE" | "FORWARD" | "BACK" | "GO";

/**
 * Where the reader sits in the history stack, and how far ahead of them it
 * still reaches.
 *
 * The history API reports whether there is anything behind the current entry
 * but not whether there is anything ahead — the browser deliberately hides
 * that. Tracking the furthest entry the session has reached recovers it, which
 * is what a forward button needs to know whether it is live or dead.
 */
export type NavigationReach = { index: number; furthest: number };

/**
 * Fold one navigation into the reach.
 *
 * A push from anywhere but the end of the stack discards every entry ahead of
 * it, so the furthest reachable entry becomes wherever the push landed. Every
 * other move walks entries that already exist and leaves the far end alone.
 */
export function nextNavigationReach(
  reach: NavigationReach,
  action: NavigationAction,
  index: number,
): NavigationReach {
  if (action === "PUSH") return { index, furthest: index };
  return { index, furthest: Math.max(reach.furthest, index) };
}

function entryIndex(location: HistoryLocation): number {
  return location.state.__TSR_index ?? 0;
}

export type DesktopNavigation = {
  canGoBack: boolean;
  canGoForward: boolean;
  goBack: () => void;
  goForward: () => void;
};

/**
 * Back and forward for the app's own history, for the titlebar to drive.
 *
 * The window has no browser chrome of its own, so without this the reader has
 * no way back from a screen they reached by following something — the rail only
 * offers the places it lists.
 */
export function useDesktopNavigation(): DesktopNavigation {
  const history = useRouter().history;
  const [reach, setReach] = useState<NavigationReach>(() => {
    const index = entryIndex(history.location);
    return { index, furthest: index };
  });

  useEffect(
    () =>
      history.subscribe(({ location, action }) => {
        setReach((current) =>
          nextNavigationReach(current, action.type, entryIndex(location)),
        );
      }),
    [history],
  );

  return {
    canGoBack: reach.index > 0,
    canGoForward: reach.index < reach.furthest,
    goBack: () => history.back(),
    goForward: () => history.forward(),
  };
}

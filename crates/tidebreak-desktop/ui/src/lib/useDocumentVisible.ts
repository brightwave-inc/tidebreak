import { useSyncExternalStore } from "react";

function subscribe(onChange: () => void): () => void {
  document.addEventListener("visibilitychange", onChange);
  return () => document.removeEventListener("visibilitychange", onChange);
}

function visible(): boolean {
  return !document.hidden;
}

/**
 * Whether the window is on screen. A conversation only counts as seen while
 * someone can see it, so the unread mark waits for this.
 */
export function useDocumentVisible(): boolean {
  return useSyncExternalStore(subscribe, visible, () => true);
}

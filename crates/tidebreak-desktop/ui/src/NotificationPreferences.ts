/**
 * Which agent events reach you as toasts, desktop notifications, and Dock
 * bounces. Both are on unless you turn them off, and both stay on this
 * device: they decide what this window shows, not what the server records.
 */

/**
 * Finished and failed turns. The key predates the second switch, when it was
 * the only one, so an existing "off" carries over.
 */
export const FINISHED_KEY = "tidebreak.desktop-notifications";
/** Approvals, questions, and plans an agent stopped for. */
export const NEEDS_YOU_KEY = "tidebreak.needs-you-notifications";

function enabled(key: string): boolean {
  if (typeof window === "undefined") return true;
  try {
    return window.localStorage.getItem(key) !== "off";
  } catch {
    return true;
  }
}

function setEnabled(key: string, on: boolean): void {
  if (typeof window === "undefined") return;
  try {
    if (on) {
      window.localStorage.removeItem(key);
    } else {
      window.localStorage.setItem(key, "off");
    }
  } catch {
    // Storage can be unavailable. Notifications then stay on.
  }
}

/** Whether to tell you when an agent stops for your approval or answer. */
export function needsYouNotificationsEnabled(): boolean {
  return enabled(NEEDS_YOU_KEY);
}

export function setNeedsYouNotificationsEnabled(on: boolean): void {
  setEnabled(NEEDS_YOU_KEY, on);
}

/** Whether to tell you when an agent finishes or fails. */
export function finishedNotificationsEnabled(): boolean {
  return enabled(FINISHED_KEY);
}

export function setFinishedNotificationsEnabled(on: boolean): void {
  setEnabled(FINISHED_KEY, on);
}

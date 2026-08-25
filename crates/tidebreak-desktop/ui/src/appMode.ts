export type AppMode = "work" | "code";

export const APP_MODE_STORAGE_KEY = "tidebreak.app-mode";

/** Read the last mode the reader used. Work remains the safe first-run default. */
export function readStoredAppMode(): AppMode {
  try {
    return window.localStorage.getItem(APP_MODE_STORAGE_KEY) === "code"
      ? "code"
      : "work";
  } catch {
    return "work";
  }
}

/** Remember the selected product surface across app launches. */
export function storeAppMode(mode: AppMode): void {
  try {
    window.localStorage.setItem(APP_MODE_STORAGE_KEY, mode);
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * Restore Code on a normal launch without overriding a deep link.
 *
 * The desktop host opens the renderer at the bare root. Replace that root
 * before the router reads it so Code does not flash Work on the way in.
 */
export function restoreStoredAppMode(): void {
  const root =
    window.location.hash === "" ||
    window.location.hash === "#" ||
    window.location.hash === "#/";
  if (!root || readStoredAppMode() !== "code") return;
  window.history.replaceState(window.history.state, "", "#/code");
}

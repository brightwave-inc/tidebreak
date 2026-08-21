import { useSyncExternalStore } from "react";

export type ThemeMode = "light" | "dark" | "system";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "tidebreak-theme";
const DARK_QUERY = "(prefers-color-scheme: dark)";

export function isThemeMode(value: unknown): value is ThemeMode {
  return value === "light" || value === "dark" || value === "system";
}

export function readStoredTheme(): ThemeMode {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    if (isThemeMode(value)) return value;
  } catch {
    // Ignore storage access failures (private mode, disabled storage).
  }
  return "system";
}

function storeTheme(mode: ThemeMode): void {
  try {
    localStorage.setItem(STORAGE_KEY, mode);
  } catch {
    // Ignore storage access failures.
  }
}

function darkMediaQuery(): MediaQueryList | null {
  if (
    typeof window === "undefined" ||
    typeof window.matchMedia !== "function"
  ) {
    return null;
  }
  return window.matchMedia(DARK_QUERY);
}

export function systemPrefersDark(): boolean {
  return darkMediaQuery()?.matches === true;
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode === "system") return systemPrefersDark() ? "dark" : "light";
  return mode;
}

/**
 * The `dark` class is what every token in the stylesheet keys off. The pre-paint
 * snippet in `index.html` writes the same class from the same storage key before
 * this module loads; keep the two in step.
 */
function paint(resolved: ResolvedTheme): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("dark", resolved === "dark");
}

type ThemeState = { mode: ThemeMode; resolved: ResolvedTheme };

/**
 * One copy of the theme for the whole app.
 *
 * The toaster, the grid and the shell all need the *resolved* theme, and they
 * mount and unmount independently, so it cannot live in any one component's
 * state — when it did, each call site kept its own boot-time snapshot and only
 * the shell's ever moved. It is a module store rather than context so a
 * consumer outside the shell's tree still sees the same value.
 */
let state: ThemeState = stateFor(readStoredTheme());
const listeners = new Set<() => void>();
let watchedQuery: MediaQueryList | null = null;

function stateFor(mode: ThemeMode): ThemeState {
  return { mode, resolved: resolveTheme(mode) };
}

/** Moves the store to `mode` and repaints, if anything actually changed. */
function commit(mode: ThemeMode): void {
  const next = stateFor(mode);
  if (next.mode === state.mode && next.resolved === state.resolved) return;
  state = next;
  paint(next.resolved);
  for (const listener of [...listeners]) listener();
}

/**
 * The OS flipping is a change of `resolved`, not of `mode`, so it has to run
 * through the store like any other: mutating the class alone leaves every
 * consumer holding the old resolved value.
 */
function onSystemChange(): void {
  commit(state.mode);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (listeners.size === 1) {
    watchedQuery = darkMediaQuery();
    watchedQuery?.addEventListener("change", onSystemChange);
    // The OS may have flipped while nothing was mounted to hear it.
    onSystemChange();
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0) {
      watchedQuery?.removeEventListener("change", onSystemChange);
      watchedQuery = null;
    }
  };
}

function getSnapshot(): ThemeState {
  return state;
}

export function setThemeMode(mode: ThemeMode): void {
  storeTheme(mode);
  commit(mode);
}

export function cycleThemeMode(): void {
  setThemeMode(
    state.mode === "light"
      ? "dark"
      : state.mode === "dark"
        ? "system"
        : "light",
  );
}

/** Applies the stored theme synchronously to avoid a flash before React mounts. */
export function initTheme(): void {
  // Runs before anything subscribes, so there is nobody to notify yet.
  state = stateFor(readStoredTheme());
  paint(state.resolved);
}

export function useTheme(): {
  mode: ThemeMode;
  resolved: ResolvedTheme;
  setMode: (mode: ThemeMode) => void;
  cycle: () => void;
} {
  const { mode, resolved } = useSyncExternalStore(
    subscribe,
    getSnapshot,
    getSnapshot,
  );
  return { mode, resolved, setMode: setThemeMode, cycle: cycleThemeMode };
}

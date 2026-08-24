import { create } from "zustand";

const SIDEBAR_COLLAPSED_KEY = "tidebreak.sidebar-collapsed";
const SIDEBAR_WIDTH_KEY = "tidebreak.sidebar-width";
const MODEL_MENU_NOT_CONNECTED_KEY =
  "tidebreak.model-menu-not-connected-collapsed";
const ACTIVE_TURN_SEND_MODE_KEY = "tidebreak.composer.sendMode";

export type ActiveTurnSendMode = "queue" | "steer";

/** Expanded rail width bounds, in CSS pixels. */
export const SIDEBAR_MIN_WIDTH = 200;
export const SIDEBAR_MAX_WIDTH = 480;
/** Default expanded width — a touch wider than the old fixed 224px rail. */
export const SIDEBAR_DEFAULT_WIDTH = 280;

export function clampSidebarWidth(width: number): number {
  if (!Number.isFinite(width)) return SIDEBAR_DEFAULT_WIDTH;
  return Math.min(
    SIDEBAR_MAX_WIDTH,
    Math.max(SIDEBAR_MIN_WIDTH, Math.round(width)),
  );
}

function readStoredSidebarCollapsed(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "true";
  } catch {
    return false;
  }
}

function storeSidebarCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(SIDEBAR_COLLAPSED_KEY, String(collapsed));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredSidebarWidth(): number {
  try {
    const raw = window.localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (raw == null) return SIDEBAR_DEFAULT_WIDTH;
    return clampSidebarWidth(Number(raw));
  } catch {
    return SIDEBAR_DEFAULT_WIDTH;
  }
}

function storeSidebarWidth(width: number): void {
  try {
    window.localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredNotConnectedCollapsed(): boolean {
  try {
    return window.localStorage.getItem(MODEL_MENU_NOT_CONNECTED_KEY) === "true";
  } catch {
    return false;
  }
}

function storeNotConnectedCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(
      MODEL_MENU_NOT_CONNECTED_KEY,
      String(collapsed),
    );
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredActiveTurnSendMode(): ActiveTurnSendMode {
  try {
    return window.localStorage.getItem(ACTIVE_TURN_SEND_MODE_KEY) === "steer"
      ? "steer"
      : "queue";
  } catch {
    return "queue";
  }
}

function storeActiveTurnSendMode(mode: ActiveTurnSendMode): void {
  try {
    window.localStorage.setItem(ACTIVE_TURN_SEND_MODE_KEY, mode);
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * Chrome state that belongs to no conversation and does not deserve a URL:
 * whether the sidebar is showing, and how wide the expanded rail is.
 *
 * Where the workspace is pointed used to live here too. It is in the URL now,
 * because a layout that cannot be linked at cannot be the target of a citation.
 */
export type UiStore = {
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
  /** Expanded rail width in CSS pixels. Ignored while the rail is compact. */
  sidebarWidth: number;
  /**
   * Set the expanded rail width. Pass `{ persist: false }` while dragging so
   * localStorage is only written once the pointer is up.
   */
  setSidebarWidth: (width: number, options?: { persist?: boolean }) => void;
  /**
   * Whether the model picker's "Not connected" section is folded away. Open by
   * default — the section exists to be noticed once — and remembered after
   * that, because a reader who has read it does not need it again.
   */
  modelMenuNotConnectedCollapsed: boolean;
  toggleModelMenuNotConnected: () => void;
  /** What Enter and the single composer action do while a response is running. */
  activeTurnSendMode: ActiveTurnSendMode;
  setActiveTurnSendMode: (mode: ActiveTurnSendMode) => void;
  /**
   * Whether the command palette is up. Here rather than in the code store
   * because the palette spans both modes, and because the native browser
   * webview has to know something is drawn over the editor area.
   */
  commandPaletteOpen: boolean;
  setCommandPaletteOpen: (open: boolean) => void;
  toggleCommandPalette: () => void;
};

export function createUiStore() {
  return create<UiStore>()((set) => ({
    sidebarCollapsed: readStoredSidebarCollapsed(),
    toggleSidebar: () =>
      set((state) => {
        const sidebarCollapsed = !state.sidebarCollapsed;
        storeSidebarCollapsed(sidebarCollapsed);
        return { sidebarCollapsed };
      }),
    sidebarWidth: readStoredSidebarWidth(),
    setSidebarWidth: (width, options) => {
      const sidebarWidth = clampSidebarWidth(width);
      if (options?.persist !== false) storeSidebarWidth(sidebarWidth);
      set({ sidebarWidth });
    },
    modelMenuNotConnectedCollapsed: readStoredNotConnectedCollapsed(),
    toggleModelMenuNotConnected: () =>
      set((state) => {
        const collapsed = !state.modelMenuNotConnectedCollapsed;
        storeNotConnectedCollapsed(collapsed);
        return { modelMenuNotConnectedCollapsed: collapsed };
      }),
    activeTurnSendMode: readStoredActiveTurnSendMode(),
    setActiveTurnSendMode: (mode) => {
      storeActiveTurnSendMode(mode);
      set({ activeTurnSendMode: mode });
    },
    commandPaletteOpen: false,
    setCommandPaletteOpen: (commandPaletteOpen) => set({ commandPaletteOpen }),
    toggleCommandPalette: () =>
      set((state) => ({ commandPaletteOpen: !state.commandPaletteOpen })),
  }));
}

export const useUiStore = createUiStore();

import { create } from "zustand";

const SIDEBAR_COLLAPSED_KEY = "tidebreak.sidebar-collapsed";
const MODEL_MENU_NOT_CONNECTED_KEY = "tidebreak.model-menu-not-connected-collapsed";
const ACTIVE_TURN_SEND_MODE_KEY = "tidebreak.composer.sendMode";

export type ActiveTurnSendMode = "queue" | "steer";

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

function readStoredNotConnectedCollapsed(): boolean {
  try {
    return window.localStorage.getItem(MODEL_MENU_NOT_CONNECTED_KEY) === "true";
  } catch {
    return false;
  }
}

function storeNotConnectedCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(MODEL_MENU_NOT_CONNECTED_KEY, String(collapsed));
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
 * whether the sidebar is showing.
 *
 * Where the workspace is pointed used to live here too. It is in the URL now,
 * because a layout that cannot be linked at cannot be the target of a citation.
 */
export type UiStore = {
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
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
  }));
}

export const useUiStore = createUiStore();

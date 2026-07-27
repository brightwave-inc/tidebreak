import { create } from "zustand";

const SIDEBAR_COLLAPSED_KEY = "openwave.sidebar-collapsed";

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

/**
 * Chrome state that belongs to no conversation and does not deserve a URL:
 * whether the sidebar is showing, and whether Settings is open over the top.
 *
 * Where the workspace is pointed used to live here too. It is in the URL now,
 * because a layout that cannot be linked at cannot be the target of a citation.
 */
export type UiStore = {
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
  settingsOpen: boolean;
  openSettings: () => void;
  closeSettings: () => void;
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
    settingsOpen: false,
    openSettings: () => set({ settingsOpen: true }),
    closeSettings: () => set({ settingsOpen: false }),
  }));
}

export const useUiStore = createUiStore();

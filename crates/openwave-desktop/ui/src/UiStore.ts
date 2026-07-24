import { create } from "zustand";

export type PrimaryView =
  | "chat"
  | "documents"
  | "deliverables"
  | "folders"
  | "settings";

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
 * App-level view state: which primary surface is showing. Actions are named
 * for intent so call sites read as navigation, not state plumbing. The
 * chat-scoped surfaces (documents, deliverables, folders) are reached from the
 * per-chat tab control; settings is global.
 */
export type UiStore = {
  primaryView: PrimaryView;
  showChat: () => void;
  showDocuments: () => void;
  showDeliverables: () => void;
  showFolders: () => void;
  showSettings: () => void;
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
};

export function createUiStore() {
  return create<UiStore>()((set) => ({
    primaryView: "chat",
    showChat: () => set({ primaryView: "chat" }),
    showDocuments: () => set({ primaryView: "documents" }),
    showDeliverables: () => set({ primaryView: "deliverables" }),
    showFolders: () => set({ primaryView: "folders" }),
    showSettings: () => set({ primaryView: "settings" }),
    sidebarCollapsed: readStoredSidebarCollapsed(),
    toggleSidebar: () =>
      set((state) => {
        const sidebarCollapsed = !state.sidebarCollapsed;
        storeSidebarCollapsed(sidebarCollapsed);
        return { sidebarCollapsed };
      }),
  }));
}

export const useUiStore = createUiStore();

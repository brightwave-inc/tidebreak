import { create } from "zustand";
import { CHAT_SURFACE, normalizeSurface, type Surface } from "./Surface";

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
 * App-level view state: which surface the workspace is showing, and where in
 * it. Actions are named for intent so call sites read as navigation, not state
 * plumbing. The chat-scoped surfaces (documents, deliverables, folders) are
 * reached from the per-chat tab control; settings is global.
 */
export type UiStore = {
  surface: Surface;
  openSurface: (surface: Surface) => void;
  showChat: () => void;
  showDocuments: (target?: {
    documentId?: string;
    citationId?: string;
  }) => void;
  showDeliverables: (target?: { filename?: string }) => void;
  showFolders: () => void;
  showSettings: () => void;
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
};

export function createUiStore() {
  return create<UiStore>()((set) => ({
    surface: CHAT_SURFACE,
    openSurface: (surface) => set({ surface: normalizeSurface(surface) }),
    showChat: () => set({ surface: CHAT_SURFACE }),
    showDocuments: (target) =>
      set({
        surface: normalizeSurface({
          kind: "documents",
          itemId: target?.documentId,
          anchor: target?.citationId,
        }),
      }),
    showDeliverables: (target) =>
      set({
        surface: normalizeSurface({
          kind: "deliverables",
          itemId: target?.filename,
        }),
      }),
    showFolders: () => set({ surface: { kind: "folders" } }),
    showSettings: () => set({ surface: { kind: "settings" } }),
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

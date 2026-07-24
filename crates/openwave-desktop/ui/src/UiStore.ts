import { create } from "zustand";

export type PrimaryView = "chat" | "documents" | "deliverables" | "settings";
export type SettingsPanel = "folders" | null;

/**
 * App-level view state: which primary surface is showing and whether the
 * folders side panel is open. Actions are named for intent so call sites
 * read as navigation, not state plumbing.
 */
export type UiStore = {
  primaryView: PrimaryView;
  settingsPanel: SettingsPanel;
  /**
   * Return to the chat view. By default any open side panel closes (chat
   * selection); `keepPanels` preserves it (new-chat and back navigation,
   * which historically left the panel alone).
   */
  showChat: (options?: { keepPanels?: boolean }) => void;
  showDocuments: () => void;
  showDeliverables: () => void;
  showSettings: () => void;
  /** Toggle the folders panel; optionally force the chat view first. */
  toggleFoldersPanel: (options?: { showChat?: boolean }) => void;
};

export function createUiStore() {
  return create<UiStore>()((set) => ({
    primaryView: "chat",
    settingsPanel: null,
    showChat: (options) =>
      set((state) => ({
        primaryView: "chat",
        settingsPanel: options?.keepPanels ? state.settingsPanel : null,
      })),
    showDocuments: () => set({ primaryView: "documents", settingsPanel: null }),
    showDeliverables: () =>
      set({ primaryView: "deliverables", settingsPanel: null }),
    showSettings: () => set({ primaryView: "settings", settingsPanel: null }),
    toggleFoldersPanel: (options) =>
      set((state) => ({
        primaryView: options?.showChat ? "chat" : state.primaryView,
        settingsPanel: state.settingsPanel === "folders" ? null : "folders",
      })),
  }));
}

export const useUiStore = createUiStore();

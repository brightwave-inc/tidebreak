import { create } from "zustand";
import { CHAT_SURFACE, normalizeSurface, type Surface } from "./Surface";
import {
  EMPTY_LAYOUTS,
  WORKSPACE_LAYOUT_KEY,
  clampFraction,
  layoutForChat,
  normalizeLayouts,
  rememberChatLayout,
  type WorkspaceLayouts,
} from "./WorkspaceLayout";

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

function readStoredLayouts(): WorkspaceLayouts {
  try {
    const raw = window.localStorage.getItem(WORKSPACE_LAYOUT_KEY);
    return raw ? normalizeLayouts(JSON.parse(raw)) : EMPTY_LAYOUTS;
  } catch {
    return EMPTY_LAYOUTS;
  }
}

function storeLayouts(layouts: WorkspaceLayouts): void {
  try {
    window.localStorage.setItem(WORKSPACE_LAYOUT_KEY, JSON.stringify(layouts));
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * App-level view state: which surface the workspace is showing, where in it,
 * and how the workspace is arranged around it. Actions are named for intent so
 * call sites read as navigation, not state plumbing. The chat-scoped surfaces
 * (documents, deliverables, folders) are reached from the per-chat tab control
 * and open beside the transcript; settings is global and replaces the pane.
 */
export type UiStore = {
  surface: Surface;
  expanded: boolean;
  fraction: number;
  openSurface: (surface: Surface) => void;
  showChat: () => void;
  showDocuments: (target?: {
    documentId?: string;
    citationId?: string;
  }) => void;
  showDeliverables: (target?: { filename?: string }) => void;
  showFolders: () => void;
  showSettings: () => void;
  /** Point the workspace at a chat, restoring the layout it was left in. */
  selectChatWorkspace: (chatId: string) => void;
  forgetChatWorkspace: (chatId: string) => void;
  toggleExpanded: () => void;
  setFraction: (fraction: number) => void;
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
};

export function createUiStore() {
  return create<UiStore>()((set, get) => {
    let layouts = readStoredLayouts();
    let chatId: string | null = null;

    // Settings replaces the whole pane and belongs to no conversation, so it
    // is never written into a chat's remembered layout.
    function remember(surface: Surface, expanded: boolean) {
      if (!chatId || surface.kind === "settings") return;
      layouts = rememberChatLayout(layouts, chatId, { surface, expanded });
      storeLayouts(layouts);
    }

    function go(surface: Surface, expanded = get().expanded) {
      const next = normalizeSurface(surface);
      const nextExpanded = next.kind === "chat" ? false : expanded;
      remember(next, nextExpanded);
      set({ surface: next, expanded: nextExpanded });
    }

    return {
      surface: CHAT_SURFACE,
      expanded: false,
      fraction: layouts.fraction,
      openSurface: (surface) => go(surface),
      showChat: () => go(CHAT_SURFACE),
      showDocuments: (target) =>
        go({
          kind: "documents",
          itemId: target?.documentId,
          anchor: target?.citationId,
        }),
      showDeliverables: (target) =>
        go({ kind: "deliverables", itemId: target?.filename }),
      showFolders: () => go({ kind: "folders" }),
      showSettings: () => set({ surface: { kind: "settings" } }),
      selectChatWorkspace: (nextChatId) => {
        chatId = nextChatId;
        const restored = layoutForChat(layouts, nextChatId);
        set({
          surface: restored?.surface ?? CHAT_SURFACE,
          expanded: restored?.expanded ?? false,
        });
      },
      forgetChatWorkspace: (deletedChatId) => {
        layouts = rememberChatLayout(layouts, deletedChatId, {
          surface: CHAT_SURFACE,
          expanded: false,
        });
        storeLayouts(layouts);
      },
      toggleExpanded: () => {
        const expanded = !get().expanded;
        remember(get().surface, expanded);
        set({ expanded });
      },
      setFraction: (value) => {
        const fraction = clampFraction(value);
        layouts = { ...layouts, fraction };
        storeLayouts(layouts);
        set({ fraction });
      },
      sidebarCollapsed: readStoredSidebarCollapsed(),
      toggleSidebar: () =>
        set((state) => {
          const sidebarCollapsed = !state.sidebarCollapsed;
          storeSidebarCollapsed(sidebarCollapsed);
          return { sidebarCollapsed };
        }),
    };
  });
}

export const useUiStore = createUiStore();

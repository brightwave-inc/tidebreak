import { create } from "zustand";

import type { HarnessKind } from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import {
  isWorkspaceSortMode,
  type WorkspaceSortMode,
} from "./workspaceCards";

const REVIEW_SIDEBAR_OPEN_KEY = "tidebreak.code-review-sidebar-open";
const LAST_CREATE_KEY = "tidebreak.code-last-create";
const WORKSPACE_SORT_KEY = "tidebreak.code-workspace-sort";
const TERMINAL_DRAWER_HEIGHTS_KEY = "tidebreak.code-terminal-drawer-heights";

/** What the reader picked the last time they created a workspace. */
export type CodeCreateDefaults = {
  repoId?: string;
  harness?: HarnessKind;
  model?: string;
};

/** Inspector filter for one turn's files and diff. `label` is the ordinal, never the id. */
export type InspectorScope = {
  turnId: string;
  label: string;
};

function readStoredCreateDefaults(): CodeCreateDefaults | null {
  try {
    const raw = window.localStorage.getItem(LAST_CREATE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    const record = parsed as Record<string, unknown>;
    const text = (value: unknown) =>
      typeof value === "string" && value.length > 0 ? value : undefined;
    return {
      repoId: text(record.repoId),
      harness: text(record.harness) as HarnessKind | undefined,
      model: text(record.model),
    };
  } catch {
    return null;
  }
}

function storeCreateDefaults(defaults: CodeCreateDefaults): void {
  try {
    window.localStorage.setItem(LAST_CREATE_KEY, JSON.stringify(defaults));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredReviewSidebarOpen(): boolean {
  try {
    const raw = window.localStorage.getItem(REVIEW_SIDEBAR_OPEN_KEY);
    // Git, the pull request, and comments live here now — open until the
    // reader hides the rail, then remember that.
    if (raw == null) return true;
    return raw === "true";
  } catch {
    return true;
  }
}

function storeReviewSidebarOpen(open: boolean): void {
  try {
    window.localStorage.setItem(REVIEW_SIDEBAR_OPEN_KEY, String(open));
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredWorkspaceSort(): WorkspaceSortMode {
  try {
    const raw = window.localStorage.getItem(WORKSPACE_SORT_KEY);
    if (raw && isWorkspaceSortMode(raw)) return raw;
  } catch {
    // Preference persistence is best-effort.
  }
  return "by-repo";
}

function storeWorkspaceSort(mode: WorkspaceSortMode): void {
  try {
    window.localStorage.setItem(WORKSPACE_SORT_KEY, mode);
  } catch {
    // Preference persistence is best-effort.
  }
}

function readStoredTerminalDrawerHeights(): Record<string, number> {
  try {
    const raw = window.localStorage.getItem(TERMINAL_DRAWER_HEIGHTS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    const heights: Record<string, number> = {};
    for (const [workspaceId, value] of Object.entries(
      parsed as Record<string, unknown>,
    )) {
      if (typeof value === "number" && Number.isFinite(value)) {
        heights[workspaceId] = value;
      }
    }
    return heights;
  } catch {
    return {};
  }
}

function storeTerminalDrawerHeights(heights: Record<string, number>): void {
  try {
    window.localStorage.setItem(
      TERMINAL_DRAWER_HEIGHTS_KEY,
      JSON.stringify(heights),
    );
  } catch {
    // Preference persistence is best-effort.
  }
}

/**
 * Code mode's dialog and chrome state, held outside the components that draw it.
 *
 * The new-workspace flow is reachable from three places — the rail, a repo
 * page, and Cmd+N — and only one of them is a component the shell can see. A
 * store lets the shortcut open the dialog without the shell importing code
 * mode's surfaces, and keeps a single dialog instance rather than one per
 * button that opens it.
 *
 * The review rail is the same kind of chrome as the left sidebar: it is not
 * a URL, and a reload should not forget whether it was showing. Terminal
 * drawer height is the same kind of chrome, remembered per workspace.
 *
 * `lastCreate` is the tie-breaker for the new-workspace dialog's defaults.
 * The catalog answers "what did you work on last" for anyone with a
 * workspace already; this covers the first run of a window and the reader
 * whose newest workspace is not the one they want to repeat.
 */
export type CodeUiStore = {
  newWorkspaceOpen: boolean;
  /** The repo the dialog opens on, when it was opened from one. */
  newWorkspaceRepoId: string | undefined;
  addRepoOpen: boolean;
  reviewSidebarOpen: boolean;
  /** Files and diff scoped to one turn, or the whole worktree when null. */
  inspectorScope: InspectorScope | null;
  workspaceSortMode: WorkspaceSortMode;
  lastCreate: CodeCreateDefaults | null;
  /** Per-workspace terminal drawer height, remembered across reloads. */
  terminalDrawerHeights: Record<string, number>;
  /**
   * Ask for a workspace, from a repo page or from anywhere in code mode.
   *
   * With nothing registered the new-workspace dialog is a form the reader
   * cannot submit, so the request lands on repo registration instead — that is
   * the step actually in their way.
   */
  startNewWorkspace: (repoId?: string) => void;
  setNewWorkspaceOpen: (open: boolean) => void;
  setAddRepoOpen: (open: boolean) => void;
  toggleReviewSidebar: () => void;
  setReviewSidebarOpen: (open: boolean) => void;
  setInspectorScope: (scope: InspectorScope | null) => void;
  setWorkspaceSortMode: (mode: WorkspaceSortMode) => void;
  /** Record a successful create so the next dialog opens on the same choices. */
  rememberCreate: (defaults: CodeCreateDefaults) => void;
  setTerminalDrawerHeight: (workspaceId: string, height: number) => void;
  /**
   * Prompt waiting for the code composer. The inspector bar writes; the
   * composer takes and clears so a remount cannot insert twice.
   */
  pendingComposerPrompt: string | null;
  offerComposerPrompt: (prompt: string) => void;
  takeComposerPrompt: () => string | null;
};

export const useCodeUiStore = create<CodeUiStore>()((set, get) => ({
  newWorkspaceOpen: false,
  newWorkspaceRepoId: undefined,
  addRepoOpen: false,
  reviewSidebarOpen: readStoredReviewSidebarOpen(),
  inspectorScope: null,
  workspaceSortMode: readStoredWorkspaceSort(),
  lastCreate: readStoredCreateDefaults(),
  terminalDrawerHeights: readStoredTerminalDrawerHeights(),
  pendingComposerPrompt: null,
  offerComposerPrompt: (prompt) => set({ pendingComposerPrompt: prompt }),
  takeComposerPrompt: (): string | null => {
    const prompt = get().pendingComposerPrompt;
    if (!prompt) return null;
    set({ pendingComposerPrompt: null });
    return prompt;
  },
  startNewWorkspace: (repoId) => {
    const { repos } = useCodeCatalogStore.getState();
    if (repos.length === 0) {
      set({ addRepoOpen: true });
      return;
    }
    // A repo the catalog does not know is not a useful default; free
    // choice is the safe fallback.
    const known = repos.some((repo) => repo.id === repoId) ? repoId : undefined;
    set({ newWorkspaceOpen: true, newWorkspaceRepoId: known });
  },
  setNewWorkspaceOpen: (open) => set({ newWorkspaceOpen: open }),
  setAddRepoOpen: (open) => set({ addRepoOpen: open }),
  toggleReviewSidebar: () =>
    set((state) => {
      const reviewSidebarOpen = !state.reviewSidebarOpen;
      storeReviewSidebarOpen(reviewSidebarOpen);
      return { reviewSidebarOpen };
    }),
  setReviewSidebarOpen: (open) => {
    storeReviewSidebarOpen(open);
    set({ reviewSidebarOpen: open });
  },
  setInspectorScope: (inspectorScope) => set({ inspectorScope }),
  setWorkspaceSortMode: (mode) => {
    storeWorkspaceSort(mode);
    set({ workspaceSortMode: mode });
  },
  rememberCreate: (defaults) => {
    storeCreateDefaults(defaults);
    set({ lastCreate: defaults });
  },
  setTerminalDrawerHeight: (workspaceId, height) => {
    set((state) => {
      const terminalDrawerHeights = {
        ...state.terminalDrawerHeights,
        [workspaceId]: height,
      };
      storeTerminalDrawerHeights(terminalDrawerHeights);
      return { terminalDrawerHeights };
    });
  },
}));

import { create } from "zustand";

import { useCodeCatalogStore } from "./CodeCatalogStore";

const REVIEW_SIDEBAR_OPEN_KEY = "tidebreak.code-review-sidebar-open";

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
 * a URL, and a reload should not forget whether it was showing.
 */
export type CodeUiStore = {
  newWorkspaceOpen: boolean;
  /** The repo the dialog opens locked to, when it was opened from one. */
  newWorkspaceRepoId: string | undefined;
  addRepoOpen: boolean;
  reviewSidebarOpen: boolean;
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
};

export const useCodeUiStore = create<CodeUiStore>()((set) => ({
  newWorkspaceOpen: false,
  newWorkspaceRepoId: undefined,
  addRepoOpen: false,
  reviewSidebarOpen: readStoredReviewSidebarOpen(),
  startNewWorkspace: (repoId) => {
    const { repos } = useCodeCatalogStore.getState();
    if (repos.length === 0) {
      set({ addRepoOpen: true });
      return;
    }
    // A repo the catalog does not know would lock the form to nothing and
    // leave the reader a dialog they cannot submit; free choice is the safe
    // fallback.
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
}));

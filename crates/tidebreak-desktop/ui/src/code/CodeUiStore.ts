import { create } from "zustand";

import { useCodeCatalogStore } from "./CodeCatalogStore";

/**
 * Code mode's dialog state, held outside the components that draw it.
 *
 * The new-workspace flow is reachable from three places — the rail, a repo
 * page, and Cmd+N — and only one of them is a component the shell can see. A
 * store lets the shortcut open the dialog without the shell importing code
 * mode's surfaces, and keeps a single dialog instance rather than one per
 * button that opens it.
 */
export type CodeUiStore = {
  newWorkspaceOpen: boolean;
  /** The repo the dialog opens locked to, when it was opened from one. */
  newWorkspaceRepoId: string | undefined;
  addRepoOpen: boolean;
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
};

export const useCodeUiStore = create<CodeUiStore>()((set) => ({
  newWorkspaceOpen: false,
  newWorkspaceRepoId: undefined,
  addRepoOpen: false,
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
}));

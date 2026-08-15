import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
} from "../api/types";

/**
 * Repos, workspaces, and the last session each workspace opened.
 *
 * The workspace page loads sessions from `GET /code/workspaces/{id}/sessions`.
 * `rememberSession` is write-through after create/reap so the same window
 * does not wait on another round trip. Nothing here is persisted: a reload
 * asks the server again.
 */

type CodeCatalogState = {
  repos: CodeRepoSnapshot[];
  workspaces: CodeWorkspaceSnapshot[];
  sessionsByWorkspace: Record<string, CodeSessionSnapshot>;
  doctor: HarnessDoctorReport | null;
  loaded: boolean;
  error: string | null;
};

type CodeCatalogStore = CodeCatalogState & {
  refresh: (client: Pick<
    ApiClient,
    "listCodeRepos" | "listCodeWorkspaces" | "getHarnessDoctor"
  >) => Promise<void>;
  rememberSession: (session: CodeSessionSnapshot) => void;
  forgetWorkspace: (workspaceId: string) => void;
  upsertRepo: (repo: CodeRepoSnapshot) => void;
  upsertWorkspace: (workspace: CodeWorkspaceSnapshot) => void;
  reset: () => void;
};

export const useCodeCatalogStore = create<CodeCatalogStore>()((set, get) => ({
  repos: [],
  workspaces: [],
  sessionsByWorkspace: {},
  doctor: null,
  loaded: false,
  error: null,
  refresh: async (client) => {
    try {
      const [repos, workspaces, doctor] = await Promise.all([
        client.listCodeRepos(),
        client.listCodeWorkspaces(),
        client.getHarnessDoctor(),
      ]);
      set({ repos, workspaces, doctor, loaded: true, error: null });
    } catch (error) {
      set({
        loaded: true,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  },
  rememberSession: (session) => {
    set({
      sessionsByWorkspace: {
        ...get().sessionsByWorkspace,
        [session.workspace_id]: session,
      },
    });
  },
  forgetWorkspace: (workspaceId) => {
    const { [workspaceId]: _removed, ...sessionsByWorkspace } =
      get().sessionsByWorkspace;
    set({
      sessionsByWorkspace,
      workspaces: get().workspaces.filter((item) => item.id !== workspaceId),
    });
  },
  upsertRepo: (repo) => {
    set({
      repos: [
        repo,
        ...get().repos.filter((item) => item.id !== repo.id),
      ],
    });
  },
  upsertWorkspace: (workspace) => {
    set({
      workspaces: [
        workspace,
        ...get().workspaces.filter((item) => item.id !== workspace.id),
      ],
    });
  },
  reset: () => {
    set({
      repos: [],
      workspaces: [],
      sessionsByWorkspace: {},
      doctor: null,
      loaded: false,
      error: null,
    });
  },
}));

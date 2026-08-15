import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
} from "../api/types";

const SESSION_CACHE_KEY = "tidebreak.code-sessions";

/**
 * Repos, workspaces, and the last session each workspace opened.
 *
 * The walking skeleton has no `/code/updates` digest and no list-sessions
 * route, so the rail and the workspace page share this catalog. Sessions are
 * remembered after create (and across reloads) so reopening a workspace can
 * still attach its socket.
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

function readCachedSessions(): Record<string, CodeSessionSnapshot> {
  try {
    const raw = window.localStorage.getItem(SESSION_CACHE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return {};
    return parsed as Record<string, CodeSessionSnapshot>;
  } catch {
    return {};
  }
}

function writeCachedSessions(sessions: Record<string, CodeSessionSnapshot>) {
  try {
    window.localStorage.setItem(SESSION_CACHE_KEY, JSON.stringify(sessions));
  } catch {
    /* ignore quota / private-mode */
  }
}

export const useCodeCatalogStore = create<CodeCatalogStore>()((set, get) => ({
  repos: [],
  workspaces: [],
  sessionsByWorkspace: readCachedSessions(),
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
    const sessionsByWorkspace = {
      ...get().sessionsByWorkspace,
      [session.workspace_id]: session,
    };
    writeCachedSessions(sessionsByWorkspace);
    set({ sessionsByWorkspace });
  },
  forgetWorkspace: (workspaceId) => {
    const { [workspaceId]: _removed, ...sessionsByWorkspace } =
      get().sessionsByWorkspace;
    writeCachedSessions(sessionsByWorkspace);
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
    writeCachedSessions({});
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

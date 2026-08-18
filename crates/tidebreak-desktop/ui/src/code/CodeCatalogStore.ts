import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
  HarnessKind,
} from "../api/types";
import { harnessCodeModels, type CodeModelOption } from "./labels";

const HARNESS_KINDS: HarnessKind[] = [
  "claude_code",
  "codex",
  "opencode",
  "grok",
];

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
  modelsByHarness: Partial<Record<HarnessKind, CodeModelOption[]>>;
  loaded: boolean;
  error: string | null;
};

type CodeCatalogStore = CodeCatalogState & {
  refresh: (client: Pick<
    ApiClient,
    | "listCodeRepos"
    | "listCodeWorkspaces"
    | "getHarnessDoctor"
    | "listCodeHarnessModels"
  >) => Promise<void>;
  refreshDoctor: (
    client: Pick<ApiClient, "refreshHarnessDoctor">,
  ) => Promise<void>;
  ensureHarnessModels: (
    client: Pick<ApiClient, "listCodeHarnessModels">,
    kind: HarnessKind,
  ) => Promise<CodeModelOption[]>;
  rememberHarnessModels: (
    kind: HarnessKind,
    models: CodeModelOption[],
  ) => void;
  rememberSession: (session: CodeSessionSnapshot) => void;
  forgetWorkspaceSession: (workspaceId: string) => void;
  upsertRepo: (repo: CodeRepoSnapshot) => void;
  upsertWorkspace: (workspace: CodeWorkspaceSnapshot) => void;
  reset: () => void;
};

export const useCodeCatalogStore = create<CodeCatalogStore>()((set, get) => ({
  repos: [],
  workspaces: [],
  sessionsByWorkspace: {},
  doctor: null,
  modelsByHarness: {},
  loaded: false,
  error: null,
  refresh: async (client) => {
    try {
      const [repos, workspaces] = await Promise.all([
        client.listCodeRepos(),
        client.listCodeWorkspaces(),
      ]);
      set({ repos, workspaces, loaded: true, error: null });
    } catch (error) {
      set({
        loaded: true,
        error: error instanceof Error ? error.message : String(error),
      });
      return;
    }
    const extras: Promise<void>[] = [
      client
        .getHarnessDoctor()
        .then((doctor) => set({ doctor }))
        .catch(() => {
          // The rail does not need the doctor. A missing report just delays
          // start-session until the next refresh.
        }),
    ];
    for (const kind of HARNESS_KINDS) {
      extras.push(
        client
          .listCodeHarnessModels(kind)
          .then((listed) => {
            get().rememberHarnessModels(
              kind,
              harnessCodeModels(listed.models, kind),
            );
          })
          .catch(() => undefined),
      );
    }
    await Promise.all(extras);
  },
  refreshDoctor: async (client) => {
    const doctor = await client.refreshHarnessDoctor();
    set({ doctor });
  },
  ensureHarnessModels: async (client, kind) => {
    const cached = get().modelsByHarness[kind];
    if (cached && cached.length > 0) return cached;
    try {
      const listed = await client.listCodeHarnessModels(kind);
      const models = harnessCodeModels(listed.models, kind);
      get().rememberHarnessModels(kind, models);
      return models;
    } catch {
      return [];
    }
  },
  rememberHarnessModels: (kind, models) => {
    set({
      modelsByHarness: { ...get().modelsByHarness, [kind]: models },
    });
  },
  rememberSession: (session) => {
    set({
      sessionsByWorkspace: {
        ...get().sessionsByWorkspace,
        [session.workspace_id]: session,
      },
    });
  },
  forgetWorkspaceSession: (workspaceId) => {
    const { [workspaceId]: _removed, ...sessionsByWorkspace } =
      get().sessionsByWorkspace;
    set({ sessionsByWorkspace });
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
    const current = get().workspaces;
    const index = current.findIndex((item) => item.id === workspace.id);
    if (index === -1) {
      set({ workspaces: [...current, workspace] });
      return;
    }
    const workspaces = current.slice();
    workspaces[index] = workspace;
    set({ workspaces });
  },
  reset: () => {
    set({
      repos: [],
      workspaces: [],
      sessionsByWorkspace: {},
      doctor: null,
      modelsByHarness: {},
      loaded: false,
      error: null,
    });
  },
}));

import { create } from "zustand";

import type { ApiClient } from "../api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
  HarnessKind,
  ReasoningEffort,
} from "../api/types";
import { harnessCodeModels, type CodeModelOption } from "./labels";

const HARNESS_KINDS: HarnessKind[] = [
  "claude_code",
  "codex",
  "opencode",
  "grok",
];

/** One native model probe per harness, shared by every open picker. */
const modelRequests = new Map<HarnessKind, Promise<CodeModelOption[]>>();
/** Sidebar and route bodies mount together; they share one catalog refresh. */
let catalogRefresh: Promise<void> | null = null;

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
  /**
   * Each engine's own effort ladder, which is what a code session runs on
   * whatever catalog the model row came from.
   */
  effortsByHarness: Partial<Record<HarnessKind, ReasoningEffort[]>>;
  loaded: boolean;
  error: string | null;
};

type CodeCatalogStore = CodeCatalogState & {
  refresh: (
    client: Pick<
      ApiClient,
      | "listCodeRepos"
      | "listCodeWorkspaces"
      | "getHarnessDoctor"
      | "listCodeHarnessModels"
    >,
  ) => Promise<void>;
  refreshDoctor: (
    client: Pick<ApiClient, "refreshHarnessDoctor">,
  ) => Promise<void>;
  reloadDoctor: (client: Pick<ApiClient, "getHarnessDoctor">) => Promise<void>;
  ensureHarnessModels: (
    client: Pick<ApiClient, "listCodeHarnessModels">,
    kind: HarnessKind,
  ) => Promise<CodeModelOption[]>;
  rememberHarnessModels: (
    kind: HarnessKind,
    models: CodeModelOption[],
    reasoningEfforts?: ReasoningEffort[],
  ) => void;
  rememberSession: (session: CodeSessionSnapshot) => void;
  forgetWorkspaceSession: (workspaceId: string) => void;
  upsertRepo: (repo: CodeRepoSnapshot) => void;
  upsertWorkspace: (workspace: CodeWorkspaceSnapshot) => void;
  replaceWorkspace: (
    workspaceId: string,
    workspace: CodeWorkspaceSnapshot,
  ) => void;
  removeWorkspace: (workspaceId: string) => void;
  reset: () => void;
};

export const OPTIMISTIC_WORKSPACE_ID_PREFIX = "optimistic-workspace:";

export function isOptimisticWorkspace(workspace: CodeWorkspaceSnapshot) {
  return workspace.id.startsWith(OPTIMISTIC_WORKSPACE_ID_PREFIX);
}

function loadHarnessModels(
  client: Pick<ApiClient, "listCodeHarnessModels">,
  kind: HarnessKind,
  get: () => CodeCatalogStore,
  force: boolean,
): Promise<CodeModelOption[]> {
  const cached = get().modelsByHarness[kind];
  // An empty array is a finished probe: this engine advertised no models.
  // Treating it as a cache miss makes every picker open run the CLI again.
  if (!force && cached !== undefined) return Promise.resolve(cached);
  const pending = modelRequests.get(kind);
  if (pending) return pending;

  const request = client
    .listCodeHarnessModels(kind)
    .then((listed) => {
      const models = harnessCodeModels(listed.models, kind);
      get().rememberHarnessModels(kind, models, listed.reasoning_efforts);
      return models;
    })
    .catch(() => [])
    .finally(() => {
      if (modelRequests.get(kind) === request) modelRequests.delete(kind);
    });
  modelRequests.set(kind, request);
  return request;
}

export const useCodeCatalogStore = create<CodeCatalogStore>()((set, get) => ({
  repos: [],
  workspaces: [],
  sessionsByWorkspace: {},
  doctor: null,
  modelsByHarness: {},
  effortsByHarness: {},
  loaded: false,
  error: null,
  refresh: (client) => {
    if (catalogRefresh) return catalogRefresh;
    const workspaceIdsAtStart = new Set(
      get().workspaces.map((workspace) => workspace.id),
    );
    let request: Promise<void>;
    request = (async () => {
      try {
        const [repos, workspaces] = await Promise.all([
          client.listCodeRepos(),
          client.listCodeWorkspaces(),
        ]);
        const localCreates = get().workspaces.filter(
          (workspace) =>
            isOptimisticWorkspace(workspace) ||
            !workspaceIdsAtStart.has(workspace.id),
        );
        const localCreateIds = new Set(
          localCreates.map((workspace) => workspace.id),
        );
        set({
          repos,
          workspaces: [
            ...workspaces.filter(
              (workspace) => !localCreateIds.has(workspace.id),
            ),
            ...localCreates,
          ],
          loaded: true,
          error: null,
        });
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
            // Home waits on a settled report. An empty one leaves the loading
            // empty and shows the install section instead of spinning forever.
            if (get().doctor === null) {
              set({ doctor: { harnesses: [] } });
            }
          }),
      ];
      for (const kind of HARNESS_KINDS) {
        extras.push(
          loadHarnessModels(client, kind, get, true).then(() => undefined),
        );
      }
      await Promise.all(extras);
    })().finally(() => {
      if (catalogRefresh === request) catalogRefresh = null;
    });
    catalogRefresh = request;
    return request;
  },
  refreshDoctor: async (client) => {
    const doctor = await client.refreshHarnessDoctor();
    set({ doctor });
  },
  // The memoized read, for picking up an engine a download just put on disk.
  // `refreshDoctor` is the doctor's own button: it drops every memoized probe
  // and takes them all cold, which is far more than reading one result that
  // already exists.
  reloadDoctor: async (client) => {
    set({ doctor: await client.getHarnessDoctor() });
  },
  ensureHarnessModels: (client, kind) =>
    loadHarnessModels(client, kind, get, false),
  rememberHarnessModels: (kind, models, reasoningEfforts) => {
    set({
      modelsByHarness: { ...get().modelsByHarness, [kind]: models },
      ...(reasoningEfforts
        ? {
            effortsByHarness: {
              ...get().effortsByHarness,
              [kind]: reasoningEfforts,
            },
          }
        : {}),
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
      repos: [repo, ...get().repos.filter((item) => item.id !== repo.id)],
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
  replaceWorkspace: (workspaceId, workspace) => {
    const current = get().workspaces;
    const index = current.findIndex((item) => item.id === workspaceId);
    if (index === -1) {
      set({
        workspaces: [
          ...current.filter((item) => item.id !== workspace.id),
          workspace,
        ],
      });
      return;
    }
    const workspaces = current.filter(
      (item) => item.id !== workspace.id || item.id === workspaceId,
    );
    const replacementIndex = workspaces.findIndex(
      (item) => item.id === workspaceId,
    );
    workspaces[replacementIndex] = workspace;
    set({ workspaces });
  },
  removeWorkspace: (workspaceId) => {
    set({
      workspaces: get().workspaces.filter((item) => item.id !== workspaceId),
    });
  },
  reset: () => {
    catalogRefresh = null;
    modelRequests.clear();
    set({
      repos: [],
      workspaces: [],
      sessionsByWorkspace: {},
      doctor: null,
      modelsByHarness: {},
      effortsByHarness: {},
      loaded: false,
      error: null,
    });
  },
}));

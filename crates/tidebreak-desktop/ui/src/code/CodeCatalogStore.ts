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
  reloadDoctor: (client: Pick<ApiClient, "getHarnessDoctor">) => Promise<void>;
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
      get().rememberHarnessModels(kind, models);
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
  loaded: false,
  error: null,
  refresh: (client) => {
    if (catalogRefresh) return catalogRefresh;
    let request: Promise<void>;
    request = (async () => {
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
  // The memoized read, for picking up an engine a warm install just put on
  // disk. `refreshDoctor` is the doctor's own button: it re-probes every
  // engine and installs every pin, which is far more than reading one result
  // that already exists.
  reloadDoctor: async (client) => {
    set({ doctor: await client.getHarnessDoctor() });
  },
  ensureHarnessModels: (client, kind) =>
    loadHarnessModels(client, kind, get, false),
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
    catalogRefresh = null;
    modelRequests.clear();
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

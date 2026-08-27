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
import {
  codeClientGeneration,
  isCodeClientGenerationActive,
} from "./CodeClientGeneration";
import {
  HARNESS_LABELS,
  harnessCodeModels,
  type CodeModelOption,
} from "./labels";
import type { ParsedHarnessModelList } from "./parsers";

const HARNESS_KINDS: HarnessKind[] = [
  "claude_code",
  "codex",
  "opencode",
  "grok",
];

/** One native model probe per harness, shared by every open picker. */
type CatalogRequestContext = {
  clientGeneration: number;
  storeGeneration: number;
};

type ModelRequest = CatalogRequestContext & {
  promise: Promise<CodeModelOption[]>;
};

type CatalogRefresh = CatalogRequestContext & {
  promise: Promise<void>;
};

const modelRequests = new Map<HarnessKind, ModelRequest>();
/** Sidebar and route bodies mount together; they share one catalog refresh. */
let catalogRefresh: CatalogRefresh | null = null;
let storeGeneration = 0;

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

/** Preserve whether the server listed the engine or the hosted gateway. */
export function codeModelsFromHarnessListing(
  listed: Pick<ParsedHarnessModelList, "models" | "source">,
  kind: HarnessKind,
): CodeModelOption[] {
  const models = harnessCodeModels(listed.models, kind);
  if (listed.source !== "model_gateway") return models;
  const source = `${HARNESS_LABELS[kind]} · model-gateway`;
  return models.map((model) => ({ ...model, source }));
}

function loadHarnessModels(
  client: Pick<ApiClient, "listCodeHarnessModels">,
  kind: HarnessKind,
  get: () => CodeCatalogStore,
  force: boolean,
  context = requestContext(client),
): Promise<CodeModelOption[]> {
  if (!requestIsCurrent(context)) return Promise.resolve([]);
  const cached = get().modelsByHarness[kind];
  // An empty array is a finished probe: this engine advertised no models.
  // Treating it as a cache miss makes every picker open run the CLI again.
  if (!force && cached !== undefined) return Promise.resolve(cached);
  const pending = modelRequests.get(kind);
  if (pending && sameRequestContext(pending, context)) return pending.promise;

  const request = client
    .listCodeHarnessModels(kind)
    .then((listed) => {
      if (!requestIsCurrent(context)) return [];
      const models = codeModelsFromHarnessListing(listed, kind);
      get().rememberHarnessModels(kind, models, listed.reasoning_efforts);
      return models;
    })
    .catch(() => [])
    .finally(() => {
      if (modelRequests.get(kind)?.promise === request) {
        modelRequests.delete(kind);
      }
    });
  modelRequests.set(kind, { ...context, promise: request });
  return request;
}

function requestContext(client: object): CatalogRequestContext {
  return {
    clientGeneration: codeClientGeneration(client),
    storeGeneration,
  };
}

function sameRequestContext(
  left: CatalogRequestContext,
  right: CatalogRequestContext,
): boolean {
  return (
    left.clientGeneration === right.clientGeneration &&
    left.storeGeneration === right.storeGeneration
  );
}

function requestIsCurrent(context: CatalogRequestContext): boolean {
  return (
    context.storeGeneration === storeGeneration &&
    isCodeClientGenerationActive(context.clientGeneration)
  );
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
    const context = requestContext(client);
    if (!requestIsCurrent(context)) return Promise.resolve();
    if (catalogRefresh && sameRequestContext(catalogRefresh, context)) {
      return catalogRefresh.promise;
    }
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
        if (!requestIsCurrent(context)) return;
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
        if (!requestIsCurrent(context)) return;
        set({
          loaded: true,
          error: error instanceof Error ? error.message : String(error),
        });
        return;
      }
      if (!requestIsCurrent(context)) return;
      const extras: Promise<void>[] = [
        client
          .getHarnessDoctor()
          .then((doctor) => {
            if (requestIsCurrent(context)) set({ doctor });
          })
          .catch(() => {
            if (!requestIsCurrent(context)) return;
            // Home waits on a settled report. An empty one leaves the loading
            // empty and shows the install section instead of spinning forever.
            if (get().doctor === null) {
              set({ doctor: { harnesses: [] } });
            }
          }),
      ];
      for (const kind of HARNESS_KINDS) {
        extras.push(
          loadHarnessModels(client, kind, get, true, context).then(
            () => undefined,
          ),
        );
      }
      await Promise.all(extras);
    })().finally(() => {
      if (catalogRefresh?.promise === request) catalogRefresh = null;
    });
    catalogRefresh = { ...context, promise: request };
    return request;
  },
  refreshDoctor: async (client) => {
    const context = requestContext(client);
    if (!requestIsCurrent(context)) return;
    const doctor = await client.refreshHarnessDoctor();
    if (requestIsCurrent(context)) set({ doctor });
  },
  // The memoized read, for picking up an engine a download just put on disk.
  // `refreshDoctor` is the doctor's own button: it drops every memoized probe
  // and takes them all cold, which is far more than reading one result that
  // already exists.
  reloadDoctor: async (client) => {
    const context = requestContext(client);
    if (!requestIsCurrent(context)) return;
    const doctor = await client.getHarnessDoctor();
    if (requestIsCurrent(context)) set({ doctor });
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
    storeGeneration += 1;
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

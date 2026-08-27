import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeRepoSnapshot, CodeWorkspaceSnapshot } from "../api/types";
import {
  OPTIMISTIC_WORKSPACE_ID_PREFIX,
  useCodeCatalogStore,
} from "./CodeCatalogStore";
import { resetCodeClientGenerationForTests } from "./CodeClientGeneration";
import { activateCodeClient } from "./CodeClientScope";
import type { ReasoningEffort } from "../api/types";
import type { ParsedHarnessModel } from "./parsers";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function repo(id: string): CodeRepoSnapshot {
  return {
    id,
    root_path: `/tmp/${id}`,
    display_name: id,
    default_base_ref: "main",
    branch_prefix: "tidebreak",
    quick_actions: [],
    created_at: "2026-08-26T00:00:00.000Z",
  };
}

function workspace(
  id: string,
  createdAt: string,
  title = id,
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: "repo-1",
    title,
    worktree_path: `/tmp/app/.worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: createdAt,
  };
}

afterEach(() => {
  useCodeCatalogStore.getState().reset();
  resetCodeClientGenerationForTests();
});

describe("CodeCatalogStore.upsertWorkspace", () => {
  it("selecting a workspace does not reorder the catalog", () => {
    const first = workspace("ws-a", "2026-08-14T00:00:00.000Z");
    const second = workspace("ws-b", "2026-08-16T00:00:00.000Z");
    useCodeCatalogStore.setState({ workspaces: [first, second] });

    useCodeCatalogStore.getState().upsertWorkspace({
      ...second,
      title: "viewed",
    });

    expect(
      useCodeCatalogStore.getState().workspaces.map((item) => item.id),
    ).toEqual(["ws-a", "ws-b"]);
    expect(useCodeCatalogStore.getState().workspaces[1]?.title).toBe("viewed");
  });

  it("replaces a temporary workspace in place", () => {
    const pending = workspace(
      `${OPTIMISTIC_WORKSPACE_ID_PREFIX}one`,
      "2026-08-24T12:00:00.000Z",
    );
    const created = workspace("ws-created", pending.created_at, "Created");
    useCodeCatalogStore.setState({ workspaces: [pending] });

    useCodeCatalogStore.getState().replaceWorkspace(pending.id, created);

    expect(useCodeCatalogStore.getState().workspaces).toEqual([created]);
  });
});

describe("CodeCatalogStore.ensureHarnessModels", () => {
  it("ignores a model result from a replaced client generation", async () => {
    const staleModels = deferred<{
      kind: "codex";
      models: ParsedHarnessModel[];
      reasoning_efforts: ReasoningEffort[];
    }>();
    const first = {
      listCodeHarnessModels: vi.fn(() => staleModels.promise),
    };
    activateCodeClient(first);
    const stale = useCodeCatalogStore
      .getState()
      .ensureHarnessModels(first, "codex");

    const replacement = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "codex" as const,
        models: [
          {
            id: "gpt-new",
            label: "New model",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
      })),
    };
    activateCodeClient(replacement);
    await useCodeCatalogStore
      .getState()
      .ensureHarnessModels(replacement, "codex");

    staleModels.resolve({
      kind: "codex",
      models: [
        {
          id: "gpt-old",
          label: "Old model",
          default: true,
          reasoning_efforts: [],
          fast_mode: false,
        },
      ],
      reasoning_efforts: [],
    });
    await stale;

    expect(replacement.listCodeHarnessModels).toHaveBeenCalledOnce();
    expect(useCodeCatalogStore.getState().modelsByHarness.codex).toEqual([
      expect.objectContaining({ id: "gpt-new" }),
    ]);
  });

  it("caches an empty native model catalog", async () => {
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "codex" as const,
        models: [],
        reasoning_efforts: [],
        fast_mode: false,
      })),
    };

    await useCodeCatalogStore.getState().ensureHarnessModels(client, "codex");
    await useCodeCatalogStore.getState().ensureHarnessModels(client, "codex");

    expect(client.listCodeHarnessModels).toHaveBeenCalledOnce();
    expect(useCodeCatalogStore.getState().modelsByHarness.codex).toEqual([]);
  });

  it("shares one in-flight native model probe across pickers", async () => {
    let resolve!: (value: {
      kind: "opencode";
      models: ParsedHarnessModel[];
      reasoning_efforts: ReasoningEffort[];
    }) => void;
    const response = new Promise<Parameters<typeof resolve>[0]>((done) => {
      resolve = done;
    });
    const client = {
      listCodeHarnessModels: vi.fn(() => response),
    };

    const first = useCodeCatalogStore
      .getState()
      .ensureHarnessModels(client, "opencode");
    const second = useCodeCatalogStore
      .getState()
      .ensureHarnessModels(client, "opencode");
    resolve({
      kind: "opencode",
      models: [
        {
          id: "glm-5.2",
          label: "GLM 5.2",
          default: true,
          reasoning_efforts: [],
          fast_mode: false,
        },
      ],
      reasoning_efforts: [],
    });

    expect(await first).toEqual(await second);
    expect(client.listCodeHarnessModels).toHaveBeenCalledOnce();
    expect(useCodeCatalogStore.getState().modelsByHarness.opencode).toEqual([
      expect.objectContaining({ id: "glm-5.2", default: true }),
    ]);
  });

  it("preserves model-gateway provenance for hosted picker rows", async () => {
    const client = {
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "opencode" as const,
        source: "model_gateway" as const,
        models: [
          {
            id: "model-gateway/glm-5.3",
            label: "GLM 5.3",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
      })),
    };

    await useCodeCatalogStore
      .getState()
      .ensureHarnessModels(client, "opencode");

    expect(useCodeCatalogStore.getState().modelsByHarness.opencode).toEqual([
      expect.objectContaining({
        id: "model-gateway/glm-5.3",
        source: "opencode · model-gateway",
      }),
    ]);
  });

  it("shares the initial catalog refresh with an opening picker", async () => {
    let resolveCodex!: (value: { kind: "codex"; models: [] }) => void;
    const codex = new Promise<Parameters<typeof resolveCodex>[0]>((done) => {
      resolveCodex = done;
    });
    const client = {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn((kind: string) =>
        kind === "codex" ? codex : Promise.resolve({ kind, models: [] }),
      ),
    };

    const refresh = useCodeCatalogStore.getState().refresh(client as never);
    await Promise.resolve();
    await Promise.resolve();
    const picker = useCodeCatalogStore
      .getState()
      .ensureHarnessModels(client as never, "codex");
    resolveCodex({ kind: "codex", models: [] });
    await Promise.all([refresh, picker]);

    expect(client.listCodeHarnessModels).toHaveBeenCalledTimes(4);
    expect(
      client.listCodeHarnessModels.mock.calls.filter(
        ([kind]) => kind === "codex",
      ),
    ).toHaveLength(1);
  });

  it("reprobes cached models on an explicit catalog refresh", async () => {
    useCodeCatalogStore.setState({ modelsByHarness: { codex: [] } });
    const client = {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models:
          kind === "codex"
            ? [
                {
                  id: "gpt-5.6-luna",
                  label: "GPT-5.6 Luna",
                  default: true,
                  reasoning_efforts: [],
                  fast_mode: false,
                },
              ]
            : [],
      })),
    };

    await useCodeCatalogStore.getState().refresh(client as never);

    expect(client.listCodeHarnessModels).toHaveBeenCalledTimes(4);
    expect(useCodeCatalogStore.getState().modelsByHarness.codex).toEqual([
      expect.objectContaining({ id: "gpt-5.6-luna", default: true }),
    ]);
  });
});

describe("CodeCatalogStore.refresh", () => {
  it("does not let an old catalog populate or block a replacement", async () => {
    const staleRepos = deferred<CodeRepoSnapshot[]>();
    const first = {
      listCodeRepos: vi.fn(() => staleRepos.promise),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models: [],
      })),
    };
    activateCodeClient(first);
    const stale = useCodeCatalogStore.getState().refresh(first as never);

    const replacement = {
      listCodeRepos: vi.fn(async () => [repo("repo-new")]),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models: [],
      })),
    };
    activateCodeClient(replacement);
    const fresh = useCodeCatalogStore.getState().refresh(replacement as never);
    await fresh;

    expect(replacement.listCodeRepos).toHaveBeenCalledOnce();
    expect(useCodeCatalogStore.getState().repos).toEqual([repo("repo-new")]);

    staleRepos.resolve([repo("repo-old")]);
    await stale;

    expect(useCodeCatalogStore.getState().repos).toEqual([repo("repo-new")]);
  });

  it("shares one catalog request across the sidebar and route body", async () => {
    const client = {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models: [],
      })),
    };

    const first = useCodeCatalogStore.getState().refresh(client as never);
    const second = useCodeCatalogStore.getState().refresh(client as never);
    await Promise.all([first, second]);

    expect(client.listCodeRepos).toHaveBeenCalledOnce();
    expect(client.listCodeWorkspaces).toHaveBeenCalledOnce();
    expect(client.getHarnessDoctor).toHaveBeenCalledOnce();
    expect(client.listCodeHarnessModels).toHaveBeenCalledTimes(4);
  });

  it("keeps temporary workspaces while the server list catches up", async () => {
    const pending = workspace(
      `${OPTIMISTIC_WORKSPACE_ID_PREFIX}one`,
      "2026-08-24T12:00:00.000Z",
    );
    useCodeCatalogStore.setState({ workspaces: [pending] });
    const client = {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models: [],
      })),
    };

    await useCodeCatalogStore.getState().refresh(client as never);

    expect(useCodeCatalogStore.getState().workspaces).toEqual([pending]);
  });

  it("keeps a completed local create when an older refresh finishes", async () => {
    const pending = workspace(
      `${OPTIMISTIC_WORKSPACE_ID_PREFIX}one`,
      "2026-08-24T12:00:00.000Z",
    );
    const created = workspace("ws-created", pending.created_at, "Created");
    useCodeCatalogStore.setState({ workspaces: [pending] });
    let resolveWorkspaces!: (workspaces: CodeWorkspaceSnapshot[]) => void;
    const listed = new Promise<CodeWorkspaceSnapshot[]>((resolve) => {
      resolveWorkspaces = resolve;
    });
    const client = {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(() => listed),
      getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
      listCodeHarnessModels: vi.fn(async (kind: string) => ({
        kind,
        models: [],
      })),
    };

    const refresh = useCodeCatalogStore.getState().refresh(client as never);
    useCodeCatalogStore.getState().replaceWorkspace(pending.id, created);
    resolveWorkspaces([]);
    await refresh;

    expect(useCodeCatalogStore.getState().workspaces).toEqual([created]);
  });
});

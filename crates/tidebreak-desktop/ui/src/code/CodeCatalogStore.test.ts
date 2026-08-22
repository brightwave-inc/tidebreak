import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeWorkspaceSnapshot } from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import type { ReasoningEffort } from "../api/types";
import type { ParsedHarnessModel } from "./parsers";

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
});

describe("CodeCatalogStore.ensureHarnessModels", () => {
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
});

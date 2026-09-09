// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const COLLAPSED_KEY = "tidebreak.code-workspace-collapsed-groups";
const PREFS_KEY = "tidebreak.code-rail-prefs";

beforeEach(() => {
  window.localStorage.clear();
  vi.resetModules();
});

afterEach(() => {
  vi.restoreAllMocks();
  window.localStorage.clear();
});

describe("workspace rail preferences", () => {
  it("restores source and subgroup collapse choices independently", async () => {
    window.localStorage.setItem(
      COLLAPSED_KEY,
      JSON.stringify(["source:slack", "slack:by-repo:app"]),
    );
    const { useCodeUiStore } = await import("./CodeUiStore");
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "source:slack",
      "slack:by-repo:app",
    ]);

    useCodeUiStore.getState().toggleWorkspaceGroup("source:slack");
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "slack:by-repo:app",
    ]);
    expect(JSON.parse(window.localStorage.getItem(COLLAPSED_KEY)!)).toEqual([
      "slack:by-repo:app",
    ]);

    useCodeUiStore.getState().toggleWorkspaceGroup("source:slack");
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "slack:by-repo:app",
      "source:slack",
    ]);
  });

  it("persists a copied, distinct set of collapsed keys", async () => {
    const { useCodeUiStore } = await import("./CodeUiStore");
    const keys = ["source:local", "source:local", "slack:by-status:idle"];
    useCodeUiStore.getState().setCollapsedWorkspaceGroups(keys);
    keys.push("source:slack");
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "source:local",
      "slack:by-status:idle",
    ]);
    expect(JSON.parse(window.localStorage.getItem(COLLAPSED_KEY)!)).toEqual([
      "source:local",
      "slack:by-status:idle",
    ]);
    useCodeUiStore.getState().setCollapsedWorkspaceGroups([]);
    expect(window.localStorage.getItem(COLLAPSED_KEY)).toBe("[]");
  });

  it.each(["{broken", "null", '"source:slack"', '["source:slack", 1]'])(
    "ignores invalid collapse storage %s",
    async (raw) => {
      window.localStorage.setItem(COLLAPSED_KEY, raw);
      const { useCodeUiStore } = await import("./CodeUiStore");
      expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([]);
    },
  );

  it("keeps collapse controls working when persistence fails", async () => {
    const { useCodeUiStore } = await import("./CodeUiStore");
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("Storage unavailable");
    });
    expect(() =>
      useCodeUiStore.getState().toggleWorkspaceGroup("source:slack"),
    ).not.toThrow();
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "source:slack",
    ]);
  });

  it("uses repository grouping for an old Created preference and preserves card choices", async () => {
    window.localStorage.setItem(
      PREFS_KEY,
      JSON.stringify({
        sortMode: "by-created",
        density: "compact",
        showRepoChip: false,
        showBranch: true,
      }),
    );
    const { useCodeUiStore } = await import("./CodeUiStore");
    expect(useCodeUiStore.getState().railPrefs).toEqual({
      sortMode: "by-repo",
      density: "compact",
      showRepoChip: false,
      showBranch: true,
    });
  });

  it("falls back from the legacy Created sort key", async () => {
    window.localStorage.setItem("tidebreak.code-workspace-sort", "by-created");
    const { useCodeUiStore } = await import("./CodeUiStore");
    expect(useCodeUiStore.getState().railPrefs.sortMode).toBe("by-repo");
  });

  it("clears selection on host change and keeps rail preferences", async () => {
    const { resetCodeUiHostState, useCodeUiStore } = await import(
      "./CodeUiStore"
    );
    useCodeUiStore.getState().setCollapsedWorkspaceGroups(["source:slack"]);
    useCodeUiStore.getState().setRailPrefs({ density: "compact" });
    useCodeUiStore.getState().replaceWorkspaceSelection(["ws-a"], "ws-a");
    resetCodeUiHostState();
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([]);
    expect(useCodeUiStore.getState().selectionAnchorId).toBeNull();
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([
      "source:slack",
    ]);
    expect(useCodeUiStore.getState().railPrefs.density).toBe("compact");
  });
});

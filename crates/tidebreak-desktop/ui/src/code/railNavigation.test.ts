import { afterEach, describe, expect, it } from "vitest";

import type { CodeSessionSnapshot, CodeWorkspaceSnapshot } from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "./CodeUiStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

import {
  nextWorkspaceAfterLeaving,
  railWorkspaceIds,
  stepRailWorkspace,
  stepWorkspaceId,
} from "./railNavigation";

describe("stepWorkspaceId", () => {
  const rail = ["a", "b", "c"];

  it("cycles the rail in both directions", () => {
    // A rail is a ring: stopping at the last card would make the chord feel
    // broken exactly when the reader is furthest from the top.
    expect(stepWorkspaceId(rail, "a", 1)).toBe("b");
    expect(stepWorkspaceId(rail, "c", 1)).toBe("a");
    expect(stepWorkspaceId(rail, "a", -1)).toBe("c");
    expect(stepWorkspaceId(rail, "b", -1)).toBe("a");
  });

  it("enters the rail at the end it is walking towards", () => {
    // From the code home there is no current workspace, and
    // doing nothing would leave the reader with no keyboard way onto the rail.
    expect(stepWorkspaceId(rail, undefined, 1)).toBe("a");
    expect(stepWorkspaceId(rail, undefined, -1)).toBe("c");
    // A workspace the rail no longer draws — archived while it was open — is
    // the same situation: there is no position to step from.
    expect(stepWorkspaceId(rail, "gone", 1)).toBe("a");
  });

  it("has nowhere to go on an empty rail", () => {
    expect(stepWorkspaceId([], undefined, 1)).toBeNull();
    expect(stepWorkspaceId([], "a", -1)).toBeNull();
  });
});

describe("nextWorkspaceAfterLeaving", () => {
  it("opens the next live card, wrapping at the end of the rail", () => {
    expect(nextWorkspaceAfterLeaving(["a", "b", "c"], "a")).toBe("b");
    expect(nextWorkspaceAfterLeaving(["a", "b", "c"], "c")).toBe("a");
  });

  it("falls through to the code home when nothing else is live", () => {
    expect(nextWorkspaceAfterLeaving(["a"], "a")).toBeNull();
    expect(nextWorkspaceAfterLeaving([], "a")).toBeNull();
  });

  it("skips every id that left in the same pass", () => {
    const left = new Set(["b", "c"]);
    expect(nextWorkspaceAfterLeaving(["a", "b", "c", "d"], "b", left)).toBe(
      "d",
    );
    expect(nextWorkspaceAfterLeaving(["a", "b", "c"], "c", left)).toBe("a");
    expect(
      nextWorkspaceAfterLeaving(["a", "b", "c"], "b", new Set(["a", "b", "c"])),
    ).toBeNull();
  });
});

function workspace(id: string, repoId: string): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: repoId,
    title: id,
    worktree_path: `/tmp/${id}`,
    branch_name: id,
    base_ref: "main",
    status: "active",
    created_at: "2026-08-15T00:00:00.000Z",
  };
}

function slackSession(workspaceId: string): CodeSessionSnapshot {
  return {
    id: `session-${workspaceId}`,
    workspace_id: workspaceId,
    kind: "interactive",
    harness_kind: "codex",
    permission_mode: "ask",
    fast_mode: false,
    unrecognized_event_count: 0,
    visibility: "private",
    execution_location: "machine",
    lifecycle: "idle",
    attention: { state: { type: "idle" }, source: "lifecycle" },
    created_at: "2026-08-15T00:00:00.000Z",
    external_origin: {
      channel_kind: "slack",
      external_key: "T123:C123:1",
    },
  };
}

describe("railWorkspaceIds", () => {
  afterEach(() => {
    useCodeCatalogStore.getState().reset();
    useCodeUpdatesStore.getState().reset();
    useCodeUiStore.setState({
      railPrefs: DEFAULT_RAIL_PREFS,
      collapsedWorkspaceGroups: [],
    });
  });

  it("navigates only expanded sources and subgroups", () => {
    useCodeCatalogStore.setState({
      repos: ["app", "lib"].map((id) => ({
        id,
        root_path: `/tmp/${id}`,
        display_name: id,
        default_base_ref: "main",
        branch_prefix: "tidebreak",
        quick_actions: [],
        created_at: "2026-08-15T00:00:00.000Z",
      })),
      workspaces: [
        workspace("local-app", "app"),
        workspace("local-lib", "lib"),
        workspace("slack-app", "app"),
      ],
      sessionsByWorkspace: { "slack-app": slackSession("slack-app") },
    });
    useCodeUiStore.setState({
      railPrefs: { ...DEFAULT_RAIL_PREFS, sortMode: "by-repo" },
      collapsedWorkspaceGroups: [],
    });
    expect(railWorkspaceIds()).toEqual(["local-app", "local-lib", "slack-app"]);

    useCodeUiStore.getState().toggleWorkspaceGroup("local:by-repo:app");
    expect(railWorkspaceIds()).toEqual(["local-lib", "slack-app"]);
    expect(stepRailWorkspace("local-app", 1)).toBe("local-lib");
    useCodeUiStore.getState().toggleWorkspaceGroup("source:slack");
    expect(railWorkspaceIds()).toEqual(["local-lib"]);
    expect(stepRailWorkspace("local-lib", 1)).toBe("local-lib");
    useCodeUiStore.getState().toggleWorkspaceGroup("source:local");
    expect(stepRailWorkspace("local-lib", 1)).toBeNull();

    useCodeUiStore.getState().toggleWorkspaceGroup("source:local");
    expect(railWorkspaceIds()).toEqual(["local-lib"]);
  });

  it("ignores a collapsed source when only that source remains", () => {
    useCodeCatalogStore.setState({
      workspaces: [workspace("slack-app", "app")],
      sessionsByWorkspace: { "slack-app": slackSession("slack-app") },
    });
    useCodeUiStore.setState({
      railPrefs: { ...DEFAULT_RAIL_PREFS, sortMode: "by-status" },
      collapsedWorkspaceGroups: ["source:slack", "slack:by-repo:app"],
    });
    expect(railWorkspaceIds()).toEqual(["slack-app"]);
    useCodeUiStore.getState().toggleWorkspaceGroup("slack:by-status:idle");
    expect(railWorkspaceIds()).toEqual([]);
  });
});

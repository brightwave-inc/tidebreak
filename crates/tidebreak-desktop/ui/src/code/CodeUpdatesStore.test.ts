import { afterEach, describe, expect, it, vi } from "vitest";

import type { Attention, CodeSessionDigest } from "../api/types";
import {
  noticeToAction,
  reduceCodeUpdates,
  shouldRequestOsAttention,
  useCodeUpdatesStore,
} from "./CodeUpdatesStore";

const working: Attention = { state: { type: "working" }, source: "lifecycle" };
const need: Attention = {
  state: { type: "needs_you", prompt: "an approval is waiting", source: "structured" },
  source: "structured",
};

function digest(overrides: Partial<CodeSessionDigest> = {}): CodeSessionDigest {
  return {
    workspace: "ws-1",
    session: "sess-1",
    lifecycle: "idle",
    attention: working,
    title: "first change",
    turn_count: 0,
    ...overrides,
  };
}

afterEach(() => {
  useCodeUpdatesStore.getState().reset();
  vi.restoreAllMocks();
});

describe("reduceCodeUpdates", () => {
  it("replaces the map on snapshot and upserts a digest", () => {
    const empty = { byWorkspace: {}, viewedWorkspaceId: null };
    const afterSnapshot = reduceCodeUpdates(empty, {
      type: "snapshot",
      sessions: [digest(), digest({ workspace: "ws-2", session: "sess-2", title: "other" })],
    });
    expect(Object.keys(afterSnapshot.byWorkspace)).toEqual(["ws-1", "ws-2"]);
    const afterDigest = reduceCodeUpdates(afterSnapshot, {
      type: "digest",
      digest: digest({ turn_count: 2, attention: need }),
    });
    expect(afterDigest.byWorkspace["ws-1"].turn_count).toBe(2);
    expect(afterDigest.byWorkspace["ws-1"].attention).toEqual(need);
    expect(afterDigest.byWorkspace["ws-2"].title).toBe("other");
    const restated = reduceCodeUpdates(afterDigest, {
      type: "snapshot",
      sessions: [digest({ workspace: "ws-3", session: "sess-3" })],
    });
    expect(Object.keys(restated.byWorkspace)).toEqual(["ws-3"]);
  });

  it("maps notices onto reducer actions", () => {
    expect(
      noticeToAction({
        type: "snapshot",
        sessions: [digest()],
      }),
    ).toEqual({ type: "snapshot", sessions: [digest()] });
    expect(
      noticeToAction({
        type: "digest",
        workspace: "ws-1",
        session: "sess-1",
        lifecycle: "running",
        attention: working,
        title: "first change",
        turn_count: 1,
      }),
    ).toEqual({
      type: "digest",
      digest: {
        workspace: "ws-1",
        session: "sess-1",
        lifecycle: "running",
        attention: working,
        title: "first change",
        turn_count: 1,
      },
    });
    expect(
      noticeToAction({
        type: "terminal_activity",
        workspace_id: "ws-1",
        terminal_id: "term-1",
      }),
    ).toBeNull();
  });
});

describe("shouldRequestOsAttention", () => {
  it("fires only on a transition into structured NeedsYou for a workspace that is not being viewed", () => {
    expect(shouldRequestOsAttention(working, need, "ws-1", null)).toBe(true);
    expect(shouldRequestOsAttention(undefined, need, "ws-1", null)).toBe(true);
    expect(shouldRequestOsAttention(need, need, "ws-1", null)).toBe(false);
    expect(shouldRequestOsAttention(working, need, "ws-1", "ws-1")).toBe(false);
    expect(shouldRequestOsAttention(working, need, "ws-1", "ws-other")).toBe(true);
    expect(
      shouldRequestOsAttention(
        working,
        { state: { type: "done_unreviewed" }, source: "lifecycle" },
        "ws-1",
        null,
      ),
    ).toBe(false);
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";

import type { Attention, CodeSessionDigest } from "../api/types";
import {
  noticeToAction,
  reduceCodeUpdates,
  shouldRequestOsAttention,
  useCodeUpdatesStore,
  watchChildren,
  type CodeUpdatesState,
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
    kind: "interactive",
    lifecycle: "idle",
    attention: working,
    title: "first change",
    turn_count: 0,
    ...overrides,
  };
}

const EMPTY_STATE: CodeUpdatesState = {
  byWorkspace: {},
  childrenByWorkspace: {},
  cloneJobs: {},
  harnessInstalls: {},
  viewedWorkspaceId: null,
};

afterEach(() => {
  useCodeUpdatesStore.getState().reset();
  vi.restoreAllMocks();
});

describe("reduceCodeUpdates", () => {
  it("replaces the map on snapshot and upserts a digest", () => {
    const afterSnapshot = reduceCodeUpdates(EMPTY_STATE, {
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

  it("keeps watch digests beside the conversation, never in its slot", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest(),
        digest({ session: "sess-watch", kind: "watch", lifecycle: "running" }),
      ],
    });
    // ADR 0050: the interactive digest keeps the workspace slot.
    expect(seeded.byWorkspace["ws-1"].session).toBe("sess-1");
    expect(watchChildren(seeded, "ws-1").map((child) => child.session)).toEqual([
      "sess-watch",
    ]);

    const afterWatchDigest = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({
        session: "sess-watch",
        kind: "watch",
        lifecycle: "running",
        turn_count: 5,
      }),
    });
    expect(afterWatchDigest.byWorkspace["ws-1"].session).toBe("sess-1");
    expect(afterWatchDigest.byWorkspace["ws-1"].turn_count).toBe(0);
    expect(watchChildren(afterWatchDigest, "ws-1")[0]?.turn_count).toBe(5);
  });

  it("drops an ended watch child and rebuilds children on snapshot", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest(),
        digest({ session: "sess-watch", kind: "watch", lifecycle: "running" }),
      ],
    });
    const ended = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({ session: "sess-watch", kind: "watch", lifecycle: "ended" }),
    });
    expect(watchChildren(ended, "ws-1")).toEqual([]);

    // A reconnect snapshot that no longer lists the watch heals a missed end.
    const healed = reduceCodeUpdates(seeded, {
      type: "snapshot",
      sessions: [digest()],
    });
    expect(watchChildren(healed, "ws-1")).toEqual([]);
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
        kind: "interactive",
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
        kind: "interactive",
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
    expect(
      noticeToAction({
        type: "clone_progress",
        job: "job-1",
        phase: "receiving objects",
        percent: 40,
        done: false,
      }),
    ).toEqual({
      type: "clone_progress",
      job: {
        id: "job-1",
        phase: "receiving objects",
        percent: 40,
        done: false,
      },
    });
    expect(
      noticeToAction({
        type: "harness_install",
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      }),
    ).toEqual({
      type: "harness_install",
      install: {
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      },
    });
  });

  it("keeps one install state per engine", () => {
    const installing = reduceCodeUpdates(EMPTY_STATE, {
      type: "harness_install",
      install: { kind: "codex", phase: "installing", done: false },
    });
    expect(installing.harnessInstalls.codex?.phase).toBe("installing");
    const ready = reduceCodeUpdates(installing, {
      type: "harness_install",
      install: { kind: "codex", phase: "ready", done: true },
    });
    expect(ready.harnessInstalls.codex).toEqual({
      kind: "codex",
      phase: "ready",
      done: true,
    });
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

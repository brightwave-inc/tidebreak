import { describe, expect, it } from "vitest";
import type {
  Attention,
  SessionDigest as CodeSessionDigest,
} from "../generated/wire";
import {
  EMPTY_UPDATES,
  attentionBadgeLabel,
  attentionSessionCount,
  sessionRecoveryPresentation,
  listedSessions,
  noticeToAction,
  reduceUpdates,
  sessionNeedsAttention,
} from "./updates";

const working: Attention = { state: { type: "working" }, source: "lifecycle" };
const need: Attention = {
  state: {
    type: "needs_you",
    prompt: "an approval is waiting",
    source: "structured",
  },
  source: "structured",
};
const done: Attention = {
  state: { type: "done_unreviewed" },
  source: "lifecycle",
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

describe("reduceUpdates", () => {
  it("replaces the list from a snapshot", () => {
    const first = digest();
    const second = digest({
      session: "sess-2",
      title: "later",
      turn_count: 3,
      attention: need,
      lifecycle: "running",
    });
    const state = reduceUpdates(EMPTY_UPDATES, {
      type: "snapshot",
      sessions: [first, second],
    });
    expect(listedSessions(state).map((row) => row.session)).toEqual([
      "sess-2",
      "sess-1",
    ]);
  });

  it("upserts a digest and sorts needs-you first", () => {
    const idle = digest({ session: "a", turn_count: 4 });
    let state = reduceUpdates(EMPTY_UPDATES, {
      type: "snapshot",
      sessions: [idle],
    });
    state = reduceUpdates(state, {
      type: "digest",
      digest: digest({
        session: "b",
        attention: need,
        lifecycle: "running",
        turn_count: 1,
      }),
    });
    expect(listedSessions(state).map((row) => row.session)).toEqual(["b", "a"]);
  });

  it("marks the list as loaded only after a snapshot", () => {
    expect(EMPTY_UPDATES.snapshotReceived).toBe(false);
    const digested = reduceUpdates(EMPTY_UPDATES, {
      type: "digest",
      digest: digest(),
    });
    expect(digested.snapshotReceived).toBe(false);
    const snapshotted = reduceUpdates(digested, {
      type: "snapshot",
      sessions: [],
    });
    expect(snapshotted.snapshotReceived).toBe(true);
    expect(reduceUpdates(snapshotted, { type: "reset" })).toEqual(
      EMPTY_UPDATES,
    );
  });

  it("maps snapshot and digest notices, ignoring delivery", () => {
    expect(
      noticeToAction({ type: "snapshot", sessions: [digest()] })?.type,
    ).toBe("snapshot");
    expect(
      noticeToAction({
        type: "digest",
        workspace: "ws-1",
        session: "sess-1",
        kind: "interactive",
        lifecycle: "idle",
        attention: working,
        title: "first change",
        turn_count: 0,
      })?.type,
    ).toBe("digest");
    expect(noticeToAction({ type: "delivery" })).toBeNull();
  });
});

describe("attentionBadgeLabel", () => {
  it("shows needs-you, done, and stalled; hides recovery, working, and idle", () => {
    expect(attentionBadgeLabel(need)).toBe("an approval is waiting");
    expect(attentionBadgeLabel(done)).toBe("Done");
    expect(
      attentionBadgeLabel({
        state: { type: "stalled", idle_secs: 12 },
        source: "lifecycle",
      }),
    ).toBe("Stalled");
    expect(attentionBadgeLabel({ state: { type: "fenced", reason: { type: "orphan_alive" } }, source: "lifecycle" })).toBeNull();
    expect(attentionBadgeLabel(working)).toBeNull();
    expect(
      attentionBadgeLabel({ state: { type: "idle" }, source: "lifecycle" }),
    ).toBeNull();
  });
});

it("shows a recovery blocker without losing the manual pin", () => {
  const row = { ...digest(), lifecycle: "fenced" as const, attention: { state: { type: "manual" as const, note: "Review today" }, source: "user" as const }, fence_reason: { type: "probe_ambiguous" as const, detail: "Automatic recovery failed" } };
  expect(sessionRecoveryPresentation(row)).toEqual({ recovering: false, prompt: "Automatic recovery failed" });
  expect(row.attention.state.type).toBe("manual");
  expect(noticeToAction({ type: "digest", ...row })).toMatchObject({ digest: { fence_reason: row.fence_reason } });
});

describe("attentionSessionCount", () => {
  it("counts sessions blocked on a human, not done or ended ones", () => {
    const needsYou = digest({ session: "a", attention: need });
    const stalled = digest({
      session: "b",
      attention: {
        state: { type: "stalled", idle_secs: 30 },
        source: "lifecycle",
      },
    });
    const quiet = digest({ session: "c", attention: working });
    const doneIdle = digest({ session: "d", attention: done });
    const endedDone = digest({
      session: "e",
      attention: done,
      lifecycle: "ended",
    });
    expect(sessionNeedsAttention(needsYou)).toBe(true);
    expect(sessionNeedsAttention(stalled)).toBe(true);
    expect(sessionNeedsAttention(quiet)).toBe(false);
    // Done-but-unreviewed is reviewable whenever; it must not pin the
    // hub's "Needs you" count above zero for days.
    expect(sessionNeedsAttention(doneIdle)).toBe(false);
    expect(sessionNeedsAttention(endedDone)).toBe(false);
    expect(
      attentionSessionCount([needsYou, stalled, quiet, doneIdle, endedDone]),
    ).toBe(2);
  });
});

import { describe, expect, it } from "vitest";
import type { Attention, CodeSessionDigest } from "../generated/wire";
import {
  EMPTY_UPDATES,
  attentionBadgeLabel,
  listedSessions,
  noticeToAction,
  reduceUpdates,
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
  it("shows needs-you, done, stalled, and fenced; hides working and idle", () => {
    expect(attentionBadgeLabel(need)).toBe("an approval is waiting");
    expect(attentionBadgeLabel(done)).toBe("Done");
    expect(
      attentionBadgeLabel({
        state: { type: "stalled", idle_secs: 12 },
        source: "lifecycle",
      }),
    ).toBe("Stalled");
    expect(attentionBadgeLabel(working)).toBeNull();
    expect(
      attentionBadgeLabel({ state: { type: "idle" }, source: "lifecycle" }),
    ).toBeNull();
  });
});

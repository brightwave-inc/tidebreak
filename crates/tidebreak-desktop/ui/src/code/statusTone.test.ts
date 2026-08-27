import { describe, expect, it } from "vitest";

import type { Attention, CodeSessionDigest } from "../api/types";
import {
  attentionMarkForDigest,
  attentionStatusTone,
  digestStatusTone,
  STATUS_MOTION,
} from "./statusTone";

function digest(
  attention: Attention["state"],
  lifecycle: CodeSessionDigest["lifecycle"],
  turnCount = 0,
): CodeSessionDigest {
  return {
    attention: { state: attention, source: "lifecycle" },
    lifecycle,
    turn_count: turnCount,
  } as CodeSessionDigest;
}

describe("attentionStatusTone", () => {
  // Working used to have no tone at all, which is what made a busy agent and
  // an idle one look the same. The tone it gets has to be one that moves.
  it("gives Working a tone that carries motion", () => {
    const tone = attentionStatusTone({
      state: { type: "working" },
      source: "lifecycle",
    });
    expect(tone).toBe("running");
    expect(STATUS_MOTION[tone]).toBeTruthy();
  });

  it("separates the two warnings from the one critical", () => {
    const tone = (state: Attention["state"]) =>
      attentionStatusTone({ state, source: "lifecycle" });
    expect(tone({ type: "needs_you", prompt: "", source: "structured" })).toBe(
      "critical",
    );
    expect(tone({ type: "stalled", idle_secs: 90 })).toBe("warning");
    expect(tone({ type: "fenced", reason: { type: "orphan_alive" } })).toBe(
      "warning",
    );
    expect(tone({ type: "done_unreviewed" })).toBe("neutral");
  });
});

describe("digestStatusTone", () => {
  it("lets attention outrank a running lifecycle", () => {
    // A session stays `running` while it waits on you, and the waiting is the
    // part worth coloring. Pulsing here would contradict the label beside it.
    expect(
      digestStatusTone(
        digest(
          { type: "needs_you", prompt: "", source: "structured" },
          "running",
        ),
      ),
    ).toBe("critical");
    expect(
      digestStatusTone(digest({ type: "stalled", idle_secs: 90 }, "running")),
    ).toBe("warning");
  });

  it("runs only when working and running agree", () => {
    expect(digestStatusTone(digest({ type: "working" }, "running"))).toBe(
      "running",
    );
    expect(digestStatusTone(digest({ type: "working" }, "ended"))).toBe(
      "neutral",
    );
    expect(digestStatusTone(undefined)).toBe("neutral");
  });
});

describe("attentionMarkForDigest", () => {
  it("shows motion only while lifecycle also says the session is running", () => {
    expect(
      attentionMarkForDigest(digest({ type: "working" }, "running")),
    ).toEqual({ state: { type: "working" }, source: "lifecycle" });
    expect(
      attentionMarkForDigest(digest({ type: "working" }, "idle")),
    ).toBeUndefined();
    expect(
      attentionMarkForDigest(digest({ type: "working" }, "ended")),
    ).toBeUndefined();
  });

  it("keeps attention that outranks lifecycle and leaves a never-started idle unmarked", () => {
    const needsYou = digest(
      { type: "needs_you", prompt: "Approve this?", source: "structured" },
      "idle",
    );
    expect(attentionMarkForDigest(needsYou)).toEqual(needsYou.attention);
    expect(
      attentionMarkForDigest(digest({ type: "idle" }, "idle")),
    ).toBeUndefined();
  });

  it("marks a parked turn as Done so the card still reads as an agent", () => {
    const done = {
      state: { type: "done_unreviewed" as const },
      source: "lifecycle" as const,
    };
    expect(
      attentionMarkForDigest(digest({ type: "working" }, "idle", 1)),
    ).toEqual(done);
    expect(attentionMarkForDigest(digest({ type: "idle" }, "idle", 3))).toEqual(
      done,
    );
  });
});

import { expect, it } from "vitest";
import type { CodeSessionDigest, CodeSessionSnapshot } from "../api/types";
import { sessionRecoveryState } from "./sessionRecovery";

const snapshot = {
  lifecycle: "fenced",
  fence_reason: { type: "orphan_alive" },
  attention: {
    state: { type: "fenced", reason: { type: "orphan_alive" } },
    source: "lifecycle",
  },
} as CodeSessionSnapshot;

it("unblocks the composer when a live digest recovers a stale snapshot", () => {
  const digest = {
    lifecycle: "idle",
    attention: { state: { type: "idle" }, source: "lifecycle" },
  } as CodeSessionDigest;
  expect(sessionRecoveryState(snapshot, digest)).toEqual({
    lifecycle: "idle",
    attention: digest.attention,
    blocksTurn: false,
    reason: undefined,
  });
});

it("shows the live failure prompt while the snapshot still says recovery is pending", () => {
  const digest = {
    lifecycle: "fenced",
    attention: {
      state: {
        type: "needs_you",
        prompt: "Check the engine credentials before retrying.",
        source: "lifecycle",
      },
      source: "lifecycle",
    },
  } as CodeSessionDigest;
  expect(sessionRecoveryState(snapshot, digest)).toEqual({
    lifecycle: "fenced",
    attention: digest.attention,
    blocksTurn: true,
    reason: undefined,
  });
});

it("takes the live reason when recovery fails on a pinned session", () => {
  const digest = {
    lifecycle: "fenced",
    fence_reason: {
      type: "probe_ambiguous",
      detail: "Automatic recovery failed. Inspect the previous process.",
    },
    attention: {
      state: { type: "manual", note: "Review today" },
      source: "user",
    },
  } as CodeSessionDigest;
  const result = sessionRecoveryState(snapshot, digest);
  expect(result.reason).toEqual(digest.fence_reason);
  expect(result.attention?.state).toMatchObject({
    type: "needs_you",
    prompt: expect.stringContaining("Inspect the previous process"),
  });
  expect(digest.attention.state.type).toBe("manual");
});

it("offers manual recovery for older servers without recovery digest fields", () => {
  const digest = {
    lifecycle: "fenced",
    attention: snapshot.attention,
  } as CodeSessionDigest;
  const result = sessionRecoveryState(snapshot, digest);
  expect(result.attention?.state).toMatchObject({
    type: "needs_you",
    prompt: expect.stringContaining("Retry recovery"),
  });
  expect(result.reason?.type).toBe("orphan_alive");
});

it("shows a safe manual fallback for an older server's pinned recovery", () => {
  const digest = {
    lifecycle: "fenced",
    attention: {
      state: { type: "manual", note: "Review today" },
      source: "user",
    },
  } as CodeSessionDigest;
  const result = sessionRecoveryState(snapshot, digest);
  expect(result.attention?.state).toMatchObject({ type: "needs_you" });
  expect(result.reason).toBeUndefined();
});

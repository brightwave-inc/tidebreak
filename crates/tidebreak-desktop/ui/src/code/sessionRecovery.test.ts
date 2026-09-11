import { expect, it } from "vitest";
import type { CodeSessionDigest, CodeSessionSnapshot } from "../api/types";
import { sessionRecoveryState } from "./sessionRecovery";

const snapshot = {
  id: "selected",
  lifecycle: "fenced",
  fence_reason: { type: "orphan_alive" },
  attention: {
    state: { type: "fenced", reason: { type: "orphan_alive" } },
    source: "lifecycle",
  },
} as CodeSessionSnapshot;

it("unblocks the composer when a live digest recovers a stale snapshot", () => {
  const digest = {
    session: snapshot.id,
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
    session: snapshot.id,
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
    session: snapshot.id,
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
    session: snapshot.id,
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
    session: snapshot.id,
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

it("ignores a fenced sibling digest when the selected session is healthy", () => {
  const selected = {
    ...snapshot,
    lifecycle: "idle" as const,
    fence_reason: undefined,
    attention: {
      state: { type: "idle" as const },
      source: "lifecycle" as const,
    },
  };
  const sibling = {
    session: "sibling",
    lifecycle: "fenced",
    fence_reason: {
      type: "probe_ambiguous",
      detail: "Sibling recovery stopped",
    },
    attention: {
      state: {
        type: "needs_you",
        prompt: "Retry the sibling session.",
        source: "lifecycle",
      },
      source: "lifecycle",
    },
  } as CodeSessionDigest;
  expect(sessionRecoveryState(selected, sibling)).toEqual({
    lifecycle: "idle",
    attention: selected.attention,
    reason: undefined,
    blocksTurn: false,
  });
});

it("ignores a healthy sibling digest when the selected session is fenced", () => {
  const sibling = {
    session: "sibling",
    lifecycle: "idle",
    attention: { state: { type: "idle" }, source: "lifecycle" },
  } as CodeSessionDigest;
  expect(sessionRecoveryState(snapshot, sibling)).toEqual({
    lifecycle: "fenced",
    attention: snapshot.attention,
    reason: snapshot.fence_reason,
    blocksTurn: true,
  });
});

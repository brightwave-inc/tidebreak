import { describe, expect, it, vi } from "vitest";
import {
  ActiveTurnSteerFence,
  canBeginActiveTurnSteer,
  MAX_STEER_CHARACTERS,
  shouldClearAcceptedSteerDraft,
} from "./ActiveTurnSteer";

const target = { chatId: "chat-1", turnId: "turn-1", selection: 4 };

describe("ActiveTurnSteerFence", () => {
  it("rejects steer admission synchronously once cancellation starts", () => {
    expect(
      canBeginActiveTurnSteer({
        busy: true,
        turnId: "turn-1",
        cancelRequestTurnId: "turn-1",
        deletionInFlight: false,
      }),
    ).toBe(false);
    expect(
      canBeginActiveTurnSteer({
        busy: true,
        turnId: "turn-1",
        cancelRequestTurnId: null,
        deletionInFlight: false,
      }),
    ).toBe(true);
  });

  it("creates one stable identity and fences a duplicate submission", () => {
    const createId = vi.fn(() => "steer-1");
    const fence = new ActiveTurnSteerFence();

    const request = fence.begin(target, "  change course  ", createId);

    expect(request).toEqual({
      ...target,
      content: "change course",
      draftSnapshot: "  change course  ",
      steerId: "steer-1",
    });
    expect(fence.begin(target, "another direction", createId)).toBeNull();
    expect(createId).toHaveBeenCalledTimes(1);
  });

  it("reuses the exact identity when unchanged guidance is retried", () => {
    const createId = vi.fn(() => "steer-1");
    const fence = new ActiveTurnSteerFence();
    const first = fence.begin(target, "change course", createId)!;
    fence.fail(first);

    const retry = fence.begin(target, "  change course  ", createId)!;

    expect(retry.steerId).toBe(first.steerId);
    expect(retry.content).toBe(first.content);
    expect(retry.draftSnapshot).toBe("  change course  ");
    expect(createId).toHaveBeenCalledTimes(1);
  });

  it("allocates a new identity when failed guidance changes", () => {
    const createId = vi
      .fn<() => string>()
      .mockReturnValueOnce("steer-1")
      .mockReturnValueOnce("steer-2");
    const fence = new ActiveTurnSteerFence();
    const first = fence.begin(target, "change course", createId)!;
    fence.fail(first);

    expect(fence.begin(target, "different course", createId)?.steerId).toBe(
      "steer-2",
    );
  });

  it("accepts a response only for the exact selected chat and active turn", () => {
    const fence = new ActiveTurnSteerFence();
    const request = fence.begin(target, "change course", () => "steer-1")!;

    expect(fence.canApplyResponse(request, target)).toBe(true);
    expect(
      fence.canApplyResponse(request, { ...target, chatId: "chat-2" }),
    ).toBe(false);
    expect(
      fence.canApplyResponse(request, { ...target, turnId: "turn-2" }),
    ).toBe(false);
    expect(
      fence.canApplyResponse(request, { ...target, selection: 5 }),
    ).toBe(false);

  });

  it.each(["terminal event", "chat switch", "unmount"])(
    "rejects a response after %s invalidates the request",
    () => {
      const fence = new ActiveTurnSteerFence();
      const request = fence.begin(target, "change course", () => "steer-1")!;

      fence.invalidate();

      expect(fence.canApplyResponse(request, target)).toBe(false);
    },
  );

  it("rejects malformed or oversized guidance before allocating an identity", () => {
    const createId = vi.fn(() => "steer-1");
    const fence = new ActiveTurnSteerFence();

    expect(fence.begin(target, "  ", createId)).toBeNull();
    expect(fence.begin(target, "bad\0input", createId)).toBeNull();
    expect(
      fence.begin(target, "x".repeat(MAX_STEER_CHARACTERS + 1), createId),
    ).toBeNull();
    expect(createId).not.toHaveBeenCalled();
  });

  it("measures the server bound in Unicode scalars rather than UTF-16 units", () => {
    const fence = new ActiveTurnSteerFence();

    expect(
      fence.begin(
        target,
        "🌊".repeat(MAX_STEER_CHARACTERS),
        () => "steer-1",
      ),
    ).not.toBeNull();
  });

  it("clears only the accepted snapshot and preserves a newer draft", () => {
    const fence = new ActiveTurnSteerFence();
    const request = fence.begin(target, "first direction", () => "steer-1")!;

    expect(shouldClearAcceptedSteerDraft(request, "first direction")).toBe(true);
    expect(shouldClearAcceptedSteerDraft(request, "next direction")).toBe(false);
  });
});

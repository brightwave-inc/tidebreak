import { beforeEach, describe, expect, it } from "vitest";
import { useSessionDraftRecoveryStore } from "./draftRecovery";

describe("session draft recovery", () => {
  beforeEach(() => useSessionDraftRecoveryStore.getState().reset());

  it("keeps an undelivered launch draft until the created session opens", () => {
    useSessionDraftRecoveryStore.getState().offer("session-1", {
      draft: "Keep these words",
      error: "The first message did not send.",
    });
    expect(
      useSessionDraftRecoveryStore.getState().bySession["session-1"],
    ).toEqual({
      draft: "Keep these words",
      error: "The first message did not send.",
    });

    useSessionDraftRecoveryStore.getState().consume("session-1");
    expect(
      useSessionDraftRecoveryStore.getState().bySession["session-1"],
    ).toBeUndefined();
  });
});


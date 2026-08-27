import { describe, expect, it } from "vitest";
import { MachineRequestError } from "./machine";
import {
  clearDeliveredDraft,
  codeSubmissionWasAccepted,
  restoreUndeliveredDraft,
  sessionActionAvailability,
  submissionFailure,
} from "./submission";

describe("code submission safety", () => {
  it("separates HTTP refusals, preflight failures, and ambiguous dispatches", () => {
    expect(
      submissionFailure(
        new MachineRequestError(409, "session_busy", "Busy (HTTP 409)"),
        true,
      ),
    ).toEqual({ message: "Busy (HTTP 409)", deliveryUnknown: false });
    expect(submissionFailure(new Error("Could not read the queue."), false)).toEqual(
      {
        message: "Could not read the queue.",
        deliveryUnknown: false,
      },
    );
    expect(submissionFailure(new TypeError("socket closed"), true)).toMatchObject(
      { deliveryUnknown: true },
    );
    expect(
      submissionFailure(
        new MachineRequestError(502, null, "Machine request failed. (HTTP 502)"),
        true,
      ),
    ).toMatchObject({ deliveryUnknown: true });
  });

  it("reconciles an ambiguous send only against rows created after it", () => {
    const attempt = {
      message: "Run the focused tests",
      knownTurnIds: new Set(["turn-old"]),
      knownQueuedIds: new Set(["queued-old"]),
    };
    const baseTurn = {
      id: "turn-old",
      session_id: "session-1",
      ordinal: 1,
      status: "completed" as const,
      fast_mode: false,
      user_input: "Run the focused tests",
      attachments: [],
      started_at: "2026-08-27T00:00:00Z",
    };
    const baseQueued = {
      id: "queued-old",
      session_id: "session-1",
      message: "Run the focused tests",
      position: 0,
      created_at: "2026-08-27T00:00:00Z",
      updated_at: "2026-08-27T00:00:00Z",
    };

    expect(codeSubmissionWasAccepted(attempt, [baseTurn], [baseQueued])).toBe(
      false,
    );
    expect(
      codeSubmissionWasAccepted(
        attempt,
        [{ ...baseTurn, id: "turn-new", ordinal: 2 }],
        [baseQueued],
      ),
    ).toBe(true);
    expect(
      codeSubmissionWasAccepted(attempt, [baseTurn], [
        { ...baseQueued, id: "queued-new", position: 1 },
      ]),
    ).toBe(true);
  });

  it("keeps live supervision enabled while an idle turn request is pending", () => {
    expect(
      sessionActionAvailability({
        submittingTurn: true,
        steering: false,
        interrupting: false,
        refreshing: false,
        deliveryUnknown: false,
      }),
    ).toEqual({
      canChangeMode: true,
      canSteer: true,
      canFollowUp: false,
      canInterrupt: true,
    });
  });

  it("clears only the delivered draft and preserves newer typing", () => {
    expect(
      clearDeliveredDraft(
        "  Run the focused tests  ",
        "Run the focused tests",
      ),
    ).toBe("");
    expect(
      clearDeliveredDraft("Then inspect the logs", "Run the focused tests"),
    ).toBe("Then inspect the logs");
    expect(restoreUndeliveredDraft("", "Run the focused tests")).toBe(
      "Run the focused tests",
    );
    expect(
      restoreUndeliveredDraft("Then inspect the logs", "Run the focused tests"),
    ).toBe("Then inspect the logs");
  });
});

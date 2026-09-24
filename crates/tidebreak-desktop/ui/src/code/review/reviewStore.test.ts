import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HttpError } from "../../api/client/http";
import type { CodeReviewSnapshot } from "../../api/types";
import { createPendingReviewStore } from "../diff/pendingReview";
import {
  createCodeReviewStore,
  LOST_REVIEW_MESSAGE,
  type ReviewClient,
} from "./reviewStore";

const DIFF = [
  "diff --git a/src/queue.ts b/src/queue.ts",
  "--- a/src/queue.ts",
  "+++ b/src/queue.ts",
  "@@ -1,2 +1,2 @@",
  " import { send } from './net';",
  "-const MAX = 10;",
  "+const MAX = 20;",
  "",
].join("\n");

function snapshot(
  status: CodeReviewSnapshot["status"],
  overrides: Partial<CodeReviewSnapshot> = {},
): CodeReviewSnapshot {
  return {
    id: "rev-1",
    workspace_id: "ws-1",
    session_id: "sess-1",
    harness: "codex",
    permission_mode: "plan",
    status,
    progress: { tool_calls: 0, files_read: 0, refused: 0 },
    started_at: "2026-09-24T10:00:00.000Z",
    ...(status === "completed"
      ? {
          finished_at: "2026-09-24T10:01:00.000Z",
          result: {
            findings: [
              {
                path: "src/queue.ts",
                start_line: 2,
                end_line: 2,
                severity: "medium",
                title: "Why double it?",
                explanation: "Say why the limit moved.",
              },
            ],
            unplaced: [],
            rejected: 0,
            diff: DIFF,
          },
        }
      : {}),
    ...(status === "timed_out"
      ? {
          finished_at: "2026-09-24T10:20:00.000Z",
          failure: {
            kind: "timed_out" as const,
            message: "Codex CLI ran past 20 minutes and was stopped",
          },
        }
      : {}),
    ...overrides,
  };
}

/** A server whose review answers with `answers`, one per read. */
function server(answers: CodeReviewSnapshot[]) {
  const reads = [...answers];
  return {
    startCodeReview: vi.fn(async () => snapshot("running")),
    getCodeReview: vi.fn(async () => reads.shift() ?? answers.at(-1)!),
    cancelCodeReview: vi.fn(async () => snapshot("cancelled")),
    listCodeReviews: vi.fn(async () => [answers.at(-1)!]),
  } satisfies ReviewClient;
}

function stores() {
  const pendingReview = createPendingReviewStore(null);
  const reviews = createCodeReviewStore({ pendingReview, pollMs: 100 });
  return { pendingReview, reviews };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("following a review", () => {
  it("polls a running review and adds its findings once it completes", async () => {
    const { pendingReview, reviews } = stores();
    const client = server([snapshot("running"), snapshot("completed")]);
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("running");
    expect(pendingReview.getState().byWorkspace["ws-1"]).toBeUndefined();

    await vi.advanceTimersByTimeAsync(100);
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("running");
    await vi.advanceTimersByTimeAsync(100);
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("completed");
    const [finding] = pendingReview.getState().byWorkspace["ws-1"] ?? [];
    expect(finding).toMatchObject({
      author: { kind: "reviewer", engine: "codex", reviewId: "rev-1" },
      title: "Why double it?",
      proposed: true,
    });

    // It stops polling, and a later read never adds a dismissed finding back.
    pendingReview.getState().remove("ws-1", finding!.id);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(client.getCodeReview).toHaveBeenCalledTimes(2);
    await reviews.getState().refresh(client, "ws-1");
    expect(pendingReview.getState().byWorkspace["ws-1"]).toBeUndefined();
  });

  it("stops a running review and adds nothing", async () => {
    const { pendingReview, reviews } = stores();
    const client = server([snapshot("running")]);
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await reviews.getState().cancel(client, "ws-1");
    expect(client.cancelCodeReview).toHaveBeenCalledWith("ws-1", "rev-1");
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("cancelled");
    expect(pendingReview.getState().byWorkspace["ws-1"]).toBeUndefined();
  });

  it("shows a review that ran out of time as it ended, with no findings", async () => {
    const { pendingReview, reviews } = stores();
    const client = server([snapshot("timed_out")]);
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await vi.advanceTimersByTimeAsync(100);
    const review = reviews.getState().byWorkspace["ws-1"];
    expect(review?.status).toBe("timed_out");
    expect(review?.failure?.kind).toBe("timed_out");
    expect(pendingReview.getState().byWorkspace["ws-1"]).toBeUndefined();
  });

  it("never lets a late answer about an older review replace a newer one", async () => {
    const { reviews } = stores();
    const newer = snapshot("running", {
      id: "rev-2",
      started_at: "2026-09-24T11:00:00.000Z",
    });
    const client = {
      ...server([snapshot("completed")]),
      startCodeReview: vi.fn(async () => newer),
      listCodeReviews: vi.fn(async () => [snapshot("completed")]),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await reviews.getState().refresh(client, "ws-1");
    expect(reviews.getState().byWorkspace["ws-1"]?.id).toBe("rev-2");
  });

  it("passes the server's refusal to the form, and forgets it started", async () => {
    const { reviews } = stores();
    const client = {
      ...server([snapshot("running")]),
      startCodeReview: vi.fn(async () => {
        throw new Error("Codex CLI is not signed in on this machine.");
      }),
    } satisfies ReviewClient;
    await expect(
      reviews.getState().start(client, "ws-1", {
        session_id: "sess-1",
        harness: "codex",
      }),
    ).rejects.toThrow("not signed in");
    expect(reviews.getState().starting["ws-1"]).toBeUndefined();
    expect(reviews.getState().byWorkspace["ws-1"]).toBeUndefined();
  });
});

describe("a review the server lost", () => {
  it("stops polling once the server answers that it does not know the review, and says it stopped", async () => {
    const { pendingReview, reviews } = stores();
    const client = {
      ...server([snapshot("running")]),
      getCodeReview: vi.fn(async () => {
        throw new HttpError(404, "code review not found");
      }),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await vi.advanceTimersByTimeAsync(100);
    const review = reviews.getState().byWorkspace["ws-1"];
    expect(review?.status).toBe("failed");
    expect(review?.failure?.message).toBe(LOST_REVIEW_MESSAGE);
    expect(review?.progress.activity).toBeUndefined();
    expect(review?.finished_at).toBeDefined();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(client.getCodeReview).toHaveBeenCalledTimes(1);
    expect(pendingReview.getState().byWorkspace["ws-1"]).toBeUndefined();
  });

  it("keeps trying while the server cannot be reached, then marks the review lost", async () => {
    const { reviews } = stores();
    let reachable = false;
    const client = {
      ...server([snapshot("running")]),
      getCodeReview: vi.fn(async () => {
        if (!reachable) throw new TypeError("Failed to fetch");
        throw new HttpError(404, "code review not found");
      }),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await vi.advanceTimersByTimeAsync(100);
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("running");
    reachable = true;
    await vi.advanceTimersByTimeAsync(300);
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("failed");
  });

  it("marks a running review missing from the list as lost, but not one started while the list was on its way", async () => {
    const { reviews } = stores();
    const client = {
      ...server([snapshot("running")]),
      listCodeReviews: vi.fn(async (): Promise<CodeReviewSnapshot[]> => []),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await reviews.getState().refresh(client, "ws-1");
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("failed");

    const { reviews: fresh } = stores();
    let answer: (reviews: CodeReviewSnapshot[]) => void = () => {};
    const slow = {
      ...server([snapshot("running")]),
      listCodeReviews: vi.fn(
        () =>
          new Promise<CodeReviewSnapshot[]>((resolve) => {
            answer = resolve;
          }),
      ),
    } satisfies ReviewClient;
    const refreshing = fresh.getState().refresh(slow, "ws-1");
    await fresh.getState().start(slow, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    answer([]);
    await refreshing;
    expect(fresh.getState().byWorkspace["ws-1"]?.status).toBe("running");
  });

  it("treats a stop the server cannot find as the review already gone", async () => {
    const { reviews } = stores();
    const client = {
      ...server([snapshot("running")]),
      cancelCodeReview: vi.fn(async (): Promise<CodeReviewSnapshot> => {
        throw new HttpError(404, "code review not found");
      }),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await reviews.getState().cancel(client, "ws-1");
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("failed");
    expect(reviews.getState().stopping["ws-1"]).toBeUndefined();
  });
});

describe("stopping a review", () => {
  it("reads as stopping until the server says the review ended", async () => {
    const { reviews } = stores();
    const client = {
      ...server([snapshot("running"), snapshot("cancelled")]),
      // The server takes the request, and the engine winds down after.
      cancelCodeReview: vi.fn(async () => snapshot("running")),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await reviews.getState().cancel(client, "ws-1");
    expect(reviews.getState().stopping["ws-1"]).toBe("rev-1");
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("running");

    await vi.advanceTimersByTimeAsync(100);
    expect(reviews.getState().stopping["ws-1"]).toBe("rev-1");
    await vi.advanceTimersByTimeAsync(100);
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("cancelled");
    expect(reviews.getState().stopping["ws-1"]).toBeUndefined();
  });

  it("stops reading as stopping when the request fails, and passes the error on", async () => {
    const { reviews } = stores();
    const client = {
      ...server([snapshot("running")]),
      cancelCodeReview: vi.fn(async (): Promise<CodeReviewSnapshot> => {
        throw new HttpError(500, "the engine did not answer");
      }),
    } satisfies ReviewClient;
    await reviews.getState().start(client, "ws-1", {
      session_id: "sess-1",
      harness: "codex",
    });
    await expect(reviews.getState().cancel(client, "ws-1")).rejects.toThrow(
      "the engine did not answer",
    );
    expect(reviews.getState().stopping["ws-1"]).toBeUndefined();
    expect(reviews.getState().byWorkspace["ws-1"]?.status).toBe("running");
  });
});

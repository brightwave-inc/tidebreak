import { describe, expect, it } from "vitest";

import type { CodeDeliveryPullRequestSummary } from "../api/types";
import {
  checkCounts,
  checkSummary,
  mergeBlockedReason,
  orderPullRequestComments,
  pullRequestLifecycle,
  pullRequestListStatus,
  pullRequestReviewSummary,
  pullRequestSettledAt,
} from "./pullRequestPresentation";

function pr(
  overrides: Partial<CodeDeliveryPullRequestSummary> = {},
): CodeDeliveryPullRequestSummary {
  return {
    id: "github.com/brightwave-inc/tidebreak#1",
    repository: {
      host: "github.com",
      owner: "brightwave-inc",
      name: "tidebreak",
      name_with_owner: "brightwave-inc/tidebreak",
      url: "https://github.com/brightwave-inc/tidebreak",
    },
    number: 1,
    url: "https://github.com/brightwave-inc/tidebreak/pull/1",
    title: "A pull request",
    state: "open",
    draft: false,
    head_branch: "feature",
    base_branch: "main",
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: "2026-08-01T00:00:00.000Z",
    updated_at: "2026-08-02T00:00:00.000Z",
    ...overrides,
  };
}

describe("pullRequestLifecycle", () => {
  it("reads open, draft, merged, and closed apart", () => {
    expect(pullRequestLifecycle(pr())).toBe("open");
    expect(pullRequestLifecycle(pr({ draft: true }))).toBe("draft");
    expect(pullRequestLifecycle(pr({ state: "merged" }))).toBe("merged");
    expect(pullRequestLifecycle(pr({ state: "closed" }))).toBe("closed");
  });

  it("trusts the merge timestamp over a host that only reports CLOSED", () => {
    const merged = pr({
      state: "closed",
      merged_at: "2026-08-02T00:00:00.000Z",
      closed_at: "2026-08-02T00:00:00.000Z",
    });
    expect(pullRequestLifecycle(merged)).toBe("merged");
  });

  it("accepts the host's upper-case state", () => {
    expect(pullRequestLifecycle(pr({ state: "MERGED" }))).toBe("merged");
    expect(pullRequestLifecycle(pr({ state: "CLOSED" }))).toBe("closed");
  });

  it("reports when a pull request settled", () => {
    expect(pullRequestSettledAt(pr())).toBeUndefined();
    expect(
      pullRequestSettledAt(pr({ closed_at: "2026-08-02T00:00:00.000Z" })),
    ).toBe("2026-08-02T00:00:00.000Z");
  });
});

describe("pullRequestReviewSummary", () => {
  // The reported bug: GitHub drops `review_decision` once a pull request
  // settles, and the column read that absence as an outstanding review.
  it("reports the outcome for a settled pull request, not a pending review", () => {
    expect(
      pullRequestReviewSummary(
        pr({ state: "merged", merged_at: "2026-08-02T00:00:00.000Z" }),
      ),
    ).toEqual({ label: "Merged", tone: "merged" });
    expect(
      pullRequestReviewSummary(
        pr({ state: "closed", closed_at: "2026-08-02T00:00:00.000Z" }),
      ),
    ).toEqual({ label: "Closed", tone: "critical" });
  });

  it("reports the review decision while the pull request is live", () => {
    expect(
      pullRequestReviewSummary(pr({ review_decision: "approved" })).label,
    ).toBe("Approved");
    expect(
      pullRequestReviewSummary(pr({ review_decision: "changes_requested" })),
    ).toEqual({ label: "Changes requested", tone: "critical" });
    expect(pullRequestReviewSummary(pr()).label).toBe("Review pending");
    expect(pullRequestReviewSummary(pr({ draft: true })).label).toBe("Draft");
  });
});

describe("checkCounts", () => {
  it("buckets a rollup in one pass", () => {
    const counts = checkCounts([
      { name: "a", bucket: "pass" },
      { name: "b", bucket: "fail" },
      { name: "c", bucket: "pending" },
      { name: "d", bucket: "skipped" },
      { name: "e", bucket: "pass" },
    ]);
    expect(counts).toEqual({
      total: 5,
      passed: 2,
      pending: 1,
      failed: 1,
      skipped: 1,
    });
  });

  it("leads with failures, then work in flight", () => {
    expect(checkSummary(checkCounts([]))).toEqual({
      label: "No checks",
      tone: "neutral",
    });
    expect(
      checkSummary(
        checkCounts([
          { name: "a", bucket: "fail" },
          { name: "b", bucket: "pending" },
        ]),
      ).label,
    ).toBe("1 failed");
    expect(
      checkSummary(
        checkCounts([
          { name: "a", bucket: "pass" },
          { name: "b", bucket: "pending" },
        ]),
      ).label,
    ).toBe("1 pending");
    expect(
      checkSummary(checkCounts([{ name: "a", bucket: "pass" }])).label,
    ).toBe("1 passed");
  });
});

describe("pullRequestListStatus", () => {
  it("treats a running check as waiting, not attention", () => {
    expect(
      pullRequestListStatus(
        pr({ checks: [{ name: "preview", bucket: "pending" }] }),
      ),
    ).toEqual({
      label: "Checks running",
      tone: "pending",
      group: "waiting",
    });
  });

  it("moves queued and clear auto-merge pull requests out of attention", () => {
    expect(
      pullRequestListStatus(
        pr({
          in_merge_queue: true,
          attention_reasons: ["checks_failed"],
          checks: [{ name: "preview", bucket: "fail" }],
        }),
      ),
    ).toMatchObject({ label: "In merge queue", group: "handed_off" });
    expect(
      pullRequestListStatus(pr({ auto_merge_enabled: true })),
    ).toMatchObject({ label: "Auto-merge armed", group: "handed_off" });
  });

  it("keeps a blocked auto-merge pull request in attention", () => {
    expect(
      pullRequestListStatus(
        pr({
          auto_merge_enabled: true,
          attention_reasons: ["conflicts"],
        }),
      ),
    ).toMatchObject({ label: "Resolve conflicts", group: "attention" });
  });
});

describe("mergeBlockedReason", () => {
  it("explains a blocked merge instead of letting the API refuse it", () => {
    expect(mergeBlockedReason(pr())).toBeNull();
    expect(mergeBlockedReason(pr({ mergeable: "conflicting" }))).toMatch(
      /conflicts/,
    );
    expect(mergeBlockedReason(pr({ merge_state_status: "behind" }))).toMatch(
      /Update the branch/,
    );
    expect(mergeBlockedReason(pr({ draft: true }))).toMatch(/ready/);
    expect(mergeBlockedReason(pr({ state: "merged" }))).toMatch(
      /already merged/,
    );
    expect(mergeBlockedReason(pr({ state: "closed" }))).toMatch(/Reopen/);
    expect(
      mergeBlockedReason(pr({ checks: [{ name: "ci", bucket: "fail" }] })),
    ).toMatch(/failing check/);
    expect(
      mergeBlockedReason(
        pr({
          merge_state_status: "blocked",
          review_decision: "review_required",
          checks: [{ name: "preview", bucket: "pending" }],
        }),
      ),
    ).toBe("Wait for the running check before merging.");
  });
});

describe("orderPullRequestComments", () => {
  const comments = [
    { kind: "issue" as const, id: "a", created_at: "2026-08-20T10:00:00Z" },
    { kind: "issue" as const, id: "b", created_at: "2026-08-20T15:00:00Z" },
    { kind: "issue" as const, id: "undated" },
    { kind: "issue" as const, id: "c", created_at: "2026-08-20T12:00:00Z" },
  ].map((comment) => ({ ...comment, body: comment.id }));

  it("puts the latest comment on top by default order", () => {
    expect(
      orderPullRequestComments(comments, "newest").map((item) => item.id),
    ).toEqual(["b", "c", "a", "undated"]);
  });

  it("restores host chronology on oldest-first", () => {
    expect(
      orderPullRequestComments(comments, "oldest").map((item) => item.id),
    ).toEqual(["undated", "a", "c", "b"]);
  });

  it("keeps the input untouched", () => {
    const before = comments.map((item) => item.id);
    orderPullRequestComments(comments, "newest");
    expect(comments.map((item) => item.id)).toEqual(before);
  });
});

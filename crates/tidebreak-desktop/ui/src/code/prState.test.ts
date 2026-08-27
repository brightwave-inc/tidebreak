import { describe, expect, it } from "vitest";

import type { CodeDeliveryPullRequestSummary } from "../api/types";
import {
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
  PR_GATE_GROUP,
  PR_GATE_LABEL,
  PR_GATE_TONE,
  checkCounts,
  checkSummary,
  mergeBlockedReasons,
  prCompactStatusLabel,
  prCompactStatusTone,
  prGate,
  prStateChips,
  prStatus,
  pullRequestLifecycle,
  pullRequestReviewSummary,
  pullRequestSettledAt,
  type PrGate,
} from "./prState";

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

  it("trusts the merge evidence over a host that only reports CLOSED", () => {
    const merged = pr({
      state: "closed",
      merged_at: "2026-08-02T00:00:00.000Z",
      closed_at: "2026-08-02T00:00:00.000Z",
    });
    expect(pullRequestLifecycle(merged)).toBe("merged");
    expect(pullRequestLifecycle({ state: "open", merged: true })).toBe(
      "merged",
    );
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

  it("renders a review requirement neutral, like GitHub's branch-rule fact", () => {
    expect(
      pullRequestReviewSummary(pr({ review_decision: "review_required" })),
    ).toEqual({ label: "Review required", tone: "neutral" });
  });
});

describe("checkCounts", () => {
  it("buckets a rollup in one pass", () => {
    const counts = checkCounts({
      checks: [
        { bucket: "pass" },
        { bucket: "fail" },
        { bucket: "pending" },
        { bucket: "skipped" },
        { bucket: "pass" },
      ],
    });
    expect(counts).toEqual({
      total: 5,
      passing: 2,
      pending: 1,
      failing: 1,
      skipped: 1,
    });
  });

  it("falls back to the one-line summary the host sends", () => {
    expect(checkCounts({ checks_summary: "2 passing, 1 failing" })).toEqual({
      total: 3,
      passing: 2,
      pending: 0,
      failing: 1,
      skipped: 0,
    });
  });

  it("leads with failures, then work in flight", () => {
    expect(checkSummary(checkCounts({}))).toEqual({
      label: "No checks",
      tone: "neutral",
    });
    expect(
      checkSummary(
        checkCounts({
          checks: [
            { name: "ci", bucket: "fail" },
            { name: "preview", bucket: "pending" },
          ],
        }),
      ).label,
    ).toBe("1 failed");
    expect(
      checkSummary(
        checkCounts({
          checks: [{ bucket: "pass" }, { bucket: "pending" }],
        }),
      ).label,
    ).toBe("1 pending");
    expect(
      checkSummary(checkCounts({ checks: [{ name: "ci", bucket: "pass" }] }))
        .label,
    ).toBe("1 passed");
  });
});

describe("prGate", () => {
  it("settles terminal lifecycles before anything else", () => {
    expect(prGate(pr({ state: "merged" }))).toBe("merged");
    expect(prGate(pr({ state: "closed" }))).toBe("closed");
    expect(prGate(pr({ draft: true, mergeable: "conflicting" }))).toBe("draft");
  });

  it("orders the blockers a reader resolves in order", () => {
    expect(prGate(pr({ mergeable: "conflicting" }))).toBe("conflict");
    expect(
      prGate(
        pr({
          mergeable: "conflicting",
          review_decision: "changes_requested",
          checks: [{ name: "ci", bucket: "fail" }],
        }),
      ),
    ).toBe("conflict");
    expect(
      prGate(
        pr({
          review_decision: "changes_requested",
          checks: [{ name: "ci", bucket: "fail" }],
        }),
      ),
    ).toBe("changes_requested");
    expect(
      prGate(
        pr({
          checks: [{ name: "ci", bucket: "fail" }],
          merge_state_status: "behind",
        }),
      ),
    ).toBe("failing");
    expect(
      prGate(
        pr({
          in_merge_queue: true,
          checks: [{ name: "ci", bucket: "fail" }],
        }),
      ),
    ).toBe("failing");
    expect(prGate(pr({ in_merge_queue: true }))).toBe("queued");
    expect(prGate(pr({ merge_state_status: "behind" }))).toBe("behind");
    expect(prGate(pr({ checks: [{ name: "ci", bucket: "pending" }] }))).toBe(
      "pending",
    );
    expect(
      prGate(
        pr({
          review_decision: "review_required",
          merge_state_status: "blocked",
        }),
      ),
    ).toBe("needs_approval");
    expect(prGate(pr({ merge_state_status: "blocked" }))).toBe("blocked");
    expect(prGate(pr({ auto_merge_enabled: true }))).toBe("auto_merge");
    expect(
      prGate(pr({ mergeable: "mergeable", merge_state_status: "clean" })),
    ).toBe("ready");
    expect(prGate(pr())).toBe("checking");
  });

  it("keeps every gate label, tone, and group in one vocabulary", () => {
    const gates = Object.keys(PR_GATE_LABEL) as PrGate[];
    expect(gates).toHaveLength(14);
    for (const gate of gates) {
      expect(PR_GATE_LABEL[gate]).toBeTruthy();
      expect(PR_GATE_TONE[gate]).toBeTruthy();
      expect(PR_GATE_GROUP[gate]).toBeTruthy();
    }
    // GitHub's own colors, on every surface.
    expect(PULL_REQUEST_LIFECYCLE_TONE.open).toBe("ready");
    expect(PULL_REQUEST_LIFECYCLE_TONE.draft).toBe("neutral");
    expect(PULL_REQUEST_LIFECYCLE_TONE.merged).toBe("merged");
    expect(PULL_REQUEST_LIFECYCLE_TONE.closed).toBe("critical");
    expect(PULL_REQUEST_LIFECYCLE_LABEL.merged).toBe("Merged");
    // A queued pull request is in flight, not a warning: info blue.
    expect(PR_GATE_TONE.queued).toBe("pending");
    expect(PR_GATE_LABEL.queued).toBe("In merge queue");
  });
});

describe("prStatus", () => {
  it("treats a running check as waiting, not attention", () => {
    expect(
      prStatus(pr({ checks: [{ name: "ci", bucket: "pending" }] })),
    ).toMatchObject({
      gate: "pending",
      headline: { label: "Checks running", tone: "pending" },
      group: "waiting",
    });
  });

  it("moves queued and clear auto-merge pull requests out of attention", () => {
    expect(prStatus(pr({ in_merge_queue: true }))).toMatchObject({
      headline: { label: "In merge queue", tone: "pending" },
      group: "handed_off",
    });
    expect(prStatus(pr({ auto_merge_enabled: true }))).toMatchObject({
      headline: { label: "Auto-merge on" },
      group: "handed_off",
    });
  });

  it("keeps a blocked auto-merge pull request in attention", () => {
    expect(
      prStatus(
        pr({
          auto_merge_enabled: true,
          mergeable: "conflicting",
        }),
      ),
    ).toMatchObject({
      headline: { label: "Resolve conflicts" },
      group: "attention",
    });
  });

  it("does not let queue membership hide a failing check", () => {
    expect(
      prStatus(
        pr({
          in_merge_queue: true,
          checks: [{ name: "ci", bucket: "fail" }],
        }),
      ),
    ).toMatchObject({
      headline: { label: "Checks failed" },
      group: "attention",
    });
  });
});

describe("mergeBlockedReasons", () => {
  it("explains a blocked merge instead of letting the API refuse it", () => {
    expect(mergeBlockedReasons(pr())).toEqual([]);
    expect(mergeBlockedReasons(pr({ mergeable: "conflicting" }))[0]).toMatch(
      /conflicts/,
    );
    expect(
      mergeBlockedReasons(pr({ merge_state_status: "behind" }))[0],
    ).toMatch(/Update the branch/);
    expect(mergeBlockedReasons(pr({ draft: true }))[0]).toMatch(/ready/);
    expect(mergeBlockedReasons(pr({ state: "merged" }))[0]).toMatch(
      /already merged/,
    );
    expect(mergeBlockedReasons(pr({ state: "closed" }))[0]).toMatch(/Reopen/);
    expect(
      mergeBlockedReasons(pr({ checks: [{ name: "ci", bucket: "fail" }] }))[0],
    ).toMatch(/failing check/);
  });

  it("lists every blocker, the way the GitHub merge box does", () => {
    expect(
      mergeBlockedReasons(
        pr({
          mergeable: "conflicting",
          review_decision: "changes_requested",
          checks: [
            { name: "ci", bucket: "fail" },
            { name: "preview", bucket: "pending" },
          ],
        }),
      ),
    ).toEqual([
      "Resolve the conflicts with the base branch first.",
      "Address the requested changes before merging.",
      "Fix the failing check before merging.",
      "Wait for the running check before merging.",
    ]);
    expect(
      mergeBlockedReasons(
        pr({
          merge_state_status: "blocked",
          review_decision: "review_required",
        }),
      ),
    ).toEqual([
      "The pull request needs a review approval before merging directly.",
    ]);
  });
});

describe("prStateChips", () => {
  it("shows lifecycle, review, and queue as separate chips", () => {
    expect(prStateChips(pr())).toEqual([
      { key: "lifecycle", label: "Open", tone: "ready" },
    ]);
    expect(
      prStateChips(pr({ review_decision: "approved", in_merge_queue: true })),
    ).toEqual([
      { key: "lifecycle", label: "Open", tone: "ready" },
      { key: "review", label: "Approved", tone: "ready" },
      { key: "queue", label: "In merge queue", tone: "pending" },
    ]);
    expect(prStateChips(pr({ draft: true }))).toEqual([
      { key: "lifecycle", label: "Draft", tone: "neutral" },
    ]);
    // A settled pull request keeps exactly one chip: the outcome.
    expect(
      prStateChips(pr({ state: "merged", review_decision: "approved" })),
    ).toEqual([{ key: "lifecycle", label: "Merged", tone: "merged" }]);
  });
});

describe("prCompactStatusLabel", () => {
  it("names queue membership in the one word a compact surface has", () => {
    expect(prCompactStatusLabel(pr())).toBe("Open");
    expect(prCompactStatusLabel(pr({ in_merge_queue: true }))).toBe(
      "In merge queue",
    );
    expect(prCompactStatusTone(pr({ in_merge_queue: true }))).toBe("pending");
    expect(prCompactStatusLabel(pr({ auto_merge_enabled: true }))).toBe(
      "Auto-merge on",
    );
    expect(prCompactStatusTone(pr({ auto_merge_enabled: true }))).toBe(
      "pending",
    );
    expect(prCompactStatusLabel(pr({ state: "merged" }))).toBe("Merged");
  });
});

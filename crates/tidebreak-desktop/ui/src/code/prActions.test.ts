import { describe, expect, it } from "vitest";

import type { PullRequestDigest } from "../api/types";
import {
  prHasConflicts,
  prIsQueued,
  prWorkflowPrompt,
  prWorkflowStatus,
} from "./prActions";

function pr(partial: Partial<PullRequestDigest>): PullRequestDigest {
  return {
    number: 41,
    state: "open",
    ...partial,
  };
}

describe("prWorkflowStatus", () => {
  it("classifies a clean open PR as ready", () => {
    const model = prWorkflowStatus(
      pr({
        checks: [{ name: "ci", bucket: "pass" }],
        mergeable: "mergeable",
        merge_state_status: "clean",
      }),
    );
    expect(model.state).toBe("ready");
    expect(model.checks).toEqual({
      passing: 1,
      pending: 0,
      failing: 0,
      skipped: 0,
      total: 1,
    });
  });

  it("counts skipped checks without treating them as pending", () => {
    const model = prWorkflowStatus(
      pr({
        checks: [
          { name: "ci", bucket: "pass" },
          { name: "release", bucket: "skipped", detail: "skipping" },
        ],
        mergeable: "mergeable",
        merge_state_status: "clean",
      }),
    );
    expect(model.state).toBe("ready");
    expect(model.checks).toEqual({
      passing: 1,
      pending: 0,
      failing: 0,
      skipped: 1,
      total: 2,
    });
  });

  it("classifies a failing check before ready state", () => {
    const model = prWorkflowStatus(
      pr({
        checks: [
          { name: "ci / rust", bucket: "fail" },
          { name: "ci / ui", bucket: "pass" },
        ],
      }),
    );
    expect(model.state).toBe("failing");
  });

  it("classifies conflicts", () => {
    const model = prWorkflowStatus(pr({ mergeable: "CONFLICTING" }));
    expect(prHasConflicts(pr({ mergeable: "CONFLICTING" }))).toBe(true);
    expect(model.state).toBe("conflict");
  });

  it("names a draft even when checks are still pending", () => {
    const model = prWorkflowStatus(
      pr({
        draft: true,
        checks: [{ name: "ci", bucket: "pending" }],
      }),
    );
    expect(model.state).toBe("draft");
  });

  it("recognizes an armed merge queue without hiding real failures", () => {
    const queued = pr({
      in_merge_queue: true,
      auto_merge_enabled: true,
      merge_state_status: "blocked",
      checks: [{ name: "ci", bucket: "pending" }],
    });
    expect(prIsQueued(queued)).toBe(true);
    expect(prWorkflowStatus(queued).state).toBe("queued");

    expect(
      prWorkflowStatus({
        ...queued,
        checks: [{ name: "ci", bucket: "fail" }],
      }).state,
    ).toBe("failing");
  });

  it("keeps auto-merge distinct from an explicit queue state", () => {
    const model = prWorkflowStatus(
      pr({
        auto_merge_enabled: true,
        merge_state_status: "clean",
        checks: [{ name: "ci", bucket: "pass" }],
      }),
    );
    expect(model.state).toBe("auto_merge");
  });

  it("does not call review-blocked or behind PRs ready", () => {
    expect(
      prWorkflowStatus(pr({ review_decision: "changes_requested" })).state,
    ).toBe("changes_requested");
    expect(prWorkflowStatus(pr({ merge_state_status: "behind" })).state).toBe(
      "behind",
    );
    expect(prWorkflowStatus(pr({ merge_state_status: "blocked" })).state).toBe(
      "blocked",
    );
  });

  it("keeps incomplete host data in a checking state", () => {
    expect(prWorkflowStatus(pr({})).state).toBe("checking");
    expect(
      prWorkflowStatus(
        pr({ mergeable: "unknown", merge_state_status: "unknown" }),
      ).state,
    ).toBe("checking");
  });

  it("classifies a merged PR", () => {
    const model = prWorkflowStatus(pr({ state: "merged", merged: true }));
    expect(model.state).toBe("merged");
  });
});

describe("prWorkflowPrompt", () => {
  it("includes enough live PR context for one-click agent actions", () => {
    const digest = pr({
      title: "Fix login",
      url: "https://github.com/acme/app/pull/41",
      head_branch: "fix-login",
      base_branch: "main",
      mergeable: "conflicting",
      review_decision: "changes_requested",
      checks: [
        {
          name: "ci / ui",
          bucket: "fail",
          detail: "Tests failed",
          url: "https://github.com/acme/app/actions/runs/7",
        },
      ],
    });
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/#41/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/main/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/Fix login/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/fix-login -> main/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/ci \/ ui/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/Tests failed/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/actions\/runs\/7/);
    expect(prWorkflowPrompt("fix_errors", digest)).toMatch(/failing checks/);
    expect(prWorkflowPrompt("resolve_conflicts", digest)).toMatch(/conflicts/);
    expect(prWorkflowPrompt("update_branch", digest)).toMatch(/Update pull request/);
    expect(prWorkflowPrompt("address_feedback", digest)).toMatch(
      /requested changes/,
    );
  });

  it("has no prompt for the pull-request state changes", () => {
    // Decision 42 keeps merging and readying a draft on user-initiated
    // endpoints. Excluding them from the prompt type is what stops either from
    // being wired back onto the agent path, where an agent's own shell would
    // route around the `gh` runner that refuses merge argv. These lines must
    // not compile.
    // @ts-expect-error merging is not an agent action
    expect(() => prWorkflowPrompt("merge", pr({}))).toBeDefined();
    // @ts-expect-error readying a draft is not an agent action
    expect(() => prWorkflowPrompt("mark_ready", pr({}))).toBeDefined();
  });
});

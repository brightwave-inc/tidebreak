import { describe, expect, it } from "vitest";

import type { PullRequestDigest } from "../api/types";
import { prBarModel, prBarPrompt, prHasConflicts } from "./prActions";

function pr(partial: Partial<PullRequestDigest>): PullRequestDigest {
  return {
    number: 41,
    state: "open",
    ...partial,
  };
}

describe("prBarModel", () => {
  it("leads with watch and fix when the PR is open and clean", () => {
    const model = prBarModel(
      pr({
        checks: [{ name: "ci", bucket: "pass" }],
      }),
    );
    expect(model.status).toBe("Ready to merge");
    expect(model.actions[0]).toBe("watch_and_fix");
    expect(model.actions).toEqual([
      "watch_and_fix",
      "merge",
      "fix_errors",
      "resolve_conflicts",
    ]);
    expect(model.checks).toEqual({
      passing: 1,
      pending: 0,
      failing: 0,
      skipped: 0,
      total: 1,
    });
  });

  it("counts skipped checks without treating them as pending", () => {
    const model = prBarModel(
      pr({
        checks: [
          { name: "ci", bucket: "pass" },
          { name: "release", bucket: "skipped", detail: "skipping" },
        ],
      }),
    );
    expect(model.status).toBe("Ready to merge");
    expect(model.checks).toEqual({
      passing: 1,
      pending: 0,
      failing: 0,
      skipped: 1,
      total: 2,
    });
  });

  it("keeps the contextual fix action directly after watch and fix", () => {
    const model = prBarModel(
      pr({
        checks: [
          { name: "ci / rust", bucket: "fail" },
          { name: "ci / ui", bucket: "pass" },
        ],
      }),
    );
    expect(model.status).toBe("1 check failing");
    expect(model.actions.slice(0, 2)).toEqual(["watch_and_fix", "fix_errors"]);
  });

  it("keeps the contextual conflict action directly after watch and fix", () => {
    const model = prBarModel(pr({ mergeable: "CONFLICTING" }));
    expect(prHasConflicts(pr({ mergeable: "CONFLICTING" }))).toBe(true);
    expect(model.status).toBe("Conflicts");
    expect(model.actions.slice(0, 2)).toEqual([
      "watch_and_fix",
      "resolve_conflicts",
    ]);
  });

  it("names a draft even when checks are still pending", () => {
    const model = prBarModel(
      pr({
        draft: true,
        checks: [{ name: "ci", bucket: "pending" }],
      }),
    );
    expect(model.status).toBe("Draft");
    expect(model.actions.slice(0, 2)).toEqual(["watch_and_fix", "merge"]);
  });

  it("hides actions on a merged PR", () => {
    const model = prBarModel(pr({ state: "merged", merged: true }));
    expect(model.status).toBe("Merged");
    expect(model.actions).toEqual([]);
  });
});

describe("prBarPrompt", () => {
  it("names the PR and the base branch", () => {
    const digest = pr({ base_branch: "main", title: "Fix login" });
    expect(prBarPrompt("merge", digest)).toMatch(/#41/);
    expect(prBarPrompt("merge", digest)).toMatch(/main/);
    expect(prBarPrompt("fix_errors", digest)).toMatch(/failing checks/);
    expect(prBarPrompt("resolve_conflicts", digest)).toMatch(/conflicts/);
    const watch = prBarPrompt("watch_and_fix", digest);
    expect(watch).toMatch(/keep watching/i);
    expect(watch).toMatch(/Enable auto-merge/);
    expect(watch).toMatch(/required human approval/);
  });
});

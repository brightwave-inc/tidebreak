import { describe, expect, it } from "vitest";

import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import {
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
} from "./workspaceWorkflow";

const CLEAN: CodeWorkspacePrSnapshot = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: true,
  suggested_commit_message: "",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

function pr(partial: Partial<PullRequestDigest>): PullRequestDigest {
  return { number: 41, state: "open", ...partial };
}

describe("workspaceWorkflowModel", () => {
  it("walks local work through review, push, and pull-request creation", () => {
    const dirty = workspaceWorkflowModel({ ...CLEAN, dirty: true });
    expect(dirty.summary).toBe("Uncommitted changes");
    expect(dirty.primary).toBe("open_source");
    expect(workspaceWorkflowActionLabel(dirty.primary!, dirty.stage)).toBe(
      "Review & commit",
    );

    const unpushed = workspaceWorkflowModel({
      ...CLEAN,
      unpushed: true,
      ahead: 2,
    });
    expect(unpushed.summary).toBe("Unpushed changes");
    expect(unpushed.primary).toBe("push");

    const ready = workspaceWorkflowModel({ ...CLEAN, ahead: 2 });
    expect(ready.summary).toBe("Ready for PR");
    expect(ready.primary).toBe("create_pr");

    const setup = workspaceWorkflowModel({
      ...CLEAN,
      ahead: 2,
      gh_found: false,
    });
    expect(setup.summary).toBe("GitHub setup");
    expect(setup.primary).toBe("open_source");
    expect(workspaceWorkflowActionLabel(setup.primary!, setup.stage)).toBe(
      "Set up GitHub",
    );
  });

  it("makes drafts, failures, queues, and merged PRs distinct", () => {
    const draft = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ draft: true }),
    });
    expect(draft.summary).toBe("#41 · Draft");
    expect(draft.primary).toBe("mark_ready");
    expect(workspaceWorkflowActionLabel("open_pr", draft.stage)).toBe(
      "View PR",
    );

    const failing = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ checks: [{ name: "ci", bucket: "fail" }] }),
    });
    expect(failing.summary).toBe("#41 · 1 failing");
    expect(failing.primary).toBe("fix_errors");
    expect(workspaceWorkflowActionLabel(failing.primary!, failing.stage)).toBe(
      "Fix CI",
    );

    const queued = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({
        url: "https://github.com/acme/app/pull/41",
        auto_merge_enabled: true,
        in_merge_queue: true,
      }),
    });
    expect(queued.summary).toBe("#41 · Queued");
    expect(queued.primary).toBe("open_pr");
    expect(workspaceWorkflowActionLabel(queued.primary!, queued.stage)).toBe(
      "View queue",
    );

    const merged = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ state: "merged", merged: true }),
    });
    expect(merged.summary).toBe("#41 · Merged");
    expect(merged.secondary).toEqual([]);
  });

  it("puts the concrete repair or merge action in the primary slot", () => {
    const conflict = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ mergeable: "conflicting" }),
    });
    expect(conflict.primary).toBe("resolve_conflicts");

    const behind = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ merge_state_status: "behind" }),
    });
    expect(behind.primary).toBe("update_branch");

    const feedback = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ review_decision: "changes_requested" }),
    });
    expect(feedback.primary).toBe("address_feedback");

    const ready = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ mergeable: "mergeable", merge_state_status: "clean" }),
    });
    expect(ready.primary).toBe("merge");
  });

  it("lets local work outrank an existing pull request", () => {
    const existing = pr({
      url: "https://github.com/acme/app/pull/41",
      checks: [{ name: "ci", bucket: "pass" }],
    });
    const dirty = workspaceWorkflowModel({
      ...CLEAN,
      dirty: true,
      pr: existing,
    });
    expect(dirty.summary).toBe("#41 · Changes");
    expect(dirty.primary).toBe("open_source");

    const unpushed = workspaceWorkflowModel({
      ...CLEAN,
      unpushed: true,
      ahead: 7,
      pr: existing,
    });
    expect(unpushed.summary).toBe("#41 · Unpushed");
    expect(unpushed.detail).not.toContain("7");
  });

  it("does not equate auto-merge with a merge-queue entry", () => {
    const model = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({
        url: "https://github.com/acme/app/pull/41",
        auto_merge_enabled: true,
        merge_state_status: "clean",
      }),
    });
    expect(model.summary).toBe("#41 · Auto-merge on");
    expect(model.stage).toBe("auto_merge");
  });

  it("does not call an incomplete PR ready", () => {
    const model = workspaceWorkflowModel({ ...CLEAN, pr: pr({}) });
    expect(model.summary).toBe("#41 · Checking");
    expect(model.stage).toBe("checking");
    expect(model.secondary).not.toContain("merge");
  });

  it("does not offer merge while checks are pending", () => {
    const model = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({ checks: [{ name: "ci", bucket: "pending" }] }),
    });
    expect(model.stage).toBe("pending");
    expect(model.secondary).not.toContain("merge");
  });

  it("lets a loaded no-PR snapshot clear a stale fallback", () => {
    const model = workspaceWorkflowModel(
      CLEAN,
      pr({ url: "https://github.com/acme/app/pull/41" }),
    );
    expect(model.stage).toBe("clean");
    expect(model.pr).toBeUndefined();
  });
});

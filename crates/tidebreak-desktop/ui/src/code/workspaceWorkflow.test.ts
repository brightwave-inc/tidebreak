import { describe, expect, it } from "vitest";

import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import {
  composePrPrompt,
  resolveWorkflowShortcut,
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkflowShortcut,
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
    expect(dirty.primary).toBe("compose_pr");
    expect(workspaceWorkflowActionLabel(dirty.primary!, dirty.stage)).toBe(
      "Create PR",
    );
    // Hand review does not vanish behind the drafted request; it moves one
    // step away into the menu.
    expect(dirty.secondary).toEqual(["open_source"]);
    expect(workspaceWorkflowActionLabel("open_source", dirty.stage)).toBe(
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
    expect(queued.tone).toBe("warning");
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
    // With a pull request open there is nothing to create: new local changes
    // go through the commit box to update it.
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

  it("shows the pending count while blocked, then names the approval", () => {
    // The decision-66 screenshot: GitHub says blocked whenever required
    // checks are still running, so the header shows the pending count; once
    // the checks are green, the missing review approval is named in those
    // words instead of a lifelong "Blocked".
    const running = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({
        url: "https://github.com/acme/app/pull/41",
        merge_state_status: "blocked",
        review_decision: "review_required",
        auto_merge_enabled: true,
        checks: [
          { name: "ci / rust", bucket: "pending" },
          { name: "ci / ui", bucket: "pending" },
          { name: "lint", bucket: "pass" },
        ],
      }),
    });
    expect(running.stage).toBe("pending");
    expect(running.summary).toBe("#41 · 2 pending");

    const green = workspaceWorkflowModel({
      ...CLEAN,
      pr: pr({
        url: "https://github.com/acme/app/pull/41",
        merge_state_status: "blocked",
        review_decision: "review_required",
        checks: [{ name: "ci / rust", bucket: "pass" }],
      }),
    });
    expect(green.stage).toBe("needs_approval");
    expect(green.summary).toBe("#41 · Needs approval");
    expect(green.primary).toBe("open_pr");
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

describe("resolveWorkflowShortcut", () => {
  /** What a chord does against a snapshot, as a string the assertions read. */
  function chord(
    shortcut: WorkflowShortcut,
    snapshot: CodeWorkspacePrSnapshot | null,
    watching = false,
  ): string {
    const resolution = resolveWorkflowShortcut(
      shortcut,
      workspaceWorkflowModel(snapshot),
      watching,
    );
    if ("run" in resolution) return resolution.run;
    if ("stopWatch" in resolution) return "stop_watch";
    if ("autoMerge" in resolution) return "auto_merge";
    return `blocked: ${resolution.blocked}`;
  }

  it("carries one chord through commit, push, create, and view", () => {
    // Cmd+Shift+P names an intent, not an action: the reader means "get this in
    // front of reviewers" at every stage, and the action that serves it changes
    // under them. Uncommitted work with no pull request gets the drafted
    // commit-and-open request; with one open, it goes to the commit box.
    expect(chord("pull_request", { ...CLEAN, dirty: true })).toBe("compose_pr");
    expect(chord("pull_request", { ...CLEAN, dirty: true, pr: pr({}) })).toBe(
      "open_source",
    );
    expect(chord("pull_request", { ...CLEAN, unpushed: true, ahead: 1 })).toBe(
      "push",
    );
    expect(chord("pull_request", { ...CLEAN, ahead: 2 })).toBe("create_pr");
    expect(
      chord("pull_request", { ...CLEAN, pr: pr({ url: "https://x/41" }) }),
    ).toBe("open_pr");
    expect(chord("pull_request", CLEAN)).toBe(
      "blocked: No commits to open a pull request for",
    );
  });

  it("runs whatever the header's primary control says next", () => {
    // The point of Cmd+Shift+Enter: one chord the reader can hold down the
    // whole workflow, wired to the same decision the button draws.
    expect(chord("next", { ...CLEAN, unpushed: true, ahead: 1 })).toBe("push");
    expect(chord("next", { ...CLEAN, pr: pr({ draft: true }) })).toBe(
      "mark_ready",
    );
    expect(
      chord("next", {
        ...CLEAN,
        pr: pr({ mergeable: "mergeable", merge_state_status: "clean" }),
      }),
    ).toBe("merge");
    // A clean workspace has no next step, and says which state it is in rather
    // than swallowing the key.
    expect(chord("next", CLEAN)).toBe("blocked: Workspace is clean");
  });

  it("picks conflict resolution over a plain rebase, and refuses over dirt", () => {
    expect(
      chord("update_branch", {
        ...CLEAN,
        pr: pr({ mergeable: "conflicting" }),
      }),
    ).toBe("resolve_conflicts");
    expect(
      chord("update_branch", {
        ...CLEAN,
        pr: pr({ merge_state_status: "behind" }),
      }),
    ).toBe("update_branch");
    // Rebasing over uncommitted work is how it gets lost, and the snapshot
    // already knows the worktree is dirty.
    expect(chord("update_branch", { ...CLEAN, dirty: true, pr: pr({}) })).toBe(
      "blocked: Commit or discard your changes before rebasing",
    );
    expect(chord("update_branch", CLEAN)).toBe("blocked: No pull request yet");
  });

  it("stops a running watch with the key that started it", () => {
    const withPr = { ...CLEAN, pr: pr({}) };
    expect(chord("watch", withPr)).toBe("watch_and_fix");
    expect(chord("watch", withPr, true)).toBe("stop_watch");
  });

  it("refuses the chords that would start a second agent in a watched worktree", () => {
    // The header disables the same actions for the same reason: two agents in
    // one worktree is a corrupt checkout, not a race worth running.
    const withPr = { ...CLEAN, pr: pr({ url: "https://x/41" }) };
    const busy =
      "blocked: A watch task is already working on this pull request";
    expect(chord("next", withPr, true)).toBe(busy);
    expect(chord("merge", withPr, true)).toBe(busy);
    expect(chord("update_branch", withPr, true)).toBe(busy);
    // Reading and pushing do not contend, so they stay live.
    expect(chord("view_pr", withPr, true)).toBe("open_pr");
    expect(chord("source_control", withPr, true)).toBe("open_source");
  });

  it("merges only when the pull request is green", () => {
    // Merging publishes to a shared branch, so the chord runs the real merge
    // rather than asking an agent to. That makes "green" a question it has to
    // answer honestly, from the same table the review sidebar's Merge button
    // reads — otherwise the chord offers merges the button refuses.
    const green = {
      ...CLEAN,
      pr: pr({ mergeable: "mergeable", merge_state_status: "clean" }),
    };
    expect(chord("merge", green)).toBe("merge");

    // Not green but still landable: the reader means "get this in" either way,
    // so the chord arms auto-merge and GitHub finishes the job. Which one is
    // about to happen is named in the confirmation, not guessed at here.
    for (const snapshot of [
      pr({ checks: [{ name: "ci", bucket: "fail" }] }),
      pr({ checks: [{ name: "ci", bucket: "pending" }] }),
      pr({ merge_state_status: "behind" }),
      pr({ review_decision: "changes_requested" }),
    ]) {
      expect(chord("merge", { ...CLEAN, pr: snapshot })).toBe("auto_merge");
    }

    // Neither path is open in these states, so the chord says why. Conflicts
    // and drafts cannot even be queued, and the last two are already landing.
    for (const [snapshot, reason] of [
      [
        pr({ draft: true }),
        "Mark the pull request ready for review on GitHub before merging it.",
      ],
      [
        pr({ mergeable: "conflicting" }),
        "Resolve the merge conflicts before merging directly.",
      ],
      [
        pr({ in_merge_queue: true }),
        "This pull request is already waiting in the merge queue.",
      ],
      [
        pr({ auto_merge_enabled: true }),
        "Auto-merge is already enabled and will merge after the remaining requirements pass.",
      ],
    ] as const) {
      expect(chord("merge", { ...CLEAN, pr: snapshot })).toBe(
        `blocked: ${reason}`,
      );
    }

    // Local state blocks too: work that is not in the pull request would be
    // left behind by a merge the reader thought was landing all of it.
    expect(chord("merge", { ...green, dirty: true })).toBe(
      "blocked: Commit or discard your changes before merging",
    );
    expect(chord("merge", { ...green, unpushed: true })).toBe(
      "blocked: Push your local commits before merging",
    );
  });

  it("says why a merged or closed pull request has nothing to do", () => {
    const merged = { ...CLEAN, pr: pr({ state: "merged" }) };
    expect(chord("merge", merged)).toBe(
      "blocked: Pull request #41 is already merged",
    );
    expect(chord("watch", { ...CLEAN, pr: pr({ state: "closed" }) })).toBe(
      "blocked: Pull request #41 is closed",
    );
    expect(chord("view_pr", CLEAN)).toBe(
      "blocked: No pull request to open yet",
    );
  });

  it("opens source control before the status has loaded", () => {
    // The review rail is chrome, not a Git operation. Everything else waits for
    // a snapshot rather than acting on a guess.
    expect(chord("source_control", null)).toBe("open_source");
    expect(chord("merge", null)).toBe(
      "blocked: Still reading this workspace's status",
    );
  });
});

describe("composePrPrompt", () => {
  it("names the base branch and keeps merging off the agent", () => {
    const prompt = composePrPrompt("main");
    expect(prompt).toContain("open a pull request against `main`");
    expect(prompt).toContain("Do not merge.");
    // A workspace without a recorded base still gets a sendable request.
    expect(composePrPrompt(" ")).toContain("the default branch");
  });
});

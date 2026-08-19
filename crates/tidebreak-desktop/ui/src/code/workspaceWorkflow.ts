import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import {
  prWorkflowStatus,
  type PrCheckCounts,
  type PrWorkflowAction,
} from "./prActions";

export type WorkspaceWorkflowAction =
  | PrWorkflowAction
  | "open_source"
  | "push"
  | "create_pr"
  | "open_pr";

export type WorkspaceWorkflowTone =
  | "neutral"
  | "ready"
  | "pending"
  | "warning"
  | "critical";

export type WorkspaceWorkflowModel = {
  stage:
    | "loading"
    | "checking"
    | "clean"
    | "dirty"
    | "unpushed"
    | "github_setup"
    | "ready_for_pr"
    | "draft"
    | "conflict"
    | "behind"
    | "blocked"
    | "changes_requested"
    | "failing"
    | "queued"
    | "auto_merge"
    | "pending"
    | "ready"
    | "merged"
    | "closed";
  tone: WorkspaceWorkflowTone;
  summary: string;
  title: string;
  detail: string;
  primary?: WorkspaceWorkflowAction;
  secondary: WorkspaceWorkflowAction[];
  pr?: PullRequestDigest;
  checks?: PrCheckCounts;
};

/** One compact model for the local Git path and the hosted PR path. */
export function workspaceWorkflowModel(
  snapshot: CodeWorkspacePrSnapshot | null,
  fallbackPr?: PullRequestDigest,
): WorkspaceWorkflowModel {
  const pr = snapshot === null ? fallbackPr : snapshot.pr;
  if (!snapshot) {
    if (pr) return pullRequestWorkflow(pr);
    return {
      stage: "loading",
      tone: "neutral",
      summary: "Checking…",
      title: "Checking workspace status",
      detail: "Reading the branch and pull-request state.",
      secondary: [],
    };
  }
  if (snapshot.dirty) {
    return {
      stage: "dirty",
      tone: "warning",
      summary: pr ? `#${pr.number} · Changes` : "Uncommitted changes",
      title: pr
        ? `Pull request #${pr.number} has local changes`
        : "Changes need review",
      detail: "Review the worktree and choose a commit message.",
      primary: "open_source",
      secondary: pr?.url ? ["open_pr"] : [],
      pr,
    };
  }
  if (snapshot.unpushed) {
    return {
      stage: "unpushed",
      tone: "warning",
      summary: pr ? `#${pr.number} · Unpushed` : "Unpushed changes",
      title: "Local commits are ready",
      detail: pr
        ? `Push the latest commits to update pull request #${pr.number}.`
        : "Push the latest local commits to origin.",
      primary: "push",
      secondary: pr?.url ? ["open_source", "open_pr"] : ["open_source"],
      pr,
    };
  }
  if (pr) return pullRequestWorkflow(pr);
  if (snapshot.ahead > 0) {
    if (!snapshot.gh_found || snapshot.gh_authenticated === false) {
      return {
        stage: "github_setup",
        tone: "warning",
        summary: "GitHub setup",
        title: "GitHub is not ready",
        detail: snapshot.gh_found
          ? "Sign in to GitHub CLI before creating a pull request."
          : "Install GitHub CLI before creating a pull request.",
        primary: "open_source",
        secondary: [],
      };
    }
    return {
      stage: "ready_for_pr",
      tone: "ready",
      summary: "Ready for PR",
      title: "The branch is ready",
      detail: "The latest commits are pushed and ready for a pull request.",
      primary: "create_pr",
      secondary: ["open_source"],
    };
  }
  return {
    stage: "clean",
    tone: "neutral",
    summary: "No changes",
    title: "Workspace is clean",
    detail: "Changes and pull-request actions will appear here.",
    secondary: ["open_source"],
  };
}

function pullRequestWorkflow(pr: PullRequestDigest): WorkspaceWorkflowModel {
  const status = prWorkflowStatus(pr);
  const common = {
    pr,
    checks: status.checks,
  };
  switch (status.state) {
    case "checking":
      return {
        ...common,
        stage: "checking",
        tone: "pending",
        summary: `#${pr.number} · Checking`,
        title: `Pull request #${pr.number} is checking`,
        detail:
          "GitHub is still determining whether this pull request can merge.",
        primary: "watch_and_fix",
        secondary: withOpenPr(pr, []),
      };
    case "draft":
      return {
        ...common,
        stage: "draft",
        tone: "neutral",
        summary: `#${pr.number} · Draft`,
        title: `Pull request #${pr.number} is a draft`,
        detail: "The pull request is not accepting review yet.",
        primary: "mark_ready",
        secondary: withOpenPr(pr, ["watch_and_fix"]),
      };
    case "conflict":
      return {
        ...common,
        stage: "conflict",
        tone: "warning",
        summary: `#${pr.number} · Conflicts`,
        title: `Pull request #${pr.number} has conflicts`,
        detail: "Update the branch before it can merge.",
        primary: "resolve_conflicts",
        secondary: withOpenPr(pr, ["watch_and_fix"]),
      };
    case "behind":
      return {
        ...common,
        stage: "behind",
        tone: "warning",
        summary: `#${pr.number} · Behind`,
        title: `Pull request #${pr.number} is behind`,
        detail: `Update the branch from ${pr.base_branch ?? "its base branch"}.`,
        primary: "update_branch",
        secondary: withOpenPr(pr, []),
      };
    case "blocked":
      return {
        ...common,
        stage: "blocked",
        tone: "warning",
        summary: `#${pr.number} · Blocked`,
        title: `Pull request #${pr.number} is blocked`,
        detail: "A review or repository requirement is still outstanding.",
        primary: "watch_and_fix",
        secondary: withOpenPr(pr, []),
      };
    case "changes_requested":
      return {
        ...common,
        stage: "changes_requested",
        tone: "critical",
        summary: `#${pr.number} · Changes requested`,
        title: `Pull request #${pr.number} needs changes`,
        detail: "Review feedback needs a code or response update.",
        primary: "address_feedback",
        secondary: withOpenPr(pr, ["watch_and_fix"]),
      };
    case "failing":
      return {
        ...common,
        stage: "failing",
        tone: "critical",
        summary: `#${pr.number} · ${status.checks.failing} failing`,
        title: `Pull request #${pr.number} needs attention`,
        detail: checkSummary(status.checks),
        primary: "fix_errors",
        secondary: withOpenPr(pr, ["watch_and_fix"]),
      };
    case "queued":
      return {
        ...common,
        stage: "queued",
        tone: "pending",
        summary: `#${pr.number} · Queued`,
        title: `Pull request #${pr.number} is queued`,
        detail: "GitHub will merge it when the queue requirements pass.",
        primary: pr.url ? "open_pr" : "watch_and_fix",
        secondary: pr.url ? ["watch_and_fix"] : [],
      };
    case "auto_merge":
      return {
        ...common,
        stage: "auto_merge",
        tone: "pending",
        summary: `#${pr.number} · Auto-merge on`,
        title: `Auto-merge is on for #${pr.number}`,
        detail: "GitHub will merge when the remaining requirements pass.",
        primary: pr.url ? "open_pr" : "watch_and_fix",
        secondary: pr.url ? ["watch_and_fix"] : [],
      };
    case "pending":
      return {
        ...common,
        stage: "pending",
        tone: "pending",
        summary: `#${pr.number} · ${status.checks.pending} pending`,
        title: `Pull request #${pr.number} is checking`,
        detail: checkSummary(status.checks),
        primary: "watch_and_fix",
        secondary: withOpenPr(pr, []),
      };
    case "ready":
      return {
        ...common,
        stage: "ready",
        tone: "ready",
        summary: `#${pr.number} · Ready`,
        title: `Pull request #${pr.number} is ready`,
        detail: checkSummary(status.checks) || "No blockers reported.",
        primary: "merge",
        secondary: withOpenPr(pr, ["watch_and_fix"]),
      };
    case "merged":
      return {
        ...common,
        stage: "merged",
        tone: "ready",
        summary: `#${pr.number} · Merged`,
        title: `Pull request #${pr.number} merged`,
        detail: "The change has landed on the base branch.",
        primary: pr.url ? "open_pr" : undefined,
        secondary: [],
      };
    case "closed":
      return {
        ...common,
        stage: "closed",
        tone: "critical",
        summary: `#${pr.number} · Closed`,
        title: `Pull request #${pr.number} is closed`,
        detail: "This pull request was closed without merging.",
        primary: pr.url ? "open_pr" : undefined,
        secondary: [],
      };
  }
}

function withOpenPr(
  pr: PullRequestDigest,
  actions: WorkspaceWorkflowAction[],
): WorkspaceWorkflowAction[] {
  return pr.url ? [...actions, "open_pr"] : actions;
}

export function checkSummary(checks: PrCheckCounts): string {
  const parts: string[] = [];
  if (checks.passing > 0) parts.push(`${checks.passing} passing`);
  if (checks.pending > 0) parts.push(`${checks.pending} pending`);
  if (checks.failing > 0) parts.push(`${checks.failing} failing`);
  if (checks.skipped > 0) parts.push(`${checks.skipped} skipped`);
  return parts.join(" · ");
}

export function workspaceWorkflowActionLabel(
  action: WorkspaceWorkflowAction,
  stage: WorkspaceWorkflowModel["stage"],
): string {
  switch (action) {
    case "open_source":
      if (stage === "github_setup") return "Set up GitHub";
      return stage === "dirty" ? "Review & commit" : "Source control";
    case "push":
      return "Push";
    case "create_pr":
      return "Open PR";
    case "open_pr":
      if (stage === "queued") return "View queue";
      return "View PR";
    case "watch_and_fix":
      if (stage === "blocked") return "Handle blockers";
      return "Watch and fix";
    case "mark_ready":
      return "Mark ready";
    case "merge":
      return "Merge";
    case "fix_errors":
      return "Fix CI";
    case "address_feedback":
      return "Address feedback";
    case "update_branch":
      return "Update branch";
    case "resolve_conflicts":
      return "Resolve conflicts";
  }
}

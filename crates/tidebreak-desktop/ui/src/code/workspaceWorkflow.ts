import type { CodeWorkspacePrSnapshot, PullRequestDigest } from "../api/types";
import {
  prDirectMergeAction,
  prMergeControls,
  prWorkflowStatus,
  type PrCheckCounts,
  type PrWorkflowAction,
} from "./prActions";

export type WorkspaceWorkflowAction =
  | PrWorkflowAction
  | "open_source"
  | "push"
  | "create_pr"
  | "compose_pr"
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
    | "needs_approval"
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
    if (pr) {
      return {
        stage: "dirty",
        tone: "warning",
        summary: `#${pr.number} · Changes`,
        title: `Pull request #${pr.number} has local changes`,
        detail: "Review the worktree and choose a commit message.",
        primary: "open_source",
        secondary: pr.url ? ["open_pr"] : [],
        pr,
      };
    }
    // No pull request yet, so the useful next step is the whole trip: Create
    // PR drafts the commit-push-open request into the composer for the reader
    // to send. Hand review stays one step away in the menu.
    return {
      stage: "dirty",
      tone: "warning",
      summary: "Uncommitted changes",
      title: "Changes need a pull request",
      detail:
        "Create PR drafts a request in the composer to commit, push, and open a pull request.",
      primary: "compose_pr",
      secondary: ["open_source"],
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
        detail: "A repository requirement is still outstanding.",
        primary: "watch_and_fix",
        secondary: withOpenPr(pr, []),
      };
    case "needs_approval":
      return {
        ...common,
        stage: "needs_approval",
        tone: "warning",
        summary: `#${pr.number} · Needs approval`,
        title: `Pull request #${pr.number} needs approval`,
        detail:
          "GitHub requires a review approval before this pull request can merge.",
        primary: pr.url ? "open_pr" : "watch_and_fix",
        secondary: pr.url ? ["watch_and_fix"] : [],
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
        tone: "warning",
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

/**
 * A keyboard ask against the workflow.
 *
 * A chord names an intent, not an action: `pull_request` means "get this
 * branch in front of reviewers", which is a drafted commit-and-open request on
 * a dirty worktree, a push on an unpushed branch, a create on a pushed one,
 * and a trip to GitHub once the pull request exists. Binding chords to intents
 * rather than to actions is what lets the reader press the same key at every
 * stage and always get the useful thing.
 */
export type WorkflowShortcut =
  | "next"
  | "pull_request"
  | "update_branch"
  | "watch"
  | "merge"
  | "view_pr"
  | "source_control";

/**
 * What a chord does in the state the workspace is actually in.
 *
 * `blocked` carries the sentence to show the reader. A chord that quietly did
 * nothing would be indistinguishable from one that did not fire, which is the
 * fastest way to lose trust in a keyboard-driven surface.
 */
export type WorkflowShortcutResolution =
  | { run: WorkspaceWorkflowAction }
  | { stopWatch: true }
  /** Arm host auto-merge rather than merging now. */
  | { autoMerge: true }
  | { blocked: string };

/**
 * Resolve a chord against the current workflow model.
 *
 * `watching` mirrors the header's own rule: while a watch task holds the
 * worktree, the chords that would start a second agent in it are refused, and
 * the ones that only read or push are not.
 */
export function resolveWorkflowShortcut(
  shortcut: WorkflowShortcut,
  model: WorkspaceWorkflowModel,
  watching: boolean,
): WorkflowShortcutResolution {
  // The review rail is chrome, not a Git operation: it opens whatever else is
  // going on, including while the status is still loading.
  if (shortcut === "source_control") return { run: "open_source" };
  if (shortcut === "view_pr") {
    return model.pr?.url
      ? { run: "open_pr" }
      : { blocked: "No pull request to open yet" };
  }
  if (model.stage === "loading") {
    return { blocked: "Still reading this workspace's status" };
  }
  if (shortcut === "watch") {
    if (watching) return { stopWatch: true };
    return prBlocker(model) ?? { run: "watch_and_fix" };
  }
  if (watching) {
    return { blocked: "A watch task is already working on this pull request" };
  }
  switch (shortcut) {
    case "next":
      // `title` rather than `detail`: the title is the one-line statement of
      // what this workspace is, which is exactly the answer to "why did
      // nothing happen".
      return model.primary ? { run: model.primary } : { blocked: model.title };
    case "pull_request":
      // Commit and push come first whether or not a pull request exists, so
      // these stages lead. With no pull request yet, uncommitted work gets the
      // drafted request that carries it all the way; with one open, new local
      // changes go through the commit box to update it.
      if (model.stage === "dirty") {
        return { run: model.pr ? "open_source" : "compose_pr" };
      }
      if (model.stage === "github_setup") return { run: "open_source" };
      if (model.stage === "unpushed") return { run: "push" };
      if (model.stage === "ready_for_pr") return { run: "create_pr" };
      if (model.pr) {
        return model.pr.url
          ? { run: "open_pr" }
          : { blocked: `Pull request #${model.pr.number} has no URL yet` };
      }
      return { blocked: "No commits to open a pull request for" };
    case "update_branch":
      // A rebase over uncommitted work is the classic way to lose it, and the
      // snapshot already knows. Send the reader to the commit box instead.
      if (model.stage === "dirty") {
        return { blocked: "Commit or discard your changes before rebasing" };
      }
      return (
        prBlocker(model) ?? {
          run:
            model.stage === "conflict" ? "resolve_conflicts" : "update_branch",
        }
      );
    case "merge":
      return prBlocker(model) ?? mergeIfGreen(model);
  }
}

/**
 * Land the pull request: merge it now when it is green, otherwise ask GitHub
 * to merge it once the remaining requirements pass.
 *
 * One chord for one intent. The reader pressing it means "this is done, get it
 * in" whether the checks have finished or not, and the two ways to say that
 * differ only in when GitHub acts. The confirmation names which one is about
 * to happen, so the difference is visible before anything lands.
 *
 * Merging publishes to a shared branch and is the step decision 42 keeps for
 * the user, so the chord runs the real merge rather than asking an agent to.
 * That makes "is it green" a question this has to answer honestly: the same
 * `prMergeControls` table the review sidebar's Merge button reads, so the
 * chord can never offer a merge the button refuses.
 *
 * Local state blocks both paths. Uncommitted or unpushed work is not in the
 * pull request, and landing without it leaves behind work the reader thought
 * was going in.
 */
function mergeIfGreen(
  model: WorkspaceWorkflowModel,
): WorkflowShortcutResolution {
  if (model.stage === "dirty") {
    return { blocked: "Commit or discard your changes before merging" };
  }
  if (model.stage === "unpushed") {
    return { blocked: "Push your local commits before merging" };
  }
  const pr = model.pr;
  if (!pr) return { blocked: "No pull request yet" };
  const action = prDirectMergeAction(pr);
  if (action?.kind === "merge") return { run: "merge" };
  if (action) return { autoMerge: true };
  const controls = prMergeControls(prWorkflowStatus(pr).state);
  return {
    blocked:
      controls.explanation ?? `Pull request #${pr.number} cannot merge yet`,
  };
}

/** Why a pull request cannot be acted on, or `null` when it can. */
function prBlocker(model: WorkspaceWorkflowModel): { blocked: string } | null {
  if (!model.pr) return { blocked: "No pull request yet" };
  if (model.stage === "merged") {
    return { blocked: `Pull request #${model.pr.number} is already merged` };
  }
  if (model.stage === "closed") {
    return { blocked: `Pull request #${model.pr.number} is closed` };
  }
  return null;
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
    case "compose_pr":
      return "Create PR";
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

/**
 * The request the Create PR control drafts into the composer.
 *
 * Offered as a draft rather than run: opening a pull request publishes the
 * branch, so the reader reads, edits, and sends the request instead of the
 * button firing it. The agent it reaches sits in the worktree and can see the
 * diff, so the prompt says what to do with the changes rather than repeating
 * them. Merging stays with the user per decision 42.
 */
export function composePrPrompt(base?: string): string {
  const target = base?.trim() ? `\`${base.trim()}\`` : "the default branch";
  return [
    "Review the uncommitted changes in this workspace, run focused validation,",
    "commit with a clear message (split unrelated work into separate commits),",
    `push the branch, and open a pull request against ${target}.`,
    "Give the pull request a title and description that summarize what changed",
    "and why. Do not merge.",
  ].join(" ");
}

import { create } from "zustand";

/**
 * Agent prompts the workspace actions send, plus the settings that edit them.
 *
 * Clicking Create PR, Fix checks, and the other prompt actions submits this text
 * into the workspace chat. Settings stores only the deviations from the
 * defaults; an empty or matching value is the shipped wording.
 */

export const WORKFLOW_PROMPT_IDS = [
  "compose_pr",
  "update_pr",
  "follow_up_pr",
  "watch_and_fix",
  "sync_branch",
  "resolve_divergence",
  "resolve_local_conflicts",
  "fix_errors",
  "address_feedback",
  "update_branch",
  "resolve_conflicts",
] as const;

export type WorkflowPromptId = (typeof WORKFLOW_PROMPT_IDS)[number];

export const DEFAULT_WORKFLOW_PROMPTS: Record<WorkflowPromptId, string> = {
  compose_pr: [
    "Inspect this workspace and check whether its branch already has an open pull request. If it does, update that pull request instead of creating a duplicate. Review the local changes and commits, run focused validation,",
    "commit with a clear message (split unrelated work into separate commits),",
    "push the branch, and open a pull request against {base}.",
    "Give the pull request a title and description that summarize what changed",
    "and why. Do not merge.",
  ].join(" "),
  update_pr: [
    "Update pull request {pr} from this workspace. Verify its repository and head branch first.",
    "Review the staged, unstaged, and untracked changes, run focused validation, commit the intended changes with a clear message, and push all unpushed commits to the PR head branch.",
    "If the PR has merged or closed, stop and explain that a follow-up PR is needed. Do not merge or change draft status.",
  ].join(" "),
  follow_up_pr: [
    "Create a follow-up to pull request {pr} against {base}. Inspect the PR outcome and identify only the remaining work.",
    "Preserve all local changes. Create a fresh branch from the current base and carry over only the follow-up changes, without replaying changes that already landed.",
    "Run focused validation, commit, push, and create the follow-up PR. Do not merge.",
  ].join(" "),
  watch_and_fix: [
    "Watch and fix pull request {pr} in this conversation. Verify its repository, branch, and current head SHA before acting.",
    "Monitor checks, issue comments, formal reviews, and inline review comments. Follow the repository's polling cadence and review requirements.",
    "For actionable failures or feedback, inspect the evidence, make the smallest appropriate change, run focused validation, commit, and push to the PR head branch. After every push, monitor the new head and wait for its required checks and reviews.",
    "Continue until the PR is ready to merge, has merged or closed, or needs a decision or permission you cannot supply. Report the exact blocker when you stop. Do not merge, enable auto-merge, change draft status, or create a separate watch session.",
  ].join(" "),
  sync_branch:
    "Inspect the workspace branch and upstream. Preserve local work, fetch the upstream, and fast-forward when possible. If histories diverge, explain the divergence before choosing a safe reconciliation. Do not discard changes or merge a pull request.",
  resolve_divergence:
    "Inspect the workspace branch and upstream divergence. Preserve local changes and commits, fetch the remote, reconcile the histories, resolve conflicts, and run focused validation. Never force-push over work you have not accounted for. Do not merge a pull request.",
  resolve_local_conflicts:
    "Inspect the unfinished Git operation in this workspace. Preserve local work, resolve the conflicts, complete the merge or rebase when appropriate, and run focused validation. Do not merge a pull request.",
  fix_errors: [
    "Pull request {pr} has failing checks. Inspect the latest failing CI logs",
    "for the current head SHA, reproduce the cause when practical, make the",
    "smallest safe fix in this workspace, run focused validation, commit, and",
    "push. Do not merge.",
  ].join(" "),
  address_feedback: [
    "Pull request {pr} has requested changes. Inspect the latest unresolved",
    "review feedback, implement each actionable request in this workspace, run",
    "focused validation, commit, push, and reply where context is useful. Do",
    "not merge.",
  ].join(" "),
  update_branch: [
    "Update pull request {pr} from {base}. Fetch the latest base branch,",
    "rebase this workspace branch onto it, resolve any conflicts, run focused",
    "validation, and push the updated head. Do not merge.",
  ].join(" "),
  resolve_conflicts: [
    "Pull request {pr} has merge conflicts with {base}. Fetch and rebase",
    "onto {base}, resolve every conflict in this workspace, run focused",
    "validation, commit if needed, and push the updated head. Do not merge the",
    "pull request.",
  ].join(" "),
};

/** Uncustomized Fix checks wording when job logs are already on disk. */
export const DEFAULT_FIX_ERRORS_WITH_LOGS = [
  "Pull request {pr} has failing checks, and their job logs are already",
  "downloaded — read them first. Reproduce the cause when practical, make the",
  "smallest safe fix in this workspace, run focused validation, commit, and",
  "push. Do not merge.",
].join(" ");

export const WORKFLOW_PROMPT_FIELDS: readonly {
  id: WorkflowPromptId;
  label: string;
  hint: string;
}[] = [
  {
    id: "compose_pr",
    label: "Create PR",
    hint: "{base} is the target branch. Reviews, commits, and publishes the workspace changes.",
  },
  {
    id: "update_pr",
    label: "Update PR",
    hint: "{pr} is the existing pull request. Commits and pushes the workspace changes.",
  },
  {
    id: "follow_up_pr",
    label: "Create follow-up PR",
    hint: "{pr} is the previous pull request. {base} is the target branch.",
  },
  {
    id: "watch_and_fix",
    label: "Watch and fix",
    hint: "Monitors {pr} and handles actionable failures in the workspace conversation.",
  },
  {
    id: "sync_branch",
    label: "Sync branch",
    hint: "Fetches and reconciles the workspace with its upstream.",
  },
  {
    id: "resolve_divergence",
    label: "Resolve divergence",
    hint: "Preserves and reconciles local and remote commits.",
  },
  {
    id: "resolve_local_conflicts",
    label: "Resolve local conflicts",
    hint: "Completes an unfinished merge or rebase.",
  },
  {
    id: "fix_errors",
    label: "Fix checks",
    hint: "{pr} is the pull request number. Downloaded job logs are named after this prompt.",
  },
  {
    id: "address_feedback",
    label: "Address feedback",
    hint: "{pr} is the pull request number. Live review state is appended after this prompt.",
  },
  {
    id: "update_branch",
    label: "Update branch",
    hint: "{pr} is the pull request number. {base} is the base branch.",
  },
  {
    id: "resolve_conflicts",
    label: "Resolve conflicts",
    hint: "{pr} is the pull request number. {base} is the base branch.",
  },
];

const STORAGE_KEY = "tidebreak.workflowPrompts";

export type WorkflowPromptVars = {
  base?: string;
  pr?: string;
};

export function interpolateWorkflowPrompt(
  template: string,
  vars: WorkflowPromptVars,
): string {
  return template.replace(/\{(base|pr)\}/g, (match, key: string) => {
    if (key === "base") return vars.base ?? match;
    if (key === "pr") return vars.pr ?? match;
    return match;
  });
}

export function isWorkflowPromptId(value: string): value is WorkflowPromptId {
  return (WORKFLOW_PROMPT_IDS as readonly string[]).includes(value);
}

function storage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}

function readStoredOverrides(): Partial<Record<WorkflowPromptId, string>> {
  try {
    const raw = storage()?.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }
    const overrides: Partial<Record<WorkflowPromptId, string>> = {};
    for (const [key, value] of Object.entries(parsed)) {
      if (!isWorkflowPromptId(key) || typeof value !== "string") continue;
      if (value === DEFAULT_WORKFLOW_PROMPTS[key]) continue;
      overrides[key] = value;
    }
    return overrides;
  } catch {
    return {};
  }
}

function storeOverrides(overrides: Partial<Record<WorkflowPromptId, string>>) {
  const local = storage();
  if (!local) return;
  try {
    if (Object.keys(overrides).length === 0) {
      local.removeItem(STORAGE_KEY);
      return;
    }
    local.setItem(STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // Preference persistence is best-effort.
  }
}

export type WorkflowPromptStore = {
  overrides: Partial<Record<WorkflowPromptId, string>>;
  setPrompt: (id: WorkflowPromptId, value: string) => void;
  resetPrompt: (id: WorkflowPromptId) => void;
};

export function createWorkflowPromptStore() {
  return create<WorkflowPromptStore>()((set, get) => ({
    overrides: readStoredOverrides(),
    setPrompt: (id, value) => {
      const overrides = { ...get().overrides };
      if (value === DEFAULT_WORKFLOW_PROMPTS[id]) {
        delete overrides[id];
      } else {
        overrides[id] = value;
      }
      storeOverrides(overrides);
      set({ overrides });
    },
    resetPrompt: (id) => {
      const overrides = { ...get().overrides };
      delete overrides[id];
      storeOverrides(overrides);
      set({ overrides });
    },
  }));
}

export const useWorkflowPromptStore = createWorkflowPromptStore();

/** The text the settings field shows, default when nothing is stored. */
export function workflowPromptDraft(
  id: WorkflowPromptId,
  overrides: Partial<Record<WorkflowPromptId, string>> = currentOverrides(),
): string {
  return overrides[id] ?? DEFAULT_WORKFLOW_PROMPTS[id];
}

export function workflowPromptIsCustom(
  id: WorkflowPromptId,
  overrides: Partial<Record<WorkflowPromptId, string>> = currentOverrides(),
): boolean {
  return Object.hasOwn(overrides, id);
}

function currentOverrides(): Partial<Record<WorkflowPromptId, string>> {
  return useWorkflowPromptStore.getState().overrides;
}

/**
 * The instruction one action sends, after placeholders and any override.
 *
 * Uncustomized Fix checks keeps a second default when job logs are already
 * downloaded, so the agent is told to read them first. A stored override
 * replaces both wordings; the log paths still follow the instruction.
 */
export function renderWorkflowPrompt(
  id: WorkflowPromptId,
  vars: WorkflowPromptVars,
  options?: { logsAttached?: boolean },
): string {
  const override = useWorkflowPromptStore.getState().overrides[id];
  const template =
    override !== undefined
      ? override
      : id === "fix_errors" && options?.logsAttached
        ? DEFAULT_FIX_ERRORS_WITH_LOGS
        : DEFAULT_WORKFLOW_PROMPTS[id];
  const source = template.trim() ? template : DEFAULT_WORKFLOW_PROMPTS[id];
  return interpolateWorkflowPrompt(source, vars);
}

/** Test helper: drop overrides without touching unrelated stores. */
export function resetWorkflowPromptStore(): void {
  storeOverrides({});
  useWorkflowPromptStore.setState({ overrides: {} });
}

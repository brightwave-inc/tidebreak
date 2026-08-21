import type {
  Attention,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunDetail,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWatchSnapshot,
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
  HarnessCaps,
  HarnessDoctorEntry,
  HarnessDoctorReport,
  PendingUserQuestions,
  TaskPlan,
  ToolActionPreview,
} from "@/api";
import type { CodeDeliveryNotification } from "@/code/CodeDeliveryStore";

export const taskPlan: TaskPlan = {
  turn_id: "turn-storybook",
  updated_at: "2026-08-19T12:00:00Z",
  steps: [
    { content: "Inspect the current component boundary", status: "completed" },
    { content: "Build the isolated UI state", status: "in_progress" },
    { content: "Exercise the narrow layout", status: "pending" },
    { content: "Verify the finished state", status: "pending" },
  ],
};

export const execPreview: Extract<ToolActionPreview, { tool: "exec" }> = {
  tool: "exec",
  command: "pnpm",
  args: ["test", "--", "TaskPlanCard.dom.test.tsx"],
  cwd: "crates/tidebreak-desktop/ui",
  files: ["src/TaskPlanCard.tsx", "src/TaskPlanCard.dom.test.tsx"],
  summary: "Run the focused component tests",
};

export const userQuestions: PendingUserQuestions = {
  callId: "call-storybook",
  turnId: "turn-storybook",
  askedAt: "2026-08-19T12:00:00Z",
  questions: [
    {
      id: "scope",
      header: "Scope",
      question: "Which UI states should this workshop cover first?",
      options: [
        {
          id: "conversation",
          label: "Conversation states",
          description: "Approvals, plans, questions, messages, and tool activity.",
        },
        {
          id: "code",
          label: "Code workspace states",
          description: "Git, diffs, approvals, and workspace status.",
        },
        {
          id: "documents",
          label: "Document viewers",
          description: "Heavier browser surfaces that can follow in a later slice.",
        },
      ],
      questionType: "multi_select",
      allowFreeForm: true,
    },
    {
      id: "gate",
      header: "Gate",
      question: "Should visual snapshots block pull requests yet?",
      options: [
        {
          id: "no",
          label: "No, keep it exploratory",
          description: "Build the habit before making screenshots a required check.",
        },
        {
          id: "yes",
          label: "Yes, gate immediately",
          description: "Treat every captured visual change as review-required.",
        },
      ],
      questionType: "single_select",
      allowFreeForm: false,
    },
  ],
};

export const cleanGit: CodeWorkspacePrSnapshot = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: false,
  suggested_commit_message: "Add the scoped Storybook workshop",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

export const dirtyGit: CodeWorkspacePrSnapshot = {
  ...cleanGit,
  dirty: true,
};

export const unpushedGit: CodeWorkspacePrSnapshot = {
  ...cleanGit,
  unpushed: true,
  ahead: 1,
};

export const readyForPrGit: CodeWorkspacePrSnapshot = {
  ...cleanGit,
  ahead: 1,
  has_upstream: true,
};

export const openPrGit: CodeWorkspacePrSnapshot = {
  ...readyForPrGit,
  pr: {
    number: 184,
    url: "https://github.com/example/tidebreak/pull/184",
    state: "open",
    title: "Add a scoped UI workshop",
    checks_summary: "8 passing, 1 pending, 0 failing",
  },
};

const watchBase: CodeWatchSnapshot = {
  id: "watch-1",
  workspace_id: "ws-1",
  session_id: "sess-watch",
  pr_number: 184,
  state: "watching",
  cycles: 0,
  created_at: "2026-08-20T09:00:00.000Z",
  updated_at: "2026-08-20T09:05:00.000Z",
};

/** Watch task polling a PR whose checks are still running. */
export const watchingPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    checks: [
      { name: "test", bucket: "pass" },
      { name: "clippy", bucket: "pending" },
    ],
  },
  watch: watchBase,
};

/** Watch task driving a fix turn against failing checks. */
export const fixingPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    checks_summary: "7 passing, 1 failing",
    checks: [
      { name: "test", bucket: "pass" },
      { name: "clippy", bucket: "fail", detail: "exit 101" },
    ],
  },
  watch: {
    ...watchBase,
    state: "fixing",
    detail: "fixing failing checks",
    cycles: 2,
  },
};

/** Watch task parked on something only the user can do. */
export const blockedWatchPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    review_decision: "review_required",
  },
  watch: {
    ...watchBase,
    state: "blocked",
    detail: "a review or repository requirement is outstanding",
    cycles: 1,
  },
};

// ---------------------------------------------------------------------------
// Code mode: attention vocabulary, rail cards, and the harness doctor.
// ---------------------------------------------------------------------------

export const attentionWorking: Attention = {
  state: { type: "working" },
  source: "lifecycle",
};

/** Strongest state: a structured signal that only the user can resolve. */
export const attentionNeedsYou: Attention = {
  state: {
    type: "needs_you",
    prompt: "an approval is waiting",
    source: "structured",
  },
  source: "structured",
};

export const attentionStalled: Attention = {
  state: { type: "stalled", idle_secs: 154 },
  source: "heuristic",
};

export const attentionDoneUnreviewed: Attention = {
  state: { type: "done_unreviewed" },
  source: "lifecycle",
};

export const attentionFenced: Attention = {
  state: { type: "fenced", reason: { type: "orphan_alive" } },
  source: "lifecycle",
};

export const attentionManual: Attention = {
  state: { type: "manual", note: "waiting on the design call" },
  source: "user",
};

export const codeWorkspace: CodeWorkspaceSnapshot = {
  id: "ws-1",
  repo_id: "repo-1",
  title: "Scoped UI workshop",
  worktree_path: "/Users/sam/tidebreak/code/worktrees/tidebreak/scoped-ui-workshop",
  branch_name: "tidebreak/scoped-ui-workshop",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-20T09:00:00.000Z",
};

export const codeSession: CodeSessionSnapshot = {
  id: "sess-1",
  workspace_id: "ws-1",
  kind: "interactive",
  harness_kind: "claude_code",
  harness_version: "2.1.234 (Claude Code)",
  permission_mode: "ask",
  lifecycle: "running",
  attention: attentionWorking,
  unrecognized_event_count: 0,
  created_at: "2026-08-20T09:05:00.000Z",
};

export function codeDigest(
  overrides: Partial<CodeSessionDigest> = {},
): CodeSessionDigest {
  return {
    workspace: "ws-1",
    session: "sess-1",
    kind: "interactive",
    lifecycle: "running",
    attention: attentionWorking,
    title: "Scoped UI workshop",
    turn_count: 4,
    ...overrides,
  };
}

const fullCaps: HarnessCaps = {
  resume: "supported",
  streaming_deltas: "supported",
  structured_approvals: "supported",
  mid_turn_steering: "unknown",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "supported",
  native_file_change_events: "supported",
  native_interrupt: "supported",
  image_input: "supported",
  slash_commands: "supported",
};

function doctorEntry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  return {
    found: true,
    tier: "reference",
    caps: fullCaps,
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    ...overrides,
  };
}

/** The live matrix: every engine present, honest capability differences. */
export const harnessDoctor: HarnessDoctorReport = {
  harnesses: [
    doctorEntry({
      kind: "claude_code",
      version: "2.1.234 (Claude Code)",
      path: "~/.local/share/tidebreak/tools/harnesses/claude_code",
      authenticated: true,
    }),
    doctorEntry({
      kind: "codex",
      version: "codex-cli 0.147.0",
      tier: "secondary",
      authenticated: true,
      caps: { ...fullCaps, mid_turn_steering: "supported", image_input: "unknown" },
    }),
    doctorEntry({
      kind: "opencode",
      version: "1.18.18",
      tier: "tertiary",
      authenticated: true,
      caps: { ...fullCaps, allow_mode: "unknown", reasoning_levels: "unknown" },
    }),
    doctorEntry({
      kind: "grok",
      version: "grok 1.0.5",
      tier: "best_effort",
      authenticated: false,
      caps: {
        ...fullCaps,
        mid_turn_steering: "unsupported",
        plan_mode: "unsupported",
        structured_approvals: "unsupported",
      },
    }),
  ],
};

/** A machine with work to do before a session can start. */
export const harnessDoctorDegraded: HarnessDoctorReport = {
  harnesses: [
    doctorEntry({
      kind: "claude_code",
      found: false,
      remediation: "Install Claude Code, then refresh.",
      stderr: "claude: command not found",
    }),
    doctorEntry({
      kind: "codex",
      version: "codex-cli 0.147.0",
      tier: "secondary",
      authenticated: false,
      remediation: "Run codex login in a terminal, then refresh.",
      caps: { ...fullCaps, mid_turn_steering: "supported" },
    }),
  ],
};

// ---------------------------------------------------------------------------
// Rail card states: digests per attention, PR chip tones, watch child rows.
// ---------------------------------------------------------------------------

/** Digest for a session mid-turn: the card's live state line. */
export const runningDigest: CodeSessionDigest = codeDigest({
  turn_count: 3,
  activity: "agent",
});

/** The agent is blocked on a foreground command, not generating. */
export const shellDigest: CodeSessionDigest = codeDigest({
  turn_count: 3,
  activity: "shell",
});

/** The session is only observing a background task. */
export const monitorDigest: CodeSessionDigest = codeDigest({
  turn_count: 3,
  activity: "monitor",
});

/** Digest for a structured question the harness is waiting on. */
export const needsYouDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: attentionNeedsYou,
});

/** Digest for a session that went quiet without finishing. */
export const stalledDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: attentionStalled,
});

/** Digest for a finished turn nobody has looked at yet. */
export const doneDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: attentionDoneUnreviewed,
  turn_count: 6,
});

/** A watch-and-fix task riding under the workspace (decision 50). */
export const watchDigest: CodeSessionDigest = codeDigest({
  session: "sess-watch",
  kind: "watch",
  lifecycle: "running",
  turn_count: 2,
});

/**
 * An interactive digest carrying harness subagents (decision 52): one still
 * running, one done, one failed — the three row states the rail shows.
 */
export const subagentsDigest: CodeSessionDigest = codeDigest({
  turn_count: 3,
  activity: "subagents",
  subagents: [
    {
      call_id: "toolu_task_running",
      name: "Audit the migration plan (general-purpose)",
      status: "running",
    },
    {
      call_id: "toolu_task_done",
      name: "Find the config parser",
      status: "done",
    },
    {
      call_id: "toolu_task_failed",
      name: "Run the flaky suite",
      status: "failed",
    },
  ],
});

/** Workspace PR digests, one per chip tone. */
export const openPrDigest: PullRequestDigest = {
  number: 184,
  url: "https://github.com/example/tidebreak/pull/184",
  state: "open",
  title: "Add a scoped UI workshop",
};

export const draftPrDigest: PullRequestDigest = {
  ...openPrDigest,
  draft: true,
};

export const mergedPrDigest: PullRequestDigest = {
  ...openPrDigest,
  state: "merged",
  merged: true,
};

export const closedPrDigest: PullRequestDigest = {
  ...openPrDigest,
  state: "closed",
};

/** A put-away workspace: worktree gone, branch and history kept. */
export const archivedWorkspace: CodeWorkspaceSnapshot = {
  ...codeWorkspace,
  id: "ws-archived",
  title: "Shipped last week",
  status: "archived",
  archived_at: "2026-08-18T17:00:00.000Z",
};

// ---------------------------------------------------------------------------
// Delivery center: cross-repository pull requests, runs, archive, notifications.
// ---------------------------------------------------------------------------

export const deliveryCodeRepo: CodeRepoSnapshot = {
  id: "repo-tidebreak",
  root_path: "/Users/sam/tidebreak",
  display_name: "tidebreak",
  default_base_ref: "main",
  branch_prefix: "thet",
  quick_actions: [],
  created_at: "2026-08-10T12:00:00.000Z",
};

export const deliveryRepository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  default_branch: "main",
  tidebreak_repo_id: deliveryCodeRepo.id,
};

export const deliveryDocsRepository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "docs",
  name_with_owner: "brightwave-inc/docs",
  url: "https://github.com/brightwave-inc/docs",
  default_branch: "main",
};

export const deliveryRepositoriesSnapshot: CodeDeliveryRepositoriesSnapshot = {
  capability: {
    found: true,
    authenticated: true,
    viewer_login: "mara",
    remediation: "",
  },
  repositories: [deliveryRepository, deliveryDocsRepository],
  errors: [],
  fetched_at: "2026-08-20T15:20:00.000Z",
};

const deliveryWorkspaceLink = {
  workspace_id: "ws-delivery-center",
  repo_id: deliveryCodeRepo.id,
  title: "Build the delivery center",
  branch_name: "thet/delivery-center",
  status: "active" as const,
  exact: true,
};

export const deliveryWorkspaces: CodeWorkspaceSnapshot[] = [
  {
    id: deliveryWorkspaceLink.workspace_id,
    repo_id: deliveryCodeRepo.id,
    title: deliveryWorkspaceLink.title,
    worktree_path: "/Users/sam/tidebreak/worktrees/delivery-center",
    branch_name: deliveryWorkspaceLink.branch_name,
    base_ref: "main",
    status: "active",
    created_at: "2026-08-19T14:00:00.000Z",
    pr: {
      number: 2251,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2251",
      state: "open",
      title: "Build the delivery center",
      head_branch: deliveryWorkspaceLink.branch_name,
      base_branch: "main",
      draft: false,
      review_decision: "changes_requested",
      checks_summary: "7 passing, 1 failing",
      checks: [
        { name: "desktop / tests", bucket: "pass" },
        { name: "desktop / storybook", bucket: "fail" },
      ],
    },
  },
  {
    id: "ws-archived-search",
    repo_id: deliveryCodeRepo.id,
    title: "Add workspace search",
    worktree_path: "/Users/sam/tidebreak/worktrees/workspace-search",
    branch_name: "thet/workspace-search",
    base_ref: "main",
    status: "archived",
    pr: {
      number: 2194,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2194",
      state: "merged",
      title: "Add workspace search",
    },
    created_at: "2026-07-28T09:00:00.000Z",
    archived_at: "2026-08-18T17:00:00.000Z",
  },
  {
    id: "ws-archived-shortcuts",
    repo_id: deliveryCodeRepo.id,
    title: "Unify keyboard shortcuts",
    worktree_path: "/Users/sam/tidebreak/worktrees/keyboard-shortcuts",
    branch_name: "thet/keyboard-shortcuts",
    base_ref: "main",
    status: "archived",
    created_at: "2026-07-18T11:00:00.000Z",
    archived_at: "2026-08-12T09:30:00.000Z",
  },
];

export const deliveryPullRequests: CodeDeliveryPullRequestSummary[] = [
  {
    id: "github.com/brightwave-inc/tidebreak#2251",
    repository: deliveryRepository,
    number: 2251,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2251",
    title: "Build the delivery center",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/delivery-center",
    base_branch: "main",
    head_sha: "82ab990",
    review_decision: "changes_requested",
    mergeable: "mergeable",
    merge_state_status: "blocked",
    auto_merge_enabled: false,
    checks: [
      { name: "desktop / tests", bucket: "pass", workflow_run_id: 4401 },
      {
        name: "desktop / storybook",
        bucket: "fail",
        detail: "Build failed",
        url: "https://github.com/brightwave-inc/tidebreak/actions/runs/4401",
        workflow_run_id: 4401,
      },
    ],
    attention_reasons: ["changes_requested", "checks_failed"],
    ready_to_merge: false,
    workspace_links: [deliveryWorkspaceLink],
    created_at: "2026-08-19T14:12:00.000Z",
    updated_at: "2026-08-20T15:08:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak#2247",
    repository: deliveryRepository,
    number: 2247,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2247",
    title: "Make workspace deep links durable",
    state: "open",
    draft: false,
    author: "devon",
    head_branch: "devon/durable-workspace-links",
    base_branch: "main",
    head_sha: "73fc201",
    review_decision: "approved",
    mergeable: "mergeable",
    merge_state_status: "clean",
    auto_merge_enabled: false,
    checks: [
      { name: "workspace / tests", bucket: "pass", workflow_run_id: 4392 },
      { name: "desktop / build", bucket: "pass", workflow_run_id: 4392 },
    ],
    attention_reasons: [],
    ready_to_merge: true,
    workspace_links: [],
    created_at: "2026-08-18T16:30:00.000Z",
    updated_at: "2026-08-20T14:32:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/docs#311",
    repository: deliveryDocsRepository,
    number: 311,
    url: "https://github.com/brightwave-inc/docs/pull/311",
    title: "Document managed deployments",
    state: "open",
    draft: true,
    author: "ines",
    head_branch: "ines/managed-deployments",
    base_branch: "main",
    head_sha: "555a0cd",
    auto_merge_enabled: false,
    checks: [{ name: "docs / build", bucket: "pending" }],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    created_at: "2026-08-20T09:10:00.000Z",
    updated_at: "2026-08-20T14:04:00.000Z",
  },
];

export const deliveryPullRequestDetails: Record<
  number,
  CodeDeliveryPullRequestDetail
> = {
  2251: {
    summary: deliveryPullRequests[0]!,
    body: "Adds cross-repository pull request, Actions, deployment, archive, and notification views to the desktop app.",
    labels: ["desktop", "ui"],
    assignees: ["mara"],
    changed_files: 19,
    additions: 2140,
    deletions: 83,
    comments: [
      {
        kind: "review",
        author: "devon",
        created_at: "2026-08-20T14:48:00.000Z",
        review_state: "changes_requested",
        body: "The narrow detail state still needs a pass.",
      },
      {
        kind: "inline",
        author: "ines",
        created_at: "2026-08-20T15:02:00.000Z",
        body: "Keep repository failures visible without hiding usable results.",
        path: "src/code/CodeDeliveryPage.tsx",
        line: 702,
      },
    ],
    can_mark_ready: false,
    can_merge: false,
    can_rerun_failed: true,
  },
  2247: {
    summary: deliveryPullRequests[1]!,
    body: "Keeps workspace URLs stable when panels and delivery links navigate back into active work.",
    labels: ["desktop"],
    assignees: ["devon"],
    changed_files: 6,
    additions: 184,
    deletions: 39,
    comments: [
      {
        kind: "review",
        author: "mara",
        created_at: "2026-08-20T14:16:00.000Z",
        review_state: "approved",
        body: "The route behavior is clear and the focused tests cover it.",
      },
    ],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
  },
};

export const deliveryRuns: CodeDeliveryRunSummary[] = [
  {
    id: "github.com/brightwave-inc/tidebreak:workflow_run:4401",
    repository: deliveryRepository,
    kind: "workflow_run",
    github_id: 4401,
    name: "Desktop CI",
    url: "https://github.com/brightwave-inc/tidebreak/actions/runs/4401",
    status: "completed",
    conclusion: "failure",
    workflow: "Desktop CI",
    branch: "thet/delivery-center",
    sha: "82ab990",
    event: "pull_request",
    actor: "mara",
    attention_reasons: ["failure"],
    workspace_links: [deliveryWorkspaceLink],
    created_at: "2026-08-20T14:55:00.000Z",
    updated_at: "2026-08-20T15:11:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak:deployment:901",
    repository: deliveryRepository,
    kind: "deployment",
    github_id: 901,
    name: "Production",
    url: "https://github.com/brightwave-inc/tidebreak/deployments/activity_log?environment=production",
    status: "success",
    conclusion: "success",
    environment: "production",
    branch: "main",
    sha: "4fa2bb1",
    actor: "release-bot",
    attention_reasons: [],
    workspace_links: [],
    created_at: "2026-08-20T13:22:00.000Z",
    updated_at: "2026-08-20T13:29:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/docs:workflow_run:981",
    repository: deliveryDocsRepository,
    kind: "workflow_run",
    github_id: 981,
    name: "Docs preview",
    url: "https://github.com/brightwave-inc/docs/actions/runs/981",
    status: "in_progress",
    workflow: "Docs preview",
    branch: "ines/managed-deployments",
    event: "pull_request",
    actor: "ines",
    attention_reasons: [],
    workspace_links: [],
    created_at: "2026-08-20T14:02:00.000Z",
    updated_at: "2026-08-20T15:17:00.000Z",
  },
];

export const deliveryRunDetails: Record<number, CodeDeliveryRunDetail> = {
  4401: {
    summary: deliveryRuns[0]!,
    jobs: [
      {
        id: 7101,
        name: "Focused tests",
        status: "completed",
        conclusion: "success",
        url: "https://github.com/brightwave-inc/tidebreak/actions/runs/4401/job/7101",
        started_at: "2026-08-20T14:56:00.000Z",
        completed_at: "2026-08-20T15:01:00.000Z",
        failed_steps: [],
      },
      {
        id: 7102,
        name: "Storybook build",
        status: "completed",
        conclusion: "failure",
        url: "https://github.com/brightwave-inc/tidebreak/actions/runs/4401/job/7102",
        started_at: "2026-08-20T14:56:00.000Z",
        completed_at: "2026-08-20T15:10:00.000Z",
        failed_steps: ["Build static Storybook"],
      },
    ],
    deployment_statuses: [],
    can_rerun_failed: true,
  },
  901: {
    summary: deliveryRuns[1]!,
    jobs: [],
    deployment_statuses: [
      {
        id: 991,
        state: "success",
        description: "Production deployment completed.",
        environment_url: "https://tidebreak.example.com",
        log_url: "https://github.com/brightwave-inc/tidebreak/actions/runs/4388",
        created_at: "2026-08-20T13:29:00.000Z",
      },
    ],
    can_rerun_failed: false,
  },
};

export const deliveryNotifications: CodeDeliveryNotification[] = [
  {
    id: "pr-attention:2251:82ab990",
    fingerprint: "pr-attention:2251:82ab990",
    rule: "pull_request_attention",
    title: "brightwave-inc/tidebreak #2251 needs attention",
    detail: "Build the delivery center",
    repositoryName: "brightwave-inc/tidebreak",
    occurredAt: "2026-08-20T15:08:00.000Z",
    receivedAt: "2026-08-20T15:10:00.000Z",
    url: deliveryPullRequests[0]!.url,
    workspaceId: deliveryWorkspaceLink.workspace_id,
    target: {
      kind: "pull_request",
      repository: {
        host: deliveryRepository.host,
        owner: deliveryRepository.owner,
        name: deliveryRepository.name,
      },
      number: 2251,
    },
  },
  {
    id: "run-failure:4401:failure",
    fingerprint: "run-failure:4401:failure",
    rule: "run_failure",
    title: "brightwave-inc/tidebreak Desktop CI failed",
    detail: "failure",
    repositoryName: "brightwave-inc/tidebreak",
    occurredAt: "2026-08-20T15:11:00.000Z",
    receivedAt: "2026-08-20T15:12:00.000Z",
    readAt: "2026-08-20T15:14:00.000Z",
    url: deliveryRuns[0]!.url,
    workspaceId: deliveryWorkspaceLink.workspace_id,
    target: {
      kind: "run",
      repository: {
        host: deliveryRepository.host,
        owner: deliveryRepository.owner,
        name: deliveryRepository.name,
      },
      runKind: "workflow_run",
      id: 4401,
    },
  },
  {
    id: "pr-ready:2247:73fc201",
    fingerprint: "pr-ready:2247:73fc201",
    rule: "pull_request_ready",
    title: "brightwave-inc/tidebreak #2247 is ready",
    detail: "Make workspace deep links durable",
    repositoryName: "brightwave-inc/tidebreak",
    occurredAt: "2026-08-20T14:32:00.000Z",
    receivedAt: "2026-08-20T14:34:00.000Z",
    url: deliveryPullRequests[1]!.url,
    target: {
      kind: "pull_request",
      repository: {
        host: deliveryRepository.host,
        owner: deliveryRepository.owner,
        name: deliveryRepository.name,
      },
      number: 2247,
    },
  },
];

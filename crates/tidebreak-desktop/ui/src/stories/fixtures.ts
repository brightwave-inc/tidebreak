import type {
  Attention,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunDetail,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
  CodeHarnessInstallSnapshot,
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
  HarnessKind,
  PendingUserQuestions,
  GatewayApps,
  GatewayStatus,
  MemoryRecord,
  RemoteMachineState,
  TaskPlan,
  ToolActionPreview,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
} from "@/api";
import type { ContextUsageReading } from "@/ContextUsageIndicator";

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
          description:
            "Approvals, plans, questions, messages, and tool activity.",
        },
        {
          id: "code",
          label: "Code workspace states",
          description: "Git, diffs, approvals, and workspace status.",
        },
        {
          id: "documents",
          label: "Document viewers",
          description:
            "Heavier browser surfaces that can follow in a later slice.",
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
          description:
            "Build the habit before making screenshots a required check.",
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

/** A hosted machine whose pushes land as the deployment's GitHub App. */
export const hostedAppGit: CodeWorkspacePrSnapshot = {
  ...unpushedGit,
  gh_found: false,
  gh_authenticated: undefined,
  pushes_as: "tidebreak-ship[bot]",
};

/** A hosted machine whose pushes land as the caller's own account. */
export const hostedPersonGit: CodeWorkspacePrSnapshot = {
  ...unpushedGit,
  gh_found: false,
  gh_authenticated: undefined,
  pushes_as: "mira-chen",
  pushes_as_self: true,
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

/** GitHub has accepted the pull request into its merge queue. */
export const queuedPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    auto_merge_enabled: true,
    in_merge_queue: true,
    checks_summary: "8 passing, 1 pending, 0 failing",
    checks: [
      { name: "desktop test", bucket: "pass" },
      { name: "merge queue", bucket: "pending" },
    ],
  },
};

/** Failing checks with nobody watching: the Fix errors action's own state. */
export const failingChecksPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    checks_summary: "7 passing, 1 failing",
    checks: [
      { name: "desktop test", bucket: "pass" },
      {
        name: "clippy",
        bucket: "fail",
        detail: "exit 101",
        url: "https://github.com/example/tidebreak/actions/runs/32664268801/job/97255126659",
      },
    ],
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
    checks_summary: "9 passing, 0 pending, 0 failing",
    review_decision: "review_required",
  },
  watch: {
    ...watchBase,
    state: "blocked",
    detail: "the pull request needs a review approval",
    cycles: 1,
  },
};

/** Checks are green and GitHub still wants a review approval (decision 66). */
export const needsApprovalPrGit: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  pr: {
    ...openPrGit.pr!,
    checks_summary: "9 passing, 0 pending, 0 failing",
    review_decision: "review_required",
    mergeable: "mergeable",
    merge_state_status: "blocked",
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

export const attentionIdle: Attention = {
  state: { type: "idle" },
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
  worktree_path:
    "/Users/sam/tidebreak/code/worktrees/tidebreak/scoped-ui-workshop",
  branch_name: "tidebreak/scoped-ui-workshop",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-20T09:00:00.000Z",
};

/** Repositories shared by the home, setup, and sidebar stories. */
export const codeRepositories: CodeRepoSnapshot[] = [
  {
    id: "repo-tidebreak",
    root_path: "/Users/sam/src/brightwave/tidebreak",
    display_name: "tidebreak",
    default_base_ref: "main",
    branch_prefix: "thet",
    quick_actions: [],
    created_at: "2026-08-10T12:00:00.000Z",
  },
  {
    id: "repo-model-gateway",
    root_path: "/Users/sam/src/platform/model-gateway",
    display_name: "model-gateway",
    default_base_ref: "main",
    branch_prefix: "thet",
    quick_actions: [],
    created_at: "2026-08-08T15:30:00.000Z",
  },
  {
    id: "repo-design-system",
    root_path:
      "/Users/sam/src/brightwave/product-foundations/design-system-components",
    display_name: "design-system-components",
    default_base_ref: "main",
    branch_prefix: "thet",
    quick_actions: [],
    created_at: "2026-08-03T10:15:00.000Z",
  },
];

/** A realistic worktree for quick open and file-panel stories. */
export const codeWorkspaceFilePaths = [
  "README.md",
  "crates/tidebreak-desktop/ui/src/code/CodeHome.tsx",
  "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
  "crates/tidebreak-desktop/ui/src/code/browser/CodeBrowserTab.tsx",
  "crates/tidebreak-desktop/ui/src/stories/CodeWorkspacePage.stories.tsx",
  "crates/tidebreak-desktop/ui/src/stories/fixtures.ts",
  "docs/decisions/0062-pull-request-stacks.md",
  "scripts/storybook.sh",
] as const;

/** A busy but readable rail for density and progressive-disclosure review. */
export const codeSidebarWorkspaces: CodeWorkspaceSnapshot[] = [
  {
    ...codeWorkspace,
    id: "ws-storybook-audit",
    repo_id: "repo-tidebreak",
    title: "Audit Storybook coverage",
    branch_name: "thet/storybook-audit",
    created_at: "2026-08-24T13:40:00.000Z",
  },
  {
    ...codeWorkspace,
    id: "ws-browser-recovery",
    repo_id: "repo-tidebreak",
    title: "Make browser recovery clear",
    branch_name: "thet/browser-recovery",
    created_at: "2026-08-24T12:20:00.000Z",
    pr: {
      number: 2314,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2314",
      state: "open",
      title: "Make browser recovery clear",
      review_decision: "changes_requested",
      checks_summary: "7 passing, 1 failing",
    },
  },
  {
    ...codeWorkspace,
    id: "ws-delivery-density",
    repo_id: "repo-tidebreak",
    title: "Clarify delivery density",
    branch_name: "thet/delivery-density",
    created_at: "2026-08-24T11:05:00.000Z",
    pr: {
      number: 2311,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2311",
      state: "open",
      title: "Clarify delivery density",
      review_decision: "approved",
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks_summary: "9 passing",
    },
  },
  {
    ...codeWorkspace,
    id: "ws-provider-errors",
    repo_id: "repo-model-gateway",
    title: "Recover provider errors",
    branch_name: "thet/provider-errors",
    created_at: "2026-08-23T17:45:00.000Z",
  },
  {
    ...codeWorkspace,
    id: "ws-usage-progress",
    repo_id: "repo-model-gateway",
    title: "Show subscription progress",
    branch_name: "thet/usage-progress",
    created_at: "2026-08-23T16:10:00.000Z",
  },
  {
    ...codeWorkspace,
    id: "ws-token-rhythm",
    repo_id: "repo-design-system",
    title: "Tune dense panel rhythm",
    branch_name: "thet/panel-rhythm",
    created_at: "2026-08-22T14:30:00.000Z",
  },
];

export const codeSession: CodeSessionSnapshot = {
  visibility: "private",
  id: "sess-1",
  workspace_id: "ws-1",
  kind: "interactive",
  harness_kind: "claude_code",
  execution_location: "machine",
  harness_version: "2.1.234 (Claude Code)",
  permission_mode: "ask",
  fast_mode: false,
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
    harness_kind: "claude_code",
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
  durable_parks: "unsupported",
  user_questions: "unsupported",
  standing_grants: "unsupported",
  mid_turn_resume: "unsupported",
  transcript: "unsupported",
  memory_loopback: "unsupported",
};

function doctorEntry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  return {
    found: true,
    installable: true,
    authenticated: true,
    auth_mode: "local_sign_in",
    tier: "reference",
    caps: fullCaps,
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    relaunch_composes_permission_mode: true,
    update_available: false,
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
      caps: {
        ...fullCaps,
        mid_turn_steering: "supported",
        image_input: "unknown",
      },
    }),
    doctorEntry({
      kind: "opencode",
      version: "1.18.18",
      tier: "tertiary",
      authenticated: true,
      relaunch_composes_permission_mode: false,
      caps: { ...fullCaps, allow_mode: "unknown", reasoning_levels: "unknown" },
    }),
    doctorEntry({
      kind: "grok",
      version: "grok 1.0.5",
      tier: "best_effort",
      authenticated: false,
      remediation: "Sign in to grok in your own terminal, then re-check.",
      caps: {
        ...fullCaps,
        mid_turn_steering: "unsupported",
        plan_mode: "unsupported",
        structured_approvals: "unsupported",
      },
    }),
    doctorEntry({
      kind: "internal",
      installable: false,
      version: "0.1.0",
      caps: {
        ...fullCaps,
        slash_commands: "unsupported",
        native_file_change_events: "unsupported",
      },
    }),
  ],
};

/**
 * The `latest` channel after Check for updates: one engine behind the
 * registry, one already on the newest release, and one the registry has
 * not been asked about since this process started.
 */
export const harnessDoctorUpdates: HarnessDoctorReport = {
  update_channel: "latest",
  harnesses: harnessDoctor.harnesses.map((entry) => {
    if (entry.kind === "claude_code") {
      return {
        ...entry,
        pinned_version: "2.1.234",
        managed_version: "2.1.234",
        latest_version: "2.1.258",
        update_available: true,
      };
    }
    if (entry.kind === "codex") {
      return {
        ...entry,
        version: "codex-cli 0.152.0",
        pinned_version: "0.147.0",
        managed_version: "0.152.0",
        latest_version: "0.152.0",
      };
    }
    return { ...entry, pinned_version: entry.version };
  }),
};

/** A machine with work to do before a session can start. */
export const harnessDoctorDegraded: HarnessDoctorReport = {
  harnesses: [
    doctorEntry({
      kind: "claude_code",
      version: "2.1.234 (Claude Code)",
      path: "~/.local/share/tidebreak/tools/harnesses/claude_code",
      authenticated: undefined,
      remediation:
        "Tidebreak could not verify the Claude Code sign-in. Sign in to Claude Code in your own terminal, then re-check.",
    }),
    doctorEntry({
      kind: "codex",
      version: "codex-cli 0.147.0",
      tier: "secondary",
      authenticated: false,
      remediation: "Sign in to Codex CLI in your own terminal, then re-check.",
      caps: { ...fullCaps, mid_turn_steering: "supported" },
    }),
  ],
};

/** A fresh machine: nothing downloaded, and every engine one click away. */
export const harnessDoctorCold: HarnessDoctorReport = {
  harnesses: harnessDoctor.harnesses.map((entry) => ({
    ...entry,
    found: false,
    authenticated: undefined,
    path: undefined,
    version: undefined,
    remediation: "",
  })),
};

/**
 * The common middle: one engine in use, the rest never fetched, and one that
 * arrived but was never signed into.
 */
export const harnessDoctorMixed: HarnessDoctorReport = {
  harnesses: harnessDoctor.harnesses.map((entry, index) =>
    index === 0 || entry.authenticated === false
      ? entry
      : {
          ...entry,
          found: false,
          authenticated: undefined,
          path: undefined,
          version: undefined,
        },
  ),
};

/**
 * A gateway-hosted machine (decision 71): the relay engines are ready with no
 * sign-in to perform, and the ones the relay does not cover yet say so
 * instead of demanding a terminal nobody can open.
 */
export const harnessDoctorHosted: HarnessDoctorReport = {
  harnesses: [
    doctorEntry({
      kind: "claude_code",
      version: "2.1.234 (Claude Code)",
      path: "~/.local/share/tidebreak/tools/harnesses/claude_code",
      authenticated: false,
      auth_mode: "gateway_relay",
    }),
    doctorEntry({
      kind: "codex",
      version: "codex-cli 0.147.0",
      tier: "secondary",
      authenticated: false,
      auth_mode: "gateway_relay",
      caps: {
        ...fullCaps,
        mid_turn_steering: "supported",
        image_input: "unknown",
      },
    }),
    doctorEntry({
      kind: "opencode",
      version: "1.18.18",
      tier: "tertiary",
      authenticated: false,
      auth_mode: "hosted_unavailable",
      remediation: "opencode is not available on hosted machines yet.",
      caps: { ...fullCaps, allow_mode: "unknown", reasoning_levels: "unknown" },
    }),
    doctorEntry({
      kind: "grok",
      version: "grok 1.0.5",
      tier: "best_effort",
      authenticated: false,
      auth_mode: "hosted_unavailable",
      remediation: "Grok CLI is not available on hosted machines yet.",
      caps: {
        ...fullCaps,
        mid_turn_steering: "unsupported",
        plan_mode: "unsupported",
        structured_approvals: "unsupported",
      },
    }),
  ],
};

/**
 * A machine whose engines are pointed at a Model Gateway: no vendor login
 * anywhere, and every engine that reads a credential override works. The two
 * engines whose override surfaces Tidebreak does not read stay on the local
 * sign-in they actually have.
 */
export const harnessDoctorGatewayManaged: HarnessDoctorReport = {
  harnesses: [
    doctorEntry({
      kind: "claude_code",
      version: "2.1.234 (Claude Code)",
      path: "~/.local/share/tidebreak/tools/harnesses/claude_code",
      authenticated: false,
      auth_mode: "gateway_managed",
    }),
    doctorEntry({
      kind: "codex",
      version: "codex-cli 0.147.0",
      tier: "secondary",
      authenticated: false,
      auth_mode: "gateway_managed",
      caps: {
        ...fullCaps,
        mid_turn_steering: "supported",
        image_input: "unknown",
      },
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
      remediation: "Sign in to Grok CLI in your own terminal, then re-check.",
      caps: {
        ...fullCaps,
        mid_turn_steering: "unsupported",
        plan_mode: "unsupported",
        structured_approvals: "unsupported",
      },
    }),
  ],
};

/** One engine mid-download, one that failed, for the doctor's live states. */
export const harnessInstallsInFlight: Partial<
  Record<HarnessKind, CodeHarnessInstallSnapshot>
> = {
  codex: {
    kind: "codex",
    version: "0.147.0",
    phase: "installing",
    done: false,
  },
  opencode: {
    kind: "opencode",
    version: "1.18.18",
    phase: "failed",
    done: true,
    error:
      "npm install opencode-ai@1.18.18 failed: ETIMEDOUT registry.npmjs.org",
  },
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
  activity_detail: "cargo test -p tidebreak-server code_parser",
});

/** The session is only observing a background task. */
export const monitorDigest: CodeSessionDigest = codeDigest({
  turn_count: 3,
  activity: "monitor",
  activity_detail: "CI on pull request #3040",
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

/** Parked after a turn, still carrying Working from an older digest. */
export const idleDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  turn_count: 2,
});

/** Parked after a turn, with the recap the rail uses as the complete read. */
export const idleCompleteDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: attentionIdle,
  harness_kind: "grok",
  turn_count: 4,
  recap: "Folded the backoff into refresh and left the retry passing.",
});

export const idleSession: CodeSessionSnapshot = {
  ...codeSession,
  lifecycle: "idle",
};

export const grokIdleSession: CodeSessionSnapshot = {
  ...codeSession,
  harness_kind: "grok",
  lifecycle: "idle",
};

/** A watch-and-fix task riding under the workspace (decision 50). */
export const watchDigest: CodeSessionDigest = codeDigest({
  session: "sess-watch",
  kind: "watch",
  lifecycle: "running",
  turn_count: 2,
});

/** Every subagent settled: the card folds them behind one summary row. */
export const settledSubagentsDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: attentionDoneUnreviewed,
  turn_count: 3,
  recap: "Audited the migration plan and found the parser; the suite is flaky.",
  subagents: [
    {
      call_id: "toolu_task_done_a",
      name: "Audit the migration plan (general-purpose)",
      status: "done",
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

export const queuedPrDigest: PullRequestDigest = {
  ...openPrDigest,
  auto_merge_enabled: true,
  in_merge_queue: true,
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

/** Trigger/watch wrote that the pull request is mergeable. */
export const readyToMergeDigest: CodeSessionDigest = codeDigest({
  lifecycle: "idle",
  attention: {
    state: {
      type: "needs_you",
      prompt: "#184 is ready to merge",
      source: "structured",
    },
    source: "structured",
  },
  turn_count: 4,
  recap: "Opened the page as soon as the session existed.",
  pr_state: {
    ...openPrDigest,
    mergeable: "mergeable",
    merge_state_status: "clean",
    review_decision: "approved",
  },
});

/** A remote-style put-away workspace: no host worktree, history kept. */
export const archivedWorkspace: CodeWorkspaceSnapshot = {
  ...codeWorkspace,
  id: "ws-archived",
  title: "Shipped last week",
  status: "archived",
  archived_at: "2026-08-18T17:00:00.000Z",
};

/** A local archived workspace: worktree and branch gone, commits saved as a bundle. */
export const releasedWorkspace: CodeWorkspaceSnapshot = {
  ...codeWorkspace,
  id: "ws-released",
  title: "Reclaimed after review",
  status: "released",
  archived_at: "2026-08-18T17:00:00.000Z",
  released_at: "2026-08-20T09:30:00.000Z",
  released_tip: "9f3c1ab2d4e5f60718293a4b5c6d7e8f90a1b2c3",
  bundle_bytes: 12_288,
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

export const deliveryGatewayRepository: CodeGitHubRepositoryRef = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "model-gateway",
  name_with_owner: "brightwave-inc/model-gateway",
  url: "https://github.com/brightwave-inc/model-gateway",
  default_branch: "main",
};

export const deliveryRepositoriesSnapshot: CodeDeliveryRepositoriesSnapshot = {
  capability: {
    found: true,
    authenticated: true,
    viewer_login: "mara",
    remediation: "",
  },
  repositories: [
    deliveryRepository,
    deliveryDocsRepository,
    deliveryGatewayRepository,
  ],
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
    status: "released",
    pr: {
      number: 2194,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2194",
      state: "merged",
      title: "Add workspace search",
    },
    created_at: "2026-07-28T09:00:00.000Z",
    archived_at: "2026-08-18T17:00:00.000Z",
    released_at: "2026-08-18T17:00:01.000Z",
    released_tip: "2c9d8e7f60a1b2c3d4e5f60718293a4b5c6d7e8f",
    bundle_bytes: 12_288,
  },
  {
    id: "ws-archived-shortcuts",
    repo_id: deliveryCodeRepo.id,
    title: "Unify keyboard shortcuts",
    worktree_path: "/Users/sam/tidebreak/worktrees/keyboard-shortcuts",
    branch_name: "thet/keyboard-shortcuts",
    base_ref: "main",
    status: "released",
    created_at: "2026-07-18T11:00:00.000Z",
    archived_at: "2026-08-12T09:30:00.000Z",
    released_at: "2026-08-12T09:30:01.000Z",
    released_tip: "4f2a6b8c90d1e2f3a4b5c6d7e8f90a1b2c3d4e5",
    bundle_bytes: 8192,
  },
];

/**
 * A three-level stack plus an orphan child (decision 77): parent on main,
 * child on the parent's head, grandchild on the child's, and one row whose
 * parent is not loaded. Separate from `deliveryPullRequests`, whose stories
 * assert exact tone counts.
 */
export const stackedDeliveryPullRequests: CodeDeliveryPullRequestSummary[] = [
  {
    id: "github.com/brightwave-inc/tidebreak#2301",
    repository: deliveryRepository,
    number: 2301,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2301",
    title: "Stack base: extract the fact store",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/stack-base",
    base_branch: "main",
    head_sha: "a1a1a1a",
    auto_merge_enabled: false,
    mergeable: "mergeable",
    merge_state_status: "clean",
    checks: [],
    attention_reasons: [],
    ready_to_merge: true,
    workspace_links: [deliveryWorkspaceLink],
    stack_number: 2273,
    stack_size: 3,
    labels: [],
    created_at: "2026-08-21T09:00:00.000Z",
    updated_at: "2026-08-21T15:00:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak#2302",
    repository: deliveryRepository,
    number: 2302,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2302",
    title: "Stack middle: reconcile sweep",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/stack-middle",
    base_branch: "thet/stack-base",
    head_sha: "b2b2b2b",
    auto_merge_enabled: false,
    mergeable: "mergeable",
    merge_state_status: "clean",
    checks: [],
    attention_reasons: [],
    ready_to_merge: true,
    workspace_links: [],
    stack_parent_number: 2301,
    stack_number: 2273,
    stack_size: 3,
    labels: [],
    created_at: "2026-08-21T10:00:00.000Z",
    updated_at: "2026-08-21T14:00:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak#2303",
    repository: deliveryRepository,
    number: 2303,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2303",
    title: "Stack tip: lanes in the delivery page",
    state: "open",
    draft: true,
    author: "mara",
    head_branch: "thet/stack-tip",
    base_branch: "thet/stack-middle",
    head_sha: "c3c3c3c",
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    stack_parent_number: 2302,
    stack_number: 2273,
    stack_size: 3,
    labels: [],
    created_at: "2026-08-21T11:00:00.000Z",
    updated_at: "2026-08-21T13:00:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak#2290",
    repository: deliveryRepository,
    number: 2290,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2290",
    title: "Orphan child: parent merged away",
    state: "open",
    draft: false,
    author: "devon",
    head_branch: "thet/orphan-child",
    base_branch: "thet/parent-not-loaded",
    head_sha: "d4d4d4d",
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    stack_parent_number: 2288,
    labels: [],
    created_at: "2026-08-20T11:00:00.000Z",
    updated_at: "2026-08-21T12:00:00.000Z",
  },
];

/**
 * A chain the host has no stack for (GitHub stacked pull requests): the
 * edges are branch inference only, so the page marks it and the detail
 * sheet offers to register it. Separate from the host-registered stack
 * fixtures above.
 */
export const unregisteredDeliveryPullRequests: CodeDeliveryPullRequestSummary[] =
  [
    {
      id: "github.com/brightwave-inc/tidebreak#2310",
      repository: deliveryRepository,
      number: 2310,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2310",
      title: "Unregistered base: land the schema",
      state: "open",
      draft: false,
      author: "mara",
      head_branch: "thet/unregistered-base",
      base_branch: "main",
      head_sha: "a0a0a0a",
      auto_merge_enabled: false,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / rust", bucket: "pass" }],
      attention_reasons: [],
      ready_to_merge: true,
      workspace_links: [],
      unregistered_stack_numbers: [2310, 2311, 2312],
      labels: [],
      created_at: "2026-08-21T09:00:00.000Z",
      updated_at: "2026-08-21T15:00:00.000Z",
    },
    {
      id: "github.com/brightwave-inc/tidebreak#2311",
      repository: deliveryRepository,
      number: 2311,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2311",
      title: "Unregistered middle: the queries",
      state: "open",
      draft: false,
      author: "mara",
      head_branch: "thet/unregistered-middle",
      base_branch: "thet/unregistered-base",
      head_sha: "b1b1b1b",
      auto_merge_enabled: false,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / rust", bucket: "pass" }],
      attention_reasons: [],
      ready_to_merge: true,
      workspace_links: [],
      stack_parent_number: 2310,
      unregistered_stack_numbers: [2310, 2311, 2312],
      labels: [],
      created_at: "2026-08-21T10:00:00.000Z",
      updated_at: "2026-08-21T14:00:00.000Z",
    },
    {
      id: "github.com/brightwave-inc/tidebreak#2312",
      repository: deliveryRepository,
      number: 2312,
      url: "https://github.com/brightwave-inc/tidebreak/pull/2312",
      title: "Unregistered tip: the migration",
      state: "open",
      draft: false,
      author: "mara",
      head_branch: "thet/unregistered-tip",
      base_branch: "thet/unregistered-middle",
      head_sha: "c2c2c2c",
      auto_merge_enabled: false,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / rust", bucket: "pass" }],
      attention_reasons: [],
      ready_to_merge: true,
      workspace_links: [],
      stack_parent_number: 2311,
      unregistered_stack_numbers: [2310, 2311, 2312],
      labels: [],
      created_at: "2026-08-21T11:00:00.000Z",
      updated_at: "2026-08-21T13:00:00.000Z",
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
    comment_count: 3,
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
    labels: ["desktop", "ui"],
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
    comment_count: 0,
    checks: [
      { name: "workspace / tests", bucket: "pass", workflow_run_id: 4392 },
      { name: "desktop / build", bucket: "pass", workflow_run_id: 4392 },
    ],
    attention_reasons: [],
    ready_to_merge: true,
    workspace_links: [],
    labels: ["desktop"],
    created_at: "2026-08-18T16:30:00.000Z",
    updated_at: "2026-08-20T14:32:00.000Z",
  },
  // A merged pull request. Its review decision is empty, which is exactly the
  // shape that used to render as "Review Pending" in the list.
  {
    id: "github.com/brightwave-inc/tidebreak#2240",
    repository: deliveryRepository,
    number: 2240,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2240",
    title: "Cache the workspace digest between polls",
    state: "merged",
    draft: false,
    author: "mara",
    head_branch: "mara/cache-workspace-digest",
    base_branch: "main",
    head_sha: "1c9dd40",
    mergeable: "mergeable",
    merge_state_status: "clean",
    auto_merge_enabled: false,
    comment_count: 5,
    checks: [
      { name: "workspace / tests", bucket: "pass", workflow_run_id: 4380 },
      { name: "desktop / build", bucket: "pass", workflow_run_id: 4380 },
      { name: "release policy", bucket: "skipped", detail: "skipped" },
    ],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["performance"],
    created_at: "2026-08-17T09:05:00.000Z",
    updated_at: "2026-08-19T11:41:00.000Z",
    merged_at: "2026-08-19T11:41:00.000Z",
  },
  // Closed without merging: same empty review decision, different outcome.
  {
    id: "github.com/brightwave-inc/docs#309",
    repository: deliveryDocsRepository,
    number: 309,
    url: "https://github.com/brightwave-inc/docs/pull/309",
    title: "Rewrite the deployment runbook",
    state: "closed",
    draft: false,
    author: "devon",
    head_branch: "devon/deployment-runbook",
    base_branch: "main",
    head_sha: "9be1140",
    auto_merge_enabled: false,
    comment_count: 1,
    checks: [{ name: "docs / build", bucket: "pass" }],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: "2026-08-14T10:20:00.000Z",
    updated_at: "2026-08-16T08:15:00.000Z",
    closed_at: "2026-08-16T08:15:00.000Z",
  },
  // Merged, but the host still reports CLOSED. Only `merged_at` separates it.
  {
    id: "github.com/brightwave-inc/tidebreak#2233",
    repository: deliveryRepository,
    number: 2233,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2233",
    title: "Split the workspace route",
    state: "closed",
    draft: false,
    author: "ines",
    head_branch: "ines/split-workspace-route",
    base_branch: "main",
    head_sha: "44ad217",
    auto_merge_enabled: false,
    comment_count: 2,
    checks: [{ name: "desktop / tests", bucket: "pass" }],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["desktop"],
    created_at: "2026-08-12T13:00:00.000Z",
    updated_at: "2026-08-15T16:02:00.000Z",
    merged_at: "2026-08-15T16:02:00.000Z",
    closed_at: "2026-08-15T16:02:00.000Z",
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
    comment_count: 0,
    checks: [{ name: "docs / build", bucket: "pending" }],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: "2026-08-20T09:10:00.000Z",
    updated_at: "2026-08-20T14:04:00.000Z",
  },
  // Conflicting: the merge button explains itself instead of failing at the API.
  {
    id: "github.com/brightwave-inc/tidebreak#2229",
    repository: deliveryRepository,
    number: 2229,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2229",
    title: "Adopt the shared status tone map",
    state: "open",
    draft: false,
    author: "devon",
    head_branch: "devon/shared-status-tone",
    base_branch: "main",
    head_sha: "b0c7712",
    review_decision: "review_required",
    mergeable: "conflicting",
    merge_state_status: "dirty",
    auto_merge_enabled: true,
    comment_count: 6,
    checks: [
      { name: "desktop / tests", bucket: "pass" },
      { name: "desktop / lint", bucket: "pending" },
    ],
    attention_reasons: ["conflicts"],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["desktop", "needs-rebase"],
    created_at: "2026-08-11T08:44:00.000Z",
    updated_at: "2026-08-20T09:58:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/model-gateway#1370",
    repository: deliveryGatewayRepository,
    number: 1370,
    url: "https://github.com/brightwave-inc/model-gateway/pull/1370",
    title: "Ask a hosted MCP server which scopes it accepts",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/discovery-scope-preview-stacked",
    base_branch: "main",
    head_sha: "c74f310",
    review_decision: "review_required",
    mergeable: "mergeable",
    merge_state_status: "blocked",
    auto_merge_enabled: true,
    in_merge_queue: true,
    comment_count: 4,
    checks: [
      {
        name: "Preview / Build preview image",
        bucket: "pending",
        detail: "in_progress",
      },
    ],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["console"],
    created_at: "2026-08-24T20:30:00.000Z",
    updated_at: "2026-08-24T21:47:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/tidebreak#2258",
    repository: deliveryRepository,
    number: 2258,
    url: "https://github.com/brightwave-inc/tidebreak/pull/2258",
    title: "Apply reasoning effort changes to the next turn",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/reasoning-effort-next-turn",
    base_branch: "main",
    head_sha: "8c904ac",
    review_decision: "approved",
    mergeable: "mergeable",
    merge_state_status: "blocked",
    auto_merge_enabled: true,
    comment_count: 2,
    checks: [
      { name: "desktop / tests", bucket: "pass" },
      { name: "preview / build", bucket: "pending", detail: "in_progress" },
    ],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["desktop"],
    created_at: "2026-08-24T20:55:00.000Z",
    updated_at: "2026-08-24T21:44:00.000Z",
  },
  {
    id: "github.com/brightwave-inc/model-gateway#1369",
    repository: deliveryGatewayRepository,
    number: 1369,
    url: "https://github.com/brightwave-inc/model-gateway/pull/1369",
    title: "Let the catalog say which scopes a server accepts",
    state: "open",
    draft: false,
    author: "mara",
    head_branch: "thet/console-scope-guidance",
    base_branch: "main",
    head_sha: "e28d4a1",
    mergeable: "mergeable",
    merge_state_status: "blocked",
    auto_merge_enabled: false,
    comment_count: 2,
    checks: [
      {
        name: "Preview / Build preview image",
        bucket: "pending",
        detail: "in_progress",
      },
    ],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: ["console"],
    created_at: "2026-08-24T20:20:00.000Z",
    updated_at: "2026-08-24T21:41:00.000Z",
  },
];

const deliveryFiles: CodeDeliveryPullRequestFile[] = [
  {
    path: "crates/tidebreak-desktop/ui/src/code/CodeDeliveryPage.tsx",
    status: "modified",
    additions: 184,
    deletions: 61,
    // The second hunk carries lines far wider than the sheet: the diff view
    // must scroll them with full-width line backgrounds, not clip them.
    patch: [
      "@@ -858,12 +858,18 @@ function PullRequestList({",
      "       <span>Pull request</span>",
      "-      <span>Review</span>",
      "+      <span>Status</span>",
      "       <span>Checks</span>",
      '       <span className="text-right">Updated</span>',
      "@@ -1020,6 +1026,8 @@ function PullRequestRow({",
      "-  const summary = usePullRequestSummary(item, { includeChecks: true, includeReviewDecision: true, includeMergeState: true, includeWorkspaceLinks: true });",
      "+  const summary = usePullRequestSummary(item, { includeChecks: true, includeReviewDecision: true, includeMergeState: true, includeWorkspaceLinks: true, includeAttentionReasons: true });",
      "+  const checks = checkSummary(checkCounts(item.checks));",
    ].join("\n"),
  },
  {
    path: "crates/tidebreak-desktop/ui/src/code/pullRequestPresentation.ts",
    status: "added",
    additions: 232,
    deletions: 0,
    patch: [
      "@@ -0,0 +1,8 @@",
      "+export function pullRequestLifecycle(item) {",
      '+  if (item.state === "merged" || item.merged_at) return "merged";',
      '+  if (item.state === "closed") return "closed";',
      '+  return item.draft ? "draft" : "open";',
      "+}",
    ].join("\n"),
  },
  {
    path: "crates/tidebreak-desktop/ui/src/code/PrCommentCard.tsx",
    status: "renamed",
    previous_path: "crates/tidebreak-desktop/ui/src/code/CommentRow.tsx",
    additions: 12,
    deletions: 4,
  },
  {
    path: "docs/assets/delivery-center.png",
    status: "added",
    additions: 0,
    deletions: 0,
  },
];

export const deliveryPullRequestDetails: Record<
  number,
  CodeDeliveryPullRequestDetail
> = {
  2251: {
    summary: deliveryPullRequests[0]!,
    body: [
      "## Summary",
      "",
      "- add cross-repository pull request, Actions, and deployment views",
      "- keep repository failures visible beside usable rows",
      "- reuse the shared status tones so `merged` reads as merged",
      "",
      "## Test plan",
      "",
      "- `pnpm --dir crates/tidebreak-desktop/ui test`",
      "- `cargo test -p tidebreak-server delivery`",
    ].join("\n"),
    labels: ["desktop", "ui"],
    assignees: ["mara"],
    requested_reviewers: ["devon"],
    changed_files: 19,
    additions: 2140,
    deletions: 83,
    commits: 7,
    files: deliveryFiles,
    files_truncated: false,
    comments: [
      {
        kind: "review",
        author: "devon",
        created_at: "2026-08-20T14:48:00.000Z",
        review_state: "changes_requested",
        url: "https://github.com/brightwave-inc/tidebreak/pull/2251#pullrequestreview-1",
        body: [
          "The narrow detail state still needs a pass. Two things:",
          "",
          "1. The tab strip wraps under 380px.",
          "2. `Merged` should not borrow the success tone. :eyes:",
        ].join("\n"),
      },
      {
        kind: "inline",
        author: "ines",
        created_at: "2026-08-20T15:02:00.000Z",
        body: "Keep repository failures visible without hiding usable results.",
        path: "src/code/CodeDeliveryPage.tsx",
        line: 702,
      },
      // A fenced, language-tagged block: the newest comment, and the one the
      // syntax palette has to render readably inside a comment card.
      {
        kind: "review",
        author: "mara",
        created_at: "2026-08-20T15:26:00.000Z",
        review_state: "commented",
        url: "https://github.com/brightwave-inc/tidebreak/pull/2251#pullrequestreview-2",
        body: [
          "The lifecycle helper reads better as a lookup. Something like:",
          "",
          "```ts",
          "const LIFECYCLE_BADGE_VARIANT: Record<PullRequestLifecycle, BadgeVariant> = {",
          '  draft: "outline",',
          '  open: "success",',
          '  merged: "merged",',
          '  closed: "critical",',
          "};",
          "```",
          "",
          "Not blocking. :rocket:",
        ].join("\n"),
      },
    ],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: true,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2247: {
    summary: deliveryPullRequests[1]!,
    body: "Keeps workspace URLs stable when panels and delivery links navigate back into active work.",
    labels: ["desktop"],
    assignees: ["devon"],
    requested_reviewers: [],
    changed_files: 6,
    additions: 184,
    deletions: 39,
    commits: 3,
    files: deliveryFiles.slice(0, 2),
    files_truncated: false,
    comments: [
      {
        kind: "review",
        author: "mara",
        created_at: "2026-08-20T14:16:00.000Z",
        review_state: "approved",
        body: "The route behavior is clear and the focused tests cover it. :rocket:",
      },
    ],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2240: {
    summary: deliveryPullRequests[2]!,
    body: "Holds the digest for the poll interval so the rail stops refetching every repository on every tick.",
    labels: ["performance"],
    assignees: [],
    requested_reviewers: [],
    changed_files: 4,
    additions: 96,
    deletions: 51,
    commits: 2,
    merged_by: "devon",
    files: deliveryFiles.slice(0, 1),
    files_truncated: false,
    comments: [
      {
        kind: "review",
        author: "devon",
        created_at: "2026-08-19T11:38:00.000Z",
        review_state: "approved",
        body: "Numbers look right. Merging.",
      },
    ],
    errors: [],
    can_mark_ready: false,
    can_merge: false,
    can_rerun_failed: false,
    can_close: false,
    can_reopen: false,
    can_comment: true,
  },
  309: {
    summary: deliveryPullRequests[3]!,
    body: "Superseded by the runbook in the handbook repo.",
    labels: [],
    assignees: [],
    requested_reviewers: [],
    changed_files: 2,
    additions: 41,
    deletions: 220,
    commits: 1,
    files: [],
    files_truncated: false,
    comments: [
      {
        kind: "issue",
        author: "devon",
        created_at: "2026-08-16T08:14:00.000Z",
        body: "Closing — this moved to the handbook.",
      },
    ],
    errors: [],
    can_mark_ready: false,
    can_merge: false,
    can_rerun_failed: false,
    can_close: false,
    can_reopen: true,
    can_comment: true,
  },
  311: {
    summary: deliveryPullRequests[5]!,
    body: "",
    labels: [],
    assignees: ["ines"],
    requested_reviewers: [],
    changed_files: 3,
    additions: 120,
    deletions: 8,
    commits: 1,
    files: deliveryFiles.slice(3),
    files_truncated: true,
    comments: [],
    errors: [],
    can_mark_ready: true,
    can_merge: false,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2229: {
    summary: deliveryPullRequests[6]!,
    body: "Moves every delivery surface onto the shared status tone maps.",
    labels: ["desktop", "needs-rebase"],
    assignees: [],
    requested_reviewers: ["mara", "ines"],
    changed_files: 11,
    additions: 310,
    deletions: 288,
    commits: 5,
    files: deliveryFiles.slice(0, 3),
    files_truncated: false,
    comments: [],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  1369: {
    summary: deliveryPullRequests[9]!,
    body: "Exposes each server's accepted scopes in the catalog so the console can explain scope mismatches before a connection starts.",
    labels: ["console"],
    assignees: ["mara"],
    requested_reviewers: [],
    changed_files: 5,
    additions: 142,
    deletions: 28,
    commits: 2,
    files: deliveryFiles.slice(0, 2),
    files_truncated: false,
    comments: [
      {
        kind: "issue",
        author: "devon",
        created_at: "2026-08-24T21:12:00.000Z",
        body: "The catalog response now includes the accepted scopes.",
      },
      {
        kind: "issue",
        author: "mara",
        created_at: "2026-08-24T21:32:00.000Z",
        body: "Preview is still building, so the merge stays disabled for now.",
      },
    ],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2311: {
    // The middle layer of the unregistered chain: the sheet offers to
    // register the chain as a host stack instead of merging into it.
    summary: unregisteredDeliveryPullRequests[1]!,
    body: "Stacks the queries layer onto the schema layer.",
    labels: [],
    assignees: [],
    requested_reviewers: [],
    changed_files: 3,
    additions: 180,
    deletions: 40,
    commits: 2,
    files: [],
    files_truncated: false,
    comments: [],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2301: {
    // The bottom layer of the host-reported stack: the stack merge starts
    // here, so its detail fixture carries the same chain.
    summary: stackedDeliveryPullRequests[0]!,
    body: "Lands the fact-store extraction the layers above build on.",
    labels: [],
    assignees: [],
    requested_reviewers: [],
    changed_files: 6,
    additions: 410,
    deletions: 22,
    commits: 2,
    stack: [
      {
        number: 2301,
        state: "open",
        draft: false,
        head_branch: "thet/stack-base",
        head_sha: "a1a1a1a",
      },
      {
        number: 2302,
        state: "open",
        draft: false,
        head_branch: "thet/stack-middle",
        head_sha: "b2b2b2b",
      },
      {
        number: 2303,
        state: "open",
        draft: true,
        head_branch: "thet/stack-tip",
        head_sha: "c3c3c3c",
      },
    ],
    files: [],
    files_truncated: false,
    comments: [],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
  2302: {
    // The middle layer of the host-reported stack: the stack map and the
    // whole-stack merge offer both hang off this payload.
    summary: stackedDeliveryPullRequests[1]!,
    body: [
      "## Summary",
      "",
      "- land the middle layer without stranding the stack",
      "- GitHub retargets and rebases the layers above",
    ].join("\n"),
    labels: [],
    assignees: [],
    requested_reviewers: [],
    changed_files: 4,
    additions: 260,
    deletions: 90,
    commits: 3,
    stack: [
      {
        number: 2301,
        state: "open",
        draft: false,
        head_branch: "thet/stack-base",
        head_sha: "a1a1a1a",
      },
      {
        number: 2302,
        state: "open",
        draft: false,
        head_branch: "thet/stack-middle",
        head_sha: "b2b2b2b",
      },
      {
        number: 2303,
        state: "open",
        draft: true,
        head_branch: "thet/stack-tip",
        head_sha: "c3c3c3c",
      },
    ],
    files: [],
    files_truncated: false,
    comments: [],
    errors: [],
    can_mark_ready: false,
    can_merge: true,
    can_rerun_failed: false,
    can_close: true,
    can_reopen: false,
    can_comment: true,
  },
};

export const deliveryRuns: CodeDeliveryRunSummary[] = [
  {
    id: "github.com/brightwave-inc/tidebreak:workflow_run:4401",
    repository: deliveryRepository,
    kind: "workflow_run",
    github_id: 4401,
    run_attempt: 2,
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
    errors: [],
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
        log_url:
          "https://github.com/brightwave-inc/tidebreak/actions/runs/4388",
        created_at: "2026-08-20T13:29:00.000Z",
      },
    ],
    errors: [],
    can_rerun_failed: false,
  },
};

/**
 * Web-search settings, one entry per verdict the panel can reach.
 *
 * The interesting axis is not "configured or not" but who ends up searching:
 * an engine on this host, or the model the chat is already running on. Every
 * entry below is a state a reader can actually land in.
 */
export const webSearchNoProvider: WebSearchConfigInfo = {
  mode: "automatic",
  timeout_ms: 20000,
  has_credential: false,
  available: false,
};

export const webSearchKeyMissing: WebSearchConfigInfo = {
  ...webSearchNoProvider,
  provider: "brave",
};

export const webSearchReady: WebSearchConfigInfo = {
  mode: "automatic",
  provider: "brave",
  timeout_ms: 20000,
  has_credential: true,
  available: true,
};

export const webSearchFirecrawlReady: WebSearchConfigInfo = {
  ...webSearchReady,
  provider: "firecrawl",
};

export const webSearchVendorOnly: WebSearchConfigInfo = {
  ...webSearchNoProvider,
  mode: "vendor",
};

export const webSearchHostOnlyUnconfigured: WebSearchConfigInfo = {
  ...webSearchNoProvider,
  mode: "host",
  provider: "exa",
};

export const webSearchOff: WebSearchConfigInfo = {
  ...webSearchNoProvider,
  mode: "off",
};

export const webSearchCredentials: WebSearchCredentialReadiness[] = [
  { provider: "exa", has_credential: false },
  { provider: "tavily", has_credential: false },
  { provider: "brave", has_credential: true },
  { provider: "firecrawl", has_credential: false },
];

export const webSearchFirecrawlCredentials: WebSearchCredentialReadiness[] =
  webSearchCredentials.map((credential) => ({
    ...credential,
    has_credential: credential.provider === "firecrawl",
  }));

/**
 * A finished six-call code turn, as the composer's ring receives it.
 *
 * The spend is the shape that made the ring lie: 258,738 prompt-side tokens
 * summed across every call against a 200k window, while the prompt actually
 * resident at the end was 44,172. The ring reads the latter.
 */
export const contextUsageNormal: ContextUsageReading = {
  contextTokens: 44_172,
  spend: {
    input: 1_244,
    output: 12_902,
    cacheRead: 236_180,
    cacheWrite: 8_412,
  },
  contextWindow: 200_000,
  modelName: "Sonnet 5",
};

export const contextUsageWarning: ContextUsageReading = {
  ...contextUsageNormal,
  contextTokens: 158_400,
};

export const contextUsageCritical: ContextUsageReading = {
  ...contextUsageNormal,
  contextTokens: 191_600,
};

/** A model whose window the catalog does not publish: tokens, no percent. */
export const contextUsageUnmetered: ContextUsageReading = {
  ...contextUsageNormal,
  contextWindow: undefined,
  modelName: "Local model",
};

/** An engine that publishes no per-call figure: no fill, no invented percent. */
export const contextUsageNoReading: ContextUsageReading = {
  ...contextUsageNormal,
  contextTokens: null,
};

/** A managed profile whose gateway is named by policy but not signed in to. */
export const gatewaySignedOut: GatewayStatus = {
  base_url: "https://gateway.example.com",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
};

export const gatewaySignedIn: GatewayStatus = {
  ...gatewaySignedOut,
  signed_in: true,
  account_hint: "abaas@example.test",
  installation_id: "inst-4f21",
  model_count: 4,
  member_catalog: "v1",
};

/** Two entitled apps, one of them still waiting on its connection. */
export const gatewayApps: GatewayApps = {
  supported: true,
  apps: [
    {
      id: "github",
      name: "GitHub",
      app_kind: "rest",
      enabled: true,
      mcp_endpoint_slugs: ["engineering"],
      connection: "ready",
      used_by_app_count: 2,
    },
    {
      id: "salesforce",
      name: "Salesforce",
      app_kind: "rest",
      enabled: true,
      mcp_endpoint_slugs: ["revenue"],
      connection: "not_connected",
      used_by_app_count: 0,
    },
  ],
};

/** This window runs on the server inside the app — the default. */
export const machineLocal: RemoteMachineState = {
  attachment: "local",
  baseUrl: null,
};

export const machineAttached: RemoteMachineState = {
  attachment: "remote",
  baseUrl: "https://tidebreak.example.com",
};

/**
 * A model-authored memory record still waiting for review, with the full
 * envelope the strict parsers require: provenance, origin, evidence,
 * revision, and timestamps.
 */
export const memoryProposal: MemoryRecord = {
  id: "3f19d0d5-8f46-4f57-a35a-000000000001",
  scope: { kind: "personal" },
  kind: "lesson",
  status: "proposed",
  title: "When changing database migrations",
  body: "Run the migration chain test before publishing.",
  provenance: {
    author: "model",
    origin: {
      chat_id: "9d5d84a0-6ba6-4c73-9e10-000000000001",
      turn_id: "turn-storybook-memory",
      code_session_id: null,
      code_turn_id: null,
      workspace_id: null,
    },
    evidence: [
      { kind: "message", message_id: "c6a0d000-0000-4000-8000-000000000001" },
    ],
  },
  links: [],
  expires_at: null,
  superseded_by: null,
  observation_count: 0,
  revision: 1,
  created_at: "2026-09-01T09:00:00Z",
  updated_at: "2026-09-01T09:00:00Z",
};

/** A reviewed record that already carries authority in the digest. */
export const memoryActive: MemoryRecord = {
  ...memoryProposal,
  id: "3f19d0d5-8f46-4f57-a35a-000000000002",
  status: "active",
  title: "When preparing a release",
  body: "Run the release smoke test before publishing.",
  revision: 2,
  updated_at: "2026-08-30T14:00:00Z",
};

/** A tracked hypothesis gathering observations before it can be proposed. */
export const memoryTracking: MemoryRecord = {
  ...memoryProposal,
  id: "3f19d0d5-8f46-4f57-a35a-000000000003",
  status: "tracking",
  title: "When reviewing pull requests",
  body: "The reader prefers concrete migration checks over general praise.",
  provenance: {
    ...memoryProposal.provenance,
    evidence: [
      { kind: "message", message_id: "c6a0d000-0000-4000-8000-000000000002" },
    ],
  },
  observation_count: 2,
};

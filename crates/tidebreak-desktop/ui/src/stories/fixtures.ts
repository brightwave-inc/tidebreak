import type {
  Attention,
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWatchSnapshot,
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  HarnessCaps,
  HarnessDoctorEntry,
  HarnessDoctorReport,
  PendingUserQuestions,
  TaskPlan,
  ToolActionPreview,
} from "@/api";

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

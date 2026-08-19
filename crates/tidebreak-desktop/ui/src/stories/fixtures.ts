import type {
  CodeWorkspacePrSnapshot,
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

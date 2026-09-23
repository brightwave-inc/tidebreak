import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { CodeApprovalCard } from "@/code/CodeApprovalCard";
import type { CodeApprovalSnapshot } from "@/api/types";

const pending: CodeApprovalSnapshot = {
  id: "3f1c0d4a-0000-4000-8000-000000000001",
  session_id: "3f1c0d4a-0000-4000-8000-000000000002",
  turn_id: "3f1c0d4a-0000-4000-8000-000000000003",
  kind: {
    type: "command",
    cmd: "cargo test -p tidebreak-server",
    cwd: "/workspace",
  },
  harness_raw_json: JSON.stringify(
    {
      tool_name: "Bash",
      input: { command: "cargo test -p tidebreak-server" },
      tool_use_id: "toolu_01ApprovalStory",
    },
    null,
    2,
  ),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

/**
 * The code transcript's approval card. The states that matter are the two the
 * user creates and the one the engine creates for them: an approval whose tool
 * call resolved before anyone decided is `abandoned`, and it must read as a
 * request that went undecided rather than as a denial.
 */
const meta = {
  title: "Code/Approval card",
  component: CodeApprovalCard,
  args: {
    approval: pending,
    onDecide: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof CodeApprovalCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Pending: Story = {};

export const Deciding: Story = {
  args: { deciding: true },
};

export const Approved: Story = {
  args: {
    approval: {
      ...pending,
      state: "approved",
      decided_at: "2026-08-15T12:00:12.000Z",
    },
  },
};

export const Denied: Story = {
  args: {
    approval: {
      ...pending,
      state: "denied",
      feedback:
        "Run the focused test instead — the workspace suite is too slow.",
      decided_at: "2026-08-15T12:00:20.000Z",
    },
  },
};

/**
 * The engine timed the parked tool call out. Nobody decided, and nobody can:
 * the card drops its buttons and says so, because the alternative is a row
 * that sits pending forever and accepts an approval that reaches nothing.
 */
export const Abandoned: Story = {
  args: {
    approval: {
      ...pending,
      state: "abandoned",
      decided_at: "2026-08-15T12:01:00.000Z",
    },
  },
};

export const DecisionFailed: Story = {
  args: {
    error: "The decision could not be saved. The command has not run.",
  },
};

/**
 * A structured tool_use approval from an engine behind the adapter. The card
 * shows the literal action — argv, working directory, staged files — and
 * never the call's own display-only narration (decision 0018).
 */
export const ToolUse: Story = {
  args: {
    approval: {
      ...pending,
      kind: {
        type: "tool_use",
        preview: {
          tool: "exec",
          command: "python3",
          args: ["analyze.py", "--input", "sales report.csv"],
          cwd: "work",
          files: ["sales report.csv"],
          summary: "Analyzing the sales report",
        },
        offered_grants: [],
      },
      harness_raw_json: "",
    },
  },
};

const repositoryWork: CodeApprovalSnapshot = {
  ...pending,
  kind: {
    type: "tool_use",
    preview: {
      tool: "code_session",
      operation: "create",
      target: "example/repository",
      task: "Inspect the failing test and report the cause. Wait before changing files.",
      harness: "codex",
      model: "example-model",
    },
    offered_grants: [],
  },
  harness_raw_json: "",
};

export const RepositoryWork: Story = {
  args: { approval: repositoryWork },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Start this repository work?")).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Approve" })).toBeEnabled();
    await expect(
      canvas.queryByText(/don't ask again/i),
    ).not.toBeInTheDocument();
  },
};

export const RepositoryFollowUp: Story = {
  args: {
    approval: {
      ...repositoryWork,
      kind: {
        type: "tool_use",
        offered_grants: [],
        preview: {
          tool: "code_session",
          operation: "continue",
          target: "3f1c0d4a-0000-4000-8000-000000000004",
          task: "Run the focused regression test and report its result.",
          harness: null,
          model: null,
        },
      },
    },
  },
};

export const RepositoryWorkDeciding: Story = {
  args: { approval: repositoryWork, deciding: true },
};

export const RepositoryWorkFailed: Story = {
  args: {
    approval: repositoryWork,
    error: "Your decision could not be saved. Repository work has not started.",
  },
};

export const RepositoryWorkLong: Story = {
  args: {
    approval: {
      ...repositoryWork,
      kind: {
        type: "tool_use",
        offered_grants: [],
        preview: {
          tool: "code_session",
          operation: "create",
          target:
            "example/repository-with-a-long-name-for-verifying-the-consent-card-at-narrow-widths",
          task: "Inspect the regression that prevents Slack child sessions from receiving approvals. Check the exact session and turn identifiers, preserve the parent's status card, and verify that reconnecting does not duplicate the request. Report the finding before changing files.",
          harness: "claude_code",
          model: "provider-model-with-a-long-identifier-for-consent-preview",
        },
      },
    },
  },
};

export const Questions: Story = {
  args: {
    approval: {
      ...pending,
      kind: {
        type: "questions",
        questions: [
          {
            id: "region",
            header: "Region",
            question: "Which region should the deploy target?",
            options: [
              { id: "east", label: "us-east", description: "" },
              { id: "west", label: "us-west", description: "" },
            ],
            question_type: "single_select",
            allow_free_form: false,
          },
        ],
      },
      harness_raw_json: "",
    },
  },
};

const nativePlan: CodeApprovalSnapshot = {
  ...pending,
  kind: { type: "plan", proposed_mode: "allow" },
  harness_raw_json: JSON.stringify({
    title: "Verify the hosted approval flow",
    plan: [
      "## Proposed steps",
      "1. Inspect the pending approval and confirm its conversation identity.",
      "2. After you approve, run the bounded command:",
      "```sh\nprintf 'TB-PLAN-EXECUTED\\n'\n```",
      "3. Report the command output once in this thread.",
      "## Limits",
      "Do not clone a repository, edit files, or start background work.",
    ].join("\n\n"),
  }),
};

export const Plan: Story = {
  args: { approval: nativePlan },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("heading", { name: "Verify the hosted approval flow" }),
    ).toBeVisible();
    await expect(
      canvas.getByRole("region", { name: "Proposed plan" }),
    ).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Engine request" }),
    ).toHaveAttribute("aria-expanded", "false");
  },
};

export const PlanLong: Story = {
  args: {
    approval: {
      ...nativePlan,
      harness_raw_json: JSON.stringify({
        title:
          "Verify the hosted approval flow across retries and session recovery",
        plan: [
          "## Scope",
          "Check the pending decision in Slack and the hosted workspace. Keep the same approval identifier throughout the test.",
          ...Array.from(
            { length: 7 },
            (_, index) =>
              `### Check ${index + 1}\n\n1. Read the pending state and record the conversation identifier.\n2. Reload the hosted workspace and confirm that the exact plan remains readable.\n3. Verify that no command executes before you approve.`,
          ),
          "## Final check",
          "Report the result once and release the test resources.",
        ].join("\n\n"),
      }),
    },
  },
};

export const PlanExpanded: Story = {
  args: PlanLong.args,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Show full plan" }),
    );
    await expect(
      canvas.getByRole("button", { name: "Show less" }),
    ).toHaveAttribute("aria-expanded", "true");
  },
};

export const PlanDeciding: Story = {
  args: { approval: nativePlan, deciding: true },
};

export const PlanDecisionFailed: Story = {
  args: {
    approval: nativePlan,
    error: "Your decision could not be saved. The plan has not run. Try again.",
  },
};

export const PlanReadOnly: Story = {
  args: { approval: nativePlan, canDecide: false },
};

export const PlanWithoutBody: Story = {
  args: { approval: { ...nativePlan, harness_raw_json: "" } },
};

export const QuestionsSelected: Story = {
  args: Questions.args,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole("radio", { name: "us-east" }));
    await expect(
      canvas.getByRole("button", { name: "Continue" }),
    ).toBeEnabled();
  },
};
export const QuestionsSending: Story = {
  args: { ...Questions.args, deciding: true },
};
export const QuestionsSaveFailed: Story = {
  args: {
    ...Questions.args,
    error: "Your answer could not be saved. Try again.",
  },
  play: QuestionsSelected.play,
};
export const QuestionsReadOnly: Story = {
  args: { ...Questions.args, canDecide: false },
};
export const QuestionsMultiSelect: Story = {
  args: {
    approval: {
      ...pending,
      harness_raw_json: "",
      kind: {
        type: "questions",
        questions: [
          {
            id: "checks",
            header: "Checks",
            question: "Which checks should run before deployment?",
            question_type: "multi_select",
            allow_free_form: true,
            options: [
              {
                id: "unit",
                label: "Unit tests",
                description: "Run the focused test suite.",
              },
              {
                id: "browser",
                label: "Browser tests",
                description: "Check the complete user journey.",
              },
            ],
          },
        ],
      },
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(canvas.getByRole("checkbox", { name: "Unit tests" }));
    await userEvent.click(
      canvas.getByRole("checkbox", { name: "Browser tests" }),
    );
    await userEvent.type(
      canvas.getByRole("textbox", { name: "Other answer" }),
      "Check the service logs",
    );
  },
};
export const QuestionsSeveralPages: Story = {
  args: {
    approval: {
      ...pending,
      harness_raw_json: "",
      kind: {
        type: "questions",
        questions: [
          {
            id: "environment",
            header: "Environment",
            question: "Which environment should receive this deployment?",
            question_type: "single_select",
            allow_free_form: false,
            options: [
              {
                id: "staging",
                label: "Staging",
                description: "Verify the change before production.",
              },
              {
                id: "production",
                label: "Production",
                description: "Deploy to the live environment.",
              },
            ],
          },
          {
            id: "notes",
            header: "Notes",
            question: "What else should the agent verify?",
            question_type: "single_select",
            allow_free_form: true,
            options: [],
          },
          {
            id: "later",
            header: "Follow-up",
            question: "Which follow-up should the agent prepare?",
            question_type: "single_select",
            allow_free_form: false,
            options: [
              {
                id: "release",
                label: "Release notes",
                description: "Write a short summary of the final changes.",
              },
            ],
          },
        ],
      },
    },
  },
};

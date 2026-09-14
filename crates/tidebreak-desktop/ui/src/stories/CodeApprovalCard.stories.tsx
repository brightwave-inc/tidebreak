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

export const Plan: Story = {
  args: {
    approval: {
      ...pending,
      kind: { type: "plan", proposed_mode: "auto" },
      harness_raw_json: "",
    },
  },
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

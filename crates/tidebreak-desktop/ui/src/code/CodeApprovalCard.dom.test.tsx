// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeApprovalCard, MAX_PAYLOAD_CHARS } from "./CodeApprovalCard";
import type { CodeApprovalSnapshot } from "../api/types";
import { parseCodeApproval } from "./parsers";

afterEach(() => {
  cleanup();
});

const pendingCommand: CodeApprovalSnapshot = {
  id: "appr-1",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "command", cmd: "rm -rf /tmp/scratch", cwd: "/workspace" },
  harness_raw_json: JSON.stringify({
    tool_name: "Bash",
    command: "rm -rf /tmp/scratch",
    tool_use_id: "toolu_1",
  }),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingWrite: CodeApprovalSnapshot = {
  id: "appr-2",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "file_write", paths: ["/workspace/probe.txt"] },
  harness_raw_json: JSON.stringify({
    tool_name: "Write",
    input: { file_path: "/workspace/probe.txt", content: "hello" },
    tool_use_id: "toolu_2",
  }),
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingToolUse: CodeApprovalSnapshot = {
  id: "appr-3",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: {
    type: "tool_use",
    preview: {
      tool: "exec",
      command: "rm",
      args: ["-rf", "two words"],
      cwd: "work",
      files: ["notes.md"],
      summary: "Cleaning temporary caches",
    },
    offered_grants: [],
  },
  harness_raw_json: "",
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingQuestions: CodeApprovalSnapshot = {
  id: "appr-4",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: {
    type: "questions",
    questions: [
      {
        id: "q1",
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
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

const pendingPlan: CodeApprovalSnapshot = {
  id: "appr-5",
  session_id: "sess-1",
  turn_id: "turn-1",
  kind: { type: "plan", proposed_mode: "auto" },
  harness_raw_json: "",
  state: "pending",
  requested_at: "2026-08-15T12:00:00.000Z",
};

describe("CodeApprovalCard", () => {
  it.each(["create", "continue"] as const)(
    "approves %s repository work once",
    async (operation) => {
      const user = userEvent.setup();
      const onDecide = vi.fn();
      render(
        <CodeApprovalCard
          approval={{
            ...pendingToolUse,
            kind: {
              type: "tool_use",
              offered_grants: [],
              preview: {
                tool: "code_session",
                operation,
                target: "example/repository",
                task: "Inspect the failing test.",
                harness: "codex",
                model: "example-model",
              },
            },
          }}
          onDecide={onDecide}
        />,
      );
      expect(
        screen.getByText(
          operation === "create"
            ? "Start this repository work?"
            : "Send this follow-up?",
        ),
      ).toBeVisible();
      expect(screen.getByText(/Inspect the failing test/)).toHaveTextContent(
        "Harness: codex",
      );
      expect(screen.queryByText(/don't ask again/i)).not.toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "Approve" }));
      expect(onDecide).toHaveBeenCalledOnce();
      expect(onDecide).toHaveBeenCalledWith("approve");
    },
  );

  it.each([pendingCommand, pendingQuestions, pendingPlan])(
    "shows the Slack contributor after parsing a settled $kind.type approval",
    (pending) => {
      const approval = parseCodeApproval({
        ...pending,
        id: "49fbc2e2-8a49-4f2a-a1b4-d88a4dbabf35",
        session_id: "a61fba8a-c67f-4a50-baf7-106d5d4561ed",
        turn_id: "7dccefd4-f54f-4c3b-bd52-399741af7c83",
        state: "approved",
        decided_at: "2026-09-12T12:05:00.000Z",
        actor: {
          principal: null,
          channel_kind: "slack",
          external_identity: "U-native-canary",
          display: "Ada Lovelace",
        },
      });
      expect(approval).not.toBeNull();
      render(<CodeApprovalCard approval={approval!} onDecide={vi.fn()} />);
      expect(screen.getByText("Decided by Ada Lovelace")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
      expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
    },
  );

  it("shows the literal tool action, never the model's narration", () => {
    render(<CodeApprovalCard approval={pendingToolUse} onDecide={vi.fn()} />);
    expect(screen.getByText("Run this tool?")).toBeInTheDocument();
    const detail = screen.getByText(/rm -rf 'two words'/);
    expect(detail.tagName).toBe("PRE");
    expect(detail.textContent).toContain("# working directory: work");
    expect(detail.textContent).toContain("# staged files: notes.md");
    // Decision 0018: the display-only summary never reaches the consent card.
    expect(
      screen.queryByText(/Cleaning temporary caches/),
    ).not.toBeInTheDocument();
  });

  it("lists the questions and options the engine is asking", () => {
    render(<CodeApprovalCard approval={pendingQuestions} onDecide={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(
      screen.getByText("Which region should the deploy target?"),
    ).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "us-east" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "us-west" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
  });

  it("names the mode a plan approval would move the session to", () => {
    render(<CodeApprovalCard approval={pendingPlan} onDecide={vi.fn()} />);
    expect(screen.getByText("Approve this plan?")).toBeInTheDocument();
    expect(screen.getByText("auto")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Deny" })).toBeEnabled();
  });

  it.each(["pending", "approved"] as const)(
    "shows the exact native plan from the stored payload while %s",
    (state) => {
      render(
        <CodeApprovalCard
          approval={{
            ...pendingPlan,
            state,
            harness_raw_json: JSON.stringify({
              title: "Verify the hosted approval",
              plan: "## Verification\n\n1. Inspect the pending request.\n2. Run `printf TB-PLAN`.\n\nDo not change any repository files.",
            }),
          }}
          onDecide={vi.fn()}
        />,
      );
      expect(
        screen.getByRole("heading", { name: "Verify the hosted approval" }),
      ).toBeVisible();
      const plan = within(
        screen.getByRole("region", { name: "Proposed plan" }),
      );
      expect(plan.getByRole("heading", { name: "Verification" })).toBeVisible();
      expect(plan.getAllByRole("listitem")).toHaveLength(2);
      expect(plan.getByText("printf TB-PLAN").tagName).toBe("CODE");
      expect(
        plan.getByText("Do not change any repository files."),
      ).toBeVisible();
      expect(
        screen.getByRole("button", { name: "Harness payload" }),
      ).toHaveAttribute("aria-expanded", "false");
      if (state === "approved") {
        expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
      }
    },
  );

  it("keeps a long plan complete beyond the raw payload preview limit", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingPlan,
          harness_raw_json: JSON.stringify({
            title: "A complete plan",
            plan: `${"Review the next step.\n\n".repeat(1100)}Final condition: remove the temporary output.`,
          }),
        }}
        onDecide={vi.fn()}
      />,
    );
    const plan = screen.getByRole("region", { name: "Proposed plan" });
    expect(plan.textContent!.length).toBeGreaterThan(MAX_PAYLOAD_CHARS);
    expect(
      within(plan).getByText("Final condition: remove the temporary output."),
    ).toBeVisible();
    expect(plan).toHaveAttribute("tabindex", "0");
  });

  it("lets the reader expand and collapse an overflowing plan", () => {
    const scrollHeight = vi
      .spyOn(HTMLElement.prototype, "scrollHeight", "get")
      .mockReturnValue(800);
    const clientHeight = vi
      .spyOn(HTMLElement.prototype, "clientHeight", "get")
      .mockReturnValue(336);
    const onReveal = vi.fn();
    try {
      render(
        <CodeApprovalCard
          approval={{
            ...pendingPlan,
            harness_raw_json: JSON.stringify({
              title: "Review all steps",
              plan: "Read the complete plan before approving.",
            }),
          }}
          onDecide={vi.fn()}
          onReveal={onReveal}
        />,
      );
      const region = screen.getByRole("region", { name: "Proposed plan" });
      fireEvent.click(screen.getByRole("button", { name: "Show full plan" }));
      expect(region).not.toHaveClass("max-h-96");
      expect(screen.getByRole("button", { name: "Show less" })).toHaveAttribute(
        "aria-expanded",
        "true",
      );
      expect(onReveal).toHaveBeenCalledOnce();
      fireEvent.click(screen.getByRole("button", { name: "Show less" }));
      expect(region).toHaveClass("max-h-96");
    } finally {
      scrollHeight.mockRestore();
      clientHeight.mockRestore();
    }
  });

  it.each([
    "",
    "not json",
    "null",
    "[]",
    '{"title":"Plan","plan":5}',
    '{"title":"Plan","plan":"  "}',
  ])(
    "keeps legacy or malformed plan payloads readable without crashing: %s",
    (harness_raw_json) => {
      render(
        <CodeApprovalCard
          approval={{ ...pendingPlan, harness_raw_json }}
          onDecide={vi.fn()}
        />,
      );
      expect(screen.getByText("Approve this plan?")).toBeVisible();
      expect(
        screen.queryByRole("region", { name: "Proposed plan" }),
      ).toBeNull();
      expect(screen.getByText("auto")).toBeVisible();
    },
  );

  it("leads with the command and keeps the harness payload collapsed", () => {
    render(<CodeApprovalCard approval={pendingCommand} onDecide={vi.fn()} />);
    expect(screen.getByText("Run this command?")).toBeInTheDocument();
    expect(screen.getByText("rm -rf /tmp/scratch").tagName).toBe("PRE");
    expect(screen.getByText("cwd /workspace")).toBeInTheDocument();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:00:00.000Z"]'),
    ).not.toBeNull();

    const toggle = screen.getByRole("button", { name: "Harness payload" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(/"tool_name": "Bash"/)).toBeInTheDocument();
  });

  it("lists the paths a file write would touch", () => {
    render(<CodeApprovalCard approval={pendingWrite} onDecide={vi.fn()} />);
    expect(screen.getByText("Write this file?")).toBeInTheDocument();
    expect(screen.getByText("/workspace/probe.txt")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Harness payload" }),
    ).toHaveAttribute("aria-expanded", "false");
  });

  it("opens a feedback field on deny and submits it", () => {
    const onDecide = vi.fn();
    render(<CodeApprovalCard approval={pendingCommand} onDecide={onDecide} />);
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    const box = screen.getByRole("textbox", { name: "Denial feedback" });
    fireEvent.change(box, {
      target: { value: "no — use the fixtures directory instead" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(onDecide).toHaveBeenCalledWith(
      "deny",
      "no — use the fixtures directory instead",
    );
  });

  it("caps a payload an engine sent a whole file in", () => {
    const huge = "x".repeat(MAX_PAYLOAD_CHARS * 2);
    render(
      <CodeApprovalCard
        approval={{
          ...pendingWrite,
          harness_raw_json: JSON.stringify({ content: huge }),
        }}
        onDecide={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Harness payload" }));
    const shown = screen.getByText(/more characters not shown/);
    expect(shown.textContent!.length).toBeLessThan(MAX_PAYLOAD_CHARS + 200);
  });

  it("tones an approved card as success and stamps both times", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "approved",
          decided_at: "2026-08-15T12:05:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Approved")).toHaveClass("text-success-foreground");
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:00:00.000Z"]'),
    ).not.toBeNull();
    expect(
      document.querySelector('time[datetime="2026-08-15T12:05:00.000Z"]'),
    ).not.toBeNull();
  });

  it("drops the buttons on an approval nobody decided", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "abandoned",
          decided_at: "2026-08-15T12:01:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Not decided")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
    expect(
      screen.getByText(/a decision can no longer\s+reach it/),
    ).toBeInTheDocument();
  });

  it("does not render an unknown engine summary as the decision", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          kind: { type: "other", summary: "unknown" },
          harness_raw_json: "null",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Allow this?")).toBeInTheDocument();
    expect(screen.getByText("The engine needs approval")).toBeInTheDocument();
    expect(screen.queryByText("unknown")).not.toBeInTheDocument();
  });

  it("tones a denial as a warning and shows the feedback", () => {
    render(
      <CodeApprovalCard
        approval={{
          ...pendingCommand,
          state: "denied",
          feedback: "no — use the fixtures directory instead",
          decided_at: "2026-08-15T12:05:00.000Z",
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Denied")).toHaveClass("text-warning-foreground");
    expect(
      screen.getByText("no — use the fixtures directory instead"),
    ).toBeInTheDocument();
  });
});

it("submits a structured answer and never sends a bare approve for questions", async () => {
  const onDecide = vi.fn();
  render(<CodeApprovalCard approval={pendingQuestions} onDecide={onDecide} />);
  const user = userEvent.setup();
  expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
  expect(screen.queryByText("Continue and add context")).toBeNull();
  await user.click(screen.getByRole("radio", { name: "us-west" }));
  await user.click(screen.getByRole("button", { name: "Continue" }));
  expect(onDecide).toHaveBeenCalledExactlyOnceWith({
    answers: {
      answers: [{ question_id: "q1", selected_option_ids: ["west"] }],
    },
  });
});
it("preserves several choices and free-form text while omitting unanswered questions", async () => {
  const approval: CodeApprovalSnapshot = {
    ...pendingQuestions,
    kind: {
      type: "questions",
      questions: [
        {
          id: "q1",
          header: "Region",
          question: "Which region?",
          question_type: "single_select",
          allow_free_form: false,
          options: [{ id: "east", label: "East", description: "US east" }],
        },
        {
          id: "q2",
          header: "Optional",
          question: "Optional note?",
          question_type: "single_select",
          allow_free_form: true,
          options: [],
        },
        {
          id: "q3",
          header: "Checks",
          question: "Which checks?",
          question_type: "multi_select",
          allow_free_form: true,
          options: [
            { id: "unit", label: "Unit tests", description: "Fast checks" },
            { id: "browser", label: "Browser tests", description: "UI checks" },
          ],
        },
      ],
    },
  };
  const onDecide = vi.fn();
  render(<CodeApprovalCard approval={approval} onDecide={onDecide} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("radio", { name: "East" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.click(screen.getByRole("checkbox", { name: "Unit tests" }));
  await user.click(screen.getByRole("checkbox", { name: "Browser tests" }));
  await user.type(
    screen.getByRole("textbox", { name: "Other answer" }),
    " Also check logs ",
  );
  await user.click(screen.getByRole("button", { name: "Continue" }));
  expect(onDecide).toHaveBeenCalledExactlyOnceWith({
    answers: {
      answers: [
        { question_id: "q1", selected_option_ids: ["east"] },
        {
          question_id: "q3",
          selected_option_ids: ["unit", "browser"],
          custom_answer: "Also check logs",
        },
      ],
    },
  });
});
it("retains an answer after a failed request and disables it while sending", async () => {
  const onDecide = vi.fn();
  const view = render(
    <CodeApprovalCard approval={pendingQuestions} onDecide={onDecide} />,
  );
  await userEvent.setup().click(screen.getByRole("radio", { name: "us-east" }));
  view.rerender(
    <CodeApprovalCard
      approval={pendingQuestions}
      onDecide={onDecide}
      deciding
    />,
  );
  expect(screen.getByRole("radio", { name: "us-east" })).toBeDisabled();
  view.rerender(
    <CodeApprovalCard
      approval={pendingQuestions}
      onDecide={onDecide}
      error="Could not record your answer. Try again."
    />,
  );
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Could not record your answer",
  );
  expect(screen.getByRole("radio", { name: "us-east" })).toBeChecked();
  expect(screen.getByRole("button", { name: "Continue" })).toBeEnabled();
});
it("skips questions through the supported deny decision", async () => {
  const onDecide = vi.fn();
  render(<CodeApprovalCard approval={pendingQuestions} onDecide={onDecide} />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Skip" }));
  expect(onDecide).toHaveBeenCalledExactlyOnceWith(
    "deny",
    "Questions skipped.",
  );
});
it("shows pending questions without actions to a read-only viewer", () => {
  render(
    <CodeApprovalCard
      approval={pendingQuestions}
      onDecide={vi.fn()}
      canDecide={false}
    />,
  );
  expect(
    screen.getByText("Which region should the deploy target?"),
  ).toBeVisible();
  expect(screen.queryByRole("radio")).toBeNull();
  expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Deny" })).toBeNull();
});
it("sends an explicit plan decision from the existing approve action", async () => {
  const onDecide = vi.fn();
  render(<CodeApprovalCard approval={pendingPlan} onDecide={onDecide} />);
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "Approve" }));
  expect(onDecide).toHaveBeenCalledExactlyOnceWith({
    plan_decision: { approve: true },
  });
});

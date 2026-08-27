import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  answerMobileUserQuestions,
  decideMobilePlan,
  decideMobileToolApproval,
  listMobilePendingPlanApprovals,
  listMobilePendingToolApprovals,
  listMobilePendingUserQuestions,
  mobileApprovalQuestion,
  mobileToolPreviewDetail,
  parseMobilePendingPlanApproval,
  parseMobilePendingToolApproval,
  parseMobilePendingUserQuestions,
  parseMobileToolActionPreview,
} from "./chatPrompts";

const execPreview = {
  tool: "exec",
  command: "pnpm",
  args: ["test", "--", "chat prompts"],
  cwd: "mobile",
  files: ["package.json", "src/lib/chatPrompts.ts"],
  summary: "Run the focused mobile tests.",
};

const approval = {
  call_id: "call-approval",
  turn_id: "turn-1",
  action: "exec",
  approval: "exec_may_run_networked_command",
  class: "sensitive",
  preview: execPreview,
  can_approve: true,
  can_remember: true,
  grant_rungs: [
    "exact_action",
    { command_prefix: { tokens: 1 } },
    "whole_tool",
  ],
  auto_judge_status: "judging",
};

const questions = {
  call_id: "call-questions",
  turn_id: "turn-1",
  questions: [
    {
      id: "target",
      header: "Target",
      question: "Where should I deploy?",
      options: [
        {
          id: "staging",
          label: "Staging",
          description: "Deploy for internal verification.",
        },
        {
          id: "production",
          label: "Production",
          description: "Deploy to customers.",
        },
      ],
      question_type: "single_select",
      allow_free_form: true,
    },
  ],
  asked_at: "2026-08-27T20:00:00Z",
};

const plan = {
  call_id: "call-plan",
  turn_id: "turn-1",
  title: "Add mobile prompt cards",
  plan:
    "## Steps\n1. Parse each pending prompt strictly.\n2. Show its mobile card.\n3. Submit the exact decision.",
  proposed_at: "2026-08-27T20:00:01Z",
};

function fakeClient(response: unknown): {
  client: Pick<MachineClient, "getJson" | "requestJson">;
  getJson: ReturnType<typeof vi.fn>;
  requestJson: ReturnType<typeof vi.fn>;
} {
  const getJson = vi.fn(async () => response);
  const requestJson = vi.fn(async () => response);
  return { client: { getJson, requestJson }, getJson, requestJson };
}

describe("mobile chat prompt contracts", () => {
  it("parses a closed approval and preserves its exact action preview", () => {
    expect(parseMobilePendingToolApproval(approval)).toEqual({
      callId: "call-approval",
      turnId: "turn-1",
      action: "exec",
      approval: "exec_may_run_networked_command",
      class: "sensitive",
      preview: execPreview,
      canApprove: true,
      canRemember: true,
      grantRungs: approval.grant_rungs,
      autoJudgeStatus: "judging",
    });
    expect(
      parseMobilePendingToolApproval({ ...approval, can_approve: false }),
    ).toBeNull();
    expect(
      parseMobilePendingToolApproval({ ...approval, hidden_argument: "no" }),
    ).toBeNull();
    expect(
      parseMobilePendingToolApproval({
        ...approval,
        grant_rungs: [{ command_prefix: { tokens: 0 } }],
      }),
    ).toBeNull();
  });

  it("rejects an untrusted preview instead of partially rendering it", () => {
    expect(parseMobileToolActionPreview(execPreview)).toEqual(execPreview);
    expect(
      parseMobileToolActionPreview({
        ...execPreview,
        command: "pnpm\ntouch hidden",
      }),
    ).toBeNull();
    expect(
      parseMobileToolActionPreview({
        ...execPreview,
        secret: "raw arguments stay behind the server boundary",
      }),
    ).toBeNull();
    expect(
      parseMobileToolActionPreview({
        tool: "delegate_agent",
        task: "Inspect the mobile build.",
        network: {
          mode: "allowed_hosts",
          allowed_hosts: ["registry.npmjs.org"],
          package_managers: true,
        },
      }),
    ).toMatchObject({ tool: "delegate_agent" });
  });

  it("loads approvals only when calls are unique and belong to one turn", async () => {
    const listed = fakeClient([approval]);
    await expect(
      listMobilePendingToolApprovals(listed.client, "chat/1"),
    ).resolves.toHaveLength(1);
    expect(listed.getJson).toHaveBeenCalledWith("/chats/chat%2F1/approvals");

    const duplicate = fakeClient([approval, approval]);
    await expect(
      listMobilePendingToolApprovals(duplicate.client, "chat-1"),
    ).rejects.toThrow(/duplicate call/i);

    const mixedTurns = fakeClient([
      approval,
      { ...approval, call_id: "call-2", turn_id: "turn-2" },
    ]);
    await expect(
      listMobilePendingToolApprovals(mixedTurns.client, "chat-1"),
    ).rejects.toThrow(/multiple turns/i);
  });

  it("sends approve-once and bounded rejection feedback", async () => {
    const sent = fakeClient(undefined);
    await decideMobileToolApproval(
      sent.client,
      "chat/1",
      "call/1",
      { decision: "approve" },
    );
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat%2F1/approvals/call%2F1",
      {
        method: "POST",
        body: { decision: "approve", grant: null },
        expectedStatus: 204,
      },
    );

    await decideMobileToolApproval(
      sent.client,
      "chat-1",
      "call-2",
      { decision: "reject", feedback: "  Use the cached result.  " },
    );
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat-1/approvals/call-2",
      {
        method: "POST",
        body: {
          decision: "reject",
          reason: "Use the cached result.",
        },
        expectedStatus: 204,
      },
    );
    await expect(
      decideMobileToolApproval(sent.client, "chat-1", "call-3", {
        decision: "reject",
        feedback: "   ",
      }),
    ).rejects.toThrow(/what to change/i);
  });

  it("parses question blocks as one closed, unique answer contract", async () => {
    expect(parseMobilePendingUserQuestions(questions)).toEqual({
      callId: "call-questions",
      turnId: "turn-1",
      questions: [
        {
          id: "target",
          header: "Target",
          question: "Where should I deploy?",
          options: questions.questions[0]!.options,
          questionType: "single_select",
          allowFreeForm: true,
        },
      ],
      askedAt: "2026-08-27T20:00:00Z",
    });
    expect(
      parseMobilePendingUserQuestions({
        ...questions,
        questions: [
          {
            ...questions.questions[0],
            options: [
              questions.questions[0]!.options[0],
              questions.questions[0]!.options[0],
            ],
          },
        ],
      }),
    ).toBeNull();
    expect(
      parseMobilePendingUserQuestions({
        ...questions,
        questions: [
          {
            ...questions.questions[0],
            options: [],
            allow_free_form: false,
          },
        ],
      }),
    ).toBeNull();

    const listed = fakeClient([questions]);
    await expect(
      listMobilePendingUserQuestions(listed.client, "chat/1"),
    ).resolves.toHaveLength(1);
    expect(listed.getJson).toHaveBeenCalledWith(
      "/chats/chat%2F1/questions/pending",
    );
  });

  it("submits selected options, free-form text, context, and explicit skips", async () => {
    const sent = fakeClient(undefined);
    await answerMobileUserQuestions(
      sent.client,
      "chat/1",
      "call/1",
      [
        {
          questionId: "target",
          selectedOptionIds: ["staging"],
          customAnswer: "  Use the blue environment.  ",
        },
      ],
      "  Keep production untouched.  ",
    );
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat%2F1/questions/call%2F1/answer",
      {
        method: "POST",
        body: {
          answers: [
            {
              question_id: "target",
              selected_option_ids: ["staging"],
              custom_answer: "Use the blue environment.",
            },
          ],
          additional_user_context: "Keep production untouched.",
        },
      },
    );

    await answerMobileUserQuestions(
      sent.client,
      "chat-1",
      "call-2",
      [],
    );
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat-1/questions/call-2/answer",
      { method: "POST", body: { answers: [] } },
    );
  });

  it("parses plans and submits accept or revision decisions", async () => {
    expect(parseMobilePendingPlanApproval(plan)).toEqual({
      callId: "call-plan",
      turnId: "turn-1",
      title: "Add mobile prompt cards",
      plan: plan.plan,
      proposedAt: "2026-08-27T20:00:01Z",
    });
    expect(
      parseMobilePendingPlanApproval({ ...plan, plan: "bad\u202eplan" }),
    ).toBeNull();

    const listed = fakeClient([plan]);
    await expect(
      listMobilePendingPlanApprovals(listed.client, "chat/1"),
    ).resolves.toHaveLength(1);
    expect(listed.getJson).toHaveBeenCalledWith(
      "/chats/chat%2F1/plans/pending",
    );

    const sent = fakeClient(undefined);
    await decideMobilePlan(sent.client, "chat/1", "call/1", {
      decision: "accept",
    });
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat%2F1/plans/call%2F1/decision",
      { method: "POST", body: { decision: "accept" } },
    );
    await decideMobilePlan(sent.client, "chat-1", "call-2", {
      decision: "reject",
      feedback: "  Split the deployment into another step.  ",
    });
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat-1/plans/call-2/decision",
      {
        method: "POST",
        body: {
          decision: "reject",
          feedback: "Split the deployment into another step.",
        },
      },
    );
  });

  it("uses literal renderer-safe details instead of model narration", () => {
    const parsed = parseMobilePendingToolApproval(approval)!;
    expect(mobileApprovalQuestion(parsed)).toBe(
      "Run this command with network access?",
    );
    expect(mobileToolPreviewDetail(parsed.preview!)).toBe(
      "pnpm test -- 'chat prompts'\n" +
        "# working directory: mobile\n" +
        "# staged files: package.json, src/lib/chatPrompts.ts",
    );
    expect(mobileToolPreviewDetail(parsed.preview!)).not.toContain(
      execPreview.summary,
    );
  });
});

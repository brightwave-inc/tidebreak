import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  answerMobileUserQuestions,
  decideMobilePlan,
  decideMobileToolApproval,
  listMobilePendingPlanApprovals,
  listMobilePendingToolApprovals,
  listMobilePendingUserQuestions,
  mobileApprovalDetail,
  mobileApprovalQuestion,
  mobileToolPreviewDetail,
  parseMobilePendingPlanApproval,
  parseMobilePendingToolApproval,
  parseMobilePendingToolApprovals,
  parseMobilePendingUserQuestions,
  parseMobileToolActionPreview,
} from "./chatPrompts";

/** Collect every note a parser makes about what it tolerated. */
function driftNotes(): { notes: string[]; drift: (note: string) => void } {
  const notes: string[] = [];
  return { notes, drift: (note) => notes.push(note) };
}

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
  plan: "## Steps\n1. Parse each pending prompt strictly.\n2. Show its mobile card.\n3. Submit the exact decision.",
  proposed_at: "2026-08-27T20:00:01Z",
};

/**
 * The renderer's validator fixtures, generated from the server's own values
 * (`crates/tidebreak-desktop/ui/src/generated/fixtures.ts`).
 */
function serverFixtures(): Record<string, unknown> {
  const here = dirname(fileURLToPath(import.meta.url));
  const source = readFileSync(
    join(
      here,
      "../../../crates/tidebreak-desktop/ui/src/generated/fixtures.ts",
    ),
    "utf8",
  );
  const fixtures: Record<string, unknown> = {};
  for (const match of source.matchAll(
    /export const (\w+) = (\{[\s\S]*?\n\}) as const;/g,
  )) {
    fixtures[match[1]!] = JSON.parse(match[2]!);
  }
  return fixtures;
}

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
  it("recovers repository work without allowing a remembered grant", () => {
    const preview = {
      tool: "code_session",
      operation: "create",
      target: "example/repository",
      task: "Inspect tests.",
      harness: "codex",
      model: null,
    };
    const action = {
      ...approval,
      action: "other",
      approval: "code_session_may_run_repository_agent",
      can_remember: false,
      grant_rungs: [],
      preview,
    };
    expect(parseMobilePendingToolApproval(action)).toMatchObject({
      canApprove: true,
      canRemember: false,
      grantRungs: [],
      preview,
      unrecognized: false,
    });
    // A server offering to remember what this app never remembers still gets
    // no remembered grant, and the disagreement is noted rather than fatal.
    const { notes, drift } = driftNotes();
    expect(
      parseMobilePendingToolApproval(
        { ...action, can_remember: true, grant_rungs: ["whole_tool"] },
        drift,
      ),
    ).toMatchObject({ canApprove: true, canRemember: false, grantRungs: [] });
    expect(notes).toEqual([
      "approval call-approval: can_remember disagrees with the grant rungs",
    ]);
    expect(
      parseMobileToolActionPreview({ ...preview, operation: "delete" }),
    ).toBeNull();
    expect(
      parseMobileToolActionPreview({ ...preview, hidden_arguments: "secret" }),
    ).toBeNull();
    expect(parseMobileToolActionPreview({ ...preview, task: "" })).toBeNull();
    expect(
      mobileToolPreviewDetail(parseMobileToolActionPreview(preview)!),
    ).toBe("Repository: example/repository\nInspect tests.\nHarness: codex");
  });

  it("parses a closed approval and preserves its exact action preview", () => {
    const { notes, drift } = driftNotes();
    expect(parseMobilePendingToolApproval(approval, drift)).toEqual({
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
      unrecognized: false,
    });
    expect(notes).toEqual([]);
  });

  it("takes whichever policy allows less when the server and app disagree", () => {
    expect(
      parseMobilePendingToolApproval({ ...approval, can_approve: false }),
    ).toMatchObject({ canApprove: false, unrecognized: false });
    expect(
      parseMobilePendingToolApproval({
        ...approval,
        grant_rungs: [{ command_prefix: { tokens: 0 } }],
      }),
    ).toMatchObject({ canApprove: true, canRemember: false, grantRungs: [] });
  });

  it("ignores a key a newer server added instead of dropping the approval", () => {
    const { notes, drift } = driftNotes();
    expect(
      parseMobilePendingToolApproval(
        { ...approval, expires_at: "2026-09-23T00:00:00Z" },
        drift,
      ),
    ).toMatchObject({ callId: "call-approval", canApprove: true });
    expect(notes).toEqual(["approval call-approval: unknown key expires_at"]);
  });

  it("lists a request it cannot fully read as one it can only reject", () => {
    for (const unknown of [
      { action: "browser_open" },
      { approval: "browser_may_open_page" },
      { class: "destructive" },
      // The literal action is what consent is given to. A preview with a
      // key this app does not know may carry more than the app would show.
      { preview: { ...execPreview, env: { TOKEN: "hidden" } } },
      { preview: { tool: "browser_open", url: "https://example.com" } },
    ]) {
      const parsed = parseMobilePendingToolApproval({ ...approval, ...unknown });
      expect(parsed, JSON.stringify(unknown)).toMatchObject({
        callId: "call-approval",
        turnId: "turn-1",
        canApprove: false,
        canRemember: false,
        grantRungs: [],
        unrecognized: true,
      });
      expect(mobileApprovalQuestion(parsed!)).toBe("Reject this request?");
      expect(mobileApprovalDetail(parsed!)).toContain("Update the app");
    }
    // Nothing partial is shown for a preview it could not read whole.
    expect(
      parseMobilePendingToolApproval({
        ...approval,
        preview: { ...execPreview, env: {} },
      })?.preview,
    ).toBeNull();
  });

  it("keeps the rest of the list when one approval cannot be read", () => {
    const { notes, drift } = driftNotes();
    const listed = parseMobilePendingToolApprovals(
      [
        approval,
        { ...approval, call_id: "call-future", approval: "some_future_kind" },
        { turn_id: "turn-1" },
      ],
      drift,
    );
    expect(listed.map((item) => [item.callId, item.unrecognized])).toEqual([
      ["call-approval", false],
      ["call-future", true],
    ]);
    expect(notes).toContain("approval: no call or turn to decide");
  });

  /**
   * The strict half. `PENDING_APPROVAL*` are serialized from the server's
   * own types, so a field the server renamed shows up here as a note, and
   * this test fails even though a user's app keeps working.
   */
  it("reads the server's real approvals without tolerating anything", () => {
    const fixtures = serverFixtures();
    for (const name of ["PENDING_APPROVAL", "PENDING_APPROVAL_WITHOUT_PREVIEW"]) {
      const { notes, drift } = driftNotes();
      const parsed = parseMobilePendingToolApproval(fixtures[name], drift);
      expect(notes, name).toEqual([]);
      expect(parsed?.unrecognized, name).toBe(false);
    }
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
    await decideMobileToolApproval(sent.client, "chat/1", "call/1", {
      decision: "approve",
    });
    expect(sent.requestJson).toHaveBeenLastCalledWith(
      "/chats/chat%2F1/approvals/call%2F1",
      {
        method: "POST",
        body: { decision: "approve", grant: null },
        expectedStatus: 204,
      },
    );

    await decideMobileToolApproval(sent.client, "chat-1", "call-2", {
      decision: "reject",
      feedback: "  Use the cached result.  ",
    });
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

  it("ignores keys a newer server added to questions and plans", () => {
    const { notes, drift } = driftNotes();
    expect(
      parseMobilePendingUserQuestions(
        {
          ...questions,
          priority: "high",
          questions: [{ ...questions.questions[0], hint: "Pick one." }],
        },
        drift,
      ),
    ).toMatchObject({ callId: "call-questions" });
    expect(
      parseMobilePendingPlanApproval({ ...plan, summary: "Two steps." }, drift),
    ).toMatchObject({ callId: "call-plan" });
    expect(notes).toEqual([
      "questions: unknown key priority",
      "question: unknown key hint",
      "plan: unknown key summary",
    ]);
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

    await answerMobileUserQuestions(sent.client, "chat-1", "call-2", []);
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

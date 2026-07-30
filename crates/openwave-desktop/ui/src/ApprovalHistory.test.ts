import { describe, expect, it } from "vitest";
import {
  reconcilePendingApprovalCards,
  upsertPendingApprovalCard,
} from "./ApprovalHistory";

const pending = {
  callId: "call-search",
  turnId: "turn-live",
  action: "search" as const,
  approval: "search_may_share_query_and_excerpts" as const,
  class: "sensitive" as const,
  preview: null,
  canApprove: true,
  canRemember: true,
  grantRungs: ["whole_tool" as const],
  autoJudgeStatus: null,
};

describe("reconcilePendingApprovalCards", () => {
  it("rebuilds a parked approval after reload without an old event frame", () => {
    expect(reconcilePendingApprovalCards([], [pending])).toEqual([
      {
        id: "tool-call-search",
        role: "tool",
        callId: "call-search",
        name: "search",
        status: "waiting_approval",
        preview: null,
      },
      {
        id: "approval-call-search",
        role: "approval",
        callId: "call-search",
        summary:
          "Allow search to send your query and potentially matching document excerpts to configured AI services outside OpenWave?",
        preview: null,
        canApprove: true,
        canRemember: true,
        autoJudging: false,
        grantRungs: ["whole_tool"],
      },
    ]);
  });

  it("removes stale waiting cards and never carries them into another chat", () => {
    const stale = reconcilePendingApprovalCards(
      [
        {
          id: "tool-old",
          role: "tool",
          callId: "call-old",
          name: "search",
          status: "waiting_approval",
        },
        {
          id: "approval-old",
          role: "approval",
          callId: "call-old",
          summary: "old",
          canApprove: true,
          canRemember: true,
        },
        { id: "message-new", role: "user", text: "new chat" },
      ],
      [],
    );
    expect(stale).toEqual([{ id: "message-new", role: "user", text: "new chat" }]);
  });

  it("moves a hydrated card behind earlier events when its live frame replays", () => {
    const hydrated = upsertPendingApprovalCard(
      [{ id: "user", role: "user", text: "run it" }],
      pending,
    );
    const withEarlierReplay = [
      ...hydrated,
      {
        id: "thought",
        role: "assistant" as const,
        text: "",
        reasoning: "checking the command",
        sources: [],
      },
    ];

    expect(upsertPendingApprovalCard(withEarlierReplay, pending, true)).toEqual([
      { id: "user", role: "user", text: "run it" },
      {
        id: "thought",
        role: "assistant",
        text: "",
        reasoning: "checking the command",
        sources: [],
      },
      expect.objectContaining({ role: "tool", callId: "call-search" }),
      expect.objectContaining({ role: "approval", callId: "call-search" }),
    ]);
  });
});

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
  canApprove: true,
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
      },
      {
        id: "approval-call-search",
        role: "approval",
        callId: "call-search",
        summary:
          "Allow search to send your query and potentially matching document excerpts to configured AI services outside OpenWave?",
        canApprove: true,
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
        },
        { id: "message-new", role: "user", text: "new chat" },
      ],
      [],
    );
    expect(stale).toEqual([{ id: "message-new", role: "user", text: "new chat" }]);
  });

  it("upserts a replayed live requirement by call id", () => {
    const once = upsertPendingApprovalCard([], pending);
    expect(upsertPendingApprovalCard(once, pending)).toEqual(once);
  });
});

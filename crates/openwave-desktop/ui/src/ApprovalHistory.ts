import type { PendingToolApproval } from "./api";
import type { ChatMessage } from "./MessageList";
import { toolApprovalPresentation } from "./ToolCallCard";

/** Rebuild only currently actionable approval cards from an authoritative page. */
export function reconcilePendingApprovalCards(
  messages: ChatMessage[],
  approvals: PendingToolApproval[],
): ChatMessage[] {
  const pendingIds = new Set(approvals.map((approval) => approval.callId));
  let next = messages.filter(
    (message) =>
      !(
        (message.role === "approval" &&
          !message.resolved &&
          !pendingIds.has(message.callId)) ||
        (message.role === "tool" &&
          message.status === "waiting_approval" &&
          !pendingIds.has(message.callId))
      ),
  );

  for (const approval of approvals) {
    next = upsertPendingApprovalCard(next, approval);
  }
  return next;
}

export function upsertPendingApprovalCard(
  messages: ChatMessage[],
  approval: Pick<
    PendingToolApproval,
    "callId" | "action" | "approval" | "canApprove" | "canRemember"
  > &
    Partial<Pick<PendingToolApproval, "preview">>,
): ChatMessage[] {
    let next = messages;
    const presentation = toolApprovalPresentation(approval.approval);
    const preview = approval.preview ?? null;
    const toolIndex = next.findIndex(
      (message) =>
        message.role === "tool" && message.callId === approval.callId,
    );
    if (toolIndex >= 0) {
      next = next.map((message, index) =>
        index === toolIndex && message.role === "tool"
          ? {
              ...message,
              name: approval.action,
              status: "waiting_approval",
              preview,
            }
          : message,
      );
    } else {
      next = [
        ...next,
        {
          id: `tool-${approval.callId}`,
          role: "tool",
          callId: approval.callId,
          name: approval.action,
          status: "waiting_approval",
          preview,
        },
      ];
    }

    const cardIndex = next.findIndex(
      (message) =>
        message.role === "approval" && message.callId === approval.callId,
    );
    const card: ChatMessage = {
      id: `approval-${approval.callId}`,
      role: "approval",
      callId: approval.callId,
      summary: presentation.summary,
      preview,
      canApprove: approval.canApprove && presentation.canApprove,
      canRemember: approval.canRemember && presentation.canRemember,
    };
    if (cardIndex >= 0) {
      next = next.map((message, index) => (index === cardIndex ? card : message));
    } else {
      next = [...next, card];
    }
  return next;
}

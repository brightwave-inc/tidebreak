import type { ApiClient, InboxItem } from "./api";
import { RENDERER_FOLDER_ACCESS_REASON } from "./api";
import { codeApprovalQuestion } from "./code/CodeApprovalCard";
import {
  oneLine,
  workApprovalQuestion,
  type NeedsYouQuestion,
} from "./needsYou";
import { toolApprovalPresentation } from "./ToolCallCard";

/**
 * What a parked agent asked, read from the conversation that owns it.
 *
 * The inbox and the Code digest stay opaque on purpose: they say that
 * something waits, never what. A notice that names the question reads it
 * from the same routes the conversation's cards use, and only when the
 * notice is about to be shown.
 */

type WorkPromptClient = Pick<
  ApiClient,
  | "listPendingApprovals"
  | "listPendingUserQuestions"
  | "listPendingPlanApprovals"
  | "listPendingOutputWritebackRequests"
>;

/** The question behind one parked item in a Work conversation. */
export async function workParkedQuestion(
  client: WorkPromptClient,
  chatId: string,
  item: Pick<InboxItem, "callId" | "kind">,
): Promise<NeedsYouQuestion> {
  switch (item.kind) {
    case "question": {
      const pending = await client.listPendingUserQuestions(chatId);
      const asked = pending.find((request) => request.callId === item.callId);
      return {
        kind: "question",
        text: oneLine(asked?.questions[0]?.question ?? ""),
      };
    }
    case "plan_review": {
      const pending = await client.listPendingPlanApprovals(chatId);
      const plan = pending.find((request) => request.callId === item.callId);
      return { kind: "plan", text: oneLine(plan?.title ?? "") };
    }
    case "tool_approval": {
      const pending = await client.listPendingApprovals(chatId);
      const approval = pending.find(
        (request) => request.callId === item.callId,
      );
      if (!approval) return { kind: "approval", text: "" };
      return workApprovalQuestion(
        toolApprovalPresentation(approval.approval).summary,
        approval.preview,
      );
    }
    case "folder_access":
      return { kind: "approval", text: RENDERER_FOLDER_ACCESS_REASON };
    case "output_writeback": {
      const pending = await client.listPendingOutputWritebackRequests(chatId);
      const writeback = pending.find(
        (request) => request.callId === item.callId,
      );
      return {
        kind: "approval",
        text:
          writeback?.mode === "replace"
            ? "Replace an existing file?"
            : "Write a file to a connected folder?",
      };
    }
  }
}

/** The question behind the newest approval a Code session is parked on. */
export async function codeParkedQuestion(
  client: Pick<ApiClient, "listCodeApprovals">,
  sessionId: string,
): Promise<NeedsYouQuestion> {
  const pending = await client.listCodeApprovals({
    state: "pending",
    sessionId,
  });
  const newest = [...pending].sort((left, right) =>
    right.requested_at.localeCompare(left.requested_at),
  )[0];
  return newest ? codeApprovalQuestion(newest) : { kind: "approval", text: "" };
}

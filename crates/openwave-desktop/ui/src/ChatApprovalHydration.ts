import type {
  ApiClient,
  ChatTranscript,
  PendingToolApproval,
} from "./api";

type ApprovalHydrationClient = Pick<
  ApiClient,
  "listChatMessages" | "listPendingApprovals"
>;

/** Load a transcript boundary, then its authoritative pending approvals. */
export async function loadChatApprovalHydration(
  client: ApprovalHydrationClient,
  chatId: string,
  isCurrent: () => boolean,
): Promise<{
  transcript: ChatTranscript;
  pendingApprovals: PendingToolApproval[];
} | null> {
  const transcript = await client.listChatMessages(chatId);
  if (!isCurrent()) return null;
  const pendingApprovals = await client.listPendingApprovals(chatId);
  if (!isCurrent()) return null;
  return { transcript, pendingApprovals };
}

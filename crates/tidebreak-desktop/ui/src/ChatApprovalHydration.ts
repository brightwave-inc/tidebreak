import type { ApiClient, ChatTranscript, PendingToolApproval } from "./api";
import { reconcilePendingApprovalCards } from "./ApprovalHistory";
import {
  applyTerminalHydration,
  type ChatSessionState,
} from "./ChatSessionReducer";
import {
  presentChatTranscript,
  TRANSCRIPT_PAGE_TURNS,
} from "./ChatTranscriptPresentation";

type ApprovalHydrationClient = Pick<
  ApiClient,
  "listChatMessages" | "listPendingApprovals"
>;

/**
 * Load a transcript boundary, then its authoritative pending approvals.
 *
 * Reads the newest page of the conversation, not all of it: the transcript
 * shows the end, and earlier turns load when the reader asks for them.
 */
export async function loadChatApprovalHydration(
  client: ApprovalHydrationClient,
  chatId: string,
  isCurrent: () => boolean,
): Promise<{
  transcript: ChatTranscript;
  pendingApprovals: PendingToolApproval[];
} | null> {
  const transcript = await client.listChatMessages(chatId, {
    limit: TRANSCRIPT_PAGE_TURNS,
  });
  if (!isCurrent()) return null;
  const pendingApprovals = await client.listPendingApprovals(chatId);
  if (!isCurrent()) return null;
  return { transcript, pendingApprovals };
}

/**
 * Fold a freshly opened chat's durable transcript into session state.
 *
 * Routes through {@link applyTerminalHydration} so the context meter sees the
 * same `lastTurnUsage` path as a post-turn refresh — a reopened chat should
 * meter without waiting for another turn to finish.
 */
export function sessionFromOpenedChat(
  state: ChatSessionState,
  transcript: ChatTranscript,
  pendingApprovals: PendingToolApproval[],
): ChatSessionState {
  const presented = presentChatTranscript(transcript);
  const pendingTurnId = pendingApprovals[0]?.turnId ?? null;
  return {
    ...applyTerminalHydration(state, presented),
    messages: reconcilePendingApprovalCards(
      presented.messages,
      pendingApprovals,
    ),
    activeTurnId: pendingTurnId,
    busy: pendingTurnId !== null,
  };
}

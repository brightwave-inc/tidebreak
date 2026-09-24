import type { ApiClient } from "../api";
import type { ChatSessionState } from "../ChatSessionReducer";
import {
  type AnswerVersions,
  type PresentedTranscript,
  presentChatTranscript,
  TRANSCRIPT_PAGE_TURNS,
} from "../ChatTranscriptPresentation";
import type { ChatMessage } from "../MessageList";

/**
 * Putting a found message of a Work chat where the transcript can show it.
 *
 * A long conversation holds only its newest pages. A message on an older
 * page is read with the one page that holds it, never with the pages in
 * between: when that page meets the pages held, it joins them; when a gap
 * remains, it is shown on its own as a stretch of history the reader can
 * page back through and leave again.
 */

/** A stretch of a conversation shown apart from its newest pages. */
export type ChatHistoryView = {
  messages: ChatMessage[];
  messageIds: ReadonlySet<string>;
  answerVersions: AnswerVersions;
  /** The cursor that reads the page before the stretch, or null at the start. */
  earlierCursor: number | null;
};

export type ChatMessageLoad =
  /** The live transcript holds the message, now or after a page joined it. */
  | { kind: "held" }
  /** The message is in a stretch of history apart from the live transcript. */
  | { kind: "history"; view: ChatHistoryView }
  /** The conversation no longer has the message. */
  | { kind: "missing" };

type TranscriptClient = Pick<ApiClient, "listChatMessages">;

function holds(messages: readonly ChatMessage[], messageId: string): boolean {
  return messages.some((message) => message.id === messageId);
}

/**
 * Put an earlier page above the held transcript when the two meet: the page
 * ends at or after where the held pages start. Messages both hold keep the
 * held copy.
 */
export function joinPageAbove(
  state: ChatSessionState,
  page: Pick<
    PresentedTranscript,
    "messages" | "messageIds" | "answerVersions" | "earlierCursor"
  >,
): ChatSessionState {
  const held = new Set(state.messages.map((message) => message.id));
  const added = page.messages.filter((message) => !held.has(message.id));
  if (added.length === 0) return state;
  return {
    ...state,
    messages: [...added, ...state.messages],
    hydratedMessageIds: new Set([
      ...page.messageIds,
      ...state.hydratedMessageIds,
    ]),
    answerVersions: { ...page.answerVersions, ...state.answerVersions },
    // A transcript held from its start stays held from its start.
    earlierCursor:
      state.earlierCursor === null ? null : (page.earlierCursor ?? null),
  };
}

/** Whether a page read around a message meets the pages already held. */
export function pageMeetsHeld(
  page: Pick<PresentedTranscript, "laterCursor">,
  heldEarlierCursor: number | null,
): boolean {
  if (heldEarlierCursor === null) return true;
  if (page.laterCursor === null) return true;
  return page.laterCursor >= heldEarlierCursor;
}

/**
 * Make `messageId` showable, loading the one page that holds it when neither
 * the live transcript nor the history already on screen has it.
 */
export async function loadChatMessage({
  client,
  chatId,
  messageId,
  session,
  update,
  view,
  isCurrent,
}: {
  client: TranscriptClient;
  chatId: string;
  messageId: string;
  /** The live session as it is now. */
  session: () => ChatSessionState;
  /** Apply a change to the live session. */
  update: (change: (state: ChatSessionState) => ChatSessionState) => void;
  /** The history already on screen, if any. */
  view: ChatHistoryView | null;
  /** Whether the answer still matters when it lands. */
  isCurrent?: () => boolean;
}): Promise<ChatMessageLoad | null> {
  if (holds(session().messages, messageId)) return { kind: "held" };
  if (view && holds(view.messages, messageId)) {
    return { kind: "history", view };
  }
  const transcript = await client.listChatMessages(chatId, {
    around: messageId,
    limit: TRANSCRIPT_PAGE_TURNS,
  });
  if (isCurrent && !isCurrent()) return null;
  const page = presentChatTranscript(transcript);
  if (!holds(page.messages, messageId)) return { kind: "missing" };
  if (pageMeetsHeld(page, session().earlierCursor)) {
    update((state) => joinPageAbove(state, page));
    return { kind: "held" };
  }
  return {
    kind: "history",
    view: {
      messages: page.messages,
      messageIds: page.messageIds,
      answerVersions: page.answerVersions,
      earlierCursor: page.earlierCursor,
    },
  };
}

/** The page before a stretch of history, put above it. */
export async function loadEarlierHistory(
  client: TranscriptClient,
  chatId: string,
  view: ChatHistoryView,
): Promise<ChatHistoryView> {
  if (view.earlierCursor === null) return view;
  const transcript = await client.listChatMessages(chatId, {
    before: view.earlierCursor,
    limit: TRANSCRIPT_PAGE_TURNS,
  });
  const page = presentChatTranscript(transcript);
  const held = new Set(view.messages.map((message) => message.id));
  return {
    messages: [
      ...page.messages.filter((message) => !held.has(message.id)),
      ...view.messages,
    ],
    messageIds: new Set([...page.messageIds, ...view.messageIds]),
    answerVersions: { ...page.answerVersions, ...view.answerVersions },
    earlierCursor: page.earlierCursor,
  };
}

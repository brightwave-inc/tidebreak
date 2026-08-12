import { useMemo } from "react";

import type { CitationLocator } from "@/api";
import { useChatSessionStore } from "@/ChatSessionStore";
import type { ChatMessage } from "@/MessageList";

/** Resolve a citation id to the model-authored locator already in the transcript. */
export function findCitationPlacement(
  messages: readonly ChatMessage[],
  documentId: string,
  citationId: string,
): CitationLocator | null {
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    for (const source of message.sources) {
      if (source.id === citationId && source.documentId === documentId) {
        return source.locator;
      }
    }
  }
  return null;
}

export function useCitationPlacement(
  documentId: string,
  citationId: string | undefined,
): CitationLocator | null {
  const messages = useChatSessionStore((session) => session.messages);
  return useMemo(
    () =>
      citationId
        ? findCitationPlacement(messages, documentId, citationId)
        : null,
    [messages, documentId, citationId],
  );
}

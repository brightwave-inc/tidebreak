import { useEffect, useState } from "react";

import type { ApiClient, Chat } from "./api";

/** What the memory row needs from the connected client. */
export type ChatMemoryPresenceClient = Pick<
  ApiClient,
  "getSettings" | "getMemoryDigest"
>;

/** The facts the row summarizes, read once per conversation. */
type MemoryFacts = { enabled: boolean; recordCount: number };

/**
 * The one line the activity chip shows for memory.
 *
 * Read from settings and the personal digest when the conversation opens,
 * then phrased against the chat's own incognito flag, so the row answers
 * "is memory reaching this conversation?" without a second data source.
 * A failed read hides the row rather than guessing: memory is enrichment.
 */
export function useChatMemoryPresence(
  client: ChatMemoryPresenceClient,
  chat: Pick<Chat, "id" | "memory_incognito"> | null,
): string | null {
  const [facts, setFacts] = useState<MemoryFacts | null>(null);
  const chatId = chat?.id ?? null;

  useEffect(() => {
    if (chatId == null) return;
    let cancelled = false;
    void Promise.all([
      client.getSettings(),
      client.getMemoryDigest({ kind: "personal" }),
    ])
      .then(([settings, digest]) => {
        if (cancelled) return;
        setFacts({
          enabled: settings.memory.enabled,
          recordCount: digest.record_count,
        });
      })
      .catch(() => {
        if (!cancelled) setFacts(null);
      });
    return () => {
      cancelled = true;
    };
  }, [client, chatId]);

  if (facts == null || chat == null) return null;
  return memorySummary(facts, chat.memory_incognito);
}

/** Phrase the memory facts for one conversation. */
export function memorySummary(facts: MemoryFacts, incognito: boolean): string {
  if (!facts.enabled) return "Off";
  if (incognito) return "Off for this chat";
  if (facts.recordCount === 0) return "On · nothing approved yet";
  return facts.recordCount === 1
    ? "1 record in context"
    : `${facts.recordCount} records in context`;
}

import { useEffect } from "react";

import type { Chat } from "./api";
import {
  type MemoryFacts,
  type MemoryPresenceClient,
  useMemoryPresenceStore,
} from "./MemoryPresenceStore";

/**
 * The one line the activity chip shows for memory.
 *
 * Reads the shared memory snapshot, loading it once for the app if nothing
 * has yet, and phrases it against the chat's own incognito flag, so the row
 * answers "is memory reaching this conversation?" and follows any change
 * the settings page makes. Nothing loaded means no row: memory is
 * enrichment, and the chip never guesses.
 */
export function useChatMemoryPresence(
  client: MemoryPresenceClient,
  chat: Pick<Chat, "memory_incognito"> | null,
): string | null {
  const facts = useMemoryPresenceStore((state) => state.facts);
  const refresh = useMemoryPresenceStore((state) => state.refresh);

  useEffect(() => {
    if (facts == null) refresh(client).catch(() => undefined);
  }, [client, facts, refresh]);

  if (facts == null || chat == null) return null;
  return memorySummary(facts, chat.memory_incognito);
}

/** Phrase the memory snapshot for one conversation. */
export function memorySummary(facts: MemoryFacts, incognito: boolean): string {
  if (!facts.settings.enabled) return "Off";
  if (incognito) return "Off for this conversation";
  const count = facts.digest.record_count;
  if (count === 0) return "On · nothing approved yet";
  return count === 1 ? "1 record in context" : `${count} records in context`;
}

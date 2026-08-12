import { useEffect, useState } from "react";

import type { ApiClient, PendingOutputWritebackRequest } from "./api";
import {
  hasNativeHost,
  resolveOutputWritebackRequest,
  type OutputWritebackDecision,
} from "./host";
import { useOpenConversation } from "./OpenConversation";
import { usePendingPrompts } from "./PendingPrompts";

export type OutputWritebackRequests = {
  requests: PendingOutputWritebackRequest[];
  resolving: Set<string>;
  errors: Record<string, string>;
  decide: (callId: string, decision: OutputWritebackDecision) => void;
  cancel: (callId: string, turnId: string) => void;
};

/**
 * Resolves write-back consent. Replacement always asks; a create asks only
 * where the chat's permission mode says workspace mutations ask, and otherwise
 * runs natively without reaching this hook.
 */
export function useOutputWritebackRequests(
  client: ApiClient | null,
  chatId: string | null,
): OutputWritebackRequests {
  const requests = usePendingPrompts((state) => state.outputWritebacks);
  const refresh = usePendingPrompts((state) => state.refresh);
  const [resolving, setResolving] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const stillOpen = useOpenConversation(chatId);

  useEffect(
    () => () => {
      setResolving(new Set());
      setErrors({});
    },
    [chatId],
  );

  async function decide(callId: string, decision: OutputWritebackDecision) {
    if (!chatId || !hasNativeHost() || resolving.has(callId)) return;
    const startedChatId = chatId;
    setResolving((current) => new Set(current).add(callId));
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
    try {
      await resolveOutputWritebackRequest(startedChatId, callId, decision);
    } catch (err) {
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: String(err) }));
      }
    } finally {
      if (stillOpen(startedChatId)) {
        setResolving((current) => {
          const next = new Set(current);
          next.delete(callId);
          return next;
        });
        refresh();
      }
    }
  }

  async function cancel(callId: string, turnId: string) {
    if (!client || !chatId) return;
    const startedChatId = chatId;
    try {
      await client.cancel(startedChatId, turnId);
      if (stillOpen(startedChatId)) refresh();
    } catch (err) {
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: String(err) }));
      }
    }
  }

  return {
    requests,
    resolving,
    errors,
    decide: (callId, decision) => void decide(callId, decision),
    cancel: (callId, turnId) => void cancel(callId, turnId),
  };
}

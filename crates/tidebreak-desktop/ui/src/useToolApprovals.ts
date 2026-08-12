import type { ApprovalGrantRung } from "./api";
import { useEffect, useRef, useState } from "react";
import type { ApiClient } from "./api";
import { useChatSessionStore } from "./ChatSessionStore";
import { useOpenConversation } from "./OpenConversation";

export type ToolApprovals = {
  deciding: Set<string>;
  errors: Record<string, string>;
  decide: (
    callId: string,
    decision: "approve" | "reject",
    grant?: ApprovalGrantRung | null,
  ) => void;
};

/**
 * Decisions on the tool approvals waiting in one conversation.
 *
 * Unlike folder access, several approvals can be resolved at once — the guard
 * is per call, so a second click on the same card is ignored while its decision
 * is in flight. There is nothing to poll: approvals arrive on the event stream
 * as transcript messages, and a decision marks its own card resolved.
 */
export function useToolApprovals(
  client: ApiClient | null,
  chatId: string | null,
): ToolApprovals {
  const [deciding, setDeciding] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const decidingRef = useRef<Set<string>>(new Set());
  const stillOpen = useOpenConversation(chatId);

  // The pane is keyed on the conversation, so this hook is normally replaced
  // rather than reused. Reset anyway: nothing held here belongs to a different
  // conversation, and leaving the keying to do it makes removing that key a
  // silent bug rather than a loud one.
  useEffect(
    () => () => {
      setDeciding(new Set());
      setErrors({});
      decidingRef.current = new Set();
    },
    [chatId],
  );

  async function send(
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null,
  ) {
    if (!client || !chatId || decidingRef.current.has(callId)) return;
    const startedChatId = chatId;
    decidingRef.current.add(callId);
    setDeciding((calls) => new Set(calls).add(callId));
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
    try {
      await client.decideApproval(startedChatId, callId, decision, grant);
      // The session store holds whichever conversation is open now, not the one
      // this decision was made in. Marking the card resolved without checking
      // would rewrite a different chat's transcript — and because the map
      // allocates unconditionally, that is a visible re-render even though no
      // message matches.
      if (stillOpen(startedChatId)) {
        useChatSessionStore.getState().update((session) => ({
          ...session,
          messages: session.messages.map((message) =>
            message.role === "approval" && message.callId === callId
              ? { ...message, resolved: true }
              : message,
          ),
        }));
      }
    } catch (err) {
      // A decision that fails against a chat on its way out has nowhere to be
      // read; the conversation it belonged to is going with it.
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({
          ...current,
          [callId]: `Could not send your decision: ${String(err)}`,
        }));
      }
    } finally {
      decidingRef.current.delete(callId);
      setDeciding((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
    }
  }

  return {
    deciding,
    errors,
    decide: (callId, decision, grant = null) =>
      void send(callId, decision, grant),
  };
}

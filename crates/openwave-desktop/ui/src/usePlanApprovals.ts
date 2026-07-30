import { useEffect, useRef, useState } from "react";
import type { ApiClient, PendingPlanApproval, PlanDecision } from "./api";
import { useOpenConversation } from "./OpenConversation";
import { usePendingPrompts } from "./PendingPrompts";

export type PlanApprovals = {
  requests: PendingPlanApproval[];
  deciding: Set<string>;
  errors: Record<string, string>;
  decide: (callId: string, decision: PlanDecision) => void;
  cancel: (turnId: string) => void;
};

/**
 * Deciding the plan the agent is waiting on.
 *
 * The pending plans themselves are watched by the shell, exactly like user
 * questions: the agent parks a turn until the plan is decided, so being told
 * about it must survive the reader looking at another screen. This hook owns
 * only what is genuinely the view's — which decisions are in flight, and which
 * failed.
 */
export function usePlanApprovals(
  client: ApiClient | null,
  chatId: string | null,
): PlanApprovals {
  const requests = usePendingPrompts((state) => state.planApprovals);
  const refresh = usePendingPrompts((state) => state.refresh);
  const [deciding, setDeciding] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const decidingRef = useRef<Set<string>>(new Set());
  const stillOpen = useOpenConversation(chatId);

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
    startedChatId: string,
    request: () => Promise<unknown>,
    failure: (err: unknown) => string,
  ) {
    decidingRef.current.add(callId);
    setDeciding((calls) => new Set(calls).add(callId));
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
    try {
      await request();
    } catch (err) {
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: failure(err) }));
      }
    } finally {
      decidingRef.current.delete(callId);
      setDeciding((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      if (stillOpen(startedChatId)) refresh();
    }
  }

  function decide(callId: string, decision: PlanDecision) {
    if (!client || !chatId || decidingRef.current.has(callId)) return;
    const startedChatId = chatId;
    void send(
      callId,
      startedChatId,
      () => client.decidePlan(startedChatId, callId, decision),
      (err) => `Could not send your decision: ${String(err)}`,
    );
  }

  function cancel(turnId: string) {
    if (!client || !chatId) return;
    const request = requests.find((candidate) => candidate.turnId === turnId);
    if (!request || decidingRef.current.has(request.callId)) return;
    const startedChatId = chatId;
    void send(
      request.callId,
      startedChatId,
      () => client.cancel(startedChatId, turnId),
      (err) => `Could not cancel the turn: ${String(err)}`,
    );
  }

  return { requests, deciding, errors, decide, cancel };
}

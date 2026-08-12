import { useEffect, useRef, useState } from "react";

import type { ApiClient, TaskPlan } from "./api";
import { useRefreshSignals } from "./RefreshSignals";

/**
 * The conversation's task plan, kept current from the durable route.
 *
 * The event stream says only that the plan moved on, never what it now says,
 * so every read comes from `GET /chats/{id}/task-plan`: once when the chat
 * opens, which is also how a reload recovers the plan, and again on each hint.
 * There is nothing to poll for — a plan only changes when the agent replaces
 * it, and replacing it always raises the hint.
 *
 * A failed read keeps the last plan on screen rather than blanking it: a
 * transient error is not evidence that the agent abandoned its checklist.
 */
export function useTaskPlan(
  client: ApiClient | null,
  chatId: string | null,
): TaskPlan | null {
  const [plan, setPlan] = useState<TaskPlan | null>(null);
  const refreshRef = useRef<(() => void) | null>(null);
  const signal = useRefreshSignals((state) => state.taskPlan);

  useEffect(() => {
    if (!client || !chatId) {
      setPlan(null);
      refreshRef.current = null;
      return;
    }

    let cancelled = false;
    let requestSeq = 0;
    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const current = await client.getTaskPlan(chatId);
        if (cancelled || seq !== requestSeq) return;
        setPlan(current);
      } catch (error) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to read the task plan", error);
        }
      }
    };

    setPlan(null);
    refreshRef.current = () => void refresh();
    void refresh();
    return () => {
      cancelled = true;
      requestSeq += 1;
      refreshRef.current = null;
    };
  }, [client, chatId]);

  // The counter is app-wide and may already be well past zero on arrival, so
  // only a signal raised after this mounted means anything.
  const lastSignalRef = useRef(signal);
  useEffect(() => {
    if (lastSignalRef.current === signal) return;
    lastSignalRef.current = signal;
    refreshRef.current?.();
  }, [signal]);

  return plan;
}

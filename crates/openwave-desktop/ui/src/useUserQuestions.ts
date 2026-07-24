import { useEffect, useRef, useState } from "react";
import type { ApiClient, PendingUserQuestions, UserQuestionAnswer } from "./api";
import { requestUserAttention } from "./host";
import { useRefreshSignals } from "./RefreshSignals";

const POLL_INTERVAL_MS = 10_000;

export type UserQuestions = {
  requests: PendingUserQuestions[];
  answering: Set<string>;
  errors: Record<string, string>;
  answer: (callId: string, answers: UserQuestionAnswer[]) => void;
  cancel: (turnId: string) => void;
};

/**
 * Questions the agent is waiting on for one conversation, and the answers that
 * release it.
 *
 * A reply that lands after the conversation has moved on must not write an
 * error onto, or trigger a refresh for, whatever chat is open now — so every
 * request records the chat it started under and checks it before touching
 * state.
 */
export function useUserQuestions(
  client: ApiClient | null,
  chatId: string | null,
): UserQuestions {
  const [requests, setRequests] = useState<PendingUserQuestions[]>([]);
  const [answering, setAnswering] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const answeringRef = useRef<Set<string>>(new Set());
  const seenCallIdsRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const currentChatIdRef = useRef(chatId);
  currentChatIdRef.current = chatId;
  const signal = useRefreshSignals((state) => state.userQuestions);

  useEffect(() => {
    if (!client || !chatId) return;
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const pending = await client.listPendingUserQuestions(chatId);
        if (cancelled || seq !== requestSeq) return;
        const hasNewRequest = pending.some(
          (request) => !seenCallIdsRef.current.has(request.callId),
        );
        seenCallIdsRef.current = new Set(
          pending.map((request) => request.callId),
        );
        setRequests(pending);
        if (hasNewRequest) {
          void requestUserAttention().catch(() => {
            // Attention is a best-effort hint. Durable polling is truth.
          });
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to refresh pending user questions", err);
        }
      }
    };

    refreshRef.current = () => void refresh();
    void refresh();
    const interval = window.setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      requestSeq += 1;
      window.clearInterval(interval);
      refreshRef.current = null;
      setRequests([]);
      seenCallIdsRef.current = new Set();
    };
  }, [client, chatId]);

  // Only a signal raised after this hook mounted means anything to it; the
  // counter is app-wide and may already be well past zero on arrival.
  const lastSignalRef = useRef(signal);
  useEffect(() => {
    if (lastSignalRef.current === signal) return;
    lastSignalRef.current = signal;
    refreshRef.current?.();
  }, [signal]);

  async function send(
    callId: string,
    startedChatId: string,
    request: () => Promise<unknown>,
    failure: (err: unknown) => string,
  ) {
    answeringRef.current.add(callId);
    setAnswering((calls) => new Set(calls).add(callId));
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
    try {
      await request();
    } catch (err) {
      if (currentChatIdRef.current === startedChatId) {
        setErrors((current) => ({ ...current, [callId]: failure(err) }));
      }
    } finally {
      answeringRef.current.delete(callId);
      setAnswering((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      if (currentChatIdRef.current === startedChatId) refreshRef.current?.();
    }
  }

  function answer(callId: string, answers: UserQuestionAnswer[]) {
    if (!client || !chatId || answeringRef.current.has(callId)) return;
    const startedChatId = chatId;
    void send(
      callId,
      startedChatId,
      () => client.answerUserQuestions(startedChatId, callId, answers),
      (err) => `Could not send your answer: ${String(err)}`,
    );
  }

  function cancel(turnId: string) {
    if (!client || !chatId) return;
    const request = requests.find((candidate) => candidate.turnId === turnId);
    if (!request || answeringRef.current.has(request.callId)) return;
    const startedChatId = chatId;
    void send(
      request.callId,
      startedChatId,
      () => client.cancel(startedChatId, turnId),
      (err) => `Could not cancel the turn: ${String(err)}`,
    );
  }

  return { requests, answering, errors, answer, cancel };
}

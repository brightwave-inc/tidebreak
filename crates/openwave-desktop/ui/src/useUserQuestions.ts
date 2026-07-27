import { useEffect, useRef, useState } from "react";
import type { ApiClient, PendingUserQuestions, UserQuestionAnswer } from "./api";
import { useOpenConversation } from "./OpenConversation";
import { usePendingPrompts } from "./PendingPrompts";

export type UserQuestions = {
  requests: PendingUserQuestions[];
  answering: Set<string>;
  errors: Record<string, string>;
  answer: (callId: string, answers: UserQuestionAnswer[]) => void;
  cancel: (turnId: string) => void;
};

/**
 * Answering the questions the agent is waiting on.
 *
 * The questions themselves are watched by the shell, not read here: the agent
 * parks a turn until one is answered, so being told about it has to survive the
 * reader looking at another screen. This hook owns only what is genuinely the
 * view's — which answers are in flight, and which failed.
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
  const requests = usePendingPrompts((state) => state.userQuestions);
  const refresh = usePendingPrompts((state) => state.refresh);
  const [answering, setAnswering] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const answeringRef = useRef<Set<string>>(new Set());
  const stillOpen = useOpenConversation(chatId);

  // The pane is keyed on the conversation, so this hook is normally replaced
  // rather than reused. Reset anyway: nothing held here belongs to a different
  // conversation, and leaving the keying to do it makes removing that key a
  // silent bug rather than a loud one.
  useEffect(
    () => () => {
      setAnswering(new Set());
      setErrors({});
      answeringRef.current = new Set();
    },
    [chatId],
  );

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
      if (stillOpen(startedChatId)) {
        setErrors((current) => ({ ...current, [callId]: failure(err) }));
      }
    } finally {
      answeringRef.current.delete(callId);
      setAnswering((calls) => {
        const next = new Set(calls);
        next.delete(callId);
        return next;
      });
      if (stillOpen(startedChatId)) refresh();
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

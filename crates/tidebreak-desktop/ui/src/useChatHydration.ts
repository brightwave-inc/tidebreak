import { useCallback, useEffect, useState } from "react";
import type { ApiClient } from "./api";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";
import {
  loadChatApprovalHydration,
  sessionFromOpenedChat,
} from "./ChatApprovalHydration";
import { useChatSessionStore } from "./ChatSessionStore";
import { friendlyErrorMessage } from "./lib/utils";

type HydrationClient = Pick<
  ApiClient,
  "listChatMessages" | "listPendingApprovals"
>;

/** Keep sends and event replay behind the transcript and approval boundary. */
export function useChatHydration(client: HydrationClient, chatId: string) {
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{
    client: HydrationClient;
    chatId: string;
    attempt: number;
    hydrated: boolean;
    error: string | null;
  } | null>(null);
  const retry = useCallback(() => setAttempt((value) => value + 1), []);

  useEffect(() => {
    let cancelled = false;
    const store = useChatSessionStore.getState();
    store.reset();
    store.update((session) => ({
      ...session,
      markerScrubber: new AssistantSourceMarkerStreamScrubber(),
    }));
    void loadChatApprovalHydration(client, chatId, () => !cancelled).then(
      (hydration) => {
        if (cancelled || !hydration) return;
        store.update((session) =>
          sessionFromOpenedChat(
            session,
            hydration.transcript,
            hydration.pendingApprovals,
          ),
        );
        setResult({ client, chatId, attempt, hydrated: true, error: null });
      },
      (error) => {
        if (cancelled) return;
        setResult({
          client,
          chatId,
          attempt,
          hydrated: false,
          error: friendlyErrorMessage(error, "Could not load this work."),
        });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [client, chatId, attempt]);

  const current =
    result?.client === client &&
    result.chatId === chatId &&
    result.attempt === attempt;
  return {
    hydrated: current && result.hydrated,
    error: current ? result.error : null,
    retry,
  };
}

import { useEffect } from "react";

import type { ApiClient } from "../../api/client";
import {
  hydrateCodeTurns,
  markCodeSessionHydrated,
} from "../CodeSessionReducer";
import { peekCodeSession } from "../CodeSessionRegistry";
import { useCodeUiStore } from "../CodeUiStore";

const POLL_MS = 400;

/**
 * Drop the startup screen once the first turn row exists.
 *
 * The screen is in-memory. A reload clears it and reads the journal, which is
 * why the conversation appears only after refresh when the post is still
 * running. Poll the turn list and reveal the pane as soon as the message is
 * accepted, without waiting for the engine to finish.
 */
export function useReleaseStartupWhenTurnLands(
  workspaceId: string,
  sessionId: string | null,
  client: Pick<ApiClient, "listCodeSessionTurns">,
  covered: boolean,
): void {
  useEffect(() => {
    if (!covered || !sessionId) return;
    let cancelled = false;
    let inFlight = false;

    const release = async () => {
      if (cancelled || inFlight) return;
      inFlight = true;
      try {
        const turns = await client.listCodeSessionTurns(sessionId);
        if (cancelled) return;
        if (!turns.some((turn) => turn.user_input.trim().length > 0)) return;
        peekCodeSession(sessionId)
          ?.store.getState()
          .update((state) =>
            markCodeSessionHydrated(hydrateCodeTurns(state, turns)),
          );
        useCodeUiStore.getState().setWorkspaceStartup(workspaceId, null);
      } catch {
        // The next poll tries again. A reload still reads the journal.
      } finally {
        inFlight = false;
      }
    };

    void release();
    const timer = setInterval(() => void release(), POLL_MS);
    const unsubscribe = peekCodeSession(sessionId)?.store.subscribe((state) => {
      if (state.busy || state.journalTurnId !== null) void release();
    });
    return () => {
      cancelled = true;
      clearInterval(timer);
      unsubscribe?.();
    };
  }, [client, covered, sessionId, workspaceId]);
}

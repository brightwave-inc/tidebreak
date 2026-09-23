import { useEffect, useRef } from "react";

import type { ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useRefreshSignals } from "./RefreshSignals";
import { useVisibilityGatedPoll } from "./useVisibilityGatedPoll";

/**
 * Safety-net cadence. The open conversation's stream signals its own turns as
 * they start and end, and a finished background turn raises a notification;
 * the timer covers the rest, such as a turn started from the CLI or the phone.
 * Hidden, it slows rather than stops, so the list is close to right the
 * moment the window comes back.
 */
const POLL_INTERVAL_MS = 30_000;
const HIDDEN_POLL_INTERVAL_MS = 120_000;

/**
 * Keeps the list of work current: which conversations are running, which
 * finished while you were elsewhere, and the order activity puts them in.
 *
 * The first read belongs to the shell's boot. After that this re-reads the
 * list on the timer and whenever a signal says it moved. A read that started
 * before a local change — a new chat, an archive, a pin — is dropped and asked
 * again, so it cannot undo the change it did not see.
 */
export function useChatListWatcher(client: ApiClient | null): void {
  const refreshRef = useRef<(() => void) | null>(null);
  const chatsSignal = useRefreshSignals((state) => state.chats);
  const notificationsSignal = useRefreshSignals((state) => state.notifications);

  useEffect(() => {
    if (!client) {
      refreshRef.current = null;
      return;
    }
    let cancelled = false;
    let inFlight = false;
    let queued = false;

    const refresh = async () => {
      if (inFlight) {
        queued = true;
        return;
      }
      inFlight = true;
      try {
        // One retry covers a local change that landed mid-read; a second one
        // in the same moment waits for the next signal or tick.
        for (let attempt = 0; attempt < 2; attempt += 1) {
          const startedAt = useChatListStore.getState().revision;
          const chats = await client.listChats();
          if (cancelled) return;
          if (
            useChatListStore.getState().acceptFetchedChats(chats, startedAt)
          ) {
            break;
          }
        }
      } catch (err) {
        // Keep the last list. A transient failure is no reason to blank the
        // rail, and the boot load already reports a list that never arrived.
        if (!cancelled) console.error("failed to refresh the work list", err);
      } finally {
        inFlight = false;
        if (queued && !cancelled) {
          queued = false;
          void refresh();
        }
      }
    };

    refreshRef.current = () => void refresh();
    return () => {
      cancelled = true;
      refreshRef.current = null;
    };
  }, [client]);

  useVisibilityGatedPoll(() => refreshRef.current?.(), POLL_INTERVAL_MS, {
    enabled: client !== null,
    hiddenIntervalMs: HIDDEN_POLL_INTERVAL_MS,
    revision: chatsSignal + notificationsSignal,
  });
}

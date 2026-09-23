import type { ApiClient } from "../../api/client";
import type { CodeForkTranscript } from "../../api/types";
import { useSyncExternalStore } from "react";

/**
 * A fork's transcript, waiting for the first message of the session it was
 * forked into.
 *
 * The start surface names the transcript in that first message. If the send
 * is refused, the message goes back to the new session's composer, and this
 * keeps the transcript chip beside it, so a retry names the file again. The
 * first accepted turn clears it.
 */
export type FirstTurnRecovery = {
  id: string;
  sessionId: string;
  forkSource: CodeForkTranscript | null;
};

const firstTurnRecoveryByClient = new WeakMap<
  ApiClient,
  Map<string, FirstTurnRecovery>
>();

const firstTurnRecoveryListeners = new Set<() => void>();

function readFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
): FirstTurnRecovery | null {
  return firstTurnRecoveryByClient.get(client)?.get(sessionId) ?? null;
}

export function writeFirstTurnRecovery(
  client: ApiClient,
  recovery: FirstTurnRecovery,
): void {
  let recoveries = firstTurnRecoveryByClient.get(client);
  if (!recoveries) {
    recoveries = new Map();
    firstTurnRecoveryByClient.set(client, recoveries);
  }
  recoveries.set(recovery.sessionId, recovery);
  for (const listener of firstTurnRecoveryListeners) listener();
}

export function clearFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
  recoveryId: string,
): void {
  const recoveries = firstTurnRecoveryByClient.get(client);
  if (recoveries?.get(sessionId)?.id !== recoveryId) return;
  recoveries.delete(sessionId);
  for (const listener of firstTurnRecoveryListeners) listener();
}

export function updateFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
  recoveryId: string,
  update: (current: FirstTurnRecovery) => FirstTurnRecovery,
): void {
  const current = readFirstTurnRecovery(client, sessionId);
  if (!current || current.id !== recoveryId) return;
  writeFirstTurnRecovery(client, update(current));
}

export function useFirstTurnRecovery(
  client: ApiClient,
  sessionId: string,
): FirstTurnRecovery | null {
  return useSyncExternalStore(
    (listener) => {
      firstTurnRecoveryListeners.add(listener);
      return () => {
        firstTurnRecoveryListeners.delete(listener);
      };
    },
    () => readFirstTurnRecovery(client, sessionId),
    () => null,
  );
}

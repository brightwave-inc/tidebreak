import type { ApiClient } from "../../api/client";
import type { CodeForkTranscript } from "../../api/types";
import { useSyncExternalStore } from "react";

export type FirstTurnRecovery = {
  id: string;
  sessionId: string;
  draft: string;
  forkSource: CodeForkTranscript | null;
  message: string;
  status: "sending" | "failed";
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

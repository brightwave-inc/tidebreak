import { useCallback, useEffect } from "react";
import { create } from "zustand";

import { useApp } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { CodeSubscriptionUsage } from "@/api/types";
import { startVisibilityGatedPoll } from "@/useVisibilityGatedPoll";
import {
  codeClientGeneration,
  isCodeClientGenerationActive,
} from "./CodeClientGeneration";

const REFRESH_INTERVAL_MS = 60_000;

type CodeSubscriptionUsageState = {
  report: CodeSubscriptionUsage | null;
  refreshing: boolean;
  error: string | null;
  refreshInFlight: boolean;
  refresh: (client: ApiClient) => Promise<void>;
};

type SubscriptionRequest = {
  clientGeneration: number;
  storeGeneration: number;
  promise: Promise<void>;
};

let storeGeneration = 0;
let refreshRequest: SubscriptionRequest | null = null;

function requestIsCurrent(
  clientGeneration: number,
  requestStoreGeneration: number,
): boolean {
  return (
    requestStoreGeneration === storeGeneration &&
    isCodeClientGenerationActive(clientGeneration)
  );
}

export const useCodeSubscriptionUsageStore = create<CodeSubscriptionUsageState>(
  (set) => ({
    report: null,
    refreshing: false,
    error: null,
    refreshInFlight: false,
    refresh: (client) => {
      const clientGeneration = codeClientGeneration(client);
      const requestStoreGeneration = storeGeneration;
      if (!requestIsCurrent(clientGeneration, requestStoreGeneration)) {
        return Promise.resolve();
      }
      if (
        refreshRequest?.clientGeneration === clientGeneration &&
        refreshRequest.storeGeneration === requestStoreGeneration
      ) {
        return refreshRequest.promise;
      }
      set({ refreshInFlight: true, refreshing: true });
      const promise = Promise.resolve()
        .then(() => client.getCodeSubscriptionUsage())
        .then((report) => {
          if (requestIsCurrent(clientGeneration, requestStoreGeneration)) {
            set({ report, error: null });
          }
        })
        .catch(() => {
          if (requestIsCurrent(clientGeneration, requestStoreGeneration)) {
            set({ error: "Usage could not be refreshed." });
          }
        })
        .finally(() => {
          if (refreshRequest?.promise === promise) refreshRequest = null;
          if (requestIsCurrent(clientGeneration, requestStoreGeneration)) {
            set({ refreshInFlight: false, refreshing: false });
          }
        });
      refreshRequest = {
        clientGeneration,
        storeGeneration: requestStoreGeneration,
        promise,
      };
      return promise;
    },
  }),
);

/** Share one quota fetch between the code rail and the analytics page. */
export function useCodeSubscriptionUsage() {
  const { client } = useApp();
  const report = useCodeSubscriptionUsageStore((state) => state.report);
  const refreshing = useCodeSubscriptionUsageStore((state) => state.refreshing);
  const error = useCodeSubscriptionUsageStore((state) => state.error);
  const refreshStore = useCodeSubscriptionUsageStore((state) => state.refresh);
  const refresh = useCallback(
    () => refreshStore(client),
    [client, refreshStore],
  );

  useEffect(() => acquireUsagePoller(client), [client]);

  return { report, refreshing, error, refresh };
}

/**
 * One timer for every consumer. The rail and the analytics page both mount
 * this hook; before this each ran its own minute clock, so an open analytics
 * page doubled the quota reads. The first consumer reads at once and starts
 * the clock; later ones join it; the last one leaving stops it. Hidden, the
 * clock pauses — a quota bar nobody can see needs no refresh, and the read on
 * return brings it current.
 */
let pollerConsumers = 0;
let stopPoller: (() => void) | null = null;

function acquireUsagePoller(client: ApiClient): () => void {
  pollerConsumers += 1;
  if (pollerConsumers === 1) {
    const read = () =>
      void useCodeSubscriptionUsageStore.getState().refresh(client);
    read();
    stopPoller = startVisibilityGatedPoll(read, REFRESH_INTERVAL_MS);
  }
  return () => {
    pollerConsumers -= 1;
    if (pollerConsumers === 0) {
      stopPoller?.();
      stopPoller = null;
    }
  };
}

export function resetCodeSubscriptionUsageStore() {
  storeGeneration += 1;
  refreshRequest = null;
  useCodeSubscriptionUsageStore.setState({
    report: null,
    refreshing: false,
    error: null,
    refreshInFlight: false,
  });
}

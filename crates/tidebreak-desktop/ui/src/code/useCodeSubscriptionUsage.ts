import { useCallback, useEffect } from "react";
import { create } from "zustand";

import { useApp } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { CodeSubscriptionUsage } from "@/api/types";

const REFRESH_INTERVAL_MS = 60_000;

type CodeSubscriptionUsageState = {
  report: CodeSubscriptionUsage | null;
  refreshing: boolean;
  error: string | null;
  refreshInFlight: boolean;
  refresh: (client: ApiClient) => Promise<void>;
};

export const useCodeSubscriptionUsageStore = create<CodeSubscriptionUsageState>(
  (set, get) => ({
    report: null,
    refreshing: false,
    error: null,
    refreshInFlight: false,
    refresh: async (client) => {
      if (get().refreshInFlight) return;
      set({ refreshInFlight: true, refreshing: true });
      try {
        const report = await client.getCodeSubscriptionUsage();
        set({ report, error: null });
      } catch {
        set({ error: "Usage could not be refreshed." });
      } finally {
        set({ refreshInFlight: false, refreshing: false });
      }
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

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  return { report, refreshing, error, refresh };
}

export function resetCodeSubscriptionUsageStore() {
  useCodeSubscriptionUsageStore.setState({
    report: null,
    refreshing: false,
    error: null,
    refreshInFlight: false,
  });
}

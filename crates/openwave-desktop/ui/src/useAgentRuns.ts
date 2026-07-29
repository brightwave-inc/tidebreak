import { useEffect, useRef, useState } from "react";

import type { AgentRun, ApiClient } from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";

const LIVE_POLL_INTERVAL_MS = 5_000;

export type AgentRuns = {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
};

/**
 * Observe the durable children attached to visible spawn steps.
 *
 * Agent state advances outside the foreground event stream, so a live or not
 * yet resolved spawn is re-read until the durable snapshot catches up. Every
 * read is scoped to the selected chat and stale responses are discarded on a
 * chat switch.
 */
export function useAgentRuns(
  client: ApiClient | null,
  chatId: string | null,
  spawnKeys: readonly string[],
): AgentRuns {
  const [runs, setRuns] = useState<AgentRun[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshRef = useRef<(() => void) | null>(null);
  const spawnKey = [...new Set(spawnKeys)].sort().join(",");
  const observing = client !== null && chatId !== null && spawnKey.length > 0;

  useEffect(() => {
    if (!client || !chatId || !spawnKey) {
      setRuns([]);
      setLoading(false);
      setError(null);
      refreshRef.current = null;
      return;
    }

    let cancelled = false;
    let requestSeq = 0;
    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const listed = await client.listAgentRuns(chatId);
        if (cancelled || seq !== requestSeq) return;
        setRuns(listed);
        setError(null);
      } catch (cause) {
        if (!cancelled && seq === requestSeq) setError(String(cause));
      } finally {
        if (!cancelled && seq === requestSeq) setLoading(false);
      }
    };

    setRuns([]);
    setLoading(true);
    setError(null);
    refreshRef.current = () => void refresh();
    void refresh();
    return () => {
      cancelled = true;
      requestSeq += 1;
      refreshRef.current = null;
    };
  }, [client, chatId, spawnKey]);

  const matchesVisibleSpawn = (run: AgentRun) =>
    run.tier === "background" &&
    spawnKeys.some((key) => run.id === key || run.spawn_call_id === key);
  const hasLiveSandbox = runs.some(
    (run) => matchesVisibleSpawn(run) && RUNNING_AGENT_STATUSES.has(run.status),
  );
  const hasUnresolvedSpawn =
    observing &&
    spawnKeys.some(
      (key) =>
        !runs.some(
          (run) => matchesVisibleSpawn(run) && (run.id === key || run.spawn_call_id === key),
        ),
    );
  useEffect(() => {
    if (!hasLiveSandbox && !hasUnresolvedSpawn) return;
    const interval = window.setInterval(
      () => refreshRef.current?.(),
      LIVE_POLL_INTERVAL_MS,
    );
    return () => window.clearInterval(interval);
  }, [hasLiveSandbox, hasUnresolvedSpawn]);

  return {
    runs,
    loading,
    error,
    refresh: () => refreshRef.current?.(),
  };
}

import { useCallback, useEffect, useRef, useState } from "react";

import type {
  AgentActivityHistoryEntry,
  AgentRun,
  AgentRunProgress,
  AgentRunTaskPlan,
  ApiClient,
} from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import type { ChatMessage } from "./MessageList";

const LIVE_POLL_INTERVAL_MS = 5_000;

/**
 * The spawn steps in a transcript worth observing, by the key each one is
 * matched on: the run it resolved to, or — until it has one — the call that
 * asked for it. A spawn that failed or was cancelled never produced a durable
 * child, so nothing is waiting on it.
 */
export function backgroundAgentSpawnKeys(
  messages: readonly ChatMessage[],
): string[] {
  return messages.flatMap((message) =>
    message.role === "tool" &&
    message.name === "spawn_sandbox_agent" &&
    message.status !== "failed" &&
    message.status !== "cancelled"
      ? [message.backgroundAgentRunId ?? message.callId]
      : [],
  );
}

export type AgentRuns = {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
  /**
   * Durably request cancellation of one background run, then refresh so the
   * poll picks up the `cancelling`/`cancelled` transition. The caller holds its
   * own optimistic "Stopping" state until that durable status arrives.
   */
  cancel: (runId: string) => Promise<void>;
  /** Fetch the ordered, renderer-safe activity history for one background run. */
  loadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>;
  /**
   * Fetch the full checklist one background run keeps.
   *
   * The snapshot poll already carries the count and the current step, so this
   * is read only when a reader opens the run — and again when the snapshot
   * says the plan has moved on.
   */
  loadTaskPlan: (runId: string) => Promise<AgentRunTaskPlan | null>;
  /**
   * Read one page of a background run's live progress, resuming from the
   * caller's cursor. The caller holds the cursor because it is what makes the
   * poll cheap: a run with nothing new to say answers with an empty page.
   */
  loadProgress: (
    runId: string,
    afterSequence: number,
  ) => Promise<AgentRunProgress>;
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

  // Stable identities so a row can list them as effect dependencies without
  // re-fetching its timeline on every parent render.
  const cancel = useCallback(
    async (runId: string) => {
      if (!client || !chatId) return;
      await client.cancelAgentRun(chatId, runId);
      refreshRef.current?.();
    },
    [client, chatId],
  );
  const loadActivity = useCallback(
    async (runId: string) => {
      if (!client || !chatId) return [];
      return client.listAgentRunActivity(chatId, runId);
    },
    [client, chatId],
  );
  const loadTaskPlan = useCallback(
    async (runId: string) => {
      if (!client || !chatId) return null;
      return client.getAgentRunTaskPlan(chatId, runId);
    },
    [client, chatId],
  );

  const loadProgress = useCallback(
    async (runId: string, afterSequence: number) => {
      if (!client || !chatId) return { entries: [], nextSequence: afterSequence };
      return client.listAgentRunProgress(chatId, runId, afterSequence);
    },
    [client, chatId],
  );

  return {
    runs,
    loading,
    error,
    refresh: () => refreshRef.current?.(),
    cancel,
    loadActivity,
    loadTaskPlan,
    loadProgress,
  };
}

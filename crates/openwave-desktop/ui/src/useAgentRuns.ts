import { useEffect, useRef, useState } from "react";
import type { AgentRun, ApiClient } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useOpenConversation } from "./OpenConversation";
import { useRefreshSignals } from "./RefreshSignals";
import {
  SandboxAgentStopFence,
  canStopSandboxAgentRun,
  reconcileSandboxAgentCancellation,
} from "./SandboxAgentStop";

/**
 * How often a live sandbox run is re-read. Sandbox progress arrives as durable
 * state rather than on the conversation's event stream, so the panel has to ask.
 */
const LIVE_POLL_INTERVAL_MS = 5_000;

const LIVE_SANDBOX_STATUSES = [
  "queued",
  "running",
  "cancelling",
  "waiting",
  "retry_wait",
];

export type AgentRuns = {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  stoppingRunIds: Set<string>;
  stopErrorRunIds: Set<string>;
  refresh: () => void;
  stop: (runId: string) => void;
};

/**
 * The agent runs behind one conversation, and the stops that end them.
 *
 * Unlike the other pollers here there is no standing interval: the event stream
 * says when a run has moved, and only a *live sandbox* run — which reports
 * progress outside that stream — is polled, for as long as it stays live.
 *
 * A stop is fenced per run so a second click while one is in flight is ignored,
 * and every completion is measured against the conversation it started under:
 * see [SandboxAgentStopFence].
 */
export function useAgentRuns(
  client: ApiClient | null,
  chatId: string | null,
): AgentRuns {
  const [runs, setRuns] = useState<AgentRun[]>([]);
  // True until the first listing comes back. "We have not asked yet" must not
  // render as "we asked and this conversation has no background work".
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [stoppingRunIds, setStoppingRunIds] = useState<Set<string>>(new Set());
  const [stopErrorRunIds, setStopErrorRunIds] = useState<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const stopFenceRef = useRef(new SandboxAgentStopFence());
  const stillOpen = useOpenConversation(chatId);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const signal = useRefreshSignals((state) => state.agentRuns);

  useEffect(() => {
    if (!client || !chatId) return;
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const listed = await client.listAgentRuns(chatId);
        if (!cancelled && seq === requestSeq) {
          setRuns(listed);
          setError(null);
        }
      } catch (err) {
        if (!cancelled && seq === requestSeq) setError(String(err));
      } finally {
        if (!cancelled && seq === requestSeq) setLoading(false);
      }
    };

    setRuns([]);
    setError(null);
    setLoading(true);
    refreshRef.current = () => void refresh();
    void refresh();
    return () => {
      cancelled = true;
      requestSeq += 1;
      refreshRef.current = null;
    };
  }, [client, chatId]);

  // Only a signal raised after this hook mounted means anything to it; the
  // counter is app-wide and may already be well past zero on arrival.
  const lastSignalRef = useRef(signal);
  useEffect(() => {
    if (lastSignalRef.current === signal) return;
    lastSignalRef.current = signal;
    refreshRef.current?.();
  }, [signal]);

  const hasLiveSandboxRun = runs.some(
    (run) =>
      run.execution === "sandbox" && LIVE_SANDBOX_STATUSES.includes(run.status),
  );
  useEffect(() => {
    if (!hasLiveSandboxRun) return;
    const interval = window.setInterval(
      () => refreshRef.current?.(),
      LIVE_POLL_INTERVAL_MS,
    );
    return () => window.clearInterval(interval);
  }, [hasLiveSandboxRun]);

  // Deleting this conversation takes its runs with it. Invalidating the fence
  // makes every stop still in flight stale, and the markers go with it so the
  // replacement conversation does not inherit a "Stopping…" button.
  useEffect(() => {
    if (!chatId || deletingChatId !== chatId) return;
    stopFenceRef.current.invalidate();
    setStoppingRunIds(new Set());
    setStopErrorRunIds(new Set());
  }, [chatId, deletingChatId]);

  /**
   * The chat a stop completion should be measured against: this conversation
   * while it is still open, and nothing once it is not. Naming no chat makes
   * every token stale, which is what a stop that fails against a chat on its
   * way out deserves — the error would have nowhere to be read.
   */
  function fencedChatId(startedChatId: string): string | null {
    return stillOpen(startedChatId) ? startedChatId : null;
  }

  async function stop(runId: string) {
    // Any deletion in flight blocks a stop, not just this chat's: the request
    // would race a chat list that is already being rebuilt.
    if (!client || !chatId || deletingChatId !== null) return;
    const target = runs.find((run) => run.id === runId);
    if (!target || !canStopSandboxAgentRun(target)) return;

    const startedChatId = chatId;
    const request = stopFenceRef.current.begin(startedChatId, runId);
    if (!request) return;
    setStoppingRunIds((current) => new Set(current).add(runId));
    setStopErrorRunIds((current) => {
      const next = new Set(current);
      next.delete(runId);
      return next;
    });

    try {
      const cancellation = await client.cancelAgentRun(startedChatId, runId);
      if (
        !stopFenceRef.current.isCurrent(request, fencedChatId(startedChatId))
      ) {
        return;
      }
      setRuns((current) =>
        reconcileSandboxAgentCancellation(current, cancellation),
      );
      refreshRef.current?.();
    } catch {
      if (
        !stopFenceRef.current.isCurrent(request, fencedChatId(startedChatId))
      ) {
        return;
      }
      setStopErrorRunIds((current) => new Set(current).add(runId));
    } finally {
      if (stopFenceRef.current.finish(request, fencedChatId(startedChatId))) {
        setStoppingRunIds((current) => {
          const next = new Set(current);
          next.delete(runId);
          return next;
        });
      }
    }
  }

  return {
    runs,
    loading,
    error,
    stoppingRunIds,
    stopErrorRunIds,
    refresh: () => refreshRef.current?.(),
    stop: (runId) => void stop(runId),
  };
}

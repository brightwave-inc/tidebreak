import { useEffect, useRef, useState } from "react";
import type { ApiClient, PendingFolderAccessRequest } from "./api";
import {
  hasNativeHost,
  resolveFolderAccessRequest,
  type FolderAccessDecision,
} from "./host";
import { useRefreshSignals } from "./RefreshSignals";

const POLL_INTERVAL_MS = 10_000;

export type FolderAccessRequests = {
  requests: PendingFolderAccessRequest[];
  resolving: Set<string>;
  errors: Record<string, string>;
  decide: (callId: string, decision: FolderAccessDecision) => void;
  cancel: (callId: string, turnId: string) => void;
};

/**
 * Pending folder-access requests for one conversation, and the decisions that
 * resolve them.
 *
 * Polling is the durable truth; the event stream only says when to look again.
 * A decision opens a native dialog, so at most one is in flight at a time —
 * a second prompt while the first is open would be answering a question the
 * reader cannot see.
 */
export function useFolderAccessRequests(
  client: ApiClient | null,
  chatId: string | null,
): FolderAccessRequests {
  const [requests, setRequests] = useState<PendingFolderAccessRequest[]>([]);
  const [resolving, setResolving] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});
  const resolvingRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const signal = useRefreshSignals((state) => state.folderAccess);

  useEffect(() => {
    if (!client || !chatId) return;
    let cancelled = false;
    let requestSeq = 0;

    const refresh = async () => {
      const seq = ++requestSeq;
      try {
        const pending = await client.listPendingFolderAccessRequests(chatId);
        if (!cancelled && seq === requestSeq) setRequests(pending);
      } catch (err) {
        if (!cancelled && seq === requestSeq) {
          console.error("failed to refresh pending folder access", err);
          setRequests([]);
        }
      }
    };

    refreshRef.current = () => void refresh();
    void refresh();
    const interval = window.setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      requestSeq += 1;
      window.clearInterval(interval);
      refreshRef.current = null;
      setRequests([]);
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

  function beginResolving(callId: string) {
    resolvingRef.current.add(callId);
    setResolving((calls) => new Set(calls).add(callId));
    setErrors((current) => {
      const next = { ...current };
      delete next[callId];
      return next;
    });
  }

  function finishResolving(callId: string) {
    resolvingRef.current.delete(callId);
    setResolving((calls) => {
      const next = new Set(calls);
      next.delete(callId);
      return next;
    });
    refreshRef.current?.();
  }

  async function decide(callId: string, decision: FolderAccessDecision) {
    if (!chatId || !hasNativeHost()) return;
    if (resolvingRef.current.size > 0) return;
    beginResolving(callId);
    try {
      await resolveFolderAccessRequest(chatId, callId, decision);
    } catch (err) {
      setErrors((current) => ({ ...current, [callId]: String(err) }));
    } finally {
      finishResolving(callId);
    }
  }

  async function cancel(callId: string, turnId: string) {
    if (!client || !chatId || resolvingRef.current.size > 0) return;
    beginResolving(callId);
    try {
      await client.cancel(chatId, turnId);
    } catch (err) {
      setErrors((current) => ({ ...current, [callId]: String(err) }));
    } finally {
      finishResolving(callId);
    }
  }

  return {
    requests,
    resolving,
    errors,
    decide: (callId, decision) => void decide(callId, decision),
    cancel: (callId, turnId) => void cancel(callId, turnId),
  };
}

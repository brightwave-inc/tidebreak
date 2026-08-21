import { useCallback, useEffect, useRef, useState } from "react";

import type { ApiClient } from "../api/client";
import {
  acquireCodeSessionFromClient,
  releaseCodeSession,
} from "./CodeSessionRegistry";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * Keeping worktree-derived views current.
 *
 * The git status, the changed-file list, and the diff are all server reads of
 * a worktree the engine is editing underneath us. Fetching once on mount left
 * every one of them showing the pre-turn state until the page was reloaded.
 * The journal already says when the worktree may have moved, so the session
 * store's `contentRevision` is the one signal these views watch.
 */

/** Debounce a burst of journal events into one refetch. */
const DEFAULT_DEBOUNCE_MS = 250;

/**
 * Subscribe to a session's content revision, holding a registry reference for
 * as long as the caller is mounted. A null session (none started yet) is a
 * constant 0.
 */
export function useCodeContentRevision(
  sessionId: string | null,
  client: Pick<ApiClient, "openCodeEvents" | "listCodeSessionTurns">,
): number {
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    if (!sessionId) {
      setRevision(0);
      return;
    }
    const store = acquireCodeSessionFromClient(sessionId, client);
    setRevision(store.getState().contentRevision);
    const unsubscribe = store.subscribe((state) => {
      setRevision(state.contentRevision);
    });
    return () => {
      unsubscribe();
      releaseCodeSession(sessionId);
    };
  }, [sessionId, client]);

  return revision;
}

export type LiveResource<T> = {
  /** The last value that loaded, kept in place while a refresh is running. */
  data: T | null;
  error: string | null;
  /** A load is in flight, whether or not there is already data to show. */
  refreshing: boolean;
  /** Refresh now, skipping the debounce. Resolves when the load settles. */
  refresh: () => Promise<void>;
  /**
   * Adopt a value an action already returned, in place of refetching it. Any
   * load in flight is abandoned so it cannot overwrite the newer value.
   */
  adopt: (value: T) => void;
};

/**
 * A fetch that reruns when `revision` changes and resets when `key` does.
 *
 * Reloading never blanks what is on screen: `data` only changes on a
 * successful load, so a refresh shows the stale value until the new one
 * arrives. Overlapping loads are collapsed — a revision that lands mid-flight
 * queues exactly one more load rather than stacking requests.
 */
export function useLiveResource<T>({
  key,
  revision,
  load,
  errorMessage,
  debounceMs = DEFAULT_DEBOUNCE_MS,
}: {
  /** Identity of what is being loaded; a change clears `data` and reloads. */
  key: string;
  revision: number;
  load: () => Promise<T>;
  errorMessage: string;
  debounceMs?: number;
}): LiveResource<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(true);

  const loadRef = useRef(load);
  loadRef.current = load;
  const errorMessageRef = useRef(errorMessage);
  errorMessageRef.current = errorMessage;

  // Every load carries the generation it started in; a response from an older
  // generation belongs to a key we have moved off, or to an unmounted view.
  const generationRef = useRef(0);
  const inFlightRef = useRef<{
    generation: number;
    promise: Promise<void>;
  } | null>(null);
  const pendingRef = useRef(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const seenRef = useRef<{ key: string; revision: number }>({ key, revision });

  const run = useCallback(function run(): Promise<void> {
    const inFlight = inFlightRef.current;
    if (inFlight) {
      pendingRef.current = true;
      return inFlight.promise;
    }
    const generation = generationRef.current;
    setRefreshing(true);
    const promise = (async () => {
      try {
        const next = await loadRef.current();
        if (generation !== generationRef.current) return;
        setData(next);
        setError(null);
      } catch (err) {
        if (generation !== generationRef.current) return;
        setError(friendlyErrorMessage(err, errorMessageRef.current));
      } finally {
        if (generation === generationRef.current) {
          inFlightRef.current = null;
          setRefreshing(false);
          if (pendingRef.current) {
            pendingRef.current = false;
            void run();
          }
        }
      }
    })();
    inFlightRef.current = { generation, promise };
    return promise;
  }, []);

  const adopt = useCallback((value: T) => {
    generationRef.current += 1;
    inFlightRef.current = null;
    pendingRef.current = false;
    setData(value);
    setError(null);
    setRefreshing(false);
  }, []);

  useEffect(() => {
    generationRef.current += 1;
    inFlightRef.current = null;
    pendingRef.current = false;
    setData(null);
    setError(null);
    void run();
    return () => {
      // Retire this generation so a late response cannot land on the next key
      // or on an unmounted view.
      generationRef.current += 1;
    };
  }, [key, run]);

  useEffect(() => {
    if (seenRef.current.key !== key) {
      // The reset effect above already reloaded for this key.
      seenRef.current = { key, revision };
      return;
    }
    if (seenRef.current.revision === revision) return;
    seenRef.current = { key, revision };
    if (timerRef.current !== null) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      void run();
    }, debounceMs);
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [key, revision, debounceMs, run]);

  return { data, error, refreshing, refresh: run, adopt };
}

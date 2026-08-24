import { useEffect, useRef } from "react";
import { useRouterState } from "@tanstack/react-router";

import type { ApiClient } from "../api/client";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryTarget,
} from "../api/types";
import { requestUserAttention } from "../host";
import {
  codeDeliveryRepositoryTarget,
  trackedCodeDeliveryRepositories,
  useCodeDeliveryStore,
} from "./CodeDeliveryStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

// Freshness rides the `delivery` nudge on the updates socket (decision 66):
// the server says when the pull-request store changed, and this monitor
// re-reads then. The remaining timers are a safety net — run summaries have
// no store yet, and a dropped socket must not silence notifications.
const SAFETY_POLL_MS = 5 * 60_000;
const HIDDEN_POLL_MS = 10 * 60_000;
const FIRST_RUN_LOOKBACK_MS = 24 * 60 * 60 * 1_000;
const MAX_LOOKBACK_MS = 30 * 24 * 60 * 60 * 1_000;
const OVERLAP_MS = 2 * 60 * 1_000;
const MAX_MONITOR_PAGES = 5;

type MonitorBatch<T> = {
  items: T[];
  complete: boolean;
  nextCursor?: string;
};

/** Shell-level delivery polling for GitHub notifications. */
export function CodeDeliveryMonitor({ client }: { client: ApiClient }) {
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const pathnameRef = useRef(pathname);
  pathnameRef.current = pathname;
  const wakeRef = useRef<(() => void) | null>(null);
  const deliveryRevision = useCodeUpdatesStore(
    (state) => state.deliveryRevision,
  );

  useEffect(() => {
    // A nudge while hidden waits for the hidden-window poll; visibility
    // returning schedules its own immediate pass.
    if (document.hidden) return;
    wakeRef.current?.();
  }, [pathname, deliveryRevision]);

  useEffect(() => {
    let cancelled = false;
    let running = false;
    let rerunRequested = false;
    let timer: number | null = null;
    let queryController: AbortController | null = null;

    const interval = () => {
      if (document.hidden) return HIDDEN_POLL_MS;
      return SAFETY_POLL_MS;
    };

    const schedule = (delay = interval()) => {
      if (cancelled) return;
      if (running) {
        rerunRequested = true;
        return;
      }
      if (timer !== null) window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = null;
        void poll();
      }, delay);
    };

    const poll = async () => {
      if (cancelled) return;
      if (running) {
        rerunRequested = true;
        return;
      }
      running = true;
      rerunRequested = false;
      const startedAt = new Date().toISOString();
      const initial = useCodeDeliveryStore.getState();
      initial.setPollState(true, null);
      try {
        const discovered = await initial.loadRepositories(client);
        if (cancelled) return;
        const current = useCodeDeliveryStore.getState();
        if (
          !discovered.capability.found ||
          discovered.capability.authenticated === false
        ) {
          current.setPollState(false, null);
          return;
        }

        const repositories = trackedCodeDeliveryRepositories(
          discovered.repositories,
          current,
        );
        const targets = repositories.map(codeDeliveryRepositoryTarget);
        if (targets.length === 0) {
          current.completeDeliveryPoll([], [], startedAt);
          return;
        }

        queryController = new AbortController();
        const since = monitorSince(current.lastPollAt, Date.parse(startedAt));
        const pullRequests: CodeDeliveryPullRequestSummary[] = [];
        const runs: CodeDeliveryRunSummary[] = [];
        let pullRequestCursor: string | undefined;
        let runCursor: string | undefined;
        let pullRequestsComplete = false;
        let runsComplete = false;

        while (!pullRequestsComplete || !runsComplete) {
          const batches: [
            MonitorBatch<CodeDeliveryPullRequestSummary>,
            MonitorBatch<CodeDeliveryRunSummary>,
          ] = await Promise.all([
            pullRequestsComplete
              ? Promise.resolve<MonitorBatch<CodeDeliveryPullRequestSummary>>({
                  items: [],
                  complete: true,
                })
              : monitorPullRequests(
                  client,
                  targets,
                  since,
                  pullRequestCursor,
                  queryController.signal,
                ),
            runsComplete
              ? Promise.resolve<MonitorBatch<CodeDeliveryRunSummary>>({
                  items: [],
                  complete: true,
                })
              : monitorRuns(
                  client,
                  targets,
                  since,
                  runCursor,
                  queryController.signal,
                ),
          ]);
          const [pullRequestBatch, runBatch] = batches;
          pullRequests.push(...pullRequestBatch.items);
          runs.push(...runBatch.items);
          pullRequestsComplete = pullRequestBatch.complete;
          runsComplete = runBatch.complete;
          pullRequestCursor = pullRequestBatch.nextCursor;
          runCursor = runBatch.nextCursor;
        }

        if (cancelled) return;
        const added = useCodeDeliveryStore
          .getState()
          .completeDeliveryPoll(pullRequests, runs, startedAt);
        if (added > 0 && pathnameRef.current !== "/code/notifications") {
          void requestUserAttention().catch(() => {
            // The notification feed remains the durable signal.
          });
        }
      } catch (error) {
        if (cancelled || isAbortError(error)) return;
        useCodeDeliveryStore
          .getState()
          .setPollState(false, deliveryErrorMessage(error));
      } finally {
        queryController = null;
        running = false;
        if (!cancelled) schedule(rerunRequested ? 0 : interval());
      }
    };

    const onVisibilityChange = () => {
      schedule(document.hidden ? interval() : 0);
    };
    wakeRef.current = () => schedule(0);
    document.addEventListener("visibilitychange", onVisibilityChange);
    schedule(0);
    return () => {
      cancelled = true;
      wakeRef.current = null;
      document.removeEventListener("visibilitychange", onVisibilityChange);
      if (timer !== null) window.clearTimeout(timer);
      queryController?.abort();
    };
  }, [client]);

  return null;
}

export function monitorSince(lastPollAt: string | null, now: number): string {
  const floor = now - MAX_LOOKBACK_MS;
  if (!lastPollAt) return new Date(now - FIRST_RUN_LOOKBACK_MS).toISOString();
  const parsed = Date.parse(lastPollAt);
  const withOverlap = Number.isFinite(parsed)
    ? parsed - OVERLAP_MS
    : now - FIRST_RUN_LOOKBACK_MS;
  return new Date(Math.max(floor, withOverlap)).toISOString();
}

export async function monitorPullRequests(
  client: Pick<ApiClient, "queryCodeDeliveryPullRequests">,
  repositories: CodeGitHubRepositoryTarget[],
  updatedAfter: string,
  initialCursor?: string,
  signal?: AbortSignal,
): Promise<MonitorBatch<CodeDeliveryPullRequestSummary>> {
  const items: CodeDeliveryPullRequestSummary[] = [];
  let cursor = initialCursor;
  for (let pageNumber = 0; pageNumber < MAX_MONITOR_PAGES; pageNumber += 1) {
    const page = await client.queryCodeDeliveryPullRequests(
      {
        repositories,
        states: ["open"],
        review_states: [],
        check_states: [],
        authors: [],
        attention_only: false,
        ready_only: false,
        updated_after: updatedAfter,
        limit: 100,
        refresh: false,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    items.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) return { items, complete: true };
  }
  return { items, complete: false, ...(cursor ? { nextCursor: cursor } : {}) };
}

export async function monitorRuns(
  client: Pick<ApiClient, "queryCodeDeliveryRuns">,
  repositories: CodeGitHubRepositoryTarget[],
  createdAfter: string,
  initialCursor?: string,
  signal?: AbortSignal,
): Promise<MonitorBatch<CodeDeliveryRunSummary>> {
  const items: CodeDeliveryRunSummary[] = [];
  let cursor = initialCursor;
  for (let pageNumber = 0; pageNumber < MAX_MONITOR_PAGES; pageNumber += 1) {
    const page = await client.queryCodeDeliveryRuns(
      {
        repositories,
        kinds: [],
        statuses: [],
        conclusions: [],
        workflows: [],
        environments: [],
        branches: [],
        events: [],
        actors: [],
        attention_only: true,
        created_after: createdAfter,
        limit: 100,
        refresh: false,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    items.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) return { items, complete: true };
  }
  return { items, complete: false, ...(cursor ? { nextCursor: cursor } : {}) };
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function deliveryErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

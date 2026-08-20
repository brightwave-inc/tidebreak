import { useEffect } from "react";
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

const ACTIVE_POLL_MS = 30_000;
const VISIBLE_POLL_MS = 2 * 60_000;
const HIDDEN_POLL_MS = 10 * 60_000;
const FIRST_RUN_LOOKBACK_MS = 24 * 60 * 60 * 1_000;
const MAX_LOOKBACK_MS = 30 * 24 * 60 * 60 * 1_000;
const OVERLAP_MS = 2 * 60 * 1_000;
const MAX_MONITOR_PAGES = 5;

/**
 * Shell-level delivery polling.
 *
 * It stays mounted behind the managed gate, so notifications keep advancing
 * while the reader works elsewhere without making any request before sign-in.
 * The first pass asks for one day of high-signal history; later passes overlap
 * slightly and never reach farther back than thirty days.
 */
export function CodeDeliveryMonitor({ client }: { client: ApiClient }) {
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });

  useEffect(() => {
    let cancelled = false;
    let timer: number | null = null;

    const interval = () => {
      if (document.hidden) return HIDDEN_POLL_MS;
      if (
        pathname.startsWith("/code/delivery/") ||
        pathname === "/code/notifications"
      ) {
        return ACTIVE_POLL_MS;
      }
      return VISIBLE_POLL_MS;
    };

    const schedule = (delay = interval()) => {
      if (cancelled) return;
      if (timer !== null) window.clearTimeout(timer);
      timer = window.setTimeout(() => void poll(), delay);
    };

    const poll = async () => {
      if (cancelled) return;
      const startedAt = new Date().toISOString();
      const state = useCodeDeliveryStore.getState();
      state.setPollState(true, null);
      try {
        const discovered = await client.getCodeDeliveryRepositories();
        if (cancelled) return;
        const current = useCodeDeliveryStore.getState();
        const repositories = trackedCodeDeliveryRepositories(
          discovered.repositories,
          current,
        );
        const targets = repositories.map(codeDeliveryRepositoryTarget);
        if (targets.length === 0 || !discovered.capability.found) {
          current.finishPoll(startedAt);
          schedule();
          return;
        }

        const since = monitorSince(current.lastPollAt, Date.parse(startedAt));
        const [pullRequests, runs] = await Promise.all([
          monitorPullRequests(client, targets, since),
          monitorRuns(client, targets, since),
        ]);
        if (cancelled) return;
        const store = useCodeDeliveryStore.getState();
        const added = store.ingestDeliveryPoll(pullRequests, runs, startedAt);
        store.finishPoll(startedAt);
        if (added > 0 && pathname !== "/code/notifications") {
          void requestUserAttention().catch(() => {
            // Best-effort dock attention. The feed remains the durable signal.
          });
        }
      } catch (error) {
        if (cancelled) return;
        useCodeDeliveryStore
          .getState()
          .setPollState(
            false,
            error instanceof Error ? error.message : String(error),
          );
      }
      schedule();
    };

    const onVisibilityChange = () => schedule(document.hidden ? interval() : 0);
    document.addEventListener("visibilitychange", onVisibilityChange);
    schedule(0);
    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", onVisibilityChange);
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [client, pathname]);

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

async function monitorPullRequests(
  client: Pick<ApiClient, "queryCodeDeliveryPullRequests">,
  repositories: CodeGitHubRepositoryTarget[],
  updatedAfter: string,
): Promise<CodeDeliveryPullRequestSummary[]> {
  const items: CodeDeliveryPullRequestSummary[] = [];
  let cursor: string | undefined;
  for (let pageNumber = 0; pageNumber < MAX_MONITOR_PAGES; pageNumber += 1) {
    const page = await client.queryCodeDeliveryPullRequests({
      repositories,
      states: ["open"],
      review_states: [],
      check_states: [],
      authors: [],
      attention_only: false,
      ready_only: false,
      updated_after: updatedAfter,
      limit: 100,
      ...(cursor ? { cursor } : {}),
    });
    items.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) break;
  }
  return items;
}

async function monitorRuns(
  client: Pick<ApiClient, "queryCodeDeliveryRuns">,
  repositories: CodeGitHubRepositoryTarget[],
  createdAfter: string,
): Promise<CodeDeliveryRunSummary[]> {
  const items: CodeDeliveryRunSummary[] = [];
  let cursor: string | undefined;
  for (let pageNumber = 0; pageNumber < MAX_MONITOR_PAGES; pageNumber += 1) {
    const page = await client.queryCodeDeliveryRuns({
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
      ...(cursor ? { cursor } : {}),
    });
    items.push(...page.items);
    cursor = page.next_cursor;
    if (!cursor) break;
  }
  return items;
}

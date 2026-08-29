import { useEffect, useRef } from "react";
import type { ApiClient } from "../api/client";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunSummary,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
} from "../api/types";
import {
  codeClientGeneration,
  isCodeClientGenerationActive,
} from "./CodeClientGeneration";
import {
  codeDeliveryRepositoryTarget,
  trackedCodeDeliveryRepositories,
  useCodeDeliveryStore,
  type CodeDeliveryNotificationRule,
} from "./CodeDeliveryStore";
import { triggersForNotificationRules } from "./CodeTriggerMigration";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

// Freshness rides the `delivery` nudge on the updates socket (decision 66):
// the server says when the pull-request or workflow-run store changed, and
// this monitor re-reads then. There is no clock. Deployments stay live
// GitHub observations, so this monitor asks only for workflow runs. The
// first mount still reads once, and becoming visible again reads once.
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
  const wakeRef = useRef<(() => void) | null>(null);
  const deliveryRevision = useCodeUpdatesStore(
    (state) => state.deliveryRevision,
  );

  useEffect(() => {
    // A nudge while hidden waits; visibility returning schedules its own
    // immediate pass.
    if (document.hidden) return;
    wakeRef.current?.();
  }, [deliveryRevision]);

  useEffect(() => {
    const clientGeneration = codeClientGeneration(client);
    let cancelled = false;
    let running = false;
    let rerunRequested = false;
    let timer: number | null = null;
    let queryController: AbortController | null = null;
    const isCurrent = () =>
      !cancelled && isCodeClientGenerationActive(clientGeneration);

    const schedule = (delay = 0) => {
      if (!isCurrent()) return;
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
      if (!isCurrent()) return;
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
        if (!isCurrent()) return;
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
        const legacyRules = current.legacyNotificationRules;
        if (legacyRules) {
          await migrateLegacyNotificationRules(
            client,
            legacyRules,
            repositories,
          );
          if (!isCurrent()) return;
          useCodeDeliveryStore
            .getState()
            .completeNotificationRuleMigration(legacyRules);
        }

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

        if (!isCurrent()) return;
        useCodeDeliveryStore
          .getState()
          .completeDeliveryPoll(pullRequests, runs, startedAt);
      } catch (error) {
        if (!isCurrent() || isAbortError(error)) return;
        useCodeDeliveryStore
          .getState()
          .setPollState(false, deliveryErrorMessage(error));
      } finally {
        queryController = null;
        running = false;
        const delay = nextMonitorDelayMs({ rerunRequested });
        if (isCurrent() && delay !== null) schedule(delay);
      }
    };

    const onVisibilityChange = () => {
      if (!document.hidden) schedule(0);
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

/** Arm every server trigger represented by the old client-side rules. */
export async function migrateLegacyNotificationRules(
  client: Pick<ApiClient, "listCodeTriggers" | "createCodeTrigger">,
  rules: readonly CodeDeliveryNotificationRule[],
  repositories: readonly CodeGitHubRepositoryRef[],
): Promise<void> {
  const byRepository = new Map<
    string,
    ReturnType<typeof triggersForNotificationRules>
  >();
  for (const trigger of triggersForNotificationRules(
    [...rules],
    [...repositories],
  )) {
    const triggers = byRepository.get(trigger.repoId) ?? [];
    triggers.push(trigger);
    byRepository.set(trigger.repoId, triggers);
  }
  for (const [repoId, triggers] of byRepository) {
    const existing = new Set(
      (await client.listCodeTriggers(repoId)).map(
        (trigger) => trigger.condition,
      ),
    );
    for (const trigger of triggers) {
      // A retry leaves rows already written alone. Re-arming would enable a
      // row that the user disabled after a partial migration.
      if (existing.has(trigger.condition)) continue;
      await client.createCodeTrigger(repoId, trigger.condition, trigger.action);
      existing.add(trigger.condition);
    }
  }
}

/** Delay until the next monitor pass. `null` means there is no clock. */
export function nextMonitorDelayMs(args: {
  rerunRequested: boolean;
}): number | null {
  if (args.rerunRequested) return 0;
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
        kinds: ["workflow_run"],
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

import type { ApiClient } from "../../api/client";
import type {
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
  CodeDeliverySourceError,
  CodeGitHubRepositoryTarget,
} from "../../api/types";
import {
  type CodeDeliveryPrViewFilters,
  deliveryAuthorSightings,
  rememberedPullRequestPage,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
import { dedupeRows, pullRequestMatchesTarget } from "./helpers";
import { friendlyErrorMessage } from "@/lib/utils";
import { preservePullRequestStackMetadata } from "../pullRequestStacks";
import { useCodeUpdatesStore } from "../CodeUpdatesStore";
import { useEffect, useRef, useState } from "react";

/** The list query's handle on the detail pane's target state. */
export type PullRequestTargetDetailSink = {
  beginTargetDetail: (key: string) => void;
  adoptTargetDetail: (
    key: string,
    detail: CodeDeliveryPullRequestDetail,
  ) => void;
  failTargetDetail: (key: string) => void;
};

/**
 * The pull-request list: its rows, its paging, and the summaries it adopts
 * from the detail pane.
 *
 * The rows come from one aggregate query across the selected repositories.
 * A page key (repositories plus filters) remembers the last answer in the
 * delivery store so a return to the same view paints at once, then the query
 * reruns behind it. The server nudges `delivery` whenever its pull-request
 * store moves (decision 66); this list is a projection of that nudge, not a
 * clock of its own.
 *
 * Adopted summaries are rows the detail pane has read more recently than the
 * list. They overlay the list's rows until the next forced refresh, so a
 * merge the pane just saw does not flicker back to "open" on the next nudge.
 */
export function usePullRequestQuery({
  client,
  selectedRepositories,
  filters,
  target,
  targetKey,
  pageKey,
  detail,
}: {
  client: ApiClient;
  selectedRepositories: CodeGitHubRepositoryTarget[];
  filters: CodeDeliveryPrViewFilters;
  target: CodeDeliveryPullRequestTarget | undefined;
  targetKey: string | null;
  pageKey: string;
  detail: PullRequestTargetDetailSink;
}) {
  const [items, setItems] = useState<CodeDeliveryPullRequestSummary[]>([]);
  const [errors, setErrors] = useState<CodeDeliverySourceError[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [fetchedAt, setFetchedAt] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const deliveryRevision = useCodeUpdatesStore(
    (state) => state.deliveryRevision,
  );
  // Set by Refresh and by a completed action; consumed by the next query that
  // actually runs. Only those two reach past the server's short list cache —
  // a filter change reruns against it, which is the whole point of caching a
  // cross-repository read.
  const forceRefresh = useRef(false);
  const generation = useRef(0);
  const skipQueryDebounce = useRef(true);
  const adoptedSummaries = useRef(
    new Map<string, CodeDeliveryPullRequestSummary>(),
  );

  useEffect(() => {
    adoptedSummaries.current.clear();
    const remembered = rememberedPullRequestPage(
      useCodeDeliveryStore.getState().lastPullRequestPages,
      pageKey,
    );
    setItems(remembered?.items ?? []);
    setErrors(remembered?.errors ?? []);
    setNextCursor(remembered?.nextCursor);
    setFetchedAt(remembered?.fetchedAt ?? null);
    setLoading(true);
  }, [pageKey]);

  const refreshList = () => {
    forceRefresh.current = true;
    setRevision((value) => value + 1);
  };

  const applyAdopted = (rows: CodeDeliveryPullRequestSummary[]) =>
    rows.map((item) => {
      const adopted = adoptedSummaries.current.get(item.id);
      if (!adopted) return item;
      const merged = preservePullRequestStackMetadata(item, adopted);
      adoptedSummaries.current.set(item.id, merged);
      return merged;
    });

  const adoptSummary = (summary: CodeDeliveryPullRequestSummary) => {
    const previous = items.find((item) => item.id === summary.id);
    const adopted = previous
      ? preservePullRequestStackMetadata(previous, summary)
      : summary;
    adoptedSummaries.current.set(adopted.id, adopted);
    setItems((current) =>
      current.map((item) =>
        item.id === adopted.id
          ? adopted
          : (adoptedSummaries.current.get(item.id) ?? item),
      ),
    );

    const cached = useCodeDeliveryStore
      .getState()
      .lastPullRequestPages.find((page) => page.key === pageKey);
    if (cached) {
      useCodeDeliveryStore.getState().rememberPullRequestPage({
        ...cached,
        items: cached.items.map((item) =>
          item.id === adopted.id
            ? adopted
            : (adoptedSummaries.current.get(item.id) ?? item),
        ),
      });
    }
  };

  const query = async (cursor?: string, append = false) => {
    const token = ++generation.current;
    // Paging never rereads: renumbering the aggregate under a cursor would
    // skip or repeat rows.
    const refresh = !append && !cursor && forceRefresh.current;
    if (refresh) {
      forceRefresh.current = false;
      adoptedSummaries.current.clear();
    }
    if (append) setLoadingMore(true);
    else setLoading(true);
    setError(null);
    try {
      if (selectedRepositories.length === 0) {
        setNextCursor(undefined);
        setErrors([]);
        return;
      }
      let detailRequest: Promise<{
        detail?: CodeDeliveryPullRequestDetail;
        detailError?: unknown;
      }> = Promise.resolve({});
      if (!append && target && targetKey) {
        const requestKey = targetKey;
        detail.beginTargetDetail(requestKey);
        detailRequest = client
          .getCodeDeliveryPullRequestDetail(target)
          .then((loaded) => {
            if (token === generation.current) {
              const previous = items.find(
                (item) => item.id === loaded.summary.id,
              );
              const summary = previous
                ? preservePullRequestStackMetadata(previous, loaded.summary)
                : loaded.summary;
              const adoptedDetail =
                summary === loaded.summary ? loaded : { ...loaded, summary };
              detail.adoptTargetDetail(requestKey, adoptedDetail);
              adoptedSummaries.current.set(summary.id, summary);
              setItems((current) =>
                applyAdopted(dedupeRows([summary, ...current])),
              );
            }
            return { detail: loaded };
          })
          .catch((detailError: unknown) => {
            if (token === generation.current) {
              detail.failTargetDetail(requestKey);
            }
            return { detailError };
          });
      }
      const page = await client.queryCodeDeliveryPullRequests({
        repositories: selectedRepositories,
        search: filters.search.trim() || undefined,
        states: filters.states,
        review_states: filters.reviewStates,
        check_states: filters.checkStates,
        authors: filters.authors,
        attention_only: filters.attentionOnly,
        ready_only: filters.readyOnly,
        tidebreak_linked: filters.tidebreakLinked,
        limit: 100,
        refresh,
        ...(cursor ? { cursor } : {}),
      });
      if (token !== generation.current) return;
      useCodeDeliveryStore
        .getState()
        .rememberDeliveryAuthors(deliveryAuthorSightings(page.items, []));
      let nextItems: CodeDeliveryPullRequestSummary[] = [];
      setItems((current) => {
        nextItems = append ? [...current, ...page.items] : page.items;
        const exactItem = target
          ? current.find((item) => pullRequestMatchesTarget(item, target))
          : undefined;
        if (exactItem) nextItems = [exactItem, ...nextItems];
        nextItems = applyAdopted(dedupeRows(nextItems));
        return nextItems;
      });
      useCodeDeliveryStore.getState().rememberPullRequestPage({
        key: pageKey,
        items: nextItems,
        fetchedAt: page.fetched_at,
        nextCursor: page.next_cursor,
        errors: page.errors,
      });
      setNextCursor(page.next_cursor);
      setErrors(page.errors);
      setFetchedAt(page.fetched_at);
      void detailRequest.then((targetResult) => {
        if (
          token === generation.current &&
          target &&
          targetResult.detailError
        ) {
          setError(
            friendlyErrorMessage(
              targetResult.detailError,
              "Could not load this pull request.",
            ),
          );
        }
      });
    } catch (caught) {
      if (token !== generation.current) return;
      setError(friendlyErrorMessage(caught, "Could not load pull requests."));
      if (!append) {
        setItems((current) =>
          target
            ? current.filter((item) => pullRequestMatchesTarget(item, target))
            : [],
        );
      }
    } finally {
      if (token === generation.current) {
        setLoading(false);
        setLoadingMore(false);
      }
    }
  };

  useEffect(() => {
    const delay = skipQueryDebounce.current ? 0 : 180;
    skipQueryDebounce.current = false;
    const timer = window.setTimeout(() => void query(), delay);
    return () => {
      window.clearTimeout(timer);
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    client,
    selectedRepositories,
    filters,
    revision,
    deliveryRevision,
    targetKey,
  ]);

  return {
    items,
    errors,
    loading,
    loadingMore,
    nextCursor,
    error,
    fetchedAt,
    query,
    refreshList,
    adoptSummary,
  };
}

import { Button } from "@/components/ui/button";
import {
  type CodeDeliveryPrViewFilters,
  codeDeliveryRepositoryKey,
  codeDeliveryRepositoryTarget,
  deliveryAuthorSightings,
  deliveryPullRequestPageKey,
  rememberedPullRequestPage,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
import type {
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
  CodeDeliverySourceError,
  CodeGitHubCapability,
  CodeGitHubRepositoryRef,
} from "../../api/types";
import {
  DeliveryListSkeleton,
  FreshnessBar,
  GitHubUnavailable,
  InlineLoadError,
  NoDeliveryRepositories,
  PartialErrorBanner,
} from "./status";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { GitPullRequest, LoaderCircle } from "lucide-react";
import { PendingDetailPane } from "./PendingDetail";
import { PullRequestDetailPane } from "../PullRequestDetail";
import type { PullRequestGrouping } from "./views";
import { PullRequestList } from "./PullRequestList";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import {
  dedupeRows,
  isEditableKeyboardTarget,
  pullRequestIdsInDisplayOrder,
  pullRequestMatchesTarget,
  selectedRepositoryTargets,
} from "./helpers";
import {
  deliveryPullRequestDigest,
  deliveryRepositoryHasMergeQueue,
  prDirectMergeAction,
} from "../prActions";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  isStackedPullRequest,
  preservePullRequestStackMetadata,
} from "../pullRequestStacks";
import { toast } from "sonner";
import { useApp } from "@/AppContext";
import { useCodeUpdatesStore } from "../CodeUpdatesStore";
import { useConfirm } from "@/components/ConfirmDialog";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

export type TargetDetailState<T> = {
  key: string;
  pending: boolean;
  detail: T | null;
};

const PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS = 140;

const MAX_PULL_REQUEST_DETAIL_CACHE = 24;

export function PullRequestsSurface({
  repositories,
  capability,
  loadingRepositories,
  repositoryLoaded,
  repositoryError,
  onRetryRepositories,
  filters,
  grouping,
  target,
}: {
  repositories: CodeGitHubRepositoryRef[];
  capability: CodeGitHubCapability | null;
  loadingRepositories: boolean;
  repositoryLoaded: boolean;
  repositoryError: string | null;
  onRetryRepositories: () => void;
  filters: CodeDeliveryPrViewFilters;
  grouping: PullRequestGrouping;
  target?: CodeDeliveryPullRequestTarget;
}) {
  const { client } = useApp();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const [busyId, setBusyId] = useState<string | null>(null);
  const [items, setItems] = useState<CodeDeliveryPullRequestSummary[]>([]);
  const [errors, setErrors] = useState<CodeDeliverySourceError[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [fetchedAt, setFetchedAt] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detailLoadDelayMs, setDetailLoadDelayMs] = useState(0);
  const [targetDetailState, setTargetDetailState] =
    useState<TargetDetailState<CodeDeliveryPullRequestDetail> | null>(null);
  const [revision, setRevision] = useState(0);
  // The server nudges `delivery` whenever the pull-request store moves
  // (decision 66). This list is a projection of that nudge, not a clock of
  // its own: a fix turn, a watch, or another window's action reaches the
  // page through the broadcast rather than waiting for a Refresh.
  const deliveryRevision = useCodeUpdatesStore(
    (state) => state.deliveryRevision,
  );
  // Set by Refresh and by a completed action; consumed by the next query that
  // actually runs. Only those two reach past the server's short list cache —
  // a filter change reruns against it, which is the whole point of caching a
  // cross-repository read.
  const forceRefresh = useRef(false);
  const generation = useRef(0);
  const routeSelectionFenced = useRef(false);
  const skipQueryDebounce = useRef(true);
  const adoptedSummaries = useRef(
    new Map<string, CodeDeliveryPullRequestSummary>(),
  );
  const detailCache = useRef(new Map<string, CodeDeliveryPullRequestDetail>());
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? null,
    [items, selectedId],
  );
  const selectedRepositories = useMemo(
    () =>
      selectedRepositoryTargets(
        repositories,
        filters.repositoryKeys,
        target?.repository,
      ),
    [repositories, filters.repositoryKeys, target?.repository],
  );
  const targetKey = target
    ? `${codeDeliveryRepositoryKey(target.repository)}:pull-request:${target.number}`
    : null;
  const pageKey = deliveryPullRequestPageKey(
    selectedRepositories.map(codeDeliveryRepositoryKey),
    filters,
  );
  const displayIds = useMemo(
    () => pullRequestIdsInDisplayOrder(items, grouping),
    [grouping, items],
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

  const runListMerge = async (item: CodeDeliveryPullRequestSummary) => {
    const action = prDirectMergeAction(deliveryPullRequestDigest(item), {
      hasMergeQueue: deliveryRepositoryHasMergeQueue(items, item.repository),
      suppressAutoMerge: isStackedPullRequest(item),
    });
    if (!action || !item.head_sha || busyId) return;
    if (action.kind === "merge") {
      // A layer of an unregistered stack merges into the branch below it,
      // not the default branch — the exact accident the stack exists to
      // prevent. Name the target and point at the registration offer.
      const unregisteredStack = item.unregistered_stack_numbers !== undefined;
      const description = unregisteredStack
        ? `The pull request is squash-merged into ${item.base_branch} — not ${
            item.repository.default_branch ?? "the default branch"
          } — because this stack is not registered on GitHub. Open the pull request and create the stack to land the whole chain instead.`
        : `The pull request is squash-merged into ${item.base_branch} on GitHub.`;
      const ok = await confirm({
        title: `Merge #${item.number}?`,
        description,
        confirmLabel: "Merge",
      });
      if (!ok) return;
    }
    setBusyId(item.id);
    try {
      const result = await client.runCodeDeliveryPullRequestAction({
        target: {
          repository: codeDeliveryRepositoryTarget(item.repository),
          number: item.number,
        },
        action: {
          type: "merge",
          method: "squash",
          auto: action.auto,
          admin: false,
          expected_head_sha: item.head_sha,
        },
      });
      if (result.success) toast.success(result.message);
      else toast.warning(result.message);
      refreshList();
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(caught, "The pull request action failed."),
      );
    } finally {
      setBusyId(null);
    }
  };

  useEffect(() => {
    routeSelectionFenced.current = false;
    setSelectedId(null);
    setDetailLoadDelayMs(0);
    setTargetDetailState(
      targetKey ? { key: targetKey, pending: true, detail: null } : null,
    );
  }, [targetKey]);

  const selectItem = (id: string, debounceDetail = false) => {
    routeSelectionFenced.current = true;
    setDetailLoadDelayMs(
      debounceDetail ? PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS : 0,
    );
    setSelectedId(id);
  };

  const closeDetail = () => {
    routeSelectionFenced.current = true;
    setDetailLoadDelayMs(0);
    setSelectedId(null);
  };

  const rememberDetail = (detail: CodeDeliveryPullRequestDetail) => {
    detailCache.current.delete(detail.summary.id);
    detailCache.current.set(detail.summary.id, detail);
    if (detailCache.current.size > MAX_PULL_REQUEST_DETAIL_CACHE) {
      const oldest = detailCache.current.keys().next().value;
      if (oldest) detailCache.current.delete(oldest);
    }
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
        setTargetDetailState((current) => ({
          key: requestKey,
          pending: true,
          detail: current?.key === requestKey ? current.detail : null,
        }));
        detailRequest = client
          .getCodeDeliveryPullRequestDetail(target)
          .then((detail) => {
            if (token === generation.current) {
              const previous = items.find(
                (item) => item.id === detail.summary.id,
              );
              const summary = previous
                ? preservePullRequestStackMetadata(previous, detail.summary)
                : detail.summary;
              const adoptedDetail =
                summary === detail.summary ? detail : { ...detail, summary };
              rememberDetail(adoptedDetail);
              setTargetDetailState({
                key: requestKey,
                pending: false,
                detail: adoptedDetail,
              });
              adoptedSummaries.current.set(summary.id, summary);
              setItems((current) =>
                applyAdopted(dedupeRows([summary, ...current])),
              );
              if (!routeSelectionFenced.current) {
                setSelectedId(summary.id);
              }
            }
            return { detail };
          })
          .catch((detailError: unknown) => {
            if (token === generation.current) {
              setTargetDetailState((current) =>
                current?.key === requestKey
                  ? { ...current, pending: false }
                  : current,
              );
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

  useEffect(() => {
    if (!selectedId) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        event.altKey ||
        event.metaKey ||
        event.ctrlKey
      ) {
        return;
      }
      if (isEditableKeyboardTarget(event.target)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        closeDetail();
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const index = displayIds.indexOf(selectedId);
      if (index < 0) return;
      const nextIndex = event.key === "ArrowDown" ? index + 1 : index - 1;
      if (nextIndex < 0) return;
      event.preventDefault();
      if (nextIndex >= displayIds.length) {
        if (nextCursor && !loadingMore) void query(nextCursor, true);
        return;
      }
      const nextId = displayIds[nextIndex];
      if (nextId) selectItem(nextId, true);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [displayIds, loadingMore, nextCursor, selectedId]);

  if (loadingRepositories) return <DeliveryListSkeleton />;
  if (!repositoryLoaded && repositoryError) {
    return (
      <InlineLoadError
        message={`Could not load GitHub repositories: ${repositoryError}`}
        onRetry={onRetryRepositories}
      />
    );
  }
  if (capability && (!capability.found || capability.authenticated === false)) {
    return <GitHubUnavailable capability={capability} />;
  }
  if (repositories.length === 0) return <NoDeliveryRepositories />;

  const pendingTargetDetail =
    selected &&
    target &&
    targetDetailState?.key === targetKey &&
    targetDetailState.pending &&
    !targetDetailState.detail &&
    pullRequestMatchesTarget(selected, target);
  const cachedDetail = selected
    ? detailCache.current.get(selected.id)
    : undefined;
  const initialDetail =
    selected && targetDetailState?.detail?.summary.id === selected.id
      ? targetDetailState.detail
      : cachedDetail &&
          selected &&
          cachedDetail.summary.updated_at >= selected.updated_at
        ? cachedDetail
        : undefined;

  const list = (
    <div
      ref={scrollRef}
      className="min-h-0 h-full min-w-0 flex-1 overflow-auto"
    >
      {error && (
        <InlineLoadError message={error} onRetry={() => void query()} />
      )}
      {errors.length > 0 && <PartialErrorBanner errors={errors} compact />}
      <FreshnessBar
        fetchedAt={fetchedAt}
        loading={loading}
        count={items.length}
        noun="pull request"
        onRefresh={refreshList}
      />
      {loading && items.length === 0 ? (
        <DeliveryListSkeleton />
      ) : items.length === 0 ? (
        <Empty className="min-h-72">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <GitPullRequest />
            </EmptyMedia>
            <EmptyTitle>No pull requests match</EmptyTitle>
            <EmptyDescription>
              Change the saved view, repositories, or filters above.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <>
          <PullRequestList
            items={items}
            grouping={grouping}
            selectedId={selectedId}
            busyId={busyId}
            onSelect={selectItem}
            onMerge={runListMerge}
            scrollRef={scrollRef}
          />
          {nextCursor && (
            <div className="flex justify-center border-t border-border-subtle p-4">
              <Button
                type="button"
                size="sm"
                variant="outline"
                disabled={loadingMore}
                onClick={() => void query(nextCursor, true)}
              >
                {loadingMore && <LoaderCircle className="animate-spin" />}
                Load more
              </Button>
            </div>
          )}
        </>
      )}
    </div>
  );

  const detail = pendingTargetDetail ? (
    <PendingDetailPane
      context={`${selected.repository.name_with_owner} #${selected.number}`}
      title={selected.title}
      closeLabel="Close pull request details"
      onClose={closeDetail}
    />
  ) : selected ? (
    <PullRequestDetailPane
      key={selected.id}
      client={client}
      summary={selected}
      loadDelayMs={detailLoadDelayMs}
      hasMergeQueue={deliveryRepositoryHasMergeQueue(
        items,
        selected.repository,
      )}
      initialDetail={initialDetail}
      onClose={closeDetail}
      onChanged={refreshList}
      onDetail={rememberDetail}
      onSummary={adoptSummary}
      onOpenWorkspace={(workspaceId) =>
        void navigate({
          to: "/code/w/$workspaceId",
          params: { workspaceId },
        })
      }
    />
  ) : null;

  return (
    <div className="flex min-h-0 flex-1">
      {dialog}
      {detail ? (
        <ResizablePanelGroup
          key="with-detail"
          orientation="horizontal"
          className="min-h-0 min-w-0 flex-1"
        >
          <ResizablePanel defaultSize={50} minSize={28} className="min-h-0">
            {list}
          </ResizablePanel>
          <ResizableHandle />
          <ResizablePanel defaultSize={50} minSize={28} className="min-h-0">
            {detail}
          </ResizablePanel>
        </ResizablePanelGroup>
      ) : (
        list
      )}
    </div>
  );
}

import { Button } from "@/components/ui/button";
import type {
  CodeDeliveryRunDetail,
  CodeDeliveryRunSummary,
  CodeDeliveryRunTarget,
  CodeDeliverySourceError,
  CodeGitHubCapability,
  CodeGitHubRepositoryRef,
} from "../../api/types";
import {
  codeDeliveryRepositoryKey,
  type CodeDeliveryRunViewFilters,
  deliveryAuthorSightings,
  useCodeDeliveryStore,
} from "../CodeDeliveryStore";
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
import { LoaderCircle, Workflow } from "lucide-react";
import { PendingDetailSheet } from "./PendingDetail";
import { RunDetailSheet } from "./RunDetailSheet";
import { RunList } from "./RunList";
import type { TargetDetailState } from "./usePullRequestDetail";
import {
  dedupeRows,
  runMatchesTarget,
  selectedRepositoryTargets,
} from "./helpers";
import { friendlyErrorMessage } from "@/lib/utils";
import { useApp } from "@/AppContext";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

export function RunsSurface({
  repositories,
  capability,
  loadingRepositories,
  repositoryLoaded,
  repositoryError,
  onRetryRepositories,
  filters,
  target,
}: {
  repositories: CodeGitHubRepositoryRef[];
  capability: CodeGitHubCapability | null;
  loadingRepositories: boolean;
  repositoryLoaded: boolean;
  repositoryError: string | null;
  onRetryRepositories: () => void;
  filters: CodeDeliveryRunViewFilters;
  target?: CodeDeliveryRunTarget;
}) {
  const { client } = useApp();
  const navigate = useNavigate();
  const [items, setItems] = useState<CodeDeliveryRunSummary[]>([]);
  const [errors, setErrors] = useState<CodeDeliverySourceError[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [fetchedAt, setFetchedAt] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [targetDetailState, setTargetDetailState] =
    useState<TargetDetailState<CodeDeliveryRunDetail> | null>(null);
  const [revision, setRevision] = useState(0);
  // Set by Refresh and by a completed action; consumed by the next query that
  // actually runs. Only those two reach past the server's short list cache —
  // a filter change reruns against it, which is the whole point of caching a
  // cross-repository read.
  const forceRefresh = useRef(false);
  const generation = useRef(0);
  const routeSelectionFenced = useRef(false);
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
    ? `${codeDeliveryRepositoryKey(target.repository)}:${target.kind}:${target.id}`
    : null;

  useEffect(() => {
    routeSelectionFenced.current = false;
    setSelectedId(null);
    setTargetDetailState(
      targetKey ? { key: targetKey, pending: true, detail: null } : null,
    );
  }, [targetKey]);

  const selectItem = (id: string) => {
    routeSelectionFenced.current = true;
    setSelectedId(id);
  };

  const closeDetail = () => {
    routeSelectionFenced.current = true;
    setSelectedId(null);
  };

  const query = async (cursor?: string, append = false) => {
    const token = ++generation.current;
    // Paging never rereads: renumbering the aggregate under a cursor would
    // skip or repeat rows.
    const refresh = !append && !cursor && forceRefresh.current;
    if (refresh) forceRefresh.current = false;
    if (append) setLoadingMore(true);
    else setLoading(true);
    setError(null);
    try {
      if (selectedRepositories.length === 0) {
        if (!append) setItems([]);
        setNextCursor(undefined);
        setErrors([]);
        return;
      }
      let detailRequest: Promise<{
        detail?: CodeDeliveryRunDetail;
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
          .getCodeDeliveryRunDetail(target)
          .then((detail) => {
            if (token === generation.current) {
              setTargetDetailState({
                key: requestKey,
                pending: false,
                detail,
              });
              setItems((current) => dedupeRows([detail.summary, ...current]));
              if (!routeSelectionFenced.current) {
                setSelectedId(detail.summary.id);
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
      const page = await client.queryCodeDeliveryRuns({
        repositories: selectedRepositories,
        search: filters.search.trim() || undefined,
        kinds: filters.kinds,
        statuses: filters.statuses,
        conclusions: filters.conclusions,
        workflows: filters.workflows,
        environments: filters.environments,
        branches: filters.branches,
        events: filters.events,
        actors: filters.actors,
        attention_only: filters.attentionOnly,
        tidebreak_linked: filters.tidebreakLinked,
        limit: 100,
        refresh,
        ...(cursor ? { cursor } : {}),
      });
      if (token !== generation.current) return;
      useCodeDeliveryStore
        .getState()
        .rememberDeliveryAuthors(deliveryAuthorSightings([], page.items));
      setItems((current) => {
        let nextItems = append ? [...current, ...page.items] : page.items;
        const exactItem = target
          ? current.find((item) => runMatchesTarget(item, target))
          : undefined;
        if (exactItem) nextItems = [exactItem, ...nextItems];
        return dedupeRows(nextItems);
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
              "Could not load this run.",
            ),
          );
        }
      });
    } catch (caught) {
      if (token !== generation.current) return;
      setError(
        friendlyErrorMessage(caught, "Could not load runs and deployments."),
      );
      if (!append) {
        setItems((current) =>
          target
            ? current.filter((item) => runMatchesTarget(item, target))
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
    const timer = window.setTimeout(() => void query(), 180);
    return () => {
      window.clearTimeout(timer);
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, selectedRepositories, filters, revision, targetKey]);

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

  return (
    <div className="flex min-h-0 flex-1">
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
        {error && (
          <InlineLoadError message={error} onRetry={() => void query()} />
        )}
        {errors.length > 0 && <PartialErrorBanner errors={errors} compact />}
        <FreshnessBar
          fetchedAt={fetchedAt}
          loading={loading}
          count={items.length}
          noun="run"
          onRefresh={() => {
            forceRefresh.current = true;
            setRevision((value) => value + 1);
          }}
        />
        {loading && items.length === 0 ? (
          <DeliveryListSkeleton />
        ) : items.length === 0 ? (
          <Empty className="min-h-72">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Workflow />
              </EmptyMedia>
              <EmptyTitle>No runs match</EmptyTitle>
              <EmptyDescription>
                Change the saved view, repositories, or filters above.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <>
            <RunList
              items={items}
              selectedId={selectedId}
              onSelect={selectItem}
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
      {selected &&
      target &&
      targetDetailState?.key === targetKey &&
      targetDetailState.pending &&
      !targetDetailState.detail &&
      runMatchesTarget(selected, target) ? (
        <PendingDetailSheet
          context={`${selected.repository.name_with_owner} ${
            selected.kind === "deployment" ? "Deployment" : "Action"
          }`}
          title={selected.name}
          closeLabel="Close run details"
          onClose={closeDetail}
        />
      ) : selected ? (
        <RunDetailSheet
          key={selected.id}
          summary={selected}
          initialDetail={
            targetDetailState?.detail?.summary.id === selected.id
              ? targetDetailState.detail
              : undefined
          }
          onClose={closeDetail}
          onChanged={() => {
            forceRefresh.current = true;
            setRevision((value) => value + 1);
          }}
          onOpenWorkspace={(workspaceId) =>
            void navigate({
              to: "/code/w/$workspaceId",
              params: { workspaceId },
            })
          }
        />
      ) : null}
    </div>
  );
}

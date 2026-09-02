import { Button } from "@/components/ui/button";
import {
  type CodeDeliveryPrViewFilters,
  codeDeliveryRepositoryKey,
  codeDeliveryRepositoryTarget,
  deliveryPullRequestPageKey,
} from "../CodeDeliveryStore";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
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
  pullRequestIdsInDisplayOrder,
  selectedRepositoryTargets,
} from "./helpers";
import {
  deliveryPullRequestDigest,
  deliveryRepositoryHasMergeQueue,
  prDirectMergeAction,
} from "../prActions";
import { friendlyErrorMessage } from "@/lib/utils";
import { isStackedPullRequest } from "../pullRequestStacks";
import { toast } from "sonner";
import { useApp } from "@/AppContext";
import { useConfirm } from "@/components/ConfirmDialog";
import { useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { usePullRequestDetail } from "./usePullRequestDetail";
import { usePullRequestKeyboardNav } from "./usePullRequestKeyboardNav";
import { usePullRequestQuery } from "./usePullRequestQuery";

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
  const scrollRef = useRef<HTMLDivElement | null>(null);

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

  const detail = usePullRequestDetail(targetKey);
  const {
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
  } = usePullRequestQuery({
    client,
    selectedRepositories,
    filters,
    target,
    targetKey,
    pageKey,
    detail,
  });
  const { selectedId, detailLoadDelayMs, selectItem, closeDetail } = detail;

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? null,
    [items, selectedId],
  );
  const displayIds = useMemo(
    () => pullRequestIdsInDisplayOrder(items, grouping),
    [grouping, items],
  );

  usePullRequestKeyboardNav({
    selectedId,
    displayIds,
    nextCursor,
    loadingMore,
    onSelect: (id) => selectItem(id, true),
    onClose: closeDetail,
    onLoadMore: (cursor) => void query(cursor, true),
  });

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

  const pendingTargetDetail = detail.pendingTargetDetail(selected, target);
  const initialDetail = detail.initialDetail(selected);

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

  const detailPane =
    pendingTargetDetail && selected ? (
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
        onDetail={detail.rememberDetail}
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
      {detailPane ? (
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
            {detailPane}
          </ResizablePanel>
        </ResizablePanelGroup>
      ) : (
        list
      )}
    </div>
  );
}

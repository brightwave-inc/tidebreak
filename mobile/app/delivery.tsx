import {
  useInfiniteQuery,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useIsFocused, useRouter } from "expo-router";
import * as WebBrowser from "expo-web-browser";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  FlatList,
  type ListRenderItemInfo,
  Pressable,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Button, LoadingState, StatusPill } from "../src/components/Controls";
import { Body, ErrorText } from "../src/components/Screen";
import {
  groupMobileDeliveryPullRequests,
  listMobileDeliveryRepositories,
  mobileDeliveryCheckProgress,
  mobileDeliveryLaneCountLabel,
  mobileDeliveryLaneIsConfirmedEmpty,
  mobileDeliveryRepositoryTarget,
  queryMobileDeliveryPullRequests,
  uniqueMobileDeliveryPullRequests,
  uniqueMobileDeliverySourceErrors,
  type MobileDeliveryCapability,
  type MobileDeliveryLane,
  type MobileDeliveryPullRequest,
  type MobileDeliverySourceError,
} from "../src/lib/deliveryApi";
import { useSessionStore } from "../src/session/store";
import { useMachineClient } from "../src/session/useMachineClient";

const LANE_META: Record<
  MobileDeliveryLane,
  {
    title: string;
    description: string;
    tone: "critical" | "success" | "info";
  }
> = {
  attention: {
    title: "Needs attention",
    description: "Failed checks, requested changes, conflicts, or stale branches.",
    tone: "critical",
  },
  ready: {
    title: "Ready to merge",
    description: "Green and waiting for you.",
    tone: "success",
  },
  in_progress: {
    title: "In progress",
    description: "Checks or reviews are still moving.",
    tone: "info",
  },
};

const LANE_ORDER: readonly MobileDeliveryLane[] = [
  "attention",
  "ready",
  "in_progress",
];

type DeliveryListItem =
  | {
      id: string;
      kind: "lane";
      lane: MobileDeliveryLane;
      count: number;
      hasNextPage: boolean;
    }
  | { id: string; kind: "empty"; lane: MobileDeliveryLane }
  | {
      id: string;
      kind: "pull_request";
      pullRequest: MobileDeliveryPullRequest;
    };

export default function DeliveryScreen() {
  const router = useRouter();
  const isFocused = useIsFocused();
  const queryClient = useQueryClient();
  const machine = useSessionStore((state) => state.session?.machine);
  const client = useMachineClient();
  const repositoryRefreshRef = useRef(false);
  const pullRequestRefreshRef = useRef(false);
  const [refreshing, setRefreshing] = useState(false);
  const [linkError, setLinkError] = useState<string | null>(null);

  const repositoriesQuery = useQuery({
    queryKey: ["mobile-delivery-repositories", machine?.baseUrl],
    enabled: !!client && isFocused,
    queryFn: async ({ signal }) => {
      const refresh = repositoryRefreshRef.current;
      const snapshot = await listMobileDeliveryRepositories(client!, {
        refresh,
        signal,
      });
      if (refresh) repositoryRefreshRef.current = false;
      return snapshot;
    },
  });
  const repositorySnapshot = repositoriesQuery.data;
  const repositories = repositorySnapshot?.repositories ?? [];
  const repositoryTargets = useMemo(
    () => repositories.map(mobileDeliveryRepositoryTarget),
    [repositories],
  );
  const repositoryKey = repositoryTargets
    .map((repository) =>
      [repository.host, repository.owner, repository.name].join("/"),
    )
    .join("|");
  const repositoriesAvailable =
    repositorySnapshot?.capability.found === true &&
    repositorySnapshot.capability.authenticated !== false &&
    repositoryTargets.length > 0;
  const pullRequestsQueryKey = [
    "mobile-delivery-pull-requests",
    machine?.baseUrl,
    repositoryKey,
  ] as const;

  const pullRequestsQuery = useInfiniteQuery({
    queryKey: pullRequestsQueryKey,
    enabled: !!client && isFocused && repositoriesAvailable,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam, signal }) => {
      const refresh = pullRequestRefreshRef.current && pageParam === null;
      const page = await queryMobileDeliveryPullRequests(client!, {
        repositories: repositoryTargets,
        ...(pageParam ? { cursor: pageParam } : {}),
        refresh,
        signal,
      });
      if (refresh) pullRequestRefreshRef.current = false;
      return page;
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? null,
  });

  const pages = pullRequestsQuery.data?.pages ?? [];
  const pullRequests = useMemo(
    () =>
      uniqueMobileDeliveryPullRequests(
        pages.flatMap((page) => page.items),
      ),
    [pages],
  );
  const lanes = useMemo(
    () => groupMobileDeliveryPullRequests(pullRequests),
    [pullRequests],
  );
  const listItems = useMemo<DeliveryListItem[]>(() => {
    if (pullRequests.length === 0) return [];
    const hasNextPage = pullRequestsQuery.hasNextPage === true;
    return LANE_ORDER.flatMap((lane) => {
      const rows: DeliveryListItem[] = [
        {
          id: `lane:${lane}`,
          kind: "lane",
          lane,
          count: lanes[lane].length,
          hasNextPage,
        },
      ];
      if (mobileDeliveryLaneIsConfirmedEmpty(lanes[lane], hasNextPage)) {
        rows.push({ id: `empty:${lane}`, kind: "empty", lane });
      } else {
        rows.push(
          ...lanes[lane].map((pullRequest) => ({
            id: `pull-request:${pullRequest.id}`,
            kind: "pull_request" as const,
            pullRequest,
          })),
        );
      }
      return rows;
    });
  }, [lanes, pullRequests.length, pullRequestsQuery.hasNextPage]);
  const sourceErrors = useMemo(
    () =>
      uniqueMobileDeliverySourceErrors([
        ...(repositorySnapshot?.errors ?? []),
        ...pages.flatMap((page) => page.errors),
      ]),
    [pages, repositorySnapshot?.errors],
  );
  const latestPage = pages[pages.length - 1];
  const capability = latestPage?.capability ?? repositorySnapshot?.capability;
  const fetchedAt = latestPage?.fetched_at ?? repositorySnapshot?.fetched_at;

  useEffect(() => {
    if (isFocused) return;
    void queryClient.cancelQueries({
      queryKey: ["mobile-delivery-repositories", machine?.baseUrl],
    });
    void queryClient.cancelQueries({
      queryKey: ["mobile-delivery-pull-requests", machine?.baseUrl],
    });
  }, [isFocused, machine?.baseUrl, queryClient]);

  async function refresh() {
    if (!client || !isFocused || refreshing) return;
    setRefreshing(true);
    setLinkError(null);
    repositoryRefreshRef.current = true;
    pullRequestRefreshRef.current = true;
    try {
      const result = await repositoriesQuery.refetch();
      const refreshedRepositories = result.data?.repositories ?? [];
      const refreshedRepositoryTargets = refreshedRepositories.map(
        mobileDeliveryRepositoryTarget,
      );
      const refreshedRepositoriesAvailable =
        result.data?.capability.found === true &&
        result.data.capability.authenticated !== false &&
        refreshedRepositoryTargets.length > 0;
      if (refreshedRepositoriesAvailable) {
        const refreshedRepositoryKey = refreshedRepositoryTargets
          .map((repository) =>
            [repository.host, repository.owner, repository.name].join("/"),
          )
          .join("|");
        await queryClient.resetQueries({
          queryKey: [
            "mobile-delivery-pull-requests",
            machine?.baseUrl,
            refreshedRepositoryKey,
          ],
          exact: true,
        });
      }
    } finally {
      setRefreshing(false);
    }
  }

  function loadNextPage() {
    if (
      isFocused &&
      pullRequestsQuery.hasNextPage &&
      !pullRequestsQuery.isFetchingNextPage
    ) {
      void pullRequestsQuery.fetchNextPage();
    }
  }

  function renderItem({
    item,
    index,
  }: ListRenderItemInfo<DeliveryListItem>) {
    if (item.kind === "lane") {
      const meta = LANE_META[item.lane];
      return (
        <View className={index === 0 ? "gap-1 pb-2.5" : "mt-5 gap-1 pb-2.5"}>
          <View className="flex-row items-center justify-between gap-3">
            <Text className="text-lg font-semibold text-foreground">
              {meta.title}
            </Text>
            <StatusPill tone={meta.tone}>
              {mobileDeliveryLaneCountLabel(
                item.count,
                item.hasNextPage,
              )}
            </StatusPill>
          </View>
          <Text className="text-xs text-muted-foreground">
            {meta.description}
          </Text>
        </View>
      );
    }
    if (item.kind === "empty") {
      return (
        <Text className="mb-2.5 py-1 text-sm text-muted-foreground">
          Nothing in this lane.
        </Text>
      );
    }
    return (
      <View className="mb-2.5">
        <PullRequestRow
          pullRequest={item.pullRequest}
          onOpen={openPullRequest}
        />
      </View>
    );
  }

  async function openPullRequest(pullRequest: MobileDeliveryPullRequest) {
    setLinkError(null);
    try {
      await WebBrowser.openBrowserAsync(pullRequest.url);
    } catch (error) {
      setLinkError(
        error instanceof Error
          ? error.message
          : "The pull request could not be opened.",
      );
    }
  }

  if (!machine || !client) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <Text className="text-2xl font-semibold text-foreground">Delivery</Text>
        <View className="mt-3">
          <Body>Attach a Tidebreak machine to review pull requests.</Body>
        </View>
        <View className="mt-4">
          <Button label="Attach" onPress={() => router.replace("/attach")} />
        </View>
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <FlatList
        contentContainerClassName="px-5 py-6"
        data={listItems}
        keyExtractor={(item) => item.id}
        renderItem={renderItem}
        refreshing={refreshing}
        onRefresh={() => void refresh()}
        onEndReached={loadNextPage}
        onEndReachedThreshold={0.35}
        ListHeaderComponent={
          <View className="gap-4">
            <View className="gap-1">
              <View className="flex-row items-start justify-between gap-3">
                <Text className="flex-1 text-2xl font-semibold text-foreground">
                  Delivery
                </Text>
                {repositories.length > 0 ? (
                  <StatusPill>
                    {repositories.length} repo
                    {repositories.length === 1 ? "" : "s"}
                  </StatusPill>
                ) : null}
              </View>
              <Text className="text-sm text-muted-foreground">
                Review open pull requests across your tracked repositories.
              </Text>
              {fetchedAt ? (
                <Text className="text-xs text-muted-foreground">
                  Updated {formatDeliveryDate(fetchedAt)}
                </Text>
              ) : null}
            </View>

            {linkError ? <ErrorText>{linkError}</ErrorText> : null}
            {repositoriesQuery.isLoading ? (
              <LoadingState label="Loading repositories…" />
            ) : null}
            {repositoriesQuery.isError ? (
              <ErrorText>
                {repositoriesQuery.error instanceof Error
                  ? repositoriesQuery.error.message
                  : "Delivery repositories could not be loaded."}
              </ErrorText>
            ) : null}

            {capability && !deliveryCapabilityAvailable(capability) ? (
              <CapabilityState capability={capability} />
            ) : null}

            {repositorySnapshot &&
            deliveryCapabilityAvailable(repositorySnapshot.capability) &&
            repositories.length === 0 ? (
              <View className="rounded-xl border border-border bg-background p-4">
                <Text className="text-base font-medium text-foreground">
                  No GitHub repositories
                </Text>
                <Text className="mt-1 text-sm text-muted-foreground">
                  Add a GitHub-backed repository in Tidebreak, then pull to
                  refresh.
                </Text>
              </View>
            ) : null}

            {sourceErrors.length > 0 ? (
              <SourceErrors errors={sourceErrors} />
            ) : null}

            {repositoriesAvailable && pullRequestsQuery.isLoading ? (
              <LoadingState label="Loading pull requests…" />
            ) : null}
            {pullRequestsQuery.isError ? (
              <ErrorText>
                {pullRequestsQuery.error instanceof Error
                  ? pullRequestsQuery.error.message
                  : "Pull requests could not be loaded."}
              </ErrorText>
            ) : null}

            {repositoriesAvailable &&
            !pullRequestsQuery.isLoading &&
            !pullRequestsQuery.isError &&
            pullRequestsQuery.hasNextPage !== true &&
            pullRequests.length === 0 ? (
              <View className="rounded-xl border border-border bg-background p-4">
                <Text className="text-base font-medium text-foreground">
                  No open pull requests
                </Text>
                <Text className="mt-1 text-sm text-muted-foreground">
                  Open pull requests appear here when a tracked repository has
                  work in flight.
                </Text>
              </View>
            ) : null}

            {listItems.length > 0 ? <View className="h-1" /> : null}
          </View>
        }
        ListFooterComponent={
          pullRequestsQuery.isFetchingNextPage ? (
            <View className="flex-row items-center justify-center gap-2 py-4">
              <ActivityIndicator size="small" />
              <Text className="text-sm text-muted-foreground">
                Loading more…
              </Text>
            </View>
          ) : pullRequestsQuery.hasNextPage ? (
            <View className="pt-2">
              <Button
                label="Load more"
                variant="secondary"
                onPress={() => void pullRequestsQuery.fetchNextPage()}
              />
            </View>
          ) : null
        }
      />
    </SafeAreaView>
  );
}

function PullRequestRow({
  pullRequest,
  onOpen,
}: {
  pullRequest: MobileDeliveryPullRequest;
  onOpen: (pullRequest: MobileDeliveryPullRequest) => Promise<void>;
}) {
  const status = pullRequestStatus(pullRequest);
  const checks = pullRequestCheckStatus(pullRequest);
  return (
    <Pressable
      accessibilityRole="link"
      accessibilityLabel={`${pullRequest.repository.name_with_owner} pull request ${pullRequest.number}, ${pullRequest.title}. ${status.label}. ${checks.label}.`}
      className="gap-2 rounded-xl border border-border bg-background p-4"
      onPress={() => void onOpen(pullRequest)}
    >
      <View className="flex-row items-start justify-between gap-3">
        <Text
          className="flex-1 font-mono text-xs text-muted-foreground"
          numberOfLines={1}
        >
          {pullRequest.repository.name_with_owner} · #{pullRequest.number}
        </Text>
        <StatusPill tone={status.tone}>{status.label}</StatusPill>
      </View>
      <Text className="text-base font-medium text-foreground">
        {pullRequest.title}
      </Text>
      <Text
        className="font-mono text-xs text-muted-foreground"
        numberOfLines={1}
      >
        {pullRequest.head_branch} → {pullRequest.base_branch}
      </Text>
      <View className="flex-row flex-wrap items-center justify-between gap-2">
        <Text className={`text-xs font-medium ${checks.className}`}>
          {checks.label}
        </Text>
        <Text className="text-xs text-muted-foreground">
          {pullRequest.author ? `@${pullRequest.author} · ` : ""}
          {formatDeliveryDate(pullRequest.updated_at)}
        </Text>
      </View>
    </Pressable>
  );
}

function CapabilityState({
  capability,
}: {
  capability: MobileDeliveryCapability;
}) {
  const title = capability.found
    ? "GitHub needs sign-in"
    : "GitHub is unavailable";
  const fallback = capability.found
    ? "Sign in to GitHub on the attached machine, then pull to refresh."
    : "Install or connect GitHub on the attached machine, then pull to refresh.";
  return (
    <View className="rounded-xl border border-warning-border bg-warning-background p-4">
      <Text className="text-base font-medium text-warning-foreground">
        {title}
      </Text>
      <Text className="mt-1 text-sm text-warning-foreground">
        {capability.remediation || fallback}
      </Text>
    </View>
  );
}

function SourceErrors({ errors }: { errors: MobileDeliverySourceError[] }) {
  const visible = errors.slice(0, 3);
  return (
    <View className="rounded-xl border border-warning-border bg-warning-background p-4">
      <Text className="text-base font-medium text-warning-foreground">
        Some repositories could not be loaded
      </Text>
      <View className="mt-2 gap-2">
        {visible.map((error) => (
          <Text
            key={sourceErrorKey(error)}
            className="text-sm text-warning-foreground"
          >
            {sourceErrorRepository(error)}: {error.message}
          </Text>
        ))}
        {errors.length > visible.length ? (
          <Text className="text-xs text-warning-foreground">
            {errors.length - visible.length} more source error
            {errors.length - visible.length === 1 ? "" : "s"}
          </Text>
        ) : null}
      </View>
    </View>
  );
}

function deliveryCapabilityAvailable(
  capability: MobileDeliveryCapability,
): boolean {
  return capability.found && capability.authenticated !== false;
}

function pullRequestStatus(pullRequest: MobileDeliveryPullRequest): {
  label: string;
  tone: "neutral" | "info" | "warning" | "success" | "critical";
} {
  if (pullRequest.attention_reasons.includes("changes_requested")) {
    return { label: "Changes requested", tone: "critical" };
  }
  if (pullRequest.attention_reasons.includes("checks_failed")) {
    return { label: "Checks failed", tone: "critical" };
  }
  if (pullRequest.attention_reasons.includes("conflicts")) {
    return { label: "Conflicts", tone: "critical" };
  }
  if (pullRequest.attention_reasons.includes("behind")) {
    return { label: "Update branch", tone: "warning" };
  }
  if (pullRequest.attention_reasons.includes("blocked")) {
    return { label: "Blocked", tone: "warning" };
  }
  if (pullRequest.ready_to_merge) {
    return { label: "Ready to merge", tone: "success" };
  }
  if (pullRequest.draft) return { label: "Draft", tone: "neutral" };
  const reviewDecision = pullRequest.review_decision?.trim().toLowerCase();
  if (reviewDecision === "approved") {
    return { label: "Approved", tone: "success" };
  }
  if (reviewDecision === "review_required") {
    return { label: "Needs review", tone: "warning" };
  }
  return { label: "In progress", tone: "info" };
}

function pullRequestCheckStatus(pullRequest: MobileDeliveryPullRequest): {
  label: string;
  className: string;
} {
  const checks = mobileDeliveryCheckProgress(pullRequest.checks);
  if (checks.total === 0) {
    return { label: "No checks", className: "text-muted-foreground" };
  }
  const progress = `${checks.terminal}/${checks.total} checks`;
  if (checks.failing > 0) {
    return {
      label: `${progress} · ${checks.failing} failed`,
      className: "text-critical-foreground",
    };
  }
  if (checks.pending > 0) {
    return {
      label: `${progress} · ${checks.pending} running`,
      className: "text-info-foreground",
    };
  }
  if (checks.skipped === checks.total) {
    return {
      label: `${progress} · ${checks.skipped} skipped`,
      className: "text-muted-foreground",
    };
  }
  return { label: progress, className: "text-success-foreground" };
}

function sourceErrorRepository(error: MobileDeliverySourceError): string {
  if (!error.repository) return "GitHub";
  return `${error.repository.owner}/${error.repository.name}`;
}

function sourceErrorKey(error: MobileDeliverySourceError): string {
  return `${sourceErrorRepository(error)}:${error.kind}:${error.message}`;
}

function formatDeliveryDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ArchiveRestore,
  Check,
  CircleAlert,
  ExternalLink,
  Filter,
  GitBranch,
  GitPullRequest,
  LoaderCircle,
  Pin,
  PinOff,
  RefreshCw,
  Save,
  Settings2,
  Workflow,
  X,
} from "lucide-react";
import { toast } from "sonner";

import { useApp } from "@/AppContext";
import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import { RouteFrame } from "@/RouteFrame";
import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryRunDetail,
  CodeDeliveryRunKind,
  CodeDeliveryRunSummary,
  CodeDeliverySourceError,
  CodeGitHubCapability,
  CodeGitHubRepositoryRef,
} from "../api/types";
import {
  codeDeliveryRepositoryKey,
  codeDeliveryRepositoryTarget,
  trackedCodeDeliveryRepositories,
  useCodeDeliveryStore,
  type CodeDeliveryPrViewFilters,
  type CodeDeliveryRunViewFilters,
  type CodeDeliverySavedView,
  type CodeDeliverySurface,
} from "./CodeDeliveryStore";
import { CodeSidebar } from "./CodeSidebar";
import {
  CheckTone,
  DetailSkeleton,
  PrLifecycleIcon,
  PullRequestDetailPanel,
  relativeTime,
} from "./PullRequestDetail";
import {
  checkCounts,
  checkSummary,
  pullRequestLifecycle,
  pullRequestReviewSummary,
  pullRequestSettledAt,
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
} from "./pullRequestPresentation";
import { STATUS_MARK, STATUS_TEXT } from "./statusTone";

const PR_BUILT_IN_VIEWS: readonly {
  id: string;
  label: string;
  filters: CodeDeliveryPrViewFilters;
}[] = [
  {
    id: "attention",
    label: "Needs attention",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: true,
      readyOnly: false,
    },
  },
  {
    id: "ready",
    label: "Ready to merge",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: true,
    },
  },
  {
    id: "open",
    label: "Open",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
  },
  {
    id: "all",
    label: "All",
    filters: {
      search: "",
      repositoryKeys: [],
      states: [],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
  },
];

const RUN_BUILT_IN_VIEWS: readonly {
  id: string;
  label: string;
  filters: CodeDeliveryRunViewFilters;
}[] = [
  {
    id: "failures",
    label: "Needs attention",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: [],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: true,
    },
  },
  {
    id: "deployments",
    label: "Deployments",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: ["deployment"],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
  {
    id: "actions",
    label: "Actions",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: ["workflow_run"],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
  {
    id: "all",
    label: "All recent",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: [],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
];

export function CodeDeliveryPage({
  surface,
}: {
  surface: CodeDeliverySurface;
}) {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeDeliveryBody surface={surface} />
      </div>
    </RouteFrame>
  );
}

function CodeDeliveryBody({ surface }: { surface: CodeDeliverySurface }) {
  const { client } = useApp();
  const navigate = useNavigate();
  const manualRepositories = useCodeDeliveryStore(
    (state) => state.manualRepositories,
  );
  const excludedRegisteredRepoIds = useCodeDeliveryStore(
    (state) => state.excludedRegisteredRepoIds,
  );
  const pinnedRepositoryKeys = useCodeDeliveryStore(
    (state) => state.pinnedRepositoryKeys,
  );
  const savedViews = useCodeDeliveryStore((state) => state.savedViews);
  const [discovered, setDiscovered] = useState<CodeGitHubRepositoryRef[]>([]);
  const [capability, setCapability] = useState<CodeGitHubCapability | null>(
    null,
  );
  const [repositoryErrors, setRepositoryErrors] = useState<
    CodeDeliverySourceError[]
  >([]);
  const [repositoriesLoading, setRepositoriesLoading] = useState(true);
  const [repositoriesDialogOpen, setRepositoriesDialogOpen] = useState(false);
  const [saveDialogOpen, setSaveDialogOpen] = useState(false);
  const [activeViewId, setActiveViewId] = useState(
    surface === "pull_requests" ? "attention" : "failures",
  );
  const [prFilters, setPrFilters] = useState<CodeDeliveryPrViewFilters>(() =>
    clonePrFilters(PR_BUILT_IN_VIEWS[0]!.filters),
  );
  const [runFilters, setRunFilters] = useState<CodeDeliveryRunViewFilters>(() =>
    cloneRunFilters(RUN_BUILT_IN_VIEWS[0]!.filters),
  );

  useEffect(() => {
    setActiveViewId(surface === "pull_requests" ? "attention" : "failures");
    if (surface === "pull_requests") {
      setPrFilters(clonePrFilters(PR_BUILT_IN_VIEWS[0]!.filters));
    } else {
      setRunFilters(cloneRunFilters(RUN_BUILT_IN_VIEWS[0]!.filters));
    }
  }, [surface]);

  const loadRepositories = async () => {
    setRepositoriesLoading(true);
    try {
      const snapshot = await client.getCodeDeliveryRepositories();
      setDiscovered(snapshot.repositories);
      setCapability(snapshot.capability);
      setRepositoryErrors(snapshot.errors);
    } catch (error) {
      toast.error(
        friendlyErrorMessage(error, "Could not load GitHub repositories."),
      );
    } finally {
      setRepositoriesLoading(false);
    }
  };

  useEffect(() => {
    void loadRepositories();
  }, [client]);

  const repositories = useMemo(
    () =>
      trackedCodeDeliveryRepositories(discovered, {
        manualRepositories,
        excludedRegisteredRepoIds,
        pinnedRepositoryKeys,
      }),
    [
      discovered,
      manualRepositories,
      excludedRegisteredRepoIds,
      pinnedRepositoryKeys,
    ],
  );

  const customViews = savedViews.filter((view) => view.kind === surface);
  const filters = surface === "pull_requests" ? prFilters : runFilters;

  const applyBuiltInView = (id: string) => {
    setActiveViewId(id);
    if (surface === "pull_requests") {
      const view = PR_BUILT_IN_VIEWS.find((candidate) => candidate.id === id);
      if (view) setPrFilters(clonePrFilters(view.filters));
    } else {
      const view = RUN_BUILT_IN_VIEWS.find((candidate) => candidate.id === id);
      if (view) setRunFilters(cloneRunFilters(view.filters));
    }
  };

  const applySavedView = (view: CodeDeliverySavedView) => {
    setActiveViewId(view.id);
    if (view.kind === "pull_requests") {
      setPrFilters(clonePrFilters(view.filters));
    } else {
      setRunFilters(cloneRunFilters(view.filters));
    }
  };

  const markCustom = () => setActiveViewId("custom");
  const setSearch = (search: string) => {
    markCustom();
    if (surface === "pull_requests") {
      setPrFilters((current) => ({ ...current, search }));
    } else {
      setRunFilters((current) => ({ ...current, search }));
    }
  };

  return (
    <div className="flex size-full min-h-0 flex-col bg-background">
      <header className="shrink-0 border-b border-border-subtle px-5 pt-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">Delivery</h1>
              {!repositoriesLoading && (
                <span className="text-xs text-muted-foreground">
                  {repositories.length} tracked
                </span>
              )}
            </div>
            <p className="mt-0.5 text-sm text-muted-foreground">
              Pull requests, Actions, and deployments across your GitHub repos.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => setRepositoriesDialogOpen(true)}
            >
              <Settings2 />
              Repositories
            </Button>
          </div>
        </div>
        <nav
          className="mt-4 flex items-center gap-1"
          aria-label="Delivery views"
        >
          <DeliveryTab
            active={surface === "pull_requests"}
            icon={<GitPullRequest />}
            label="Pull requests"
            onClick={() =>
              void navigate({ to: "/code/delivery/pull-requests" })
            }
          />
          <DeliveryTab
            active={surface === "runs"}
            icon={<Workflow />}
            label="Runs & deployments"
            onClick={() => void navigate({ to: "/code/delivery/runs" })}
          />
        </nav>
      </header>

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border-subtle px-5 py-3">
        <div className="flex min-w-0 flex-wrap items-center gap-1 rounded-lg bg-muted/35 p-0.5">
          {(surface === "pull_requests"
            ? PR_BUILT_IN_VIEWS
            : RUN_BUILT_IN_VIEWS
          ).map((view) => (
            <button
              key={view.id}
              type="button"
              className={cn(
                "h-7 cursor-pointer rounded-md px-2.5 text-xs font-medium whitespace-nowrap text-muted-foreground transition-colors hover:text-foreground",
                activeViewId === view.id &&
                  "bg-background text-foreground shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_8%,transparent)]",
              )}
              onClick={() => applyBuiltInView(view.id)}
            >
              {view.label}
            </button>
          ))}
        </div>

        {customViews.length > 0 && (
          <Select
            value={
              customViews.some((view) => view.id === activeViewId)
                ? activeViewId
                : ""
            }
            onValueChange={(id) => {
              const view = customViews.find((candidate) => candidate.id === id);
              if (view) applySavedView(view);
            }}
          >
            <SelectTrigger size="sm" className="w-40">
              <SelectValue placeholder="Saved views" />
            </SelectTrigger>
            <SelectContent>
              {customViews.map((view) => (
                <SelectItem key={view.id} value={view.id}>
                  {view.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}

        <SearchInput
          size="sm"
          value={filters.search}
          onValueChange={setSearch}
          placeholder={
            surface === "pull_requests"
              ? "Search pull requests…"
              : "Search runs and deployments…"
          }
          className="min-w-52 flex-1 md:max-w-sm"
        />

        {surface === "pull_requests" ? (
          <PullRequestFilters
            repositories={repositories}
            filters={prFilters}
            onChange={(next) => {
              markCustom();
              setPrFilters(next);
            }}
          />
        ) : (
          <RunFilters
            repositories={repositories}
            filters={runFilters}
            onChange={(next) => {
              markCustom();
              setRunFilters(next);
            }}
          />
        )}

        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setSaveDialogOpen(true)}
        >
          <Save />
          Save view
        </Button>
      </div>

      {repositoryErrors.length > 0 && (
        <PartialErrorBanner errors={repositoryErrors} />
      )}

      {surface === "pull_requests" ? (
        <PullRequestsSurface
          repositories={repositories}
          capability={capability}
          loadingRepositories={repositoriesLoading}
          filters={prFilters}
        />
      ) : (
        <RunsSurface
          repositories={repositories}
          capability={capability}
          loadingRepositories={repositoriesLoading}
          filters={runFilters}
        />
      )}

      <DeliveryRepositoriesDialog
        open={repositoriesDialogOpen}
        onOpenChange={setRepositoriesDialogOpen}
        discovered={discovered}
        onResolved={(resolved, errors) => {
          setRepositoryErrors(errors);
          if (resolved.length > 0) {
            useCodeDeliveryStore
              .getState()
              .rememberManualRepositories(resolved);
          }
        }}
        onRefresh={() => void loadRepositories()}
      />
      <SaveViewDialog
        open={saveDialogOpen}
        onOpenChange={setSaveDialogOpen}
        surface={surface}
        filters={filters}
        onSaved={setActiveViewId}
      />
    </div>
  );
}

function DeliveryTab({
  active,
  icon,
  label,
  onClick,
}: {
  active: boolean;
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-current={active ? "page" : undefined}
      className={cn(
        "relative flex h-9 cursor-pointer items-center gap-1.5 px-3 text-sm font-medium text-muted-foreground transition-colors hover:text-foreground [&_svg]:size-4",
        active &&
          "text-foreground after:absolute after:right-2 after:bottom-0 after:left-2 after:h-0.5 after:rounded-full after:bg-primary",
      )}
      onClick={onClick}
    >
      {icon}
      {label}
    </button>
  );
}

function PullRequestsSurface({
  repositories,
  capability,
  loadingRepositories,
  filters,
}: {
  repositories: CodeGitHubRepositoryRef[];
  capability: CodeGitHubCapability | null;
  loadingRepositories: boolean;
  filters: CodeDeliveryPrViewFilters;
}) {
  const { client } = useApp();
  const navigate = useNavigate();
  const [items, setItems] = useState<CodeDeliveryPullRequestSummary[]>([]);
  const [errors, setErrors] = useState<CodeDeliverySourceError[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [error, setError] = useState<string | null>(null);
  const [fetchedAt, setFetchedAt] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  // Set by Refresh and by a completed action; consumed by the next query that
  // actually runs. Only those two reach past the server's short list cache —
  // a filter change reruns against it, which is the whole point of caching a
  // cross-repository read.
  const forceRefresh = useRef(false);
  const generation = useRef(0);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? null,
    [items, selectedId],
  );
  const selectedRepositories = useMemo(
    () => selectedRepositoryTargets(repositories, filters.repositoryKeys),
    [repositories, filters.repositoryKeys],
  );

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
      setItems((current) =>
        append ? dedupeRows([...current, ...page.items]) : page.items,
      );
      setNextCursor(page.next_cursor);
      setErrors(page.errors);
      setFetchedAt(page.fetched_at);
    } catch (caught) {
      if (token !== generation.current) return;
      setError(friendlyErrorMessage(caught, "Could not load pull requests."));
      if (!append) setItems([]);
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
  }, [client, selectedRepositories, filters, revision]);

  if (loadingRepositories) return <DeliveryListSkeleton />;
  if (capability && (!capability.found || capability.authenticated === false)) {
    return <GitHubUnavailable capability={capability} />;
  }
  if (repositories.length === 0) return <NoDeliveryRepositories />;

  return (
    <DeliverySplit selected={Boolean(selected)}>
      <div
        ref={scrollRef}
        className={cn(
          "min-h-0 flex-1 overflow-auto",
          selected && "max-lg:hidden",
        )}
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
              selectedId={selectedId}
              onSelect={setSelectedId}
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
      {selected && (
        <PullRequestDetailPanel
          client={client}
          summary={selected}
          onClose={() => setSelectedId(null)}
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
      )}
    </DeliverySplit>
  );
}

function RunsSurface({
  repositories,
  capability,
  loadingRepositories,
  filters,
}: {
  repositories: CodeGitHubRepositoryRef[];
  capability: CodeGitHubCapability | null;
  loadingRepositories: boolean;
  filters: CodeDeliveryRunViewFilters;
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
  const [revision, setRevision] = useState(0);
  // Set by Refresh and by a completed action; consumed by the next query that
  // actually runs. Only those two reach past the server's short list cache —
  // a filter change reruns against it, which is the whole point of caching a
  // cross-repository read.
  const forceRefresh = useRef(false);
  const generation = useRef(0);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const selected = useMemo(
    () => items.find((item) => item.id === selectedId) ?? null,
    [items, selectedId],
  );
  const selectedRepositories = useMemo(
    () => selectedRepositoryTargets(repositories, filters.repositoryKeys),
    [repositories, filters.repositoryKeys],
  );

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
      setItems((current) =>
        append ? dedupeRows([...current, ...page.items]) : page.items,
      );
      setNextCursor(page.next_cursor);
      setErrors(page.errors);
      setFetchedAt(page.fetched_at);
    } catch (caught) {
      if (token !== generation.current) return;
      setError(
        friendlyErrorMessage(caught, "Could not load runs and deployments."),
      );
      if (!append) setItems([]);
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
  }, [client, selectedRepositories, filters, revision]);

  if (loadingRepositories) return <DeliveryListSkeleton />;
  if (capability && (!capability.found || capability.authenticated === false)) {
    return <GitHubUnavailable capability={capability} />;
  }
  if (repositories.length === 0) return <NoDeliveryRepositories />;

  return (
    <DeliverySplit selected={Boolean(selected)}>
      <div
        ref={scrollRef}
        className={cn(
          "min-h-0 flex-1 overflow-auto",
          selected && "max-lg:hidden",
        )}
      >
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
              onSelect={setSelectedId}
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
      {selected && (
        <RunDetailPanel
          summary={selected}
          onClose={() => setSelectedId(null)}
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
      )}
    </DeliverySplit>
  );
}

function DeliverySplit({
  selected,
  children,
}: {
  selected: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "flex min-h-0 flex-1",
        selected && "lg:grid lg:grid-cols-[minmax(0,1fr)_minmax(360px,430px)]",
      )}
    >
      {children}
    </div>
  );
}

const PR_ROW_HEIGHT = 62;
const RUN_ROW_HEIGHT = 62;
const PR_GRID = "grid-cols-[minmax(260px,1fr)_150px_120px_110px]";
const RUN_GRID = "grid-cols-[minmax(260px,1fr)_150px_140px_110px]";

/**
 * A windowed row list.
 *
 * Delivery reads every tracked repository, and a cross-repository "All" view
 * runs into the thousands of rows once a handful of repos are tracked.
 * Mounting all of them made selecting a row feel like the app had stalled, so
 * only the visible window is mounted and the rest is spacer height.
 *
 * Rows are spacer-positioned rather than absolutely positioned, so the sticky
 * column header and the "Load more" footer stay in normal flow. Whatever sits
 * above the list inside the same scroller — a partial-failure banner, a load
 * error — shifts the rows down, so the scroll offset that banner occupies is
 * measured and handed to the virtualizer as `scrollMargin`. Without it the
 * window is wrong by exactly the banner's height.
 */
function VirtualRows<T extends { id: string }>({
  items,
  scrollRef,
  estimateSize,
  children,
}: {
  items: readonly T[];
  scrollRef: React.RefObject<HTMLDivElement | null>;
  estimateSize: number;
  children: (item: T) => React.ReactNode;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const [scrollMargin, setScrollMargin] = useState(0);

  useLayoutEffect(() => {
    const measure = () => {
      const scroller = scrollRef.current;
      const list = listRef.current;
      if (!scroller || !list) return;
      const offset =
        list.getBoundingClientRect().top -
        scroller.getBoundingClientRect().top +
        scroller.scrollTop;
      setScrollMargin((current) =>
        Math.abs(current - offset) > 0.5 ? offset : current,
      );
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    if (scrollRef.current) observer.observe(scrollRef.current);
    return () => observer.disconnect();
  }, [scrollRef, items.length]);

  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => estimateSize,
    overscan: 8,
    scrollMargin,
    getItemKey: (index) => items[index]?.id ?? index,
  });
  const rows = virtualizer.getVirtualItems();
  const paddingTop = (rows[0]?.start ?? scrollMargin) - scrollMargin;
  const paddingBottom =
    virtualizer.getTotalSize() -
    ((rows[rows.length - 1]?.end ?? scrollMargin) - scrollMargin);

  return (
    <div ref={listRef}>
      {paddingTop > 0 && <div style={{ height: paddingTop }} aria-hidden />}
      {rows.map((row) => {
        const item = items[row.index];
        if (!item) return null;
        return (
          <div
            key={row.key}
            data-index={row.index}
            ref={virtualizer.measureElement}
          >
            {children(item)}
          </div>
        );
      })}
      {paddingBottom > 0 && (
        <div style={{ height: paddingBottom }} aria-hidden />
      )}
    </div>
  );
}

function PullRequestList({
  items,
  selectedId,
  onSelect,
  scrollRef,
}: {
  items: CodeDeliveryPullRequestSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <div role="list" aria-label="Pull requests" className="min-w-[760px]">
      <div
        className={cn(
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-[11px] font-medium text-muted-foreground backdrop-blur",
          PR_GRID,
        )}
      >
        <span>Pull request</span>
        <span>Status</span>
        <span>Checks</span>
        <span className="text-right">Updated</span>
      </div>
      <VirtualRows
        items={items}
        scrollRef={scrollRef}
        estimateSize={PR_ROW_HEIGHT}
      >
        {(item) => (
          <PullRequestRow
            item={item}
            active={selectedId === item.id}
            onSelect={() => onSelect(item.id)}
          />
        )}
      </VirtualRows>
    </div>
  );
}

function PullRequestRow({
  item,
  active,
  onSelect,
}: {
  item: CodeDeliveryPullRequestSummary;
  active: boolean;
  onSelect: () => void;
}) {
  const lifecycle = pullRequestLifecycle(item);
  const review = pullRequestReviewSummary(item);
  const checks = checkSummary(checkCounts(item.checks));
  const settledAt = pullRequestSettledAt(item);
  return (
    <button
      type="button"
      role="listitem"
      data-active={active || undefined}
      className={cn(
        "grid w-full cursor-pointer gap-4 border-b border-border-subtle px-5 py-3 text-left transition-colors hover:bg-muted/35 data-[active]:bg-muted/55",
        PR_GRID,
      )}
      onClick={onSelect}
    >
      <span className="min-w-0">
        <span className="flex min-w-0 items-center gap-2">
          <PrLifecycleIcon
            lifecycle={lifecycle}
            className={cn(
              "size-4",
              STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
            )}
          />
          <span className="sr-only">
            {PULL_REQUEST_LIFECYCLE_LABEL[lifecycle]}:
          </span>
          <span className="truncate text-sm font-medium">{item.title}</span>
          {item.attention_reasons.length > 0 ? (
            <CircleAlert
              className={cn("size-3.5 shrink-0", STATUS_MARK.critical)}
              aria-label="Needs attention"
            />
          ) : item.ready_to_merge ? (
            // Tidebreak's own signal, not GitHub's: reviewed, green, and
            // nothing left blocking the merge. The lifecycle icon cannot
            // carry it, because a ready pull request is still just open.
            <Check
              className={cn("size-3.5 shrink-0", STATUS_MARK.ready)}
              aria-label="Ready to merge"
            />
          ) : null}
        </span>
        <span className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span className="truncate">{item.repository.name_with_owner}</span>
          <span className="tabular-nums">#{item.number}</span>
          <span className="truncate font-mono">{item.head_branch}</span>
          {item.workspace_links.length > 0 && (
            <span className="shrink-0 rounded bg-info-background px-1.5 py-0.5 text-[10px] text-info-foreground-muted">
              Tidebreak
            </span>
          )}
        </span>
      </span>
      <span className="flex items-center">
        <span
          className={cn("text-xs", STATUS_TEXT[review.tone])}
          title={
            settledAt ? `${review.label} ${relativeTime(settledAt)}` : undefined
          }
        >
          {review.label}
        </span>
      </span>
      <span className="flex items-center">
        <span className={cn("text-xs", STATUS_TEXT[checks.tone])}>
          {checks.label}
        </span>
      </span>
      <span className="flex items-center justify-end text-xs text-muted-foreground">
        {relativeTime(item.updated_at)}
      </span>
    </button>
  );
}

function RunList({
  items,
  selectedId,
  onSelect,
  scrollRef,
}: {
  items: CodeDeliveryRunSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  return (
    <div
      role="list"
      aria-label="Runs and deployments"
      className="min-w-[780px]"
    >
      <div
        className={cn(
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-[11px] font-medium text-muted-foreground backdrop-blur",
          RUN_GRID,
        )}
      >
        <span>Run</span>
        <span>Repository</span>
        <span>Status</span>
        <span className="text-right">Updated</span>
      </div>
      <VirtualRows
        items={items}
        scrollRef={scrollRef}
        estimateSize={RUN_ROW_HEIGHT}
      >
        {(item) => (
          <button
            type="button"
            role="listitem"
            data-active={selectedId === item.id || undefined}
            className={cn(
              "grid w-full cursor-pointer gap-4 border-b border-border-subtle px-5 py-3 text-left transition-colors hover:bg-muted/35 data-[active]:bg-muted/55",
              RUN_GRID,
            )}
            onClick={() => onSelect(item.id)}
          >
            <span className="min-w-0">
              <span className="flex min-w-0 items-center gap-2">
                {item.kind === "deployment" ? (
                  <ArchiveRestore className="size-4 shrink-0 text-muted-foreground" />
                ) : (
                  <Workflow className="size-4 shrink-0 text-muted-foreground" />
                )}
                <span className="truncate text-sm font-medium">
                  {item.name}
                </span>
              </span>
              <span className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                <span>
                  {item.kind === "deployment"
                    ? "Deployment"
                    : (item.workflow ?? "Workflow")}
                </span>
                {item.environment && <span>{item.environment}</span>}
                {item.branch && (
                  <span className="truncate font-mono">{item.branch}</span>
                )}
              </span>
            </span>
            <span className="flex min-w-0 items-center text-xs text-muted-foreground">
              <span className="truncate">
                {item.repository.name_with_owner}
              </span>
            </span>
            <span className="flex items-center">
              <RunStatusBadge item={item} />
            </span>
            <span className="flex items-center justify-end text-xs text-muted-foreground">
              {relativeTime(item.updated_at)}
            </span>
          </button>
        )}
      </VirtualRows>
    </div>
  );
}

function RunDetailPanel({
  summary,
  onClose,
  onChanged,
  onOpenWorkspace,
}: {
  summary: CodeDeliveryRunSummary;
  onClose: () => void;
  onChanged: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const { client } = useApp();
  const [detail, setDetail] = useState<CodeDeliveryRunDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);

  const load = async () => {
    const token = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeDeliveryRunDetail({
        repository: codeDeliveryRepositoryTarget(summary.repository),
        kind: summary.kind,
        id: summary.github_id,
      });
      if (token === generation.current) setDetail(next);
    } catch (caught) {
      if (token === generation.current) {
        setError(friendlyErrorMessage(caught, "Could not load this run."));
      }
    } finally {
      if (token === generation.current) setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    return () => {
      generation.current += 1;
    };
  }, [client, summary.id]);

  const rerun = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const result = await client.runCodeDeliveryRunAction({
        target: {
          repository: codeDeliveryRepositoryTarget(summary.repository),
          kind: summary.kind,
          id: summary.github_id,
        },
        action: { type: "rerun_failed" },
      });
      toast.success(result.message);
      await load();
      onChanged();
    } catch (caught) {
      toast.error(friendlyErrorMessage(caught, "Could not rerun failed jobs."));
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="flex min-h-0 w-full flex-col border-l border-border-subtle bg-background lg:w-auto">
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-4 py-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>{summary.repository.name_with_owner}</span>
            <span>
              {summary.kind === "deployment" ? "Deployment" : "Action"}
            </span>
          </div>
          <h2 className="mt-1 text-base font-semibold leading-snug">
            {summary.name}
          </h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">Close run details</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-4">
        {loading && !detail ? (
          <DetailSkeleton />
        ) : error ? (
          <InlineLoadError message={error} onRetry={() => void load()} />
        ) : detail ? (
          <div className="flex flex-col gap-5">
            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => void openInBrowser(detail.summary.url)}
              >
                <ExternalLink />
                Open on GitHub
              </Button>
              {detail.can_rerun_failed && (
                <Button
                  type="button"
                  size="sm"
                  disabled={busy}
                  onClick={() => void rerun()}
                >
                  {busy ? (
                    <LoaderCircle className="animate-spin" />
                  ) : (
                    <RefreshCw />
                  )}
                  Rerun failed
                </Button>
              )}
              {detail.summary.workspace_links.map((workspace) => (
                <Button
                  key={workspace.workspace_id}
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => onOpenWorkspace(workspace.workspace_id)}
                >
                  <GitBranch />
                  Open {workspace.title}
                </Button>
              ))}
            </div>

            <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
              <DetailStat
                label="Status"
                value={humanize(detail.summary.status)}
              />
              <DetailStat
                label="Conclusion"
                value={
                  detail.summary.conclusion
                    ? humanize(detail.summary.conclusion)
                    : "Pending"
                }
              />
              <DetailStat
                label="Workflow"
                value={detail.summary.workflow ?? detail.summary.name}
              />
              <DetailStat
                label="Environment"
                value={detail.summary.environment ?? "None"}
              />
              <DetailStat
                label="Branch"
                value={detail.summary.branch ?? "Unknown"}
                mono
              />
              <DetailStat
                label="Event"
                value={
                  detail.summary.event
                    ? humanize(detail.summary.event)
                    : "Unknown"
                }
              />
            </dl>

            {detail.jobs.length > 0 && (
              <section>
                <h3 className="text-sm font-medium">Jobs</h3>
                <div className="mt-2 flex flex-col rounded-lg border border-border-subtle">
                  {detail.jobs.map((job) => (
                    <button
                      key={job.id}
                      type="button"
                      className="flex items-start gap-2 border-b border-border-subtle px-3 py-2.5 text-left last:border-b-0 hover:bg-muted/30"
                      onClick={() => void openInBrowser(job.url)}
                    >
                      <CheckTone
                        bucket={runBucket(job.conclusion, job.status)}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-xs font-medium">
                          {job.name}
                        </span>
                        {job.failed_steps.length > 0 && (
                          <span className="mt-1 block text-[11px] text-critical">
                            {job.failed_steps.join(", ")}
                          </span>
                        )}
                      </span>
                      <ExternalLink className="size-3.5 text-muted-foreground" />
                    </button>
                  ))}
                </div>
              </section>
            )}

            {detail.deployment_statuses.length > 0 && (
              <section>
                <h3 className="text-sm font-medium">Deployment history</h3>
                <div className="mt-2 flex flex-col gap-2">
                  {detail.deployment_statuses.map((status) => (
                    <div
                      key={status.id}
                      className="rounded-lg border border-border-subtle px-3 py-2.5"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <RunStateText value={status.state} />
                        <span className="text-[11px] text-muted-foreground">
                          {relativeTime(status.created_at)}
                        </span>
                      </div>
                      {status.description && (
                        <p className="mt-1 text-xs text-muted-foreground">
                          {status.description}
                        </p>
                      )}
                      <div className="mt-2 flex gap-2">
                        {status.environment_url && (
                          <Button
                            type="button"
                            size="xs"
                            variant="outline"
                            onClick={() =>
                              void openInBrowser(status.environment_url!)
                            }
                          >
                            <ExternalLink />
                            Environment
                          </Button>
                        )}
                        {status.log_url && (
                          <Button
                            type="button"
                            size="xs"
                            variant="outline"
                            onClick={() => void openInBrowser(status.log_url!)}
                          >
                            Logs
                          </Button>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              </section>
            )}
          </div>
        ) : null}
      </div>
    </aside>
  );
}

function PullRequestFilters({
  repositories,
  filters,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  filters: CodeDeliveryPrViewFilters;
  onChange: (filters: CodeDeliveryPrViewFilters) => void;
}) {
  const count =
    filters.repositoryKeys.length +
    filters.states.length +
    filters.reviewStates.length +
    filters.checkStates.length +
    filters.authors.length +
    Number(filters.attentionOnly) +
    Number(filters.readyOnly) +
    Number(filters.tidebreakLinked !== undefined);
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" size="sm" variant="outline">
          <Filter />
          Filters
          {count > 0 && (
            <span className="rounded-full bg-primary px-1.5 text-[10px] text-primary-foreground">
              {count}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="w-[22rem] max-w-[calc(100vw-24px)] p-3"
      >
        <FilterSection title="Repositories">
          <RepositoryCheckboxes
            repositories={repositories}
            selected={filters.repositoryKeys}
            onChange={(repositoryKeys) =>
              onChange({ ...filters, repositoryKeys })
            }
          />
        </FilterSection>
        <FilterSection title="State">
          <CheckboxOptions
            options={["open", "closed", "merged"]}
            selected={filters.states}
            onChange={(states) => onChange({ ...filters, states })}
          />
        </FilterSection>
        <FilterSection title="Review">
          <CheckboxOptions
            options={["approved", "changes_requested", "review_required"]}
            selected={filters.reviewStates}
            onChange={(reviewStates) => onChange({ ...filters, reviewStates })}
          />
        </FilterSection>
        <FilterSection title="Checks">
          <CheckboxOptions
            options={["pass", "pending", "fail"]}
            selected={filters.checkStates}
            onChange={(checkStates) => onChange({ ...filters, checkStates })}
          />
        </FilterSection>
        <FilterSection title="Author logins">
          <Input
            value={filters.authors.join(", ")}
            placeholder="octocat, teammate"
            onChange={(event) =>
              onChange({ ...filters, authors: commaList(event.target.value) })
            }
          />
        </FilterSection>
        <div className="mt-3 flex flex-col gap-2 border-t border-border-subtle pt-3">
          <FilterSwitch
            label="Needs attention"
            checked={filters.attentionOnly}
            onCheckedChange={(attentionOnly) =>
              onChange({ ...filters, attentionOnly })
            }
          />
          <FilterSwitch
            label="Ready to merge"
            checked={filters.readyOnly}
            onCheckedChange={(readyOnly) => onChange({ ...filters, readyOnly })}
          />
          <LinkedFilter
            value={filters.tidebreakLinked}
            onChange={(tidebreakLinked) =>
              onChange({ ...filters, tidebreakLinked })
            }
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}

function RunFilters({
  repositories,
  filters,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  filters: CodeDeliveryRunViewFilters;
  onChange: (filters: CodeDeliveryRunViewFilters) => void;
}) {
  const count =
    filters.repositoryKeys.length +
    filters.kinds.length +
    filters.statuses.length +
    filters.conclusions.length +
    filters.workflows.length +
    filters.environments.length +
    filters.branches.length +
    filters.events.length +
    filters.actors.length +
    Number(filters.attentionOnly) +
    Number(filters.tidebreakLinked !== undefined);
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" size="sm" variant="outline">
          <Filter />
          Filters
          {count > 0 && (
            <span className="rounded-full bg-primary px-1.5 text-[10px] text-primary-foreground">
              {count}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        className="max-h-[min(720px,calc(100vh-32px))] w-[24rem] max-w-[calc(100vw-24px)] overflow-auto p-3"
      >
        <FilterSection title="Repositories">
          <RepositoryCheckboxes
            repositories={repositories}
            selected={filters.repositoryKeys}
            onChange={(repositoryKeys) =>
              onChange({ ...filters, repositoryKeys })
            }
          />
        </FilterSection>
        <FilterSection title="Kind">
          <CheckboxOptions
            options={["workflow_run", "deployment"]}
            selected={filters.kinds}
            onChange={(kinds) =>
              onChange({ ...filters, kinds: kinds as CodeDeliveryRunKind[] })
            }
          />
        </FilterSection>
        <FilterSection title="Status">
          <CheckboxOptions
            options={["queued", "in_progress", "completed", "pending"]}
            selected={filters.statuses}
            onChange={(statuses) => onChange({ ...filters, statuses })}
          />
        </FilterSection>
        <FilterSection title="Conclusion">
          <CheckboxOptions
            options={[
              "success",
              "failure",
              "cancelled",
              "timed_out",
              "action_required",
              "startup_failure",
            ]}
            selected={filters.conclusions}
            onChange={(conclusions) => onChange({ ...filters, conclusions })}
          />
        </FilterSection>
        <AdvancedTextFilter
          label="Workflows"
          value={filters.workflows}
          placeholder="CI, Release"
          onChange={(workflows) => onChange({ ...filters, workflows })}
        />
        <AdvancedTextFilter
          label="Environments"
          value={filters.environments}
          placeholder="production, staging"
          onChange={(environments) => onChange({ ...filters, environments })}
        />
        <AdvancedTextFilter
          label="Branches"
          value={filters.branches}
          placeholder="main, release/*"
          onChange={(branches) => onChange({ ...filters, branches })}
        />
        <AdvancedTextFilter
          label="Events"
          value={filters.events}
          placeholder="push, pull_request"
          onChange={(events) => onChange({ ...filters, events })}
        />
        <AdvancedTextFilter
          label="Actors"
          value={filters.actors}
          placeholder="octocat, dependabot[bot]"
          onChange={(actors) => onChange({ ...filters, actors })}
        />
        <div className="mt-3 flex flex-col gap-2 border-t border-border-subtle pt-3">
          <FilterSwitch
            label="Needs attention"
            checked={filters.attentionOnly}
            onCheckedChange={(attentionOnly) =>
              onChange({ ...filters, attentionOnly })
            }
          />
          <LinkedFilter
            value={filters.tidebreakLinked}
            onChange={(tidebreakLinked) =>
              onChange({ ...filters, tidebreakLinked })
            }
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}

function FilterSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <fieldset className="mb-3 border-b border-border-subtle pb-3 last:mb-0 last:border-b-0 last:pb-0">
      <legend className="mb-2 text-[11px] font-medium text-muted-foreground">
        {title}
      </legend>
      {children}
    </fieldset>
  );
}

function RepositoryCheckboxes({
  repositories,
  selected,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  selected: string[];
  onChange: (selected: string[]) => void;
}) {
  if (repositories.length === 0) {
    return (
      <p className="text-xs text-muted-foreground">No tracked repositories.</p>
    );
  }
  return (
    <div className="flex max-h-36 flex-col gap-1 overflow-auto pr-1">
      {repositories.map((repository) => {
        const key = codeDeliveryRepositoryKey(repository);
        return (
          <label
            key={key}
            className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
          >
            <Checkbox
              checked={selected.includes(key)}
              onCheckedChange={(checked) =>
                onChange(toggleValue(selected, key, checked === true))
              }
            />
            <span className="min-w-0 truncate">
              {repository.name_with_owner}
            </span>
          </label>
        );
      })}
    </div>
  );
}

function CheckboxOptions<T extends string>({
  options,
  selected,
  onChange,
}: {
  options: readonly T[];
  selected: readonly T[];
  onChange: (selected: T[]) => void;
}) {
  return (
    <div className="grid grid-cols-2 gap-1">
      {options.map((option) => (
        <label
          key={option}
          className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
        >
          <Checkbox
            checked={selected.includes(option)}
            onCheckedChange={(checked) =>
              onChange(toggleValue([...selected], option, checked === true))
            }
          />
          <span>{humanize(option)}</span>
        </label>
      ))}
    </div>
  );
}

function FilterSwitch({
  label,
  checked,
  onCheckedChange,
}: {
  label: string;
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between gap-3 text-xs">
      <span>{label}</span>
      <Switch
        checked={checked}
        onCheckedChange={onCheckedChange}
        aria-label={label}
      />
    </label>
  );
}

function LinkedFilter({
  value,
  onChange,
}: {
  value: boolean | undefined;
  onChange: (value: boolean | undefined) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-xs">Tidebreak link</span>
      <Select
        value={value === undefined ? "any" : value ? "linked" : "unlinked"}
        onValueChange={(next) =>
          onChange(next === "any" ? undefined : next === "linked")
        }
      >
        <SelectTrigger size="sm" className="w-28">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="any">Any</SelectItem>
          <SelectItem value="linked">Linked</SelectItem>
          <SelectItem value="unlinked">Unlinked</SelectItem>
        </SelectContent>
      </Select>
    </div>
  );
}

function AdvancedTextFilter({
  label,
  value,
  placeholder,
  onChange,
}: {
  label: string;
  value: string[];
  placeholder: string;
  onChange: (value: string[]) => void;
}) {
  return (
    <div className="mb-3 flex flex-col gap-1.5">
      <Label className="text-[11px] text-muted-foreground">{label}</Label>
      <Input
        value={value.join(", ")}
        placeholder={placeholder}
        onChange={(event) => onChange(commaList(event.target.value))}
      />
    </div>
  );
}

function DeliveryRepositoriesDialog({
  open,
  onOpenChange,
  discovered,
  onResolved,
  onRefresh,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  discovered: CodeGitHubRepositoryRef[];
  onResolved: (
    repositories: CodeGitHubRepositoryRef[],
    errors: CodeDeliverySourceError[],
  ) => void;
  onRefresh: () => void;
}) {
  const { client } = useApp();
  const manualRepositories = useCodeDeliveryStore(
    (state) => state.manualRepositories,
  );
  const excluded = useCodeDeliveryStore(
    (state) => state.excludedRegisteredRepoIds,
  );
  const pinned = useCodeDeliveryStore((state) => state.pinnedRepositoryKeys);
  const [input, setInput] = useState("");
  const [resolving, setResolving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const add = async () => {
    const repositories = input
      .split(/[\n,]+/)
      .map((item) => item.trim())
      .filter(Boolean);
    if (repositories.length === 0 || resolving) return;
    setResolving(true);
    setError(null);
    try {
      const snapshot =
        await client.resolveCodeDeliveryRepositories(repositories);
      onResolved(snapshot.repositories, snapshot.errors);
      if (snapshot.repositories.length > 0) setInput("");
      if (snapshot.errors.length > 0) {
        setError(snapshot.errors.map((item) => item.message).join(" "));
      }
    } catch (caught) {
      setError(
        friendlyErrorMessage(caught, "Could not resolve those repositories."),
      );
    } finally {
      setResolving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Delivery repositories</DialogTitle>
        </DialogHeader>
        <div className="flex max-h-[65vh] flex-col gap-5 overflow-auto pr-1">
          <section>
            <div className="flex items-center justify-between gap-2">
              <div>
                <h3 className="text-sm font-medium">Registered in Tidebreak</h3>
                <p className="text-xs text-muted-foreground">
                  GitHub repos are tracked automatically. Disable any you do not
                  want in Delivery.
                </p>
              </div>
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                onClick={onRefresh}
              >
                <RefreshCw />
                <span className="sr-only">Refresh repositories</span>
              </Button>
            </div>
            <div className="mt-3 flex flex-col rounded-lg border border-border-subtle">
              {discovered.length === 0 ? (
                <p className="px-3 py-4 text-xs text-muted-foreground">
                  No registered repositories resolve to GitHub yet.
                </p>
              ) : (
                discovered.map((repository) => {
                  const key = codeDeliveryRepositoryKey(repository);
                  const enabled =
                    !repository.tidebreak_repo_id ||
                    !excluded.includes(repository.tidebreak_repo_id);
                  return (
                    <RepositorySettingRow
                      key={key}
                      repository={repository}
                      enabled={enabled}
                      pinned={pinned.includes(key)}
                      onEnabledChange={(next) => {
                        if (!repository.tidebreak_repo_id) return;
                        useCodeDeliveryStore
                          .getState()
                          .setRegisteredRepositoryExcluded(
                            repository.tidebreak_repo_id,
                            !next,
                          );
                      }}
                      onPinnedChange={(next) =>
                        useCodeDeliveryStore
                          .getState()
                          .setRepositoryPinned(key, next)
                      }
                    />
                  );
                })
              )}
            </div>
          </section>

          <section>
            <h3 className="text-sm font-medium">Other GitHub repositories</h3>
            <p className="text-xs text-muted-foreground">
              Add owner/repo, a GitHub URL, or host/owner/repo. One per line.
            </p>
            <div className="mt-3 flex gap-2">
              <Input
                value={input}
                placeholder="brightwave-inc/tidebreak"
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void add();
                  }
                }}
              />
              <Button
                type="button"
                disabled={resolving || !input.trim()}
                onClick={() => void add()}
              >
                {resolving && <LoaderCircle className="animate-spin" />}
                Add
              </Button>
            </div>
            {error && <p className="mt-2 text-xs text-critical">{error}</p>}
            {manualRepositories.length > 0 && (
              <div className="mt-3 flex flex-col rounded-lg border border-border-subtle">
                {manualRepositories.map((repository) => {
                  const key = codeDeliveryRepositoryKey(repository);
                  return (
                    <RepositorySettingRow
                      key={key}
                      repository={repository}
                      enabled
                      pinned={pinned.includes(key)}
                      manual
                      onEnabledChange={() => {}}
                      onPinnedChange={(next) =>
                        useCodeDeliveryStore
                          .getState()
                          .setRepositoryPinned(key, next)
                      }
                      onRemove={() =>
                        useCodeDeliveryStore
                          .getState()
                          .removeManualRepository(key)
                      }
                    />
                  );
                })}
              </div>
            )}
          </section>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RepositorySettingRow({
  repository,
  enabled,
  pinned,
  manual = false,
  onEnabledChange,
  onPinnedChange,
  onRemove,
}: {
  repository: CodeGitHubRepositoryRef;
  enabled: boolean;
  pinned: boolean;
  manual?: boolean;
  onEnabledChange: (enabled: boolean) => void;
  onPinnedChange: (pinned: boolean) => void;
  onRemove?: () => void;
}) {
  return (
    <div className="flex items-center gap-3 border-b border-border-subtle px-3 py-2.5 last:border-b-0">
      {!manual && (
        <Checkbox
          checked={enabled}
          onCheckedChange={(checked) => onEnabledChange(checked === true)}
          aria-label={`Track ${repository.name_with_owner}`}
        />
      )}
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium">
          {repository.name_with_owner}
        </p>
        <p className="truncate text-[11px] text-muted-foreground">
          {repository.host}
        </p>
      </div>
      <Button
        type="button"
        size="icon-xs"
        variant="ghost"
        aria-label={pinned ? "Unpin repository" : "Pin repository"}
        onClick={() => onPinnedChange(!pinned)}
      >
        {pinned ? <PinOff /> : <Pin />}
      </Button>
      {manual && onRemove && (
        <Button
          type="button"
          size="icon-xs"
          variant="ghost-destructive"
          aria-label="Remove repository"
          onClick={onRemove}
        >
          <X />
        </Button>
      )}
    </div>
  );
}

function SaveViewDialog({
  open,
  onOpenChange,
  surface,
  filters,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  surface: CodeDeliverySurface;
  filters: CodeDeliveryPrViewFilters | CodeDeliveryRunViewFilters;
  onSaved: (id: string) => void;
}) {
  const [name, setName] = useState("");
  const save = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    const id = `${surface}:${Date.now()}`;
    const createdAt = new Date().toISOString();
    const view: CodeDeliverySavedView =
      surface === "pull_requests"
        ? {
            id,
            kind: surface,
            name: trimmed,
            filters: clonePrFilters(filters as CodeDeliveryPrViewFilters),
            createdAt,
          }
        : {
            id,
            kind: surface,
            name: trimmed,
            filters: cloneRunFilters(filters as CodeDeliveryRunViewFilters),
            createdAt,
          };
    useCodeDeliveryStore.getState().upsertSavedView(view);
    onSaved(id);
    setName("");
    onOpenChange(false);
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Save this view</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-2">
          <Label htmlFor="delivery-view-name">Name</Label>
          <Input
            id="delivery-view-name"
            value={name}
            placeholder="Production failures"
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                save();
              }
            }}
          />
          <p className="text-xs text-muted-foreground">
            Saves the current repositories, search, and filters on this device.
          </p>
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            Cancel
          </Button>
          <Button type="button" disabled={!name.trim()} onClick={save}>
            Save view
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * How many rows there are, how old they are, and a way to reread them.
 *
 * Delivery holds live GitHub state behind a thirty-second server cache and
 * refetches only when a filter moves, so a reader watching a merge land had
 * no way to tell whether the list was current and no way to ask.
 */
function FreshnessBar({
  fetchedAt,
  loading,
  count,
  noun,
  onRefresh,
}: {
  fetchedAt: string | null;
  loading: boolean;
  count: number;
  noun: string;
  onRefresh: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 px-5 py-1.5 text-[11px] text-muted-foreground">
      <span>
        {count === 0 ? "" : `${count} ${noun}${count === 1 ? "" : "s"}`}
        {fetchedAt && count > 0 && ` · updated ${relativeTime(fetchedAt)}`}
      </span>
      <Button
        type="button"
        size="xs"
        variant="ghost"
        disabled={loading}
        onClick={onRefresh}
      >
        {loading ? <LoaderCircle className="animate-spin" /> : <RefreshCw />}
        Refresh
      </Button>
    </div>
  );
}

function PartialErrorBanner({
  errors,
  compact = false,
}: {
  errors: CodeDeliverySourceError[];
  compact?: boolean;
}) {
  return (
    <div
      role="status"
      className={cn(
        "flex shrink-0 items-start gap-2 border-b border-warning-border bg-warning-background px-5 py-2.5 text-xs text-warning-foreground-muted",
        compact && "border-t",
      )}
    >
      <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
      <span>
        {errors.length === 1
          ? errors[0]!.message
          : `${errors.length} repositories could not be refreshed. Available results are still shown.`}
      </span>
    </div>
  );
}

function GitHubUnavailable({
  capability,
}: {
  capability: CodeGitHubCapability;
}) {
  return (
    <Empty className="min-h-80">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <CircleAlert />
        </EmptyMedia>
        <EmptyTitle>GitHub is not connected</EmptyTitle>
        <EmptyDescription>{capability.remediation}</EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function NoDeliveryRepositories() {
  return (
    <Empty className="min-h-80">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <GitBranch />
        </EmptyMedia>
        <EmptyTitle>No GitHub repositories tracked</EmptyTitle>
        <EmptyDescription>
          Register a GitHub-backed repo in Tidebreak or add one from
          Repositories.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}

function InlineLoadError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="m-4 flex items-center justify-between gap-3 rounded-lg border border-critical-border bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted">
      <span>{message}</span>
      <Button type="button" size="xs" variant="outline" onClick={onRetry}>
        Try again
      </Button>
    </div>
  );
}

function DeliveryListSkeleton() {
  return (
    <div className="flex min-h-0 flex-1 flex-col p-5" role="status">
      <span className="sr-only">Loading delivery items</span>
      {Array.from({ length: 7 }, (_, index) => (
        <div
          key={index}
          className="grid grid-cols-[minmax(260px,1fr)_150px_120px_110px] gap-4 border-b border-border-subtle py-3"
        >
          <div className="flex flex-col gap-2">
            <Skeleton className="h-4 w-2/3" />
            <Skeleton className="h-3 w-1/2" />
          </div>
          <Skeleton className="h-5 w-24" />
          <Skeleton className="h-5 w-20" />
          <Skeleton className="ml-auto h-3 w-16" />
        </div>
      ))}
    </div>
  );
}

function DetailStat({
  label,
  value,
  mono = false,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="min-w-0">
      <dt className="text-[11px] text-muted-foreground">{label}</dt>
      <dd className={cn("mt-0.5 truncate", mono && "font-mono")}>{value}</dd>
    </div>
  );
}

function RunStatusBadge({ item }: { item: CodeDeliveryRunSummary }) {
  const value = item.conclusion ?? item.status;
  const tone = runTone(value);
  return (
    <span
      className={cn(
        "rounded-md px-2 py-1 text-[11px] font-medium",
        tone === "success" &&
          "bg-success-background text-success-foreground-muted",
        tone === "critical" &&
          "bg-critical-background text-critical-foreground-muted",
        tone === "warning" &&
          "bg-warning-background text-warning-foreground-muted",
        tone === "muted" && "bg-muted text-muted-foreground",
      )}
    >
      {humanize(value)}
    </span>
  );
}

function RunStateText({ value }: { value: string }) {
  const tone = runTone(value);
  return (
    <span
      className={cn(
        "text-xs font-medium",
        tone === "success" && "text-success",
        tone === "critical" && "text-critical",
        tone === "warning" && "text-warning",
        tone === "muted" && "text-muted-foreground",
      )}
    >
      {humanize(value)}
    </span>
  );
}

function runBucket(
  conclusion: string | undefined,
  status: string,
): "pass" | "pending" | "fail" | "skipped" {
  if (conclusion === "success") return "pass";
  if (
    conclusion === "failure" ||
    conclusion === "timed_out" ||
    conclusion === "action_required" ||
    conclusion === "startup_failure"
  ) {
    return "fail";
  }
  if (status === "queued" || status === "in_progress" || status === "pending") {
    return "pending";
  }
  return "skipped";
}

function runTone(value: string): "success" | "critical" | "warning" | "muted" {
  if (value === "success") return "success";
  if (
    value === "failure" ||
    value === "timed_out" ||
    value === "action_required" ||
    value === "startup_failure" ||
    value === "error"
  ) {
    return "critical";
  }
  if (value === "queued" || value === "in_progress" || value === "pending") {
    return "warning";
  }
  return "muted";
}

function selectedRepositoryTargets(
  repositories: CodeGitHubRepositoryRef[],
  selected: string[],
) {
  const keys = new Set(selected);
  return repositories
    .filter(
      (repository) =>
        keys.size === 0 || keys.has(codeDeliveryRepositoryKey(repository)),
    )
    .map(codeDeliveryRepositoryTarget);
}

function clonePrFilters(
  filters: CodeDeliveryPrViewFilters,
): CodeDeliveryPrViewFilters {
  return {
    ...filters,
    repositoryKeys: [...filters.repositoryKeys],
    states: [...filters.states],
    reviewStates: [...filters.reviewStates],
    checkStates: [...filters.checkStates],
    authors: [...filters.authors],
  };
}

function cloneRunFilters(
  filters: CodeDeliveryRunViewFilters,
): CodeDeliveryRunViewFilters {
  return {
    ...filters,
    repositoryKeys: [...filters.repositoryKeys],
    kinds: [...filters.kinds],
    statuses: [...filters.statuses],
    conclusions: [...filters.conclusions],
    workflows: [...filters.workflows],
    environments: [...filters.environments],
    branches: [...filters.branches],
    events: [...filters.events],
    actors: [...filters.actors],
  };
}

function toggleValue<T>(values: T[], value: T, enabled: boolean): T[] {
  if (enabled) return values.includes(value) ? values : [...values, value];
  return values.filter((candidate) => candidate !== value);
}

function commaList(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

/**
 * A wire token as a label: `timed_out` reads "Timed out".
 *
 * Sentence case, not title case. The repository writes UI text in sentence
 * case, and title-casing turned "review pending" into "Review Pending", which
 * read like a proper noun rather than a state.
 */
function humanize(value: string): string {
  const words = value.replaceAll("_", " ").trim();
  if (!words) return words;
  return words[0]!.toUpperCase() + words.slice(1);
}

function dedupeRows<T extends { id: string }>(items: T[]): T[] {
  return [...new Map(items.map((item) => [item.id, item])).values()];
}

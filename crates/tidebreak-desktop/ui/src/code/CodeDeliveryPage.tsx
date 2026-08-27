import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ArchiveRestore,
  CircleAlert,
  CornerDownRight,
  ExternalLink,
  Filter,
  GitBranch,
  GitPullRequest,
  LoaderCircle,
  MessageSquare,
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
import { useConfirm } from "@/components/ConfirmDialog";
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
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
  CodeDeliveryRunDetail,
  CodeDeliveryRunAction,
  CodeDeliveryRunKind,
  CodeDeliveryRunSummary,
  CodeDeliveryRunTarget,
  CodeDeliverySourceError,
  CodeGitHubCapability,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
} from "../api/types";
import {
  codeDeliveryRepositoryKey,
  codeDeliveryRepositoryTarget,
  deliveryAuthorSightings,
  trackedCodeDeliveryRepositories,
  useCodeDeliveryStore,
  type CodeDeliveryAuthor,
  type CodeDeliveryPrViewFilters,
  type CodeDeliveryRunViewFilters,
  type CodeDeliverySavedView,
  type CodeDeliverySurface,
} from "./CodeDeliveryStore";
import { CodeSidebar } from "./CodeSidebar";
import { RepositoryTriggerRules } from "./RepositoryTriggerRules";
import { GithubAvatar } from "./GithubAvatar";
import {
  CheckTone,
  DetailSheet,
  DetailSkeleton,
  PrLifecycleIcon,
  PullRequestDetailSheet,
  relativeTime,
} from "./PullRequestDetail";
import {
  checkCounts,
  checkSummary,
  prStatus,
  type PullRequestListGroup,
} from "./prState";
import { arrangeStackLanes, type StackedRow } from "./pullRequestStacks";
import {
  deliveryPullRequestDigest,
  deliveryRepositoryHasMergeQueue,
  prDirectMergeAction,
} from "./prActions";
import {
  STATUS_DOT,
  STATUS_MARK,
  STATUS_TEXT,
  type StatusTone,
} from "./statusTone";

type PullRequestGrouping = "attention" | "repository" | "none";

type PrBuiltInView = {
  id: string;
  label: string;
  /**
   * Fill `authors` with the signed-in GitHub login. The login only arrives
   * with the repository snapshot, so the view carries the intent and the
   * page resolves it once `gh` reports who you are.
   */
  viewerAuthored?: boolean;
  filters: CodeDeliveryPrViewFilters;
};

/**
 * The first entry is the default view. Delivery opens on your own open work —
 * drafts included, because `state` is still `open` on a draft — rather than on
 * everyone's review queue.
 */
const PR_BUILT_IN_VIEWS: readonly PrBuiltInView[] = [
  {
    id: "mine",
    label: "Yours",
    viewerAuthored: true,
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

export type CodeDeliverySearch = {
  view?: string;
  repoHost?: string;
  repoOwner?: string;
  repoName?: string;
  pr?: number;
  runKind?: CodeDeliveryRunKind;
  runId?: number;
};

export function codeDeliverySearchFrom(
  search: Record<string, unknown>,
): CodeDeliverySearch {
  const runKind =
    search.runKind === "workflow_run" || search.runKind === "deployment"
      ? search.runKind
      : undefined;
  return {
    ...(typeof search.view === "string" ? { view: search.view } : {}),
    ...(typeof search.repoHost === "string"
      ? { repoHost: search.repoHost }
      : {}),
    ...(typeof search.repoOwner === "string"
      ? { repoOwner: search.repoOwner }
      : {}),
    ...(typeof search.repoName === "string"
      ? { repoName: search.repoName }
      : {}),
    ...(positiveSearchInteger(search.pr) !== undefined
      ? { pr: positiveSearchInteger(search.pr) }
      : {}),
    ...(runKind ? { runKind } : {}),
    ...(positiveSearchInteger(search.runId) !== undefined
      ? { runId: positiveSearchInteger(search.runId) }
      : {}),
  };
}

export function CodeDeliveryPage({
  surface,
  search = {},
}: {
  surface: CodeDeliverySurface;
  search?: CodeDeliverySearch;
}) {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeDeliveryBody surface={surface} search={search} />
      </div>
    </RouteFrame>
  );
}

function CodeDeliveryBody({
  surface,
  search,
}: {
  surface: CodeDeliverySurface;
  search: CodeDeliverySearch;
}) {
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
  const repositorySnapshot = useCodeDeliveryStore(
    (state) => state.repositorySnapshot,
  );
  const repositoryLoading = useCodeDeliveryStore(
    (state) => state.repositoryLoading,
  );
  const repositoryError = useCodeDeliveryStore(
    (state) => state.repositoryError,
  );
  const [resolutionErrors, setResolutionErrors] = useState<
    CodeDeliverySourceError[]
  >([]);
  const [repositoriesDialogOpen, setRepositoriesDialogOpen] = useState(false);
  const [saveDialogOpen, setSaveDialogOpen] = useState(false);
  const [pullRequestGrouping, setPullRequestGrouping] =
    useState<PullRequestGrouping>("attention");
  // Who `gh` is signed in as. Undefined until the repository snapshot lands,
  // and undefined for good against a `gh` too old to report it.
  const viewerLogin = repositorySnapshot?.capability.viewer_login;
  // A view that filters by "you" cannot mean anything without a login, so it
  // leaves the row rather than quietly widening to everybody's pull requests.
  // It stays put while the snapshot is still loading, because that is the
  // default and the common answer is that a login exists.
  const prViews = useMemo(
    () =>
      repositorySnapshot && !viewerLogin
        ? PR_BUILT_IN_VIEWS.filter((view) => !view.viewerAuthored)
        : PR_BUILT_IN_VIEWS,
    [repositorySnapshot, viewerLogin],
  );
  const builtInViews =
    surface === "pull_requests" ? prViews : RUN_BUILT_IN_VIEWS;
  const defaultViewId = builtInViews[0]!.id;
  const [activeViewId, setActiveViewId] = useState(defaultViewId);
  const [prFilters, setPrFilters] = useState<CodeDeliveryPrViewFilters>(() =>
    builtInPrFilters(PR_BUILT_IN_VIEWS[0]!, viewerLogin),
  );
  const [runFilters, setRunFilters] = useState<CodeDeliveryRunViewFilters>(() =>
    cloneRunFilters(RUN_BUILT_IN_VIEWS[0]!.filters),
  );

  useEffect(() => {
    const viewId = builtInViews.some((view) => view.id === search.view)
      ? search.view!
      : defaultViewId;
    setActiveViewId(viewId);
    if (surface === "pull_requests") {
      const view = PR_BUILT_IN_VIEWS.find(
        (candidate) => candidate.id === viewId,
      );
      setPrFilters(
        builtInPrFilters(view ?? PR_BUILT_IN_VIEWS[0]!, viewerLogin),
      );
    } else {
      const view = RUN_BUILT_IN_VIEWS.find(
        (candidate) => candidate.id === viewId,
      );
      setRunFilters(cloneRunFilters((view ?? RUN_BUILT_IN_VIEWS[0]!).filters));
    }
    // The login is read, not tracked: it lands after this effect has already
    // seeded the filters, and rerunning here would throw away edits made in
    // between. The effect below fills it in instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search.view, surface, defaultViewId]);

  // The signed-in login arrives with the repository snapshot, a beat after the
  // viewer view was applied. Fill the author in then, and only while the view
  // is still the untouched one — anything the reader picked outranks it.
  useEffect(() => {
    if (surface !== "pull_requests" || !viewerLogin) return;
    const view = PR_BUILT_IN_VIEWS.find(
      (candidate) => candidate.id === activeViewId,
    );
    if (!view?.viewerAuthored) return;
    setPrFilters((current) =>
      current.authors.length === 0
        ? { ...current, authors: [viewerLogin] }
        : current,
    );
  }, [activeViewId, surface, viewerLogin]);

  const loadRepositories = async (force = false, notify = false) => {
    try {
      await useCodeDeliveryStore.getState().loadRepositories(client, { force });
    } catch (error) {
      if (notify) {
        toast.error(
          friendlyErrorMessage(error, "Could not load GitHub repositories."),
        );
      }
    }
  };

  useEffect(() => {
    void loadRepositories();
    // The store owns the shared request and its visible error state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client]);

  const discovered = repositorySnapshot?.repositories ?? [];
  const capability = repositorySnapshot?.capability ?? null;
  // Local-only Tidebreak checkouts are skipped on purpose. They are not
  // GitHub sources, so they must not look like a failed refresh.
  const repositoryErrors = deliveryRefreshErrors([
    ...(repositorySnapshot?.errors ?? []),
    ...resolutionErrors,
  ]);
  const repositoriesLoading = repositoryLoading && !repositorySnapshot;
  const routeRepository = useMemo<CodeGitHubRepositoryTarget | undefined>(
    () =>
      search.repoHost && search.repoOwner && search.repoName
        ? {
            host: search.repoHost,
            owner: search.repoOwner,
            name: search.repoName,
          }
        : undefined,
    [search.repoHost, search.repoName, search.repoOwner],
  );

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
      if (view) setPrFilters(builtInPrFilters(view, viewerLogin));
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
              <h1 className="text-xl font-semibold tracking-tight">
                {surface === "runs" ? "Runs & deployments" : "Pull requests"}
              </h1>
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
          aria-label="Pull request views"
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
          {builtInViews.map((view) => (
            <button
              key={view.id}
              type="button"
              aria-pressed={activeViewId === view.id}
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

        {surface === "pull_requests" && (
          <Select
            value={pullRequestGrouping}
            onValueChange={(value) =>
              setPullRequestGrouping(value as PullRequestGrouping)
            }
          >
            <SelectTrigger
              size="sm"
              className="w-40"
              aria-label="Group pull requests"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="attention">Group: attention</SelectItem>
              <SelectItem value="repository">Group: repository</SelectItem>
              <SelectItem value="none">No grouping</SelectItem>
            </SelectContent>
          </Select>
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

      {repositoryError && repositorySnapshot && (
        <RepositoryRefreshWarning
          message={repositoryError}
          onRetry={() => void loadRepositories(true, true)}
        />
      )}

      {surface === "pull_requests" ? (
        <PullRequestsSurface
          repositories={repositories}
          capability={capability}
          loadingRepositories={repositoriesLoading}
          repositoryLoaded={repositorySnapshot !== null}
          repositoryError={repositoryError}
          onRetryRepositories={() => void loadRepositories(true, true)}
          filters={prFilters}
          grouping={pullRequestGrouping}
          target={
            routeRepository && search.pr
              ? { repository: routeRepository, number: search.pr }
              : undefined
          }
        />
      ) : (
        <RunsSurface
          repositories={repositories}
          capability={capability}
          loadingRepositories={repositoriesLoading}
          repositoryLoaded={repositorySnapshot !== null}
          repositoryError={repositoryError}
          onRetryRepositories={() => void loadRepositories(true, true)}
          filters={runFilters}
          target={
            routeRepository && search.runKind && search.runId
              ? {
                  repository: routeRepository,
                  kind: search.runKind,
                  id: search.runId,
                }
              : undefined
          }
        />
      )}

      <DeliveryRepositoriesDialog
        open={repositoriesDialogOpen}
        onOpenChange={setRepositoriesDialogOpen}
        discovered={discovered}
        onResolved={(resolved, errors) => {
          setResolutionErrors(errors);
          if (resolved.length > 0) {
            useCodeDeliveryStore
              .getState()
              .rememberManualRepositories(resolved);
          }
        }}
        onRefresh={() => void loadRepositories(true, true)}
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

type TargetDetailState<T> = {
  key: string;
  pending: boolean;
  detail: T | null;
};

function PullRequestsSurface({
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
  const [targetDetailState, setTargetDetailState] =
    useState<TargetDetailState<CodeDeliveryPullRequestDetail> | null>(null);
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
    ? `${codeDeliveryRepositoryKey(target.repository)}:pull-request:${target.number}`
    : null;

  const refreshList = () => {
    forceRefresh.current = true;
    setRevision((value) => value + 1);
  };

  const runListMerge = async (item: CodeDeliveryPullRequestSummary) => {
    const action = prDirectMergeAction(deliveryPullRequestDigest(item), {
      hasMergeQueue: deliveryRepositoryHasMergeQueue(items, item.repository),
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
      setItems((current) => {
        let nextItems = append ? [...current, ...page.items] : page.items;
        const exactItem = target
          ? current.find((item) => pullRequestMatchesTarget(item, target))
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
      {dialog}
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-auto">
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
      {selected &&
      target &&
      targetDetailState?.key === targetKey &&
      targetDetailState.pending &&
      !targetDetailState.detail &&
      pullRequestMatchesTarget(selected, target) ? (
        <PendingDetailSheet
          context={`${selected.repository.name_with_owner} #${selected.number}`}
          title={selected.title}
          closeLabel="Close pull request details"
          onClose={closeDetail}
        />
      ) : selected ? (
        <PullRequestDetailSheet
          key={selected.id}
          client={client}
          summary={selected}
          hasMergeQueue={deliveryRepositoryHasMergeQueue(
            items,
            selected.repository,
          )}
          initialDetail={
            targetDetailState?.detail?.summary.id === selected.id
              ? targetDetailState.detail
              : undefined
          }
          onClose={closeDetail}
          onChanged={refreshList}
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

function RunsSurface({
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

/**
 * The sheet while a route-targeted detail is still loading: the reader
 * followed a deep link, so the frame opens immediately with what the link
 * already knew — repository, number, title — and the body fills in.
 */
function PendingDetailSheet({
  context,
  title,
  closeLabel,
  onClose,
}: {
  context: string;
  title: string;
  closeLabel: string;
  onClose: () => void;
}) {
  return (
    <DetailSheet label={title} onClose={onClose}>
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="text-xs text-muted-foreground">{context}</div>
          <h2 className="mt-1 text-base font-semibold leading-snug">{title}</h2>
        </div>
        <Button type="button" size="icon-xs" variant="ghost" onClick={onClose}>
          <X />
          <span className="sr-only">{closeLabel}</span>
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-5">
        <DetailSkeleton />
      </div>
    </DetailSheet>
  );
}

const PR_ROW_HEIGHT = 62;
const PR_GROUP_HEIGHT = 38;
const RUN_ROW_HEIGHT = 62;
const PR_GRID =
  "grid-cols-[minmax(280px,1fr)_150px_110px_105px_minmax(8.75rem,auto)_95px]";
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
  estimateSize: number | ((item: T) => number);
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
    estimateSize: (index) => {
      const item = items[index];
      if (typeof estimateSize === "number") return estimateSize;
      return item ? estimateSize(item) : 0;
    },
    // Group headers add several short virtual rows. Keep enough surrounding
    // rows mounted that a compact list remains fully searchable and a fast
    // wheel gesture never reveals an empty gap.
    overscan: 16,
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

type PullRequestListItem =
  | {
      id: string;
      kind: "group";
      label: string;
      description: string;
      tone: StatusTone;
      count: number;
    }
  | {
      id: string;
      kind: "pull_request";
      row: StackedRow;
      showRepository: boolean;
    };

const PULL_REQUEST_GROUP_ORDER: readonly PullRequestListGroup[] = [
  "attention",
  "ready",
  "waiting",
  "handed_off",
  "draft",
  "done",
];

const PULL_REQUEST_GROUP_RANK = new Map(
  PULL_REQUEST_GROUP_ORDER.map((group, index) => [group, index]),
);

const PULL_REQUEST_GROUP_META: Record<
  PullRequestListGroup,
  { label: string; description: string; tone: StatusTone }
> = {
  attention: {
    label: "Needs your attention",
    description:
      "Failed checks, requested changes, conflicts, or stale branches",
    tone: "critical",
  },
  ready: {
    label: "Ready to merge",
    description: "Green and waiting for you",
    tone: "ready",
  },
  waiting: {
    label: "Waiting",
    description: "Checks or reviews are still moving",
    tone: "pending",
  },
  handed_off: {
    label: "Handed off",
    description: "Auto-merge is armed or GitHub has queued the merge",
    tone: "pending",
  },
  draft: {
    label: "Drafts",
    description: "Not ready for review",
    tone: "neutral",
  },
  done: {
    label: "Done",
    description: "Merged or closed",
    tone: "merged",
  },
};

function PullRequestList({
  items,
  grouping,
  selectedId,
  busyId,
  onSelect,
  onMerge,
  scrollRef,
}: {
  items: CodeDeliveryPullRequestSummary[];
  grouping: PullRequestGrouping;
  selectedId: string | null;
  busyId: string | null;
  onSelect: (id: string) => void;
  onMerge: (item: CodeDeliveryPullRequestSummary) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  const rows = useMemo(
    () => groupedPullRequestRows(items, grouping),
    [grouping, items],
  );
  return (
    <div role="list" aria-label="Pull requests" className="min-w-[1040px]">
      <div
        className={cn(
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-xs font-medium text-muted-foreground backdrop-blur",
          PR_GRID,
        )}
      >
        <span>Pull request</span>
        <span>Status</span>
        <span>Checks</span>
        <span>Comments</span>
        <span>Action</span>
        <span className="text-right">Updated</span>
      </div>
      <VirtualRows
        items={rows}
        scrollRef={scrollRef}
        estimateSize={(item) =>
          item.kind === "group" ? PR_GROUP_HEIGHT : PR_ROW_HEIGHT
        }
      >
        {(entry) =>
          entry.kind === "group" ? (
            <PullRequestGroupHeader {...entry} />
          ) : (
            <PullRequestRow
              item={entry.row.item}
              depth={entry.row.depth}
              stackedOn={entry.row.stackedOn}
              showRepository={entry.showRepository}
              active={selectedId === entry.row.item.id}
              busy={busyId === entry.row.item.id}
              hasMergeQueue={deliveryRepositoryHasMergeQueue(
                items,
                entry.row.item.repository,
              )}
              onSelect={() => onSelect(entry.row.item.id)}
              onMerge={() => onMerge(entry.row.item)}
            />
          )
        }
      </VirtualRows>
    </div>
  );
}

function groupedPullRequestRows(
  items: readonly CodeDeliveryPullRequestSummary[],
  grouping: PullRequestGrouping,
): PullRequestListItem[] {
  if (grouping === "none") {
    return arrangeStackLanes(items).map((row) => ({
      id: row.id,
      kind: "pull_request",
      row,
      showRepository: true,
    }));
  }

  if (grouping === "repository") {
    const repositories = new Map<
      string,
      { label: string; items: CodeDeliveryPullRequestSummary[] }
    >();
    for (const item of items) {
      const key = codeDeliveryRepositoryKey(item.repository);
      const group = repositories.get(key) ?? {
        label: item.repository.name_with_owner,
        items: [],
      };
      group.items.push(item);
      repositories.set(key, group);
    }
    return [...repositories.entries()].flatMap(([key, group]) => {
      const attention = group.items.filter(
        (item) => prStatus(item).group === "attention",
      ).length;
      return pullRequestGroupRows({
        key: `repository:${key}`,
        label: group.label,
        description:
          attention > 0
            ? `${attention} ${attention === 1 ? "pull request needs" : "pull requests need"} attention`
            : "No pull requests need attention",
        tone: attention > 0 ? "critical" : "neutral",
        rows: arrangeStackLanes(group.items),
        showRepository: false,
      });
    });
  }

  const groups = new Map<PullRequestListGroup, StackedRow[]>();
  for (const lane of pullRequestStackLanes(items)) {
    const group = lane.reduce<PullRequestListGroup>((mostUrgent, row) => {
      const candidate = prStatus(row.item).group;
      return (PULL_REQUEST_GROUP_RANK.get(candidate) ??
        Number.MAX_SAFE_INTEGER) <
        (PULL_REQUEST_GROUP_RANK.get(mostUrgent) ?? Number.MAX_SAFE_INTEGER)
        ? candidate
        : mostUrgent;
    }, "done");
    const grouped = groups.get(group) ?? [];
    grouped.push(...lane);
    groups.set(group, grouped);
  }
  return PULL_REQUEST_GROUP_ORDER.flatMap((group) => {
    const grouped = groups.get(group);
    if (!grouped?.length) return [];
    const meta = PULL_REQUEST_GROUP_META[group];
    return pullRequestGroupRows({
      key: `attention:${group}`,
      ...meta,
      rows: grouped,
      showRepository: true,
    });
  });
}

/** Keep a stack in one attention group so its indentation still explains it. */
function pullRequestStackLanes(
  items: readonly CodeDeliveryPullRequestSummary[],
): StackedRow[][] {
  const lanes: StackedRow[][] = [];
  for (const row of arrangeStackLanes(items)) {
    if (row.depth === 0 || lanes.length === 0) lanes.push([row]);
    else lanes[lanes.length - 1]!.push(row);
  }
  return lanes;
}

function pullRequestGroupRows({
  key,
  label,
  description,
  tone,
  rows,
  showRepository,
}: {
  key: string;
  label: string;
  description: string;
  tone: StatusTone;
  rows: readonly StackedRow[];
  showRepository: boolean;
}): PullRequestListItem[] {
  return [
    {
      id: `group:${key}`,
      kind: "group",
      label,
      description,
      tone,
      count: rows.length,
    },
    ...rows.map(
      (row): PullRequestListItem => ({
        id: row.id,
        kind: "pull_request",
        row,
        showRepository,
      }),
    ),
  ];
}

function PullRequestGroupHeader({
  label,
  description,
  tone,
  count,
}: {
  label: string;
  description: string;
  tone: StatusTone;
  count: number;
}) {
  return (
    <div
      data-pull-request-group={label}
      className="flex items-center gap-2 border-b border-border-subtle bg-muted/20 px-5 py-2.5 text-xs"
    >
      <span
        className={cn("size-1.5 shrink-0 rounded-full", STATUS_DOT[tone])}
        aria-hidden
      />
      <span className="font-semibold text-foreground">{label}</span>
      <span className="truncate text-muted-foreground">{description}</span>
      <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">
        {count}
      </span>
    </div>
  );
}

function PullRequestRow({
  item,
  depth,
  stackedOn,
  showRepository,
  active,
  busy,
  hasMergeQueue,
  onSelect,
  onMerge,
}: {
  item: CodeDeliveryPullRequestSummary;
  depth: number;
  stackedOn?: number;
  showRepository: boolean;
  active: boolean;
  busy: boolean;
  hasMergeQueue: boolean;
  onSelect: () => void;
  onMerge: () => void;
}) {
  const status = prStatus(item);
  const lifecycle = status.lifecycle;
  const checks = checkSummary(checkCounts(item));
  const comments = item.comment_count;
  const mergeAction = item.head_sha
    ? prDirectMergeAction(deliveryPullRequestDigest(item), { hasMergeQueue })
    : null;
  return (
    <div
      role="listitem"
      tabIndex={0}
      data-active={active || undefined}
      data-depth={depth}
      data-status-group={status.group}
      className={cn(
        "grid w-full cursor-pointer gap-4 border-b border-border-subtle px-5 py-3 text-left transition-colors hover:bg-muted/35 data-[active]:bg-muted/55",
        PR_GRID,
      )}
      onClick={onSelect}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect();
        }
      }}
    >
      <span
        className="min-w-0"
        style={depth > 0 ? { paddingLeft: depth * 16 } : undefined}
      >
        <span className="flex min-w-0 items-center gap-2">
          {depth > 0 && (
            <CornerDownRight
              className="size-3.5 shrink-0 text-muted-foreground/70"
              aria-label={`Stacked on the pull request above, level ${depth}`}
            />
          )}
          <PrLifecycleIcon
            lifecycle={lifecycle}
            className={cn("size-4", STATUS_MARK[status.headline.tone])}
          />
          <span className="sr-only">{status.headline.label}:</span>
          <span className="truncate text-sm font-medium">{item.title}</span>
        </span>
        <span className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          {item.author && (
            // Leading the metadata line rather than taking a column: the
            // avatars line up as their own strip down the list, and the table
            // keeps the width it has.
            <>
              <GithubAvatar
                login={item.author}
                url={item.author_avatar_url}
                className="size-4"
              />
              {/* The login holds its width while the repository and branch
                  give theirs up: a login truncated to one letter identifies
                  nobody, and those two read fine clipped. */}
              <span className="max-w-32 shrink-0 truncate">{item.author}</span>
              <span className="shrink-0" aria-hidden>
                ·
              </span>
            </>
          )}
          {showRepository && (
            <span className="truncate">{item.repository.name_with_owner}</span>
          )}
          <span className="tabular-nums">#{item.number}</span>
          <span className="truncate font-mono">{item.head_branch}</span>
          {stackedOn !== undefined && (
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-2xs tabular-nums">
              Stacked on #{stackedOn}
            </span>
          )}
          {item.unregistered_stack_numbers !== undefined && (
            <span
              className="text-info-foreground-muted shrink-0 rounded bg-info-background px-1.5 py-0.5 text-2xs"
              title="This chain is not registered as a GitHub stack. Create the stack on the pull request page so GitHub owns the ordering and the whole-chain merge."
            >
              Unregistered stack
            </span>
          )}
          {item.workspace_links.length > 0 && (
            <span className="shrink-0 rounded bg-info-background px-1.5 py-0.5 text-2xs text-info-foreground-muted">
              Tidebreak
            </span>
          )}
        </span>
      </span>
      <span className="flex items-center">
        <span
          className={cn(
            "text-xs font-medium",
            STATUS_TEXT[status.headline.tone],
          )}
        >
          {status.headline.label}
        </span>
      </span>
      <span className="flex items-center">
        <span className={cn("text-xs", STATUS_TEXT[checks.tone])}>
          {checks.label}
        </span>
      </span>
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <MessageSquare className="size-3.5 shrink-0" />
        <span className="tabular-nums">
          {comments === undefined
            ? "—"
            : comments === 0
              ? "None"
              : `${comments} ${comments === 1 ? "comment" : "comments"}`}
        </span>
      </span>
      <span className="flex items-center">
        {mergeAction ? (
          <Button
            type="button"
            size="xs"
            variant={mergeAction.kind === "merge" ? "default" : "outline"}
            disabled={busy}
            onClick={(event) => {
              event.stopPropagation();
              onMerge();
            }}
          >
            {busy && <LoaderCircle className="animate-spin" />}
            {mergeAction.label}
          </Button>
        ) : null}
      </span>
      <span className="flex items-center justify-end text-xs text-muted-foreground">
        {relativeTime(item.updated_at)}
      </span>
    </div>
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
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-xs font-medium text-muted-foreground backdrop-blur",
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

export function RunDetailSheet({
  summary,
  initialDetail,
  onClose,
  onChanged,
  onOpenWorkspace,
}: {
  summary: CodeDeliveryRunSummary;
  initialDetail?: CodeDeliveryRunDetail;
  onClose: () => void;
  onChanged: () => void;
  onOpenWorkspace: (workspaceId: string) => void;
}) {
  const { client } = useApp();
  const [detail, setDetail] = useState<CodeDeliveryRunDetail | null>(
    initialDetail ?? null,
  );
  const [loading, setLoading] = useState(!initialDetail);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"all" | "failed" | null>(null);
  const generation = useRef(0);
  const activeTarget = useRef(summary.id);
  const mounted = useRef(true);
  activeTarget.current = summary.id;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const targetIsActive = (targetId: string) =>
    mounted.current && activeTarget.current === targetId;

  const load = async () => {
    const targetId = summary.id;
    const token = ++generation.current;
    setLoading(true);
    setError(null);
    try {
      const next = await client.getCodeDeliveryRunDetail({
        repository: codeDeliveryRepositoryTarget(summary.repository),
        kind: summary.kind,
        id: summary.github_id,
      });
      if (token === generation.current && targetIsActive(targetId)) {
        setDetail(next);
      }
    } catch (caught) {
      if (token === generation.current && targetIsActive(targetId)) {
        setError(friendlyErrorMessage(caught, "Could not load this run."));
      }
    } finally {
      if (token === generation.current && targetIsActive(targetId)) {
        setLoading(false);
      }
    }
  };

  useEffect(() => {
    if (initialDetail?.summary.id === summary.id) {
      setDetail(initialDetail);
      setLoading(false);
    } else {
      setDetail(null);
      void load();
    }
    return () => {
      generation.current += 1;
    };
  }, [client, initialDetail, summary.id]);

  const rerun = async (kind: "all" | "failed") => {
    if (busy) return;
    const targetId = summary.id;
    setBusy(kind);
    const action: CodeDeliveryRunAction = {
      type: kind === "all" ? "rerun" : "rerun_failed",
    };
    try {
      const result = await client.runCodeDeliveryRunAction({
        target: {
          repository: codeDeliveryRepositoryTarget(summary.repository),
          kind: summary.kind,
          id: summary.github_id,
        },
        action,
      });
      if (!targetIsActive(targetId)) return;
      if (result.success) {
        toast.success(result.message);
      } else {
        toast.warning(result.message);
      }
      onChanged();
      await load();
    } catch (caught) {
      if (!targetIsActive(targetId)) return;
      toast.error(
        friendlyErrorMessage(
          caught,
          kind === "all"
            ? "Could not rerun this workflow."
            : "Could not rerun failed jobs.",
        ),
      );
    } finally {
      if (targetIsActive(targetId)) setBusy(null);
    }
  };

  return (
    <DetailSheet
      label={`${summary.kind === "deployment" ? "Deployment" : "Action"}: ${summary.name}`}
      onClose={onClose}
    >
      <div className="flex shrink-0 items-start gap-3 border-b border-border-subtle px-5 py-3">
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
      <div className="min-h-0 flex-1 overflow-auto p-5">
        {loading && !detail ? (
          <DetailSkeleton />
        ) : error ? (
          <InlineLoadError message={error} onRetry={() => void load()} />
        ) : detail ? (
          <div className="flex flex-col gap-5">
            {detail.errors.length > 0 && (
              <PartialErrorBanner errors={detail.errors} compact />
            )}
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
              {detail.summary.kind === "workflow_run" &&
                detail.summary.status === "completed" && (
                  <Button
                    type="button"
                    size="sm"
                    variant="outline"
                    disabled={Boolean(busy)}
                    onClick={() => void rerun("all")}
                  >
                    {busy === "all" ? (
                      <LoaderCircle className="animate-spin" />
                    ) : (
                      <RefreshCw />
                    )}
                    Rerun all
                  </Button>
                )}
              {detail.can_rerun_failed && (
                <Button
                  type="button"
                  size="sm"
                  disabled={Boolean(busy)}
                  onClick={() => void rerun("failed")}
                >
                  {busy === "failed" ? (
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
                          <span className="mt-1 block text-xs text-critical">
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
                        <span className="text-xs text-muted-foreground">
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
    </DetailSheet>
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
            <span className="rounded-full bg-primary px-1.5 text-2xs text-primary-foreground">
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
        <FilterSection title="Authors">
          <AuthorFilterOptions
            noun="author"
            emptyNote="Authors appear here as pull requests load. Type a login to filter by hand."
            selected={filters.authors}
            onChange={(authors) => onChange({ ...filters, authors })}
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
            <span className="rounded-full bg-primary px-1.5 text-2xs text-primary-foreground">
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
        <FilterSection title="Actors">
          <AuthorFilterOptions
            noun="actor"
            emptyNote="Actors appear here as runs load. Type a login to filter by hand."
            selected={filters.actors}
            onChange={(actors) => onChange({ ...filters, actors })}
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
      <legend className="mb-2 text-xs font-medium text-muted-foreground">
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

/**
 * Login selection without the memory test: the checkable pool is every login
 * Delivery has seen on a pull request or run, drawn with avatars, and the
 * search box narrows it. A login the pool has never seen — a teammate who has
 * not pushed lately, a bot — can still be typed and added by hand, which is
 * all the old free-text field could do.
 */
function AuthorFilterOptions({
  noun,
  emptyNote,
  selected,
  onChange,
}: {
  noun: "author" | "actor";
  emptyNote: string;
  selected: string[];
  onChange: (selected: string[]) => void;
}) {
  const knownAuthors = useCodeDeliveryStore((state) => state.knownAuthors);
  const [query, setQuery] = useState("");
  const trimmed = query.trim();
  const isSelected = (login: string) =>
    selected.some((entry) => entry.toLowerCase() === login.toLowerCase());

  const options = useMemo(() => {
    // Selected logins stay listed even when the pool has never seen them —
    // a saved view's author must be visible to be uncheckable.
    const byKey = new Map<string, CodeDeliveryAuthor>();
    for (const login of selected) byKey.set(login.toLowerCase(), { login });
    for (const author of knownAuthors) {
      const key = author.login.toLowerCase();
      const existing = byKey.get(key);
      if (!existing) byKey.set(key, author);
      else if (author.avatarUrl && !existing.avatarUrl) {
        byKey.set(key, { ...existing, avatarUrl: author.avatarUrl });
      }
    }
    const needle = trimmed.toLowerCase();
    const chosen = new Set(selected.map((entry) => entry.toLowerCase()));
    return [...byKey.values()]
      .filter(
        (author) => !needle || author.login.toLowerCase().includes(needle),
      )
      .sort((left, right) => {
        const bySelection =
          Number(chosen.has(right.login.toLowerCase())) -
          Number(chosen.has(left.login.toLowerCase()));
        if (bySelection !== 0) return bySelection;
        return left.login.localeCompare(right.login);
      });
  }, [knownAuthors, selected, trimmed]);

  const toggle = (login: string, enabled: boolean) => {
    const rest = selected.filter(
      (entry) => entry.toLowerCase() !== login.toLowerCase(),
    );
    onChange(enabled ? [...rest, login] : rest);
  };

  const exactMatchListed = options.some(
    (author) => author.login.toLowerCase() === trimmed.toLowerCase(),
  );
  const addTyped = () => {
    if (!trimmed) return;
    toggle(trimmed, true);
    setQuery("");
  };

  return (
    <div className="flex flex-col gap-1.5">
      <Input
        value={query}
        placeholder={`Search ${noun}s or type a login`}
        aria-label={`Search ${noun}s`}
        onChange={(event) => setQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key !== "Enter") return;
          event.preventDefault();
          if (!exactMatchListed) addTyped();
          else if (trimmed) {
            toggle(trimmed, !isSelected(trimmed));
            setQuery("");
          }
        }}
      />
      {options.length > 0 && (
        <div className="flex max-h-36 flex-col gap-1 overflow-auto pr-1">
          {options.map((author) => (
            <label
              key={author.login.toLowerCase()}
              className="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 text-xs hover:bg-muted/40"
            >
              <Checkbox
                checked={isSelected(author.login)}
                onCheckedChange={(checked) =>
                  toggle(author.login, checked === true)
                }
              />
              <GithubAvatar login={author.login} url={author.avatarUrl} />
              <span className="min-w-0 truncate">{author.login}</span>
            </label>
          ))}
        </div>
      )}
      {options.length === 0 && !trimmed && (
        <p className="text-xs text-muted-foreground">{emptyNote}</p>
      )}
      {trimmed && !exactMatchListed && (
        <button
          type="button"
          className="cursor-pointer rounded-md px-1.5 py-1 text-left text-xs text-muted-foreground hover:bg-muted/40 hover:text-foreground"
          onClick={addTyped}
        >
          Filter by “{trimmed}”
        </button>
      )}
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
      <Label className="text-xs text-muted-foreground">{label}</Label>
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
  const [triggerRepository, setTriggerRepository] =
    useState<CodeGitHubRepositoryRef | null>(null);

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
          <DialogTitle>Tracked repositories</DialogTitle>
        </DialogHeader>
        <div className="flex max-h-[65vh] flex-col gap-5 overflow-auto pr-1">
          <section>
            <div className="flex items-center justify-between gap-2">
              <div>
                <h3 className="text-sm font-medium">Registered in Tidebreak</h3>
                <p className="text-xs text-muted-foreground">
                  GitHub repos are tracked automatically. Disable any you do not
                  want listed here.
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
                      onManageTriggers={
                        repository.tidebreak_repo_id
                          ? () => setTriggerRepository(repository)
                          : undefined
                      }
                    />
                  );
                })
              )}
            </div>
            {triggerRepository && (
              <div className="mt-4">
                <RepositoryTriggerRules
                  client={client}
                  repository={triggerRepository}
                />
              </div>
            )}
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
  onManageTriggers,
}: {
  repository: CodeGitHubRepositoryRef;
  enabled: boolean;
  pinned: boolean;
  manual?: boolean;
  onEnabledChange: (enabled: boolean) => void;
  onPinnedChange: (pinned: boolean) => void;
  onRemove?: () => void;
  onManageTriggers?: () => void;
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
        <p className="truncate text-xs text-muted-foreground">
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
      {onManageTriggers && (
        <Button
          type="button"
          size="xs"
          variant="outline"
          onClick={onManageTriggers}
        >
          Triggers
        </Button>
      )}
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
    <div className="flex items-center justify-between gap-3 px-5 py-1.5 text-xs text-muted-foreground">
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

function deliveryRefreshErrors(
  errors: readonly CodeDeliverySourceError[],
): CodeDeliverySourceError[] {
  return errors.filter((error) => error.kind !== "not_github");
}

function refreshErrorSummary(
  errors: readonly CodeDeliverySourceError[],
): string {
  if (errors.length === 1) return errors[0]!.message;
  const names = errors.flatMap((error) =>
    error.repository
      ? [`${error.repository.owner}/${error.repository.name}`]
      : [],
  );
  if (names.length === errors.length) {
    return `${errors.length} repositories could not be refreshed (${names.join(", ")}). Available results are still shown.`;
  }
  return `${errors.length} repositories could not be refreshed. Available results are still shown.`;
}

function PartialErrorBanner({
  errors,
  compact = false,
}: {
  errors: CodeDeliverySourceError[];
  compact?: boolean;
}) {
  const visible = deliveryRefreshErrors(errors);
  if (visible.length === 0) return null;
  return (
    <div
      role="status"
      className={cn(
        "flex shrink-0 items-start gap-2 border-b border-warning-border bg-warning-background px-5 py-2.5 text-xs text-warning-foreground-muted",
        compact && "border-t",
      )}
    >
      <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
      <span>{refreshErrorSummary(visible)}</span>
    </div>
  );
}

function RepositoryRefreshWarning({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void;
}) {
  return (
    <div className="flex shrink-0 items-center justify-between gap-3 border-b border-warning-border bg-warning-background px-5 py-2.5 text-xs text-warning-foreground-muted">
      <span>GitHub repository discovery is stale: {message}</span>
      <Button type="button" size="xs" variant="outline" onClick={onRetry}>
        Try again
      </Button>
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
      <span className="sr-only">Loading</span>
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
      <dt className="text-xs text-muted-foreground">{label}</dt>
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
        "rounded-md px-2 py-1 text-xs font-medium",
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
  required?: CodeGitHubRepositoryTarget,
): CodeGitHubRepositoryTarget[] {
  const keys = new Set(selected);
  const targets = repositories
    .filter(
      (repository) =>
        keys.size === 0 || keys.has(codeDeliveryRepositoryKey(repository)),
    )
    .map(codeDeliveryRepositoryTarget);
  if (
    required &&
    !targets.some(
      (target) =>
        codeDeliveryRepositoryKey(target) ===
        codeDeliveryRepositoryKey(required),
    )
  ) {
    targets.push(required);
  }
  return targets;
}

function pullRequestMatchesTarget(
  item: CodeDeliveryPullRequestSummary,
  target: CodeDeliveryPullRequestTarget,
): boolean {
  return (
    item.number === target.number &&
    codeDeliveryRepositoryKey(item.repository) ===
      codeDeliveryRepositoryKey(target.repository)
  );
}

function runMatchesTarget(
  item: CodeDeliveryRunSummary,
  target: CodeDeliveryRunTarget,
): boolean {
  return (
    item.kind === target.kind &&
    item.github_id === target.id &&
    codeDeliveryRepositoryKey(item.repository) ===
      codeDeliveryRepositoryKey(target.repository)
  );
}

function positiveSearchInteger(value: unknown): number | undefined {
  const parsed =
    typeof value === "number"
      ? value
      : typeof value === "string" && value.trim() !== ""
        ? Number(value)
        : Number.NaN;
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : undefined;
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

/**
 * A built-in view's filters, with the viewer view pointed at the signed-in
 * login. Resolving to a plain `authors` entry is what keeps the author chip,
 * the filter count, and a saved copy of the view all reading the same thing.
 */
function builtInPrFilters(
  view: PrBuiltInView,
  viewerLogin: string | undefined,
): CodeDeliveryPrViewFilters {
  const filters = clonePrFilters(view.filters);
  if (view.viewerAuthored && viewerLogin) filters.authors = [viewerLogin];
  return filters;
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

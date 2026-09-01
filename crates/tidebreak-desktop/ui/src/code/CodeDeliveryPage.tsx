import { Button } from "@/components/ui/button";
import {
  type CodeDeliveryPrViewFilters,
  type CodeDeliveryRunViewFilters,
  type CodeDeliverySavedView,
  type CodeDeliverySurface,
  trackedCodeDeliveryRepositories,
  useCodeDeliveryStore,
} from "./CodeDeliveryStore";
import type {
  CodeDeliveryRunKind,
  CodeDeliverySourceError,
  CodeGitHubRepositoryTarget,
} from "../api/types";
import { CodeSidebar } from "./CodeSidebar";
import { DeliveryRepositoriesDialog } from "./delivery/DeliveryRepositoriesDialog";
import { GitPullRequest, Save, Settings2, Workflow } from "lucide-react";
import {
  PR_BUILT_IN_VIEWS,
  type PullRequestGrouping,
  RUN_BUILT_IN_VIEWS,
} from "./delivery/views";
import {
  deliveryRefreshErrors,
  PartialErrorBanner,
  RepositoryRefreshWarning,
} from "./delivery/status";
import { PullRequestFilters, RunFilters } from "./delivery/filters";
import { PullRequestsSurface } from "./delivery/PullRequestsSurface";
import { RouteFrame } from "@/RouteFrame";
import { RunsSurface } from "./delivery/RunsSurface";
import { SaveViewDialog } from "./delivery/SaveViewDialog";
import { SearchInput } from "@/components/SearchInput";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  builtInPrFilters,
  clonePrFilters,
  cloneRunFilters,
  positiveSearchInteger,
} from "./delivery/helpers";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { toast } from "sonner";
import { useApp } from "@/AppContext";
import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
export { RunDetailSheet } from "./delivery/RunDetailSheet";

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
          aria-label="Pull requests and runs"
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

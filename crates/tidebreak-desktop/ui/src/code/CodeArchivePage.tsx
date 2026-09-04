import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import {
  Archive,
  ExternalLink,
  GitBranch,
  GitPullRequest,
  LoaderCircle,
  MessageSquareText,
  RefreshCw,
  RotateCcw,
} from "lucide-react";
import { toast } from "sonner";

import { useApp } from "@/AppContext";
import type { CodeWorkspaceHistorySearchMatch } from "@/api/types";
import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { friendlyErrorMessage } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import { RouteFrame } from "@/RouteFrame";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeSidebar } from "./CodeSidebar";
import { listArchivedWorkspaces, isPutAway } from "./workspaceCards";

type AgeFilter = "all" | "7d" | "30d" | "90d";

type HistorySearchState = {
  query: string;
  matches: CodeWorkspaceHistorySearchMatch[];
  loading: boolean;
  error: string | null;
  truncated: boolean;
};

const EMPTY_HISTORY_SEARCH: HistorySearchState = {
  query: "",
  matches: [],
  loading: false,
  error: null,
  truncated: false,
};

export function CodeArchivePage() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeArchiveBody />
      </div>
    </RouteFrame>
  );
}

function CodeArchiveBody() {
  const { client } = useApp();
  const navigate = useNavigate();
  const repos = useCodeCatalogStore((state) => state.repos);
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const loaded = useCodeCatalogStore((state) => state.loaded);
  const error = useCodeCatalogStore((state) => state.error);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const upsertWorkspace = useCodeCatalogStore((state) => state.upsertWorkspace);
  const [search, setSearch] = useState("");
  const [repoId, setRepoId] = useState("all");
  const [age, setAge] = useState<AgeFilter>("all");
  const [restoring, setRestoring] = useState<string | null>(null);
  const [historySearch, setHistorySearch] =
    useState<HistorySearchState>(EMPTY_HISTORY_SEARCH);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  const archiveCandidates = useMemo(() => {
    const cutoff = archiveCutoff(age);
    return listArchivedWorkspaces(workspaces).filter((workspace) => {
      if (repoId !== "all" && workspace.repo_id !== repoId) return false;
      const archivedAt = Date.parse(
        workspace.archived_at ?? workspace.created_at,
      );
      if (cutoff !== null && archivedAt < cutoff) return false;
      return true;
    });
  }, [age, repoId, workspaces]);

  const historyAnchors = useMemo(() => {
    const seenRepos = new Set<string>();
    return archiveCandidates.filter((workspace) => {
      if (seenRepos.has(workspace.repo_id)) return false;
      seenRepos.add(workspace.repo_id);
      return true;
    });
  }, [archiveCandidates]);

  const trimmedSearch = search.trim();

  useEffect(() => {
    if (!loaded || error || !trimmedSearch || historyAnchors.length === 0) {
      setHistorySearch(EMPTY_HISTORY_SEARCH);
      return;
    }

    let cancelled = false;
    setHistorySearch({
      query: trimmedSearch,
      matches: [],
      loading: true,
      error: null,
      truncated: false,
    });
    const timeout = window.setTimeout(() => {
      void Promise.allSettled(
        historyAnchors.map((workspace) =>
          client.searchCodeWorkspace(workspace.id, {
            query: trimmedSearch,
            history: true,
            limit: 200,
          }),
        ),
      ).then((results) => {
        if (cancelled) return;
        const matches: CodeWorkspaceHistorySearchMatch[] = [];
        let truncated = false;
        let failureCount = 0;
        let firstFailure: unknown;
        for (const result of results) {
          if (result.status === "rejected") {
            failureCount += 1;
            firstFailure ??= result.reason;
            continue;
          }
          matches.push(...(result.value.history_matches ?? []));
          truncated ||= result.value.truncated;
        }
        setHistorySearch({
          query: trimmedSearch,
          matches,
          loading: false,
          error:
            failureCount === 0
              ? null
              : failureCount === results.length
                ? friendlyErrorMessage(
                    firstFailure,
                    "Could not search conversations.",
                  )
                : "Some conversations could not be searched.",
          truncated,
        });
      });
    }, 250);

    return () => {
      cancelled = true;
      window.clearTimeout(timeout);
    };
  }, [client, error, historyAnchors, loaded, trimmedSearch]);

  const activeHistorySearch =
    historySearch.query === trimmedSearch
      ? historySearch
      : {
          ...EMPTY_HISTORY_SEARCH,
          loading:
            loaded &&
            !error &&
            Boolean(trimmedSearch) &&
            historyAnchors.length > 0,
        };

  const historyMatchesByWorkspace = useMemo(() => {
    const allowedWorkspaceIds = new Set(
      archiveCandidates.map((workspace) => workspace.id),
    );
    const seenSessions = new Set<string>();
    const grouped = new Map<string, CodeWorkspaceHistorySearchMatch[]>();
    const matches = [...activeHistorySearch.matches].sort(
      (left, right) =>
        searchTimestamp(right.created_at) - searchTimestamp(left.created_at),
    );
    for (const match of matches) {
      if (!allowedWorkspaceIds.has(match.workspace_id)) continue;
      const sessionKey = `${match.workspace_id}\0${match.session_id}`;
      if (seenSessions.has(sessionKey)) continue;
      seenSessions.add(sessionKey);
      const workspaceMatches = grouped.get(match.workspace_id);
      if (workspaceMatches) {
        workspaceMatches.push(match);
      } else {
        grouped.set(match.workspace_id, [match]);
      }
    }
    return grouped;
  }, [activeHistorySearch.matches, archiveCandidates]);

  const archived = useMemo(() => {
    const query = trimmedSearch.toLocaleLowerCase();
    return archiveCandidates.filter((workspace) => {
      if (!query) return true;
      const repo = repos.find(
        (candidate) => candidate.id === workspace.repo_id,
      );
      const metadataMatches = [
        workspace.title,
        workspace.branch_name,
        workspace.worktree_path,
        repo?.display_name ?? "",
      ]
        .join(" ")
        .toLocaleLowerCase()
        .includes(query);
      return metadataMatches || historyMatchesByWorkspace.has(workspace.id);
    });
  }, [archiveCandidates, historyMatchesByWorkspace, repos, trimmedSearch]);

  const restore = async (workspaceId: string) => {
    if (restoring) return;
    setRestoring(workspaceId);
    try {
      const restored = await client.restoreCodeWorkspace(workspaceId);
      upsertWorkspace(restored);
      toast.success("Workspace restored");
    } catch (caught) {
      toast.error(
        friendlyErrorMessage(caught, "Could not restore that workspace."),
      );
    } finally {
      setRestoring(null);
    }
  };

  const totalArchived = workspaces.filter((workspace) =>
    isPutAway(workspace),
  ).length;

  return (
    <div className="flex size-full min-h-0 flex-col bg-background">
      <header className="shrink-0 border-b border-border-subtle px-5 py-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">Archive</h1>
              {loaded && (
                <span className="text-xs text-muted-foreground">
                  {totalArchived} workspace{totalArchived === 1 ? "" : "s"}
                </span>
              )}
            </div>
            <p className="mt-0.5 text-sm text-muted-foreground">
              Search old work, inspect its pull request, or restore it.
            </p>
          </div>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => void refresh(client)}
          >
            <RefreshCw />
            Refresh
          </Button>
        </div>
      </header>

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border-subtle px-5 py-3">
        <SearchInput
          size="sm"
          value={search}
          onValueChange={setSearch}
          placeholder="Search workspaces and conversations…"
          className="min-w-56 flex-1 md:max-w-md"
        />
        <Select value={repoId} onValueChange={setRepoId}>
          <SelectTrigger size="sm" className="w-44">
            <SelectValue placeholder="All repositories" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All repositories</SelectItem>
            {repos.map((repo) => (
              <SelectItem key={repo.id} value={repo.id}>
                {repo.display_name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={age}
          onValueChange={(value) => setAge(value as AgeFilter)}
        >
          <SelectTrigger size="sm" className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">Any time</SelectItem>
            <SelectItem value="7d">Last 7 days</SelectItem>
            <SelectItem value="30d">Last 30 days</SelectItem>
            <SelectItem value="90d">Last 90 days</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {(activeHistorySearch.loading ||
        activeHistorySearch.error ||
        activeHistorySearch.truncated) && (
        <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-1 border-b border-border-subtle px-5 py-2 text-xs">
          {activeHistorySearch.loading && (
            <span
              role="status"
              className="flex items-center gap-1.5 text-muted-foreground"
            >
              <LoaderCircle className="size-3.5 animate-spin" />
              Searching conversations…
            </span>
          )}
          {activeHistorySearch.error && (
            <span role="alert" className="text-critical-foreground-muted">
              {activeHistorySearch.error} Workspace matches are still shown.
            </span>
          )}
          {activeHistorySearch.truncated && (
            <span className="text-warning-foreground">
              Conversation results were truncated. Narrow the search.
            </span>
          )}
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-auto">
        {!loaded ? (
          <ArchiveSkeleton />
        ) : error ? (
          <div className="notice-surface notice-critical m-5 rounded-lg border px-3 py-2 text-sm">
            {error}
          </div>
        ) : totalArchived === 0 ? (
          <Empty className="min-h-80">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Archive />
              </EmptyMedia>
              <EmptyTitle>No archived workspaces</EmptyTitle>
              <EmptyDescription>
                Archived workspaces leave the main rail and collect here.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : activeHistorySearch.loading && archived.length === 0 ? (
          <Empty className="min-h-72">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <LoaderCircle className="animate-spin" />
              </EmptyMedia>
              <EmptyTitle>Searching conversations</EmptyTitle>
              <EmptyDescription>
                Checking archived session history for this search.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : archived.length === 0 ? (
          <Empty className="min-h-72">
            <EmptyHeader>
              <EmptyTitle>No archive results</EmptyTitle>
              <EmptyDescription>
                Change the search, repository, or time filter.
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <div
            role="list"
            aria-label="Archived workspaces"
            className="min-w-[760px]"
          >
            <div className="sticky top-0 z-10 grid grid-cols-[minmax(260px,1fr)_170px_150px_180px] gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-xs font-medium text-muted-foreground backdrop-blur">
              <span>Workspace</span>
              <span>Repository</span>
              <span>Archived</span>
              <span className="text-right">Actions</span>
            </div>
            {archived.map((workspace) => {
              const repo = repos.find(
                (candidate) => candidate.id === workspace.repo_id,
              );
              const workspaceHistory =
                historyMatchesByWorkspace.get(workspace.id) ?? [];
              return (
                <div
                  key={workspace.id}
                  role="listitem"
                  className="grid grid-cols-[minmax(260px,1fr)_170px_150px_180px] items-start gap-4 border-b border-border-subtle px-5 py-3"
                >
                  <div className="min-w-0">
                    <button
                      type="button"
                      className="block w-full min-w-0 cursor-pointer rounded-sm text-left hover:text-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      onClick={() =>
                        void navigate({
                          to: "/code/w/$workspaceId",
                          params: { workspaceId: workspace.id },
                        })
                      }
                    >
                      <span className="block truncate text-sm font-medium">
                        {workspace.title}
                      </span>
                      <span className="mt-1 flex items-center gap-1.5 truncate font-mono text-xs text-muted-foreground">
                        <GitBranch className="size-3.5 shrink-0" />
                        {workspace.branch_name}
                      </span>
                    </button>
                    {workspaceHistory.length > 0 && (
                      <div className="mt-2 space-y-1.5">
                        {workspaceHistory.map((match) => (
                          <button
                            key={match.session_id}
                            type="button"
                            aria-label={`Open conversation in ${workspace.title}: ${match.preview}`}
                            className="block w-full rounded-md border border-border-subtle bg-muted/50 px-2 py-1.5 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                            onClick={() =>
                              void navigate({
                                to: "/code/w/$workspaceId",
                                params: { workspaceId: match.workspace_id },
                                search: { task: match.session_id },
                              })
                            }
                          >
                            <span className="flex items-center gap-1.5 text-xs font-medium text-foreground">
                              <MessageSquareText className="size-3.5 shrink-0" />
                              {historySourceLabel(match.source)}
                              <span className="font-normal text-muted-foreground">
                                · {relativeTime(match.created_at)}
                              </span>
                            </span>
                            <span className="mt-0.5 line-clamp-2 block text-xs text-muted-foreground">
                              {match.preview}
                            </span>
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                  <span className="flex min-w-0 items-center truncate text-xs text-muted-foreground">
                    {repo?.display_name ?? workspace.repo_id}
                  </span>
                  <span className="flex items-center text-xs text-muted-foreground">
                    {relativeTime(
                      workspace.archived_at ?? workspace.created_at,
                    )}
                  </span>
                  <span className="flex items-center justify-end gap-2">
                    {workspace.pr?.url && (
                      <Button
                        type="button"
                        size="xs"
                        variant="outline"
                        onClick={() => void openInBrowser(workspace.pr!.url!)}
                      >
                        <GitPullRequest />
                        PR
                      </Button>
                    )}
                    <Button
                      type="button"
                      size="xs"
                      disabled={Boolean(restoring)}
                      onClick={() => void restore(workspace.id)}
                    >
                      {restoring === workspace.id ? (
                        <LoaderCircle className="animate-spin" />
                      ) : (
                        <RotateCcw />
                      )}
                      Restore
                    </Button>
                    <Button
                      type="button"
                      size="icon-xs"
                      variant="ghost"
                      aria-label={`Open ${workspace.title}`}
                      onClick={() =>
                        void navigate({
                          to: "/code/w/$workspaceId",
                          params: { workspaceId: workspace.id },
                        })
                      }
                    >
                      <ExternalLink />
                    </Button>
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function ArchiveSkeleton() {
  return (
    <div className="p-5" role="status">
      <span className="sr-only">Loading archived workspaces</span>
      {Array.from({ length: 6 }, (_, index) => (
        <div
          key={index}
          className="grid grid-cols-[minmax(260px,1fr)_170px_150px_180px] gap-4 border-b border-border-subtle py-3"
        >
          <div className="flex flex-col gap-2">
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-3 w-36" />
          </div>
          <Skeleton className="h-4 w-28" />
          <Skeleton className="h-4 w-20" />
          <Skeleton className="ml-auto h-7 w-32" />
        </div>
      ))}
    </div>
  );
}

function archiveCutoff(age: AgeFilter): number | null {
  if (age === "all") return null;
  const days = age === "7d" ? 7 : age === "30d" ? 30 : 90;
  return Date.now() - days * 24 * 60 * 60 * 1_000;
}

function relativeTime(value: string): string {
  try {
    return formatDistanceToNowStrict(new Date(value), { addSuffix: true });
  } catch {
    return value;
  }
}

function searchTimestamp(value: string): number {
  const timestamp = Date.parse(value);
  return Number.isNaN(timestamp) ? 0 : timestamp;
}

function historySourceLabel(
  source: CodeWorkspaceHistorySearchMatch["source"],
): string {
  switch (source) {
    case "turn_user_input":
      return "Your message";
    case "turn_narrative":
      return "Turn summary";
    case "event":
      return "Conversation";
  }
}

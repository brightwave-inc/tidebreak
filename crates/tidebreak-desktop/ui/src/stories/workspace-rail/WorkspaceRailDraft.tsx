import { useState } from "react";
import { FolderGit2, MessageCircle, Monitor, Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { SegmentedControl } from "@/components/ui/segmented";
import { Skeleton } from "@/components/ui/skeleton";
import { Logomark } from "@/Logomark";
import { cn } from "@/lib/utils";
import { WorkspaceLessSessionRow } from "@/code/WorkspaceLessSessionRow";
import { runningDigest, idleCompleteDigest } from "../fixtures";
import { WorkspaceCard } from "@/code/WorkspaceCard";
import {
  arrangeWorkspaceSections,
  workspaceCollapseKeys,
  type CardDensity,
  type WorkspaceSortMode,
} from "@/code/workspaceCards";
import { WorkspaceRailGroups } from "@/code/WorkspaceRailGroups";
import { WorkspaceRailToolbar } from "@/code/WorkspaceRailToolbar";
import { railEntries, railRepositories } from "./fixtures";

export type RailScenario =
  | "scratch"
  | "scratch-status"
  | "repository"
  | "status"
  | "single-source"
  | "collapsed"
  | "long-names"
  | "empty"
  | "loading"
  | "error";
export function WorkspaceRailDraft({
  scenario,
  onAddRepo,
  onNewWorkspace,
}: {
  scenario: RailScenario;
  onAddRepo: () => void;
  onNewWorkspace: () => void;
}) {
  const [mode, setMode] = useState<WorkspaceSortMode>(
    scenario === "status" || scenario === "scratch-status"
      ? "by-status"
      : "by-repo",
  );
  const [density, setDensity] = useState<CardDensity>(
    scenario === "long-names" ? "compact" : "detailed",
  );
  const [showRepoChip, setShowRepoChip] = useState(true);
  const [showBranch, setShowBranch] = useState(false);
  const [sourceChoice, setSourceChoice] = useState<"mixed" | "slack">(
    scenario === "single-source" ? "slack" : "mixed",
  );
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () =>
      new Set(
        scenario === "collapsed"
          ? ["source:slack", "local:by-repo:repo-model-gateway"]
          : [],
      ),
  );
  const [selected, setSelected] = useState("workspace-grouping");
  const [recovered, setRecovered] = useState(false);
  const state = recovered ? "repository" : scenario;
  const repos = railRepositories.map((repo, index) =>
    scenario === "long-names" && index === 1
      ? { ...repo, display_name: "platform-infrastructure-and-model-gateway" }
      : repo,
  );
  const entries =
    state === "empty"
      ? []
      : railEntries
          .filter(
            (entry) =>
              sourceChoice === "mixed" || entry.session.external_origin,
          )
          .map((entry) =>
            scenario === "long-names"
              ? {
                  ...entry,
                  workspace: {
                    ...entry.workspace,
                    title: `${entry.workspace.title} across every connected machine and repository`,
                  },
                  digest: entry.digest
                    ? {
                        ...entry.digest,
                        title: `${entry.workspace.title} across every connected machine and repository`,
                      }
                    : undefined,
                }
              : entry,
          );
  const sections = arrangeWorkspaceSections(
    mode,
    repos,
    entries.map((entry) => entry.workspace),
    Object.fromEntries(
      entries.map((entry) => [entry.workspace.id, entry.digest]),
    ),
    Object.fromEntries(
      entries.map((entry) => [entry.workspace.id, entry.session]),
    ),
    scenario.startsWith("scratch")
      ? [
          {
            ...runningDigest,
            session: "scratch-running",
            workspace: null,
            title: "Fix the deployment across both repositories",
            external_origin: { channel_kind: "slack", external_key: "T/C/123" },
          },
          {
            ...idleCompleteDigest,
            session: "scratch-done",
            workspace: null,
            title: "Inspect the default branches",
            external_origin: { channel_kind: "slack", external_key: "T/C/124" },
          },
        ]
      : [],
  );
  const multipleSources = sections.length > 1;
  const entryById = new Map(
    entries.map((entry) => [entry.workspace.id, entry]),
  );
  const selectedEntry = entryById.get(selected);
  const collapseKeys = workspaceCollapseKeys(sections, mode);
  function toggle(key: string) {
    setCollapsed((before) => {
      const next = new Set(before);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }
  return (
    <div
      className="flex h-full min-h-0 w-full overflow-hidden"
      data-testid="workspace-rail-draft"
    >
      <aside
        aria-label="Workspace navigation"
        className={cn(
          "flex h-full min-h-0 shrink-0 flex-col border-r border-border-subtle bg-page-background",
          scenario === "long-names" ? "w-[240px]" : "w-[300px] max-sm:w-full",
        )}
      >
        <div className="flex h-12 shrink-0 items-center gap-2.5 px-4">
          <Logomark width="30" height="16" />
          <span className="font-mono text-sm font-medium">Tidebreak</span>
          <span className="ml-auto text-xs text-muted-foreground">Code</span>
        </div>
        <WorkspaceRailToolbar
          className="px-4 pt-2 pb-2"
          collapseKeys={collapseKeys}
          collapsedKeys={[...collapsed]}
          onCollapsedChange={(keys) => setCollapsed(new Set(keys))}
          onAddRepo={onAddRepo}
          onNewWorkspace={onNewWorkspace}
          settings={{
            prefs: { sortMode: mode, density, showRepoChip, showBranch },
            onPrefsChange: (patch) => {
              if (patch.sortMode) setMode(patch.sortMode);
              if (patch.density) setDensity(patch.density);
              if (patch.showRepoChip !== undefined)
                setShowRepoChip(patch.showRepoChip);
              if (patch.showBranch !== undefined)
                setShowBranch(patch.showBranch);
            },
          }}
        >
          <h2 className="min-w-0 flex-1 text-sm font-medium">Workspaces</h2>
        </WorkspaceRailToolbar>
        <div
          className="min-h-0 flex-1 overflow-y-auto px-2 pb-4"
          data-testid="workspace-list"
        >
          {state === "loading" ? (
            <div
              role="status"
              aria-label="Loading workspaces"
              className="space-y-5 px-2 pt-3"
            >
              {[0, 1].map((index) => (
                <div key={index} className="space-y-3">
                  <Skeleton className="h-3 w-20" />
                  {[0, 1, 2].map((row) => (
                    <div key={row} className="space-y-2 pl-5">
                      <Skeleton className="h-3 w-4/5" />
                      <Skeleton className="h-2 w-3/5" />
                    </div>
                  ))}
                </div>
              ))}
            </div>
          ) : state === "error" ? (
            <div
              role="alert"
              className="notice-surface notice-critical mx-1 mt-2 rounded-md px-3 py-3"
            >
              <p className="text-sm font-medium">Workspaces could not load</p>
              <p className="mt-1 text-xs text-muted-foreground">
                Check your connection and try again.
              </p>
              <Button
                size="sm"
                variant="outline"
                className="mt-3"
                onClick={() => setRecovered(true)}
              >
                Try again
              </Button>
            </div>
          ) : entries.length === 0 ? (
            <Empty className="min-h-56 px-3">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <FolderGit2 />
                </EmptyMedia>
                <EmptyTitle>No workspaces yet</EmptyTitle>
                <EmptyDescription>
                  Start a workspace when you are ready to work in a repository.
                </EmptyDescription>
              </EmptyHeader>
              <Button size="sm" variant="outline" onClick={onNewWorkspace}>
                <Plus />
                New workspace
              </Button>
            </Empty>
          ) : (
            <WorkspaceRailGroups
              sections={sections}
              mode={mode}
              collapsedKeys={[...collapsed]}
              onToggle={toggle}
              renderConversation={(digest) => (
                <WorkspaceLessSessionRow
                  key={digest.session}
                  digest={digest}
                  active={selected === digest.session}
                  density={density}
                  onOpen={setSelected}
                />
              )}
              renderWorkspace={(workspace) => {
                const entry = entryById.get(workspace.id)!;
                return (
                  <WorkspaceCard
                    key={workspace.id}
                    workspace={workspace}
                    digest={entry.digest}
                    session={entry.session}
                    repoName={
                      repos.find((repo) => repo.id === workspace.repo_id)
                        ?.display_name ?? workspace.repo_id
                    }
                    active={selected === workspace.id}
                    terminalOpen={false}
                    density={density}
                    visibleMeta={{
                      repoChip: showRepoChip && mode !== "by-repo",
                      branch: showBranch,
                    }}
                    commands={[]}
                    onOpen={() => setSelected(workspace.id)}
                    onCommand={() => {}}
                  />
                );
              }}
            />
          )}
        </div>
        <div className="flex h-9 shrink-0 items-center border-t border-border-subtle px-4 text-xs text-muted-foreground">
          {state === "loading"
            ? "Loading workspaces…"
            : state === "error"
              ? "Connection unavailable"
              : `${entries.length} workspaces`}
          <span className="ml-auto">
            {multipleSources ? "2 sources" : entries.length ? "1 source" : ""}
          </span>
        </div>
      </aside>
      <main className="hidden min-w-0 flex-1 overflow-y-auto bg-background sm:block">
        <header className="flex h-12 items-center border-b border-border-subtle px-6 text-xs text-muted-foreground">
          Storybook draft<span className="ml-auto">Workspace navigation</span>
        </header>
        <div className="mx-auto max-w-xl px-8 pt-12 pb-8">
          <h1 className="text-2xl font-semibold tracking-tight">
            Source, then your grouping
          </h1>
          <p className="mt-3 text-md leading-relaxed text-muted-foreground">
            Local and Slack stay distinct. Each source keeps the repository or
            status headings you choose. With one source, the extra heading
            disappears.
          </p>
          <div className="mt-7 flex flex-col gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              Preview sources
            </span>
            <SegmentedControl
              aria-label="Preview sources"
              value={sourceChoice}
              onValueChange={setSourceChoice}
              options={[
                { value: "mixed", label: "Local + Slack" },
                { value: "slack", label: "Slack only" },
              ]}
            />
          </div>
          <div className="mt-5 flex flex-col gap-2">
            <span className="text-xs font-medium text-muted-foreground">
              Card density
            </span>
            <SegmentedControl
              aria-label="Preview card density"
              value={density}
              onValueChange={setDensity}
              options={[
                { value: "detailed", label: "Detailed" },
                { value: "compact", label: "Compact" },
              ]}
            />
          </div>
          <div className="mt-8 border-t border-border-subtle pt-6">
            <p className="text-xs font-medium text-muted-foreground">
              Selected workspace
            </p>
            <p className="mt-2 text-lg font-medium">
              {selectedEntry?.workspace.title ??
                "Choose a workspace in the list"}
            </p>
            {selectedEntry && (
              <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                {selectedEntry.session.external_origin ? (
                  <MessageCircle className="size-3.5" />
                ) : (
                  <Monitor className="size-3.5" />
                )}
                <span>
                  {selectedEntry.session.external_origin ? "Slack" : "Local"}
                </span>
                <span aria-hidden>·</span>
                <span>
                  {
                    repos.find(
                      (repo) => repo.id === selectedEntry.workspace.repo_id,
                    )?.display_name
                  }
                </span>
              </div>
            )}
          </div>
          <p className="mt-8 text-sm leading-relaxed text-muted-foreground">
            Collapse a source to put that work away. Collapse a repository or
            status to shorten just that group. Counts remain visible, and
            reopening a source preserves the groups you folded inside it.
          </p>
        </div>
      </main>
    </div>
  );
}

import { useEffect } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  ChevronRight,
  CircleAlert,
  FolderGit2,
  GitPullRequest,
  Plus,
  SquareTerminal,
} from "lucide-react";

import { useApp } from "@/AppContext";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { cn } from "@/lib/utils";
import { useLayoutState } from "@/panel/usePanelNav";
import {
  SidebarButton,
  SidebarSectionTitle,
  useSidebarWidth,
} from "@/sidebar/primitives";
import { SidebarFrame } from "@/sidebar/SidebarFrame";
import type {
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { AttentionBadge } from "./AttentionBadge";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeModeSwitch } from "./CodeModeSwitch";
import { useCodeUiStore } from "./CodeUiStore";
import { connectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { HARNESS_ICONS } from "./HarnessPicker";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";
import {
  useWorkspaceCardCommands,
  workspaceCommands,
  type WorkspaceCommand,
} from "./workspaceActions";
import {
  arrangeWorkspaces,
  formatCompactAge,
  isSessionRowWorthy,
  middleTruncate,
  nextWorkspaceSortMode,
  PR_ICON_TONE_CLASSES,
  prTone,
  repoAccentClass,
  sessionRowLabel,
  WORKSPACE_SORT_MODE_LABELS,
} from "./workspaceCards";

/**
 * The code-mode rail: repos, workspace cards, and the Chat/Code switch.
 *
 * Built on the same frame and primitives as the chat rail so the two modes
 * share chrome. It must not touch chat session stores — that separation is
 * what later convergence merges rather than translates.
 *
 * It also hosts code mode's two dialogs. The rail is on screen for every
 * `/code` route, so mounting them here is what makes them reachable from the
 * keyboard anywhere in the mode without a second instance per page.
 */

export function CodeSidebar() {
  const navigate = useNavigate();
  const { client } = useApp();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const isCompact = useSidebarWidth() === "compact";
  const repos = useCodeCatalogStore((state) => state.repos);
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const sessions = useCodeCatalogStore((state) => state.sessionsByWorkspace);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const digests = useCodeUpdatesStore((state) => state.byWorkspace);
  const newWorkspaceOpen = useCodeUiStore((state) => state.newWorkspaceOpen);
  const newWorkspaceRepoId = useCodeUiStore(
    (state) => state.newWorkspaceRepoId,
  );
  const addRepoOpen = useCodeUiStore((state) => state.addRepoOpen);
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);
  const setNewWorkspaceOpen = useCodeUiStore(
    (state) => state.setNewWorkspaceOpen,
  );
  const setAddRepoOpen = useCodeUiStore((state) => state.setAddRepoOpen);
  const sortMode = useCodeUiStore((state) => state.workspaceSortMode);
  const setSortMode = useCodeUiStore((state) => state.setWorkspaceSortMode);
  const { run, dialogs } = useWorkspaceCardCommands();
  const layout = useLayoutState();
  const terminalOpen =
    layout.tabs.some((tab) => tab.type === "terminal") &&
    pathname.startsWith("/code/w/");
  const viewedWorkspaceId = pathname.startsWith("/code/w/")
    ? pathname.slice("/code/w/".length).split("/")[0]
    : undefined;

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  useEffect(() => connectCodeUpdates(client), [client]);

  const groups = arrangeWorkspaces(sortMode, repos, workspaces, digests);
  const sortLabel = WORKSPACE_SORT_MODE_LABELS[sortMode];

  return (
    <SidebarFrame>
      <CodeModeSwitch />

      {!isCompact && (
        <div className="flex items-center justify-between px-2">
          <SidebarSectionTitle className="px-0">Repos</SidebarSectionTitle>
          <button
            type="button"
            className="cursor-pointer rounded-md p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
            aria-label="Add repo"
            onClick={() => setAddRepoOpen(true)}
          >
            <Plus size={14} />
          </button>
        </div>
      )}
      {isCompact && (
        <SidebarButton aria-label="Add repo" onClick={() => setAddRepoOpen(true)}>
          <Plus />
          <span>Add repo</span>
        </SidebarButton>
      )}
      <div className="flex shrink-0 flex-col gap-0.5">
        {repos.map((repo) => {
          const active = pathname === `/code/r/${repo.id}`;
          return (
            <SidebarButton
              key={repo.id}
              aria-current={active ? "page" : undefined}
              data-active={active || undefined}
              className="data-[active]:bg-muted"
              onClick={() =>
                void navigate({
                  to: "/code/r/$repoId",
                  params: { repoId: repo.id },
                })
              }
            >
              <FolderGit2 />
              <span>{repo.display_name}</span>
            </SidebarButton>
          );
        })}
        {repos.length === 0 && !isCompact && (
          <p className="px-2 py-1 text-xs text-muted-foreground">
            No repos registered
          </p>
        )}
      </div>

      {!isCompact && (
        <div className="mt-3 flex items-center justify-between gap-1 px-2">
          <SidebarSectionTitle className="px-0">Workspaces</SidebarSectionTitle>
          <div className="flex items-center">
            <button
              type="button"
              className="cursor-pointer rounded-md px-1.5 py-1 text-[11px] font-medium text-muted-foreground hover:bg-muted hover:text-foreground"
              aria-label={`Sort workspaces: ${sortLabel}. Activate to cycle.`}
              onClick={() => setSortMode(nextWorkspaceSortMode(sortMode))}
            >
              {sortLabel}
            </button>
            <button
              type="button"
              className="cursor-pointer rounded-md p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
              aria-label="New workspace"
              onClick={() => startNewWorkspace()}
            >
              <Plus size={14} />
            </button>
          </div>
        </div>
      )}
      {isCompact && (
        <SidebarButton
          aria-label="New workspace"
          onClick={() => startNewWorkspace()}
        >
          <Plus />
          <span>New workspace</span>
        </SidebarButton>
      )}
      <div className="flex min-h-0 flex-col gap-0.5">
        {isCompact &&
          groups
            .flatMap((group) => group.workspaces)
            .map((workspace) => {
              const active = pathname === `/code/w/${workspace.id}`;
              const digest = digests[workspace.id];
              return (
                <SidebarButton
                  key={workspace.id}
                  aria-current={active ? "page" : undefined}
                  data-active={active || undefined}
                  className="data-[active]:bg-muted"
                  onClick={() =>
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                    })
                  }
                >
                  <FolderGit2 />
                  {/* The digest restates the title on every notice, so a
                      background rename lands here without a catalog refresh. */}
                  <span>{digest?.title ?? workspace.title}</span>
                  <AttentionBadge attention={digest?.attention} compact />
                </SidebarButton>
              );
            })}
        {!isCompact &&
          groups.map((group) => (
            <div key={group.key} className="flex flex-col gap-0.5">
              {group.label && (
                <div className="mt-1 truncate px-2 pt-1 pb-0.5 text-[11px] font-medium text-muted-foreground/90">
                  {group.label}
                </div>
              )}
              {group.workspaces.map((workspace) => {
                const digest = digests[workspace.id];
                const pr = digest?.pr_state ?? workspace.pr;
                return (
                  <WorkspaceCard
                    key={workspace.id}
                    workspace={workspace}
                    digest={digest}
                    session={sessions[workspace.id]}
                    repoName={
                      repos.find((repo) => repo.id === workspace.repo_id)
                        ?.display_name ?? workspace.repo_id
                    }
                    active={pathname === `/code/w/${workspace.id}`}
                    terminalOpen={
                      terminalOpen && viewedWorkspaceId === workspace.id
                    }
                    commands={workspaceCommands({
                      hasPr: Boolean(pr),
                      archived: workspace.status === "archived",
                      hasSession: Boolean(sessions[workspace.id]),
                      attentionPinned:
                        (digest?.attention ?? sessions[workspace.id]?.attention)
                          ?.state.type === "manual",
                    })}
                    onOpen={() =>
                      void navigate({
                        to: "/code/w/$workspaceId",
                        params: { workspaceId: workspace.id },
                      })
                    }
                    onCommand={(command) =>
                      run(command, {
                        workspace,
                        title: digest?.title ?? workspace.title,
                        pr,
                        session: sessions[workspace.id],
                      })
                    }
                  />
                );
              })}
            </div>
          ))}
        {!isCompact &&
          groups.every((group) => group.workspaces.length === 0) && (
            <p className="px-2 py-1 text-xs text-muted-foreground">
              No workspaces yet
            </p>
          )}
      </div>

      {dialogs}
      <NewWorkspaceDialog
        open={newWorkspaceOpen}
        onOpenChange={setNewWorkspaceOpen}
        repos={repos}
        defaultRepoId={newWorkspaceRepoId}
      />
      <AddRepoPalette open={addRepoOpen} onOpenChange={setAddRepoOpen} />
    </SidebarFrame>
  );
}

/**
 * One workspace on the expanded rail: title + attention, repo chip + branch,
 * trailing glyphs, and a nested session row when something is live.
 */
function WorkspaceCard({
  workspace,
  digest,
  session,
  repoName,
  active,
  terminalOpen,
  commands,
  onOpen,
  onCommand,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  active: boolean;
  terminalOpen: boolean;
  commands: WorkspaceCommand[];
  onOpen: () => void;
  onCommand: (command: WorkspaceCommand["id"]) => void;
}) {
  // The digest restates the title on every notice, so a background rename
  // lands here without a catalog refresh.
  const title = digest?.title ?? workspace.title;
  const pr = digest?.pr_state ?? workspace.pr;
  const showSession = isSessionRowWorthy(digest);
  const attentionTitle = cardTooltip(title, digest);
  const branchShown = middleTruncate(workspace.branch_name, 22);
  const HarnessIcon = session ? HARNESS_ICONS[session.harness_kind] : null;
  const age = session?.created_at
    ? formatCompactAge(session.created_at)
    : null;

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <button
          type="button"
          aria-label={title}
          aria-current={active ? "page" : undefined}
          data-active={active || undefined}
          title={attentionTitle}
          className="flex w-full cursor-pointer flex-col gap-0.5 rounded-md px-2 py-1.5 text-left ring-offset-background motion-safe:transition-colors motion-safe:duration-150 motion-safe:ease-out hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none data-[active]:bg-muted"
          onClick={onOpen}
        >
          <span className="flex min-w-0 items-center gap-1.5">
            <AttentionBadge attention={digest?.attention} compact />
            <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium leading-5">
              {title}
            </span>
            <span className="flex shrink-0 items-center gap-1">
              {pr && <PrGlyph pr={pr} />}
              {terminalOpen && (
                <SquareTerminal
                  className="size-3 text-muted-foreground"
                  aria-label="Terminal open"
                />
              )}
              {digest?.attention.state.type === "needs_you" &&
                digest.attention.state.source === "structured" && (
                  <CircleAlert
                    className="size-3 text-critical"
                    aria-label="Pending approval"
                  />
                )}
            </span>
          </span>
          <span className="flex min-w-0 items-center gap-1.5">
            <span className="inline-flex min-w-0 max-w-[46%] items-center gap-1 text-[11px] text-muted-foreground">
              <span
                className={cn(
                  "size-1.5 shrink-0 rounded-[2px]",
                  repoAccentClass(workspace.repo_id),
                )}
                aria-hidden
              />
              <span className="truncate">{repoName}</span>
            </span>
            <span
              className="min-w-0 flex-1 font-mono text-[11px] text-muted-foreground/90"
              title={workspace.branch_name}
            >
              {branchShown}
            </span>
          </span>
          {showSession && digest && (
            <span className="mt-0.5 flex min-w-0 items-center gap-1.5 pl-0.5 text-[11px] text-muted-foreground">
              <ChevronRight
                className="size-3 shrink-0 opacity-50"
                aria-hidden
              />
              {HarnessIcon && (
                <HarnessIcon className="size-3 shrink-0" />
              )}
              <span className="truncate">{sessionRowLabel(digest)}</span>
              {age && (
                <span className="ml-auto shrink-0 tabular-nums">{age}</span>
              )}
            </span>
          )}
        </button>
      </ContextMenuTrigger>
      <ContextMenuContent>
        {commands.map((command) => (
          <WorkspaceMenuItem
            key={command.id}
            command={command}
            onSelect={() => onCommand(command.id)}
          />
        ))}
      </ContextMenuContent>
    </ContextMenu>
  );
}

function WorkspaceMenuItem({
  command,
  onSelect,
}: {
  command: WorkspaceCommand;
  onSelect: () => void;
}) {
  return (
    <>
      {command.separated && <ContextMenuSeparator />}
      <ContextMenuItem
        variant={command.destructive ? "destructive" : "default"}
        onSelect={onSelect}
      >
        {command.label}
      </ContextMenuItem>
    </>
  );
}

/** PR-state mark. The number and title live on the tooltip. */
function PrGlyph({ pr }: { pr: PullRequestDigest }) {
  const tone = prTone(pr);
  return (
    <GitPullRequest
      className={cn("size-3", PR_ICON_TONE_CLASSES[tone])}
      aria-label={pr.title ? `PR #${pr.number} ${pr.title}` : `PR #${pr.number}`}
      data-pr-state={tone}
    />
  );
}

function cardTooltip(
  title: string,
  digest: CodeSessionDigest | undefined,
): string {
  if (!digest || digest.attention.state.type === "working") return title;
  return `${title} · ${digest.attention.state.type === "needs_you" ? digest.attention.state.prompt || "Needs you" : sessionRowLabel(digest)}`;
}

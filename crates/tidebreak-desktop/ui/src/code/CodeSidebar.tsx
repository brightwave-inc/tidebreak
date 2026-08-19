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
import { SidebarButton, SidebarSectionTitle } from "@/sidebar/primitives";
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
import { FOCUS_RING, HOVER_TINT } from "./interactive";
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
  workspaceCardLabel,
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

      <div className="flex items-center justify-between px-2">
        <SidebarSectionTitle className="px-0">Repos</SidebarSectionTitle>
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md p-1",
            FOCUS_RING,
            HOVER_TINT,
          )}
          aria-label="Add repo"
          onClick={() => setAddRepoOpen(true)}
        >
          <Plus size={14} />
        </button>
      </div>
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
        {repos.length === 0 && (
          <SidebarEmptyAction
            label="Add a repo"
            onClick={() => setAddRepoOpen(true)}
          />
        )}
      </div>

      <div className="mt-3 flex items-center justify-between gap-1 px-2">
        <SidebarSectionTitle className="px-0">Workspaces</SidebarSectionTitle>
        <div className="flex items-center">
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md px-1.5 py-1 text-[11px] font-medium",
              FOCUS_RING,
              HOVER_TINT,
            )}
            aria-label={`Sort workspaces: ${sortLabel}. Activate to cycle.`}
            onClick={() => setSortMode(nextWorkspaceSortMode(sortMode))}
          >
            {sortLabel}
          </button>
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md p-1",
              FOCUS_RING,
              HOVER_TINT,
            )}
            aria-label="New workspace"
            onClick={() => startNewWorkspace()}
          >
            <Plus size={14} />
          </button>
        </div>
      </div>
      <div className="flex min-h-0 flex-col gap-0.5">
        {groups.map((group) => (
          <div key={group.key} className="flex flex-col gap-0.5">
            {group.label && (
              <div
                className="truncate px-2 pt-2 pb-1 text-[11px] font-medium text-muted-foreground/90"
                title={group.label}
              >
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
                  terminalOpen={terminalOpen && viewedWorkspaceId === workspace.id}
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
        {/*
          With no repo registered there is nothing to open a workspace on, and
          the line above already says so. Two dead-end lines stacked read as a
          broken rail.
        */}
        {repos.length > 0 &&
          groups.every((group) => group.workspaces.length === 0) && (
            <SidebarEmptyAction
              label="New workspace"
              onClick={() => startNewWorkspace()}
            />
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
 * A rail section with nothing in it yet.
 *
 * The empty slot is where the reader is already looking, and the section's own
 * `+` is a 14px target beside a heading. Making the line itself the control
 * answers "what now?" where the question is asked, and adds no chrome: it
 * replaces the label that was there.
 */
function SidebarEmptyAction({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={cn(
        "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md px-2 py-1 text-left text-xs",
        FOCUS_RING,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      {label}
    </button>
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
          // The card's own text names the workspace; the glyph rail names its
          // state. One explicit label carries both, in the order the card
          // reads, so nothing on the rail is pointer-only information.
          aria-label={workspaceCardLabel({
            title,
            repoName,
            branchName: workspace.branch_name,
            attention: digest?.attention,
            pr,
            terminalOpen,
          })}
          aria-current={active ? "page" : undefined}
          data-active={active || undefined}
          title={attentionTitle}
          className={cn(
            "hover:bg-muted flex w-full cursor-pointer flex-col gap-1 rounded-md px-2 py-1.5 text-left data-[active]:bg-muted",
            FOCUS_RING,
            HOVER_TINT,
          )}
          onClick={onOpen}
        >
          <span className="flex min-w-0 items-center gap-1.5">
            <AttentionBadge attention={digest?.attention} compact />
            <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium leading-5">
              {title}
            </span>
            <span className="flex shrink-0 items-center gap-1" aria-hidden>
              {pr && <PrGlyph pr={pr} />}
              {terminalOpen && (
                <SquareTerminal className="size-3 text-muted-foreground" />
              )}
              {digest?.attention.state.type === "needs_you" &&
                digest.attention.state.source === "structured" && (
                  <CircleAlert className="size-3 text-critical" />
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
              className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground/90"
              title={workspace.branch_name}
            >
              {branchShown}
            </span>
          </span>
          {showSession && digest && (
            <span className="flex min-w-0 items-center gap-1.5 pl-0.5 text-[11px] text-muted-foreground">
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

/** PR-state mark. The card's own label carries the number and state. */
function PrGlyph({ pr }: { pr: PullRequestDigest }) {
  const tone = prTone(pr);
  return (
    <GitPullRequest
      className={cn("size-3", PR_ICON_TONE_CLASSES[tone])}
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

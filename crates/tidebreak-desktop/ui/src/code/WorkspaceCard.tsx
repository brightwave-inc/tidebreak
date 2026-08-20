import { useState } from "react";
import { CircleAlert, CornerDownRight, ExternalLink, Eye, GitPullRequest, SquareTerminal } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import { cn } from "@/lib/utils";
import type {
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { AttentionBadge } from "./AttentionBadge";
import { HARNESS_ICONS } from "./HarnessPicker";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import type { WorkspaceCommand } from "./workspaceActions";
import {
  formatCompactAge,
  isSessionRowWorthy,
  middleTruncate,
  PR_CHIP_TONE_CLASSES,
  PR_ICON_TONE_CLASSES,
  prTone,
  prToneLabel,
  repoAccentClass,
  sessionRowLabel,
  watchRowLabel,
  workspaceCardLabel,
  type CardDensity,
} from "./workspaceCards";

/**
 * One workspace on the rail, in three layers with one job each:
 *
 * - The row is a single plain button that opens the workspace. It carries
 *   the triage read — attention dot, PR mark, title, view-dependent meta —
 *   and nothing interactive competes with it: no overlaid buttons, no
 *   hover choreography inside the row.
 * - Details and actions live in a hover card that opens beside the row:
 *   full title, branch, state, and the pull request front and center with
 *   its action. Pointing at a row asks "what is this one?"; the panel
 *   answers without a click and without crowding the rail.
 * - Every command is the right-click context menu, the row's one menu.
 *   It is also the keyboard path (Shift+F10 / the Menu key), since a hover
 *   card is a pointer affordance by nature.
 *
 * The aria-label always carries the full read regardless of what the row
 * draws — density and view settings change ink, never the announcement.
 */
export function WorkspaceCard({
  workspace,
  digest,
  session,
  repoName,
  active,
  terminalOpen,
  density,
  visibleMeta,
  commands,
  childSessions = [],
  detailDefaultOpen = false,
  onOpen,
  onCommand,
  onOpenChildSession,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  active: boolean;
  terminalOpen: boolean;
  density: CardDensity;
  visibleMeta: { repoChip: boolean; branch: boolean };
  commands: WorkspaceCommand[];
  /** Watch digests riding under this workspace, from the updates store. */
  childSessions?: CodeSessionDigest[];
  /** Render with the detail panel already open. For stories and screenshots. */
  detailDefaultOpen?: boolean;
  onOpen: () => void;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onOpenChildSession?: (sessionId: string) => void;
}) {
  // The digest restates the title on every notice, so a background rename
  // lands here without a catalog refresh.
  const title = digest?.title ?? workspace.title;
  const pr = digest?.pr_state ?? workspace.pr;
  const archived = workspace.status === "archived";
  const [detailOpen, setDetailOpen] = useState(detailDefaultOpen);

  return (
    <div>
      <ContextMenu
        onOpenChange={(open) => {
          // One overlay at a time: the menu wins over the hover panel.
          if (open) setDetailOpen(false);
        }}
      >
        <HoverCard
          open={detailOpen}
          onOpenChange={setDetailOpen}
          openDelay={350}
          closeDelay={150}
        >
          <ContextMenuTrigger asChild>
            <HoverCardTrigger asChild>
              <button
                type="button"
                // The row's text names the workspace; the marks name its
                // state. One explicit label carries both, in the order the
                // row reads, so nothing here is pointer-only information.
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
                className={cn(
                  "flex w-full cursor-pointer flex-col gap-1 rounded-md px-2 py-1.5 text-left hover:bg-muted data-[active]:bg-muted",
                  // Put-away work reads as put away, without becoming
                  // unreadable.
                  archived && "opacity-70",
                  FOCUS_RING,
                  HOVER_TINT,
                )}
                onClick={onOpen}
              >
                <span className="flex min-w-0 items-center gap-1.5">
                  <AttentionBadge attention={digest?.attention} compact />
                  {pr && <PrGlyph pr={pr} />}
                  <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium leading-5">
                    {title}
                  </span>
                  <span className="flex shrink-0 items-center gap-1" aria-hidden>
                    {terminalOpen && (
                      <SquareTerminal className="size-3 text-muted-foreground" />
                    )}
                    {digest?.attention.state.type === "needs_you" &&
                      digest.attention.state.source === "structured" && (
                        <CircleAlert className="size-3 text-critical" />
                      )}
                  </span>
                </span>
                {density === "detailed" &&
                  (visibleMeta.repoChip || visibleMeta.branch) && (
                    <span className="flex min-w-0 items-center gap-1.5">
                      {visibleMeta.repoChip && (
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
                      )}
                      {visibleMeta.branch && (
                        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
                          {middleTruncate(workspace.branch_name, 22)}
                        </span>
                      )}
                    </span>
                  )}
              </button>
            </HoverCardTrigger>
          </ContextMenuTrigger>

          <HoverCardContent
            side="right"
            align="start"
            sideOffset={10}
            className="w-80 p-0"
          >
            <WorkspaceDetailPanel
              workspace={workspace}
              digest={digest}
              session={session}
              repoName={repoName}
              title={title}
              pr={pr}
              archived={archived}
              onCommand={onCommand}
            />
          </HoverCardContent>
        </HoverCard>

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

      {/*
        Watch tasks ride under the row as their own buttons — siblings,
        because buttons cannot nest. ADR 0050 forked these from the
        conversation; here is where that fork becomes visible and openable.
      */}
      {density === "detailed" &&
        childSessions.map((child) => (
          <button
            key={child.session}
            type="button"
            aria-label={`Watch task for ${title}: ${watchRowLabel(child)}`}
            title={child.watch_detail ?? undefined}
            className={cn(
              "flex w-full cursor-pointer items-center gap-1.5 rounded-md py-1 pr-2 pl-6 text-left text-[11px] text-muted-foreground hover:bg-muted hover:text-foreground",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            onClick={() => onOpenChildSession?.(child.session)}
          >
            <CornerDownRight className="size-3 shrink-0 opacity-50" aria-hidden />
            <Eye className="size-3 shrink-0" aria-hidden />
            <span className="truncate">Watch · {watchRowLabel(child)}</span>
            <AttentionBadge
              attention={child.attention}
              compact
              className="ml-auto"
            />
          </button>
        ))}
    </div>
  );
}

/**
 * The hover panel: what the row would say if it had room, plus the action
 * the state calls for. The pull request owns the footer — that is the thing
 * a supervisor most often needs one click away from the rail.
 */
function WorkspaceDetailPanel({
  workspace,
  digest,
  session,
  repoName,
  title,
  pr,
  archived,
  onCommand,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  title: string;
  pr: PullRequestDigest | undefined;
  archived: boolean;
  onCommand: (command: WorkspaceCommand["id"]) => void;
}) {
  const HarnessIcon = session ? HARNESS_ICONS[session.harness_kind] : null;
  const needsYou =
    digest?.attention.state.type === "needs_you"
      ? digest.attention.state.prompt || "Needs you"
      : null;
  const sessionLine =
    digest && isSessionRowWorthy(digest)
      ? `${sessionRowLabel(digest)} · ${digest.turn_count} ${digest.turn_count === 1 ? "turn" : "turns"}`
      : null;
  const stamp = archived
    ? (workspace.archived_at ?? workspace.created_at)
    : (session?.created_at ?? workspace.created_at);
  const age = formatCompactAge(stamp);

  return (
    <div className="flex flex-col">
      <div className="flex flex-col gap-1.5 p-3">
        <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          <span
            className={cn(
              "size-1.5 shrink-0 rounded-[2px]",
              repoAccentClass(workspace.repo_id),
            )}
            aria-hidden
          />
          <span className="shrink-0">{repoName}</span>
          <span
            className="min-w-0 flex-1 truncate font-mono"
            title={workspace.branch_name}
          >
            {workspace.branch_name}
          </span>
          <AttentionBadge attention={digest?.attention} className="shrink-0" />
          {archived && (
            <span className="shrink-0 rounded-sm bg-muted px-1.5 py-0.5 font-medium text-muted-foreground">
              Archived
            </span>
          )}
        </div>
        <p className="text-sm font-medium leading-snug">{title}</p>
        {needsYou ? (
          <p className="flex items-start gap-1.5 text-xs text-critical">
            <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
            <span className="min-w-0">{needsYou}</span>
          </p>
        ) : (
          sessionLine && (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              {HarnessIcon && <HarnessIcon className="size-3.5 shrink-0" />}
              <span>{sessionLine}</span>
            </p>
          )
        )}
        {pr?.checks_summary && (
          <p className="text-xs text-muted-foreground">{pr.checks_summary}</p>
        )}
      </div>
      <div className="flex items-center gap-2 border-t px-3 py-2">
        {pr && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="gap-1.5"
            aria-label={`Open pull request #${pr.number}`}
            onClick={() => onCommand("open-pr")}
          >
            <GitPullRequest className={PR_ICON_TONE_CLASSES[prTone(pr)]} />
            #{pr.number}
            <span
              className={cn(
                "rounded-sm px-1.5 py-px text-[10px] font-medium",
                PR_CHIP_TONE_CLASSES[prTone(pr)],
              )}
            >
              {prToneLabel(pr)}
            </span>
            <ExternalLink className="size-3 text-muted-foreground" aria-hidden />
          </Button>
        )}
        {archived && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onCommand("restore")}
          >
            Restore
          </Button>
        )}
        {!pr && !archived && (
          <span className="text-xs text-muted-foreground">
            Right-click the row for all actions.
          </span>
        )}
        <span
          className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground"
          title={stamp}
        >
          {age === "now" ? "just now" : age ? `${age} ago` : null}
        </span>
      </div>
    </div>
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

/** PR-state mark. The row's own label carries the number and state. */
function PrGlyph({ pr }: { pr: PullRequestDigest }) {
  const tone = prTone(pr);
  return (
    <GitPullRequest
      className={cn("size-3 shrink-0", PR_ICON_TONE_CLASSES[tone])}
      data-pr-state={tone}
      aria-hidden
    />
  );
}

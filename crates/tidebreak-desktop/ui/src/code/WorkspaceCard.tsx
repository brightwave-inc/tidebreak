import { useEffect, useState, type ReactNode } from "react";
import {
  Archive,
  Ban,
  Bot,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  CircleAlert,
  CircleCheck,
  Clock,
  Copy,
  CornerDownRight,
  ExternalLink,
  Eye,
  FolderOpen,
  GitPullRequest,
  Pin,
  Radar,
  RotateCcw,
  SquareTerminal,
} from "lucide-react";

import { Loader } from "@/components/motion/loader";
import { Button } from "@/components/ui/button";
import { LiveLabel } from "@/LiveLabel";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
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
  CodeSubagentStatus,
  CodeSubagentSummary,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { AttentionBadge } from "./AttentionBadge";
import { HARNESS_ICONS } from "./HarnessPicker";
import { FOCUS_RING_INSET, HOVER_TINT } from "./interactive";
import {
  HARNESS_LABELS,
  LIFECYCLE_LABELS,
  WORKSPACE_STATUS_LABELS,
} from "./labels";
import type { WorkspaceCommand } from "./workspaceActions";
import {
  workspaceCreationPhase,
  type OptimisticCodeWorkspaceSnapshot,
  type WorkspaceCreationPhase,
} from "./CodeCatalogStore";
import {
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkspaceWorkflowAction,
} from "./workspaceWorkflow";
import type { CodeWorkspacePrResource } from "./useCodeWorkspacePr";
import {
  WorkspaceStatusDetails,
  workspaceStatusLabel,
} from "./WorkspaceStatusDetails";
import {
  attentionMarkForDigest,
  digestStatusTone,
  STATUS_CHIP,
  STATUS_DOT,
  STATUS_MARK,
  STATUS_MOTION,
  STATUS_TEXT,
  type StatusTone,
} from "./statusTone";
import {
  formatCompactAge,
  isSessionRowWorthy,
  isPutAway,
  middleTruncate,
  readyToMergeNotice,
  repoAccentClass,
  sessionActivityLineLabel,
  watchRowLabel,
  workspaceCardLabel,
  workspaceCardStatus,
  WORKSPACE_STATUS_RANK_LABELS,
  WORKSPACE_STATUS_RANK_TONES,
  type CardDensity,
  type WorkspaceStatusRank,
} from "./workspaceCards";
import { pointerSelectIntent } from "./workspaceSelection";
import {
  PULL_REQUEST_LIFECYCLE_TONE,
  prCompactStatusLabel,
  prCompactStatusTone,
  pullRequestLifecycle,
} from "./prState";

/** Compact rank glyph shared by the card and by-status group headers. */
export function WorkspaceStatusMark({
  rank,
  creating = false,
  prState,
}: {
  rank: WorkspaceStatusRank;
  creating?: boolean;
  prState?: string;
}) {
  if (creating) {
    return (
      <span role="img" aria-label="Creating workspace">
        <Loader variant="comet" size={12} className="text-live" decorative />
      </span>
    );
  }
  const label = WORKSPACE_STATUS_RANK_LABELS[rank];
  const className = cn(
    "size-3",
    STATUS_MARK[WORKSPACE_STATUS_RANK_TONES[rank]],
  );
  return (
    <span role="img" aria-label={label} data-workspace-status={rank}>
      {rank === "needs_you" ? (
        <CircleAlert className={className} aria-hidden />
      ) : rank === "running" ? (
        <Loader variant="comet" size={12} className="text-live" decorative />
      ) : rank === "pr_open" ? (
        <GitPullRequest
          className={className}
          aria-hidden
          data-pr-state={prState ?? "open"}
        />
      ) : rank === "done_unreviewed" ? (
        <CheckCircle2 className={className} aria-hidden />
      ) : rank === "setup_failed" ? (
        <CircleAlert className={className} aria-hidden />
      ) : rank === "archived" ? (
        <Archive className={className} aria-hidden />
      ) : (
        <Circle className={className} aria-hidden />
      )}
    </span>
  );
}

/**
 * One workspace in the rail.
 *
 * The row keeps the triage read visible. The hover card restores the room that
 * pull-request checks, review state, the full branch, and the next action need.
 * Right-click remains the complete command path for less common operations.
 */
export function WorkspaceCard({
  workspace,
  digest,
  session,
  repoName,
  active,
  selected = false,
  terminalOpen,
  density,
  visibleMeta,
  commands,
  childSessions = [],
  stackParent,
  detailDefaultOpen = false,
  prResource,
  onDetailOpenChange,
  contextMenuLabel,
  onOpen,
  onSelectPointer,
  onMenuOpen,
  onCommand,
  onOpenChildSession,
  onOpenSubagent,
  onOpenStackParent,
  onWorkflowAction,
}: {
  workspace: OptimisticCodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  active: boolean;
  selected?: boolean;
  terminalOpen: boolean;
  density: CardDensity;
  visibleMeta: { repoChip: boolean; branch: boolean };
  commands: WorkspaceCommand[];
  childSessions?: CodeSessionDigest[];
  /** The sibling workspace this branch is stacked on (decision 77). */
  stackParent?: { id: string; title: string } | null;
  /** Open the hover detail at mount time. Stories use this for visual review. */
  detailDefaultOpen?: boolean;
  prResource?: CodeWorkspacePrResource;
  onDetailOpenChange?: (open: boolean) => void;
  /** Section label for a bulk context menu. */
  contextMenuLabel?: string;
  onOpen: () => void;
  onSelectPointer?: (event: {
    shiftKey: boolean;
    metaKey: boolean;
    ctrlKey: boolean;
  }) => void;
  onMenuOpen?: () => void;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onOpenChildSession?: (sessionId: string) => void;
  onOpenSubagent?: (callId: string) => void;
  onOpenStackParent?: (workspaceId: string) => void;
  onWorkflowAction?: (
    action: WorkspaceWorkflowAction,
    pr?: PullRequestDigest,
  ) => void;
}) {
  const title = digest?.title ?? workspace.title;
  const pr = prResource?.data
    ? prResource.data.pr
    : (digest?.pr_state ?? workspace.pr);
  const archived = isPutAway(workspace);
  const creating = workspace.status === "creating";
  const creationPhase = workspaceCreationPhase(workspace);
  const creationLabel = workspaceCreationLabel(creationPhase);
  const cardStatus = workspaceCardStatus(workspace, digest);
  const attentionMark = attentionMarkForDigest(digest);
  const [detailOpen, setDetailOpen] = useState(detailDefaultOpen);
  const compactDetail = useCompactWorkspaceDetail();
  useEffect(() => {
    onDetailOpenChange?.(detailOpen);
  }, [detailOpen, onDetailOpenChange]);
  const watchActive = childSessions.some(
    (child) =>
      child.watch_state === "watching" ||
      child.watch_state === "fixing" ||
      child.watch_state === "blocked" ||
      (child.watch_state === undefined && child.lifecycle === "running"),
  );

  return (
    <article
      className={cn(
        "group/workspace relative rounded-xl border border-transparent transition-[background-color,border-color,box-shadow,opacity] duration-150",
        creating &&
          "workspace-creation-card border-live-border/35 bg-live-background/25",
        selected && "border-border bg-muted/60",
        active &&
          "shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_6%,transparent)]",
        !selected && active && "border-border-subtle bg-background",
        !selected && !active && "hover:bg-background/55",
        archived && "opacity-65",
      )}
      data-workspace-card=""
      data-active={active || undefined}
      data-selected={selected || undefined}
    >
      <ContextMenu
        onOpenChange={(open) => {
          if (!open) return;
          setDetailOpen(false);
          onMenuOpen?.();
        }}
      >
        <HoverCard
          open={detailOpen}
          onOpenChange={setDetailOpen}
          openDelay={300}
          closeDelay={120}
        >
          <ContextMenuTrigger asChild>
            <HoverCardTrigger asChild>
              <button
                type="button"
                aria-label={workspaceCardLabel({
                  title,
                  repoName,
                  branchName: workspace.branch_name,
                  attention: attentionMark,
                  session: digest,
                  pr,
                  terminalOpen,
                  workspaceStatus: workspace.status,
                  creationLabel,
                })}
                aria-current={active ? "page" : undefined}
                aria-selected={selected || undefined}
                disabled={creating}
                className={cn(
                  "flex w-full cursor-pointer flex-col gap-0.5 rounded-xl px-2.5 py-2 text-left",
                  FOCUS_RING_INSET,
                  HOVER_TINT,
                  creating && "cursor-wait",
                )}
                onClick={(event) => {
                  if (creating) return;
                  if (
                    onSelectPointer &&
                    pointerSelectIntent(event) !== "open"
                  ) {
                    event.preventDefault();
                    onSelectPointer(event);
                    return;
                  }
                  onOpen();
                }}
              >
                <span className="flex min-w-0 items-center gap-2">
                  <WorkspaceStatusMark
                    rank={cardStatus.rank}
                    creating={creating}
                    prState={pr ? pullRequestLifecycle(pr) : undefined}
                  />
                  <span className="min-w-0 flex-1 truncate text-md font-medium leading-5">
                    {title}
                  </span>
                  <span
                    className="flex shrink-0 items-center gap-1.5"
                    aria-hidden
                  >
                    {pr && cardStatus.rank !== "pr_open" && <PrGlyph pr={pr} />}
                    {terminalOpen && (
                      <SquareTerminal className="size-3 text-muted-foreground" />
                    )}
                  </span>
                </span>
                {density === "detailed" &&
                  (visibleMeta.repoChip || visibleMeta.branch) && (
                    <span className="flex min-w-0 items-center gap-1.5 pl-5 text-xs text-muted-foreground">
                      {visibleMeta.repoChip && (
                        <span className="min-w-0 max-w-[46%] truncate">
                          {repoName}
                        </span>
                      )}
                      {visibleMeta.repoChip && visibleMeta.branch && (
                        <span className="text-border" aria-hidden>
                          /
                        </span>
                      )}
                      {visibleMeta.branch && (
                        <span
                          className="min-w-0 flex-1 truncate font-mono"
                          title={workspace.branch_name}
                        >
                          {middleTruncate(workspace.branch_name, 24)}
                        </span>
                      )}
                    </span>
                  )}
              </button>
            </HoverCardTrigger>
          </ContextMenuTrigger>

          <HoverCardContent
            side={compactDetail ? "bottom" : "right"}
            align="start"
            sideOffset={10}
            className="w-[min(26rem,calc(100vw-24px))] overflow-hidden rounded-xl border-border bg-popover p-0"
          >
            <WorkspaceDetailPanel
              workspace={workspace}
              digest={digest}
              session={session}
              repoName={repoName}
              title={title}
              pr={pr}
              archived={archived}
              watchActive={watchActive}
              terminalOpen={terminalOpen}
              commands={commands}
              onCommand={onCommand}
              onWorkflowAction={onWorkflowAction}
              prResource={prResource}
            />
          </HoverCardContent>
        </HoverCard>

        <ContextMenuContent>
          {contextMenuLabel && (
            <ContextMenuLabel>{contextMenuLabel}</ContextMenuLabel>
          )}
          {commands.map((command) => (
            <WorkspaceMenuItem
              key={command.id}
              command={command}
              onSelect={() => onCommand(command.id)}
            />
          ))}
        </ContextMenuContent>
      </ContextMenu>

      {creating ? (
        <WorkspaceCreationProgress phase={creationPhase} />
      ) : (
        density === "detailed" && (
          <WorkspaceActivityLine
            workspace={workspace}
            digest={digest}
            session={session}
          />
        )
      )}

      {density === "detailed" &&
        (childSessions.length > 0 ||
          (digest?.subagents?.length ?? 0) > 0 ||
          stackParent) && (
          <div className="relative mr-2 mb-2 ml-5 flex flex-col gap-0.5 border-l border-border-subtle pl-2">
            {stackParent && (
              <WorkspaceChildRow
                label={`Stacked on ${stackParent.title}`}
                ariaLabel={`${title} is stacked on ${stackParent.title}; open that workspace`}
                icon={<CornerDownRight />}
                onClick={() => onOpenStackParent?.(stackParent.id)}
              />
            )}
            {childSessions.map((child) => (
              <WorkspaceChildRow
                key={child.session}
                label={`Watch - ${watchRowLabel(child)}`}
                ariaLabel={`Watch task for ${title}: ${watchRowLabel(child)}`}
                icon={<Eye />}
                attention={attentionMarkForDigest(child)}
                onClick={() => onOpenChildSession?.(child.session)}
              />
            ))}
            {(digest?.subagents?.length ?? 0) > 0 && (
              <WorkspaceSubagentRows
                title={title}
                subagents={digest?.subagents ?? []}
                onOpenSubagent={onOpenSubagent}
              />
            )}
          </div>
        )}
    </article>
  );
}

function useCompactWorkspaceDetail(): boolean {
  const query = "(max-width: 639px)";
  const [compact, setCompact] = useState(
    () =>
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia(query).matches,
  );

  useEffect(() => {
    if (
      typeof window === "undefined" ||
      typeof window.matchMedia !== "function"
    ) {
      return;
    }
    const media = window.matchMedia(query);
    const update = () => setCompact(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);

  return compact;
}

const SUBAGENT_STATUS_LABELS: Record<CodeSubagentStatus, string> = {
  running: "Running",
  done: "Done",
  failed: "Failed",
};

const SUBAGENT_STATUS_TONES: Record<CodeSubagentStatus, StatusTone> = {
  running: "running",
  done: "neutral",
  failed: "critical",
};

/**
 * The harness subagents under a card, behind one toggle row.
 *
 * Open while any subagent is still running — that is when the names matter —
 * and folded once they have all settled, so a card whose work is done does
 * not keep three rows of history under it. The reader can flip either way.
 */
function WorkspaceSubagentRows({
  title,
  subagents,
  onOpenSubagent,
}: {
  title: string;
  subagents: readonly CodeSubagentSummary[];
  onOpenSubagent?: (callId: string) => void;
}) {
  const running = subagents.filter((entry) => entry.status === "running");
  const [expanded, setExpanded] = useState(running.length > 0);
  // A count, not a state: the session line above already says how many are
  // working, and the rows below say which.
  const summary =
    subagents.length === 1 ? "1 subagent" : `${subagents.length} subagents`;
  const Chevron = expanded ? ChevronDown : ChevronRight;
  return (
    <>
      <button
        type="button"
        aria-expanded={expanded}
        aria-label={`${summary} for ${title}; ${expanded ? "hide" : "show"} them`}
        className={cn(
          "flex w-full cursor-pointer items-center gap-1.5 rounded-lg px-1.5 py-1 text-left text-xs text-muted-foreground hover:bg-muted hover:text-foreground",
          FOCUS_RING_INSET,
          HOVER_TINT,
        )}
        onClick={() => setExpanded((open) => !open)}
      >
        <Chevron className="size-3 shrink-0 opacity-60" aria-hidden />
        <Bot
          className={cn(
            "size-3 shrink-0",
            running.length > 0 && [STATUS_MARK.running, STATUS_MOTION.running],
          )}
          aria-hidden
        />
        <span className="min-w-0 flex-1 truncate">{summary}</span>
      </button>
      {expanded &&
        subagents.map((subagent) => (
          <WorkspaceChildRow
            key={subagent.call_id}
            label={subagent.name}
            status={SUBAGENT_STATUS_LABELS[subagent.status]}
            statusTone={SUBAGENT_STATUS_TONES[subagent.status]}
            ariaLabel={`Subagent for ${title}: ${subagent.name}, ${SUBAGENT_STATUS_LABELS[subagent.status]}`}
            icon={
              subagent.status === "running" ? (
                <Loader
                  variant="comet"
                  size={12}
                  className="text-live"
                  decorative
                />
              ) : subagent.status === "failed" ? (
                <CircleAlert className={STATUS_MARK.critical} />
              ) : (
                <CircleCheck />
              )
            }
            onClick={() => onOpenSubagent?.(subagent.call_id)}
          />
        ))}
    </>
  );
}

function WorkspaceDetailPanel({
  workspace,
  digest,
  session,
  repoName,
  title,
  pr,
  archived,
  watchActive,
  terminalOpen,
  commands,
  onCommand,
  onWorkflowAction,
  prResource,
}: {
  workspace: CodeWorkspaceSnapshot;
  prResource?: CodeWorkspacePrResource;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  title: string;
  pr: PullRequestDigest | undefined;
  archived: boolean;
  watchActive: boolean;
  terminalOpen: boolean;
  commands: readonly WorkspaceCommand[];
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onWorkflowAction?: (
    action: WorkspaceWorkflowAction,
    pr?: PullRequestDigest,
  ) => void;
}) {
  const activity = workspaceActivitySummary(digest, session, terminalOpen, pr);
  const matchingSession =
    !digest || session?.id === digest.session ? session : undefined;
  const stamp = archived
    ? (workspace.archived_at ?? workspace.created_at)
    : (matchingSession?.created_at ?? workspace.created_at);
  const age = formatCompactAge(stamp);
  const model = workspaceWorkflowModel(prResource?.data ?? null, pr);
  const primary =
    model?.primary &&
    !prResource?.error &&
    !(watchActive && model.primary !== "open_pr")
      ? model.primary
      : undefined;
  const primaryLabel =
    primary && model
      ? primary === "merge"
        ? "Review merge"
        : primary === "mark_ready"
          ? "Review draft"
          : workspaceWorkflowActionLabel(primary, model.stage)
      : null;
  const prTitle = pr?.title?.trim();
  const showPrTitle = prTitle && prTitle !== title.trim();
  const putAwayLabel =
    workspace.status === "released" ? "Released" : "Archived";
  const worktreeCommand = commands.find(
    (command) =>
      command.id === "open-worktree" || command.id === "copy-worktree",
  );

  return (
    <div data-testid="workspace-hover-card">
      <div className="p-3.5">
        <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span
            className={cn(
              "size-1.5 shrink-0 rounded-[2px]",
              repoAccentClass(workspace.repo_id),
            )}
            aria-hidden
          />
          <span className="shrink-0">{repoName}</span>
          <span className="text-border" aria-hidden>
            /
          </span>
          <span
            className="min-w-0 flex-1 truncate font-mono"
            title={workspace.branch_name}
          >
            {workspace.branch_name}
          </span>
          {archived ? (
            <span
              className={cn(
                "shrink-0 rounded-md px-1.5 py-0.5",
                STATUS_CHIP.neutral,
              )}
            >
              {putAwayLabel}
            </span>
          ) : pr ? (
            <span
              className={cn(
                "shrink-0 rounded-md px-1.5 py-0.5 font-medium",
                STATUS_CHIP[prCompactStatusTone(pr)],
              )}
            >
              {prCompactStatusLabel(pr)}
            </span>
          ) : null}
        </div>

        <p className="mt-2.5 text-base font-semibold leading-5 text-pretty">
          {title}
        </p>
        {showPrTitle && (
          <p className="mt-1 text-xs leading-5 text-muted-foreground text-pretty">
            {prTitle}
          </p>
        )}

        {activity && (
          <div
            className={cn(
              "mt-2 flex min-w-0 items-start gap-1.5 text-xs",
              STATUS_TEXT[activity.tone],
            )}
          >
            <span
              className={cn(
                "mt-1 size-1.5 shrink-0 rounded-full",
                STATUS_DOT[activity.tone],
                activity.tone === "running" && STATUS_MOTION.running,
              )}
              aria-hidden
            />
            <span className="min-w-0 leading-4">{activity.label}</span>
          </div>
        )}

        {!archived && (
          <div className="mt-3 border-t border-border-subtle pt-3">
            <p
              className={cn(
                "mb-2 text-xs font-medium",
                STATUS_TEXT[prResource?.error ? "warning" : model.tone],
              )}
            >
              {prResource?.error
                ? "Status unavailable"
                : workspaceStatusLabel(model)}
            </p>
            <WorkspaceStatusDetails
              model={model}
              snapshot={prResource?.data ?? null}
              error={prResource?.mutationError ?? prResource?.error}
            />
            {prResource && (
              <Button
                variant="ghost"
                size="sm"
                className="mt-2"
                disabled={prResource.refreshing || prResource.busy !== null}
                onClick={() =>
                  void prResource.refreshFromHost().catch(() => undefined)
                }
              >
                Refresh status
              </Button>
            )}
            {primary &&
              primary !== "open_pr" &&
              primaryLabel &&
              onWorkflowAction && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="mt-3 h-7 bg-foreground px-2.5 text-xs text-background hover:bg-foreground/88 hover:text-background"
                  disabled={prResource?.busy != null}
                  onClick={() => onWorkflowAction(primary, pr)}
                >
                  {primaryLabel}
                </Button>
              )}
          </div>
        )}
      </div>

      <div className="flex items-center gap-1 border-t border-border-subtle bg-muted/25 px-2 py-2">
        {worktreeCommand && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-7 gap-1.5 px-1.5 text-xs"
            onClick={() => onCommand(worktreeCommand.id)}
          >
            {worktreeCommand.id === "open-worktree" ? (
              <FolderOpen className="size-3" aria-hidden />
            ) : (
              <Copy className="size-3" aria-hidden />
            )}
            {worktreeCommand.id === "open-worktree"
              ? "Open folder"
              : "Copy path"}
          </Button>
        )}
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 gap-1.5 px-1.5 text-xs"
          onClick={() => onCommand(archived ? "restore" : "archive")}
        >
          {archived ? (
            <RotateCcw className="size-3" aria-hidden />
          ) : (
            <Archive className="size-3" aria-hidden />
          )}
          {archived ? "Restore" : "Archive"}
        </Button>
        {pr && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className={cn(
              "h-7 gap-1.5 px-1.5 text-xs",
              STATUS_TEXT[prCompactStatusTone(pr)],
            )}
            aria-label={`Open pull request #${pr.number}`}
            onClick={() => onCommand("open-pr")}
          >
            <GitPullRequest className="size-3" aria-hidden />
            <span className="tabular-nums">#{pr.number}</span>
            <ExternalLink className="size-2.5 opacity-55" aria-hidden />
          </Button>
        )}
        {age && (
          <span
            className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums"
            title={stamp}
          >
            {age === "now" ? "just now" : `${age} ago`}
          </span>
        )}
      </div>
    </div>
  );
}

function WorkspaceActivityLine({
  workspace,
  digest,
  session,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
}) {
  // The checkout survived, so the workspace is usable — but the setup script
  // never finished, and nothing else on the card would say so.
  if (workspace.status === "setup_failed") {
    return (
      <div
        className={cn(
          "flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7 text-xs",
          STATUS_TEXT.critical,
        )}
      >
        <CircleAlert className="size-3 shrink-0" aria-hidden />
        <span className="min-w-0 flex-1 truncate">
          {WORKSPACE_STATUS_LABELS.setup_failed}
        </span>
      </div>
    );
  }
  const railDigest = digest && isSessionRowWorthy(digest) ? digest : undefined;
  if (!railDigest) return null;

  const activity = workspaceActivitySummary(
    railDigest,
    session,
    false,
    workspace.pr,
  );
  const matchingSession =
    session?.id === railDigest.session ? session : undefined;
  const harnessKind = railDigest.harness_kind ?? matchingSession?.harness_kind;
  const HarnessIcon = harnessKind ? HARNESS_ICONS[harnessKind] : null;
  const stamp = matchingSession?.created_at ?? workspace.created_at;
  const age = formatCompactAge(stamp);

  if (!activity) return null;
  const running = activity.tone === "running";

  return (
    <div className="flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7 text-xs text-muted-foreground">
      {activity.terminalOnly ? (
        <SquareTerminal className="size-3 shrink-0" aria-hidden />
      ) : (
        <SessionStateGlyph digest={railDigest} pr={workspace.pr} />
      )}
      <span
        className={cn(
          "min-w-0 flex-1 truncate",
          activity.needsYou && STATUS_TEXT.critical,
          running && STATUS_TEXT.running,
          activity.tone === "ready" && STATUS_TEXT.ready,
        )}
        title={activity.label}
      >
        {activity.label}
      </span>
      {harnessKind && HarnessIcon && (
        // A brand mark, so it keeps its own identity and its own slot: the
        // state lives in the leading glyph, never on the engine's logo.
        <span title={HARNESS_LABELS[harnessKind]} className="shrink-0">
          <HarnessIcon className="size-3 opacity-70" aria-hidden />
        </span>
      )}
      {age && (
        <span className="shrink-0 tabular-nums">
          {age === "now" ? "now" : age}
        </span>
      )}
    </div>
  );
}

/**
 * One glyph, one meaning, on the rail row.
 *
 * The same shapes the compact attention mark draws, so the row and the badge
 * never disagree: a comet is work in motion, a circle-alert wants the
 * reader, a clock went quiet, a ban is fenced, a check is finished and
 * unread, a pin was set by hand. Two shapes are the row's own, for a turn
 * that is alive but parked on something else: a bot for subagents and a
 * radar for a monitor, both in the live tone with the live pulse.
 */
function SessionStateGlyph({
  digest,
  pr,
}: {
  digest: CodeSessionDigest;
  pr?: PullRequestDigest;
}) {
  const className = "size-3 shrink-0";
  const attention = digest.attention.state.type;
  const mergeNotice = readyToMergeNotice(
    digest.attention,
    digest.pr_state ?? pr,
  );
  if (attention === "needs_you" && mergeNotice !== "stale") {
    if (mergeNotice === "ready") {
      return (
        <CircleCheck
          className={cn(className, STATUS_MARK.ready)}
          data-state-glyph="ready_to_merge"
          aria-hidden
        />
      );
    }
    return (
      <CircleAlert
        className={cn(className, STATUS_MARK.critical)}
        data-state-glyph="needs_you"
        aria-hidden
      />
    );
  }
  if (digest.lifecycle === "running") {
    const parkedOn = runningParkedOn(digest);
    if (parkedOn === "subagents") {
      return (
        <Bot
          className={cn(className, STATUS_MARK.running, STATUS_MOTION.running)}
          data-state-glyph="subagents"
          aria-hidden
        />
      );
    }
    if (parkedOn === "monitor") {
      return (
        <Radar
          className={cn(className, STATUS_MARK.running, STATUS_MOTION.running)}
          data-state-glyph="monitor"
          aria-hidden
        />
      );
    }
    if (attention === "stalled") {
      return (
        <Clock
          className={cn(className, STATUS_MARK.warning)}
          data-state-glyph="stalled"
          aria-hidden
        />
      );
    }
    return (
      <Loader
        variant="comet"
        size={12}
        className={cn(className, "text-live")}
        data-state-glyph="working"
        decorative
      />
    );
  }
  switch (attention) {
    case "stalled":
      return (
        <Clock
          className={cn(className, STATUS_MARK.warning)}
          data-state-glyph="stalled"
          aria-hidden
        />
      );
    case "fenced":
      return (
        <Ban
          className={cn(className, STATUS_MARK.warning)}
          data-state-glyph="fenced"
          aria-hidden
        />
      );
    case "manual":
      return (
        <Pin
          className={cn(className, STATUS_MARK.pending)}
          data-state-glyph="manual"
          aria-hidden
        />
      );
    default:
      // A parked turn with work behind it is finished and unread until the
      // reader opens it; one with none is an empty seat.
      return digest.turn_count > 0 ? (
        <CircleCheck
          className={cn(className, STATUS_MARK.ready)}
          data-state-glyph="done"
          aria-hidden
        />
      ) : (
        <Circle className={className} data-state-glyph="idle" aria-hidden />
      );
  }
}

/** What a running turn is parked on, when it is parked at all. */
function runningParkedOn(
  digest: CodeSessionDigest,
): "subagents" | "monitor" | null {
  const runningSubagents = digest.subagents?.some(
    (entry) => entry.status === "running",
  );
  if (runningSubagents || digest.activity === "subagents") return "subagents";
  if (digest.activity === "monitor") return "monitor";
  return null;
}

function workspaceCreationLabel(phase: WorkspaceCreationPhase): string {
  return phase === "naming" ? "Naming workspace" : "Creating branch and folder";
}

function WorkspaceCreationProgress({
  phase,
}: {
  phase: WorkspaceCreationPhase;
}) {
  const label = workspaceCreationLabel(phase);
  return (
    <div
      className="px-2.5 pb-2 pl-7"
      role="status"
      aria-label={label}
      data-testid="workspace-creation-progress"
    >
      <LiveLabel live className="block truncate text-xs">
        {label}
      </LiveLabel>
      <span
        className="workspace-creation-progress mt-1.5 block h-0.5 overflow-hidden rounded-full bg-live-border/20"
        aria-hidden
      />
    </div>
  );
}

function workspaceActivitySummary(
  digest: CodeSessionDigest | undefined,
  session: CodeSessionSnapshot | undefined,
  terminalOpen: boolean,
  pr?: PullRequestDigest,
): {
  label: string;
  tone: StatusTone;
  needsYou: boolean;
  terminalOnly: boolean;
} | null {
  const pullRequest = digest?.pr_state ?? pr;
  const mergeNotice = readyToMergeNotice(digest?.attention, pullRequest);
  if (digest?.attention.state.type === "needs_you" && mergeNotice !== "stale") {
    if (mergeNotice === "ready") {
      return {
        label: digest.attention.state.prompt || "Ready to merge",
        tone: "ready",
        needsYou: false,
        terminalOnly: false,
      };
    }
    return {
      label: digest.attention.state.prompt || "Needs you",
      tone: "critical",
      needsYou: true,
      terminalOnly: false,
    };
  }

  const worthyDigest =
    digest && isSessionRowWorthy(digest) ? digest : undefined;
  const statusLabel = worthyDigest
    ? sessionActivityLineLabel(worthyDigest, pullRequest)
    : digest
      ? LIFECYCLE_LABELS[digest.lifecycle]
      : session
        ? LIFECYCLE_LABELS[session.lifecycle]
        : null;
  const terminalOnly = terminalOpen && !statusLabel;
  if (!statusLabel && !terminalOnly) return null;

  return {
    label: statusLabel || "Terminal open",
    tone: mergeNotice === "stale" ? "neutral" : digestStatusTone(digest),
    needsYou: false,
    terminalOnly,
  };
}

function WorkspaceChildRow({
  label,
  status,
  statusTone = "neutral",
  ariaLabel,
  icon,
  attention,
  onClick,
}: {
  label: string;
  status?: string;
  statusTone?: StatusTone;
  ariaLabel: string;
  icon: ReactNode;
  attention?: CodeSessionDigest["attention"];
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={ariaLabel}
      className={cn(
        "flex w-full cursor-pointer items-center gap-1.5 rounded-lg px-1.5 py-1 text-left text-xs text-muted-foreground hover:bg-muted hover:text-foreground",
        FOCUS_RING_INSET,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      <CornerDownRight className="size-3 shrink-0 opacity-40" aria-hidden />
      <span className="[&_svg]:size-3 [&_svg]:shrink-0" aria-hidden>
        {icon}
      </span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {status && (
        <span className={cn("shrink-0", STATUS_TEXT[statusTone])}>
          {status}
        </span>
      )}
      {attention && <AttentionBadge attention={attention} compact />}
    </button>
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

function PrGlyph({ pr }: { pr: PullRequestDigest }) {
  const lifecycle = pullRequestLifecycle(pr);
  return (
    <GitPullRequest
      className={cn(
        "size-3 shrink-0",
        STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
      )}
      data-pr-state={lifecycle}
      aria-hidden
    />
  );
}

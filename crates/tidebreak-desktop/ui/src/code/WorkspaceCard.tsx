import { useState, type ReactNode } from "react";
import {
  Archive,
  Bot,
  CheckCircle2,
  CircleAlert,
  CornerDownRight,
  ExternalLink,
  Eye,
  FileCode2,
  GitBranch,
  GitPullRequest,
  LoaderCircle,
  Radar,
  RotateCcw,
  Search,
  SquareTerminal,
  Wrench,
} from "lucide-react";

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
  CodeSubagentStatus,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import { AttentionBadge } from "./AttentionBadge";
import { HARNESS_ICONS } from "./HarnessPicker";
import { FOCUS_RING_INSET, HOVER_TINT } from "./interactive";
import { HARNESS_LABELS, LIFECYCLE_LABELS } from "./labels";
import type { WorkspaceCommand } from "./workspaceActions";
import {
  checkSummary,
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkspaceWorkflowAction,
} from "./workspaceWorkflow";
import { pullRequestReviewSummary } from "./pullRequestPresentation";
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
  prStatusLabel,
  prStatusTone,
  prTone,
  repoAccentClass,
  sessionRowLabel,
  watchRowLabel,
  workspaceCardLabel,
  workspacePrChipSummary,
  type CardDensity,
} from "./workspaceCards";

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
  terminalOpen,
  density,
  visibleMeta,
  commands,
  childSessions = [],
  stackParent,
  detailDefaultOpen = false,
  onOpen,
  onCommand,
  onOpenChildSession,
  onOpenSubagent,
  onOpenStackParent,
  onWorkflowAction,
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
  childSessions?: CodeSessionDigest[];
  /** The sibling workspace this branch is stacked on (decision 62). */
  stackParent?: { id: string; title: string } | null;
  /** Open the hover detail at mount time. Stories use this for visual review. */
  detailDefaultOpen?: boolean;
  onOpen: () => void;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onOpenChildSession?: (sessionId: string) => void;
  onOpenSubagent?: (callId: string) => void;
  onOpenStackParent?: (workspaceId: string) => void;
  onWorkflowAction?: (action: WorkspaceWorkflowAction) => void;
}) {
  const title = digest?.title ?? workspace.title;
  const pr = digest?.pr_state ?? workspace.pr;
  const archived = isPutAway(workspace);
  const creating = workspace.status === "creating";
  const attentionMark = attentionMarkForDigest(digest);
  const [detailOpen, setDetailOpen] = useState(detailDefaultOpen);
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
        active
          ? "border-border-subtle bg-background shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_6%,transparent)]"
          : "hover:bg-background/55",
        archived && "opacity-65",
      )}
      data-active={active || undefined}
    >
      <ContextMenu
        onOpenChange={(open) => {
          if (open) setDetailOpen(false);
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
                  attention: digest?.attention,
                  session: digest,
                  pr,
                  terminalOpen,
                  workspaceStatus: workspace.status,
                })}
                aria-current={active ? "page" : undefined}
                disabled={creating}
                className={cn(
                  "flex w-full cursor-pointer flex-col gap-0.5 rounded-xl px-2.5 py-2 text-left",
                  FOCUS_RING_INSET,
                  HOVER_TINT,
                  creating && "cursor-wait",
                )}
                onClick={onOpen}
              >
                <span className="flex min-w-0 items-center gap-2">
                  <AttentionBadge attention={attentionMark} compact />
                  <span className="min-w-0 flex-1 truncate text-md font-medium leading-5">
                    {title}
                  </span>
                  <span
                    className="flex shrink-0 items-center gap-1.5"
                    aria-hidden
                  >
                    {pr && <PrGlyph pr={pr} />}
                    {terminalOpen && (
                      <SquareTerminal className="size-3 text-muted-foreground" />
                    )}
                    {creating && (
                      <LoaderCircle
                        className={cn(
                          "size-3 animate-spin",
                          STATUS_MARK.pending,
                        )}
                      />
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
            side="right"
            align="start"
            sideOffset={10}
            className="w-[22rem] overflow-hidden rounded-xl border-border bg-popover p-0 shadow-[0_18px_48px_color-mix(in_oklch,var(--foreground)_16%,transparent)]"
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
              onCommand={onCommand}
              onWorkflowAction={onWorkflowAction}
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

      {density === "detailed" && (
        <WorkspaceActivityLine
          workspace={workspace}
          digest={digest}
          session={session}
        />
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
            {(digest?.subagents ?? []).map((subagent) => (
              <WorkspaceChildRow
                key={subagent.call_id}
                label={subagent.name}
                status={SUBAGENT_STATUS_LABELS[subagent.status]}
                statusTone={SUBAGENT_STATUS_TONES[subagent.status]}
                ariaLabel={`Subagent for ${title}: ${subagent.name}, ${SUBAGENT_STATUS_LABELS[subagent.status]}`}
                icon={<Bot />}
                onClick={() => onOpenSubagent?.(subagent.call_id)}
              />
            ))}
          </div>
        )}
    </article>
  );
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
  onCommand,
  onWorkflowAction,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  repoName: string;
  title: string;
  pr: PullRequestDigest | undefined;
  archived: boolean;
  watchActive: boolean;
  terminalOpen: boolean;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onWorkflowAction?: (action: WorkspaceWorkflowAction) => void;
}) {
  const activity = workspaceActivitySummary(digest, session, terminalOpen);
  const matchingSession =
    !digest || session?.id === digest.session ? session : undefined;
  const stamp = archived
    ? (workspace.archived_at ?? workspace.created_at)
    : (matchingSession?.created_at ?? workspace.created_at);
  const age = formatCompactAge(stamp);
  const model = pr ? workspaceWorkflowModel(null, pr) : null;
  const primary =
    model?.primary && !(watchActive && model.primary !== "open_pr")
      ? model.primary
      : undefined;
  const primaryLabel =
    primary && model
      ? workspaceWorkflowActionLabel(primary, model.stage)
      : null;
  const prTitle = pr?.title?.trim();
  const showPrTitle = prTitle && prTitle !== title.trim();
  const checkLabel = model?.checks?.total
    ? checkSummary(model.checks)
    : pr?.checks_summary?.trim() || null;
  const checkTone: StatusTone = model?.checks?.failing
    ? "critical"
    : model?.checks?.pending
      ? "pending"
      : model?.checks?.passing
        ? "ready"
        : "neutral";
  const review = pr
    ? pullRequestReviewSummary({
        state: pr.state,
        draft: pr.draft ?? false,
        review_decision: pr.review_decision,
      })
    : null;
  const putAwayLabel =
    workspace.status === "released" ? "Released" : "Archived";
  const prCountLabel = workspacePrChipSummary(digest?.pr_count);

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
                STATUS_CHIP[prStatusTone(pr)],
              )}
            >
              {prStatusLabel(pr)}
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

        {pr && model && (
          <div className="mt-3 border-t border-border-subtle pt-3">
            <div className="flex items-start gap-2">
              <GitPullRequest
                className={cn(
                  "mt-0.5 size-3.5 shrink-0",
                  STATUS_MARK[prStatusTone(pr)],
                )}
                aria-hidden
              />
              <div className="min-w-0 flex-1">
                <div className="flex min-w-0 items-center gap-2">
                  <span className="shrink-0 text-xs font-medium tabular-nums">
                    Pull request #{pr.number}
                  </span>
                  {prCountLabel && (
                    <span className="rounded-md bg-muted px-1.5 py-0.5 text-2xs font-medium text-muted-foreground tabular-nums">
                      {prCountLabel}
                    </span>
                  )}
                  <span
                    className={cn(
                      "ml-auto shrink-0 text-xs font-medium",
                      STATUS_TEXT[model.tone],
                    )}
                  >
                    {model.summary.replace(/^#\d+\s*·\s*/, "")}
                  </span>
                </div>
                <p className="mt-1 text-xs leading-5 text-muted-foreground">
                  {model.detail}
                </p>
              </div>
            </div>

            <div className="mt-2.5 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs text-muted-foreground">
              {checkLabel && (
                <WorkspaceStatusFact
                  icon={<CheckCircle2 />}
                  label={checkLabel}
                  tone={checkTone}
                />
              )}
              {review && (
                <WorkspaceStatusFact
                  icon={<Eye />}
                  label={review.label}
                  tone={review.tone}
                />
              )}
              {pr.base_branch && (
                <WorkspaceStatusFact
                  icon={<GitBranch />}
                  label={`into ${pr.base_branch}`}
                  tone="neutral"
                />
              )}
            </div>

            {primary &&
              primary !== "open_pr" &&
              primaryLabel &&
              onWorkflowAction && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="mt-3 h-7 bg-foreground px-2.5 text-xs text-background hover:bg-foreground/88 hover:text-background"
                  title={model.detail}
                  onClick={() => onWorkflowAction(primary)}
                >
                  {primaryLabel}
                </Button>
              )}
          </div>
        )}
      </div>

      <div className="flex items-center gap-2 border-t border-border-subtle bg-muted/25 px-3 py-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 gap-1.5 px-2 text-xs"
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
              "h-7 gap-1.5 px-2 text-xs",
              STATUS_TEXT[prStatusTone(pr)],
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

function WorkspaceStatusFact({
  icon,
  label,
  tone,
}: {
  icon: ReactNode;
  label: string;
  tone: StatusTone;
}) {
  return (
    <span className={cn("inline-flex items-center gap-1", STATUS_TEXT[tone])}>
      <span className="[&_svg]:size-3 [&_svg]:shrink-0" aria-hidden>
        {icon}
      </span>
      <span>{label}</span>
    </span>
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
  if (workspace.status === "creating") {
    return (
      <div
        className={cn(
          "flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7 text-xs",
          STATUS_TEXT.pending,
        )}
      >
        <LoaderCircle className="size-3 shrink-0 animate-spin" aria-hidden />
        <span className="min-w-0 flex-1 truncate">Creating workspace</span>
      </div>
    );
  }
  const railDigest = digest && isSessionRowWorthy(digest) ? digest : undefined;
  if (!railDigest) return null;

  const activity = workspaceActivitySummary(railDigest, session, false);
  const matchingSession =
    session?.id === railDigest.session ? session : undefined;
  const harnessKind = railDigest.harness_kind ?? matchingSession?.harness_kind;
  const HarnessIcon = harnessKind ? HARNESS_ICONS[harnessKind] : null;
  const stamp = matchingSession?.created_at ?? workspace.created_at;
  const age = formatCompactAge(stamp);

  if (!activity) return null;
  const ActivityIcon = sessionActivityIcon(railDigest);
  const running = activity.tone === "running";

  return (
    <div className="flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7 text-xs text-muted-foreground">
      {activity.needsYou ? (
        <CircleAlert
          className={cn("size-3 shrink-0", STATUS_TEXT.critical)}
          aria-hidden
        />
      ) : harnessKind && HarnessIcon ? (
        // A brand mark, so it keeps its own identity: the running tone goes
        // on the label beside it, never on the engine's logo.
        <span title={HARNESS_LABELS[harnessKind]}>
          <HarnessIcon className="size-3 shrink-0" aria-hidden />
        </span>
      ) : activity.terminalOnly ? (
        <SquareTerminal className="size-3 shrink-0" aria-hidden />
      ) : ActivityIcon ? (
        <ActivityIcon
          className={cn(
            "size-3 shrink-0",
            running && [STATUS_TEXT.running, STATUS_MOTION.running],
          )}
          aria-hidden
        />
      ) : null}
      <span
        className={cn(
          "min-w-0 flex-1 truncate",
          activity.needsYou && STATUS_TEXT.critical,
          running && STATUS_TEXT.running,
        )}
      >
        {activity.label}
      </span>
      {age && (
        <span className="shrink-0 tabular-nums">
          {age === "now" ? "now" : age}
        </span>
      )}
    </div>
  );
}

function workspaceActivitySummary(
  digest: CodeSessionDigest | undefined,
  session: CodeSessionSnapshot | undefined,
  terminalOpen: boolean,
): {
  label: string;
  tone: StatusTone;
  needsYou: boolean;
  terminalOnly: boolean;
} | null {
  const needsYou =
    digest?.attention.state.type === "needs_you"
      ? digest.attention.state.prompt || "Needs you"
      : null;
  if (needsYou) {
    return {
      label: needsYou,
      tone: "critical",
      needsYou: true,
      terminalOnly: false,
    };
  }

  const worthyDigest =
    digest && isSessionRowWorthy(digest) ? digest : undefined;
  const statusLabel = worthyDigest
    ? sessionRowLabel(worthyDigest)
    : digest
      ? LIFECYCLE_LABELS[digest.lifecycle]
      : session
        ? LIFECYCLE_LABELS[session.lifecycle]
        : null;
  const turnCount = digest?.turn_count;
  const turnLabel =
    turnCount !== undefined
      ? `${turnCount} ${turnCount === 1 ? "turn" : "turns"}`
      : null;
  const sessionLine = [statusLabel, turnLabel].filter(Boolean).join(" · ");
  const terminalOnly = terminalOpen && !sessionLine;
  if (!sessionLine && !terminalOnly) return null;

  return {
    label: sessionLine || "Terminal open",
    tone: digestStatusTone(digest),
    needsYou: false,
    terminalOnly,
  };
}

function sessionActivityIcon(digest: CodeSessionDigest) {
  const runningSubagents = digest.subagents?.some(
    (entry) => entry.status === "running",
  );
  if (runningSubagents || digest.activity === "subagents") return Bot;
  switch (digest.activity) {
    case "shell":
      return SquareTerminal;
    case "monitor":
      return Radar;
    case "file":
      return FileCode2;
    case "search":
      return Search;
    case "tool":
      return Wrench;
    case "agent":
    case undefined:
      return Bot;
  }
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
  const tone = prTone(pr);
  return (
    <GitPullRequest
      className={cn("size-3 shrink-0", STATUS_MARK[prStatusTone(pr)])}
      data-pr-state={tone}
      aria-hidden
    />
  );
}

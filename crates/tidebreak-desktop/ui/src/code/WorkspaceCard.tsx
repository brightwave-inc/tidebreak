import type { ReactNode } from "react";
import {
  Bot,
  CircleAlert,
  CornerDownRight,
  ExternalLink,
  Eye,
  FileCode2,
  GitPullRequest,
  Radar,
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
  workspaceWorkflowActionLabel,
  workspaceWorkflowModel,
  type WorkspaceWorkflowAction,
} from "./workspaceWorkflow";
import {
  digestStatusTone,
  STATUS_MARK,
  STATUS_MOTION,
  STATUS_TEXT,
  type StatusTone,
} from "./statusTone";
import {
  formatCompactAge,
  isSessionRowWorthy,
  middleTruncate,
  PR_ICON_TONE_CLASSES,
  prTone,
  sessionRowLabel,
  watchRowLabel,
  workspaceCardLabel,
  type CardDensity,
  isPutAway,
} from "./workspaceCards";

/**
 * One workspace in the rail.
 *
 * This is deliberately a row, not a miniature dashboard. The title opens the
 * conversation, live work nests beneath it, and Git / PR state sits in a
 * dedicated action line that is visible without hover. Right-click remains the
 * complete command path for less common workspace operations.
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
  onOpen,
  onCommand,
  onOpenChildSession,
  onOpenSubagent,
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
  onOpen: () => void;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onOpenChildSession?: (sessionId: string) => void;
  onOpenSubagent?: (callId: string) => void;
  onWorkflowAction?: (action: WorkspaceWorkflowAction) => void;
}) {
  const title = digest?.title ?? workspace.title;
  const pr = digest?.pr_state ?? workspace.pr;
  const archived = isPutAway(workspace);
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
      <ContextMenu>
        <ContextMenuTrigger asChild>
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
            })}
            aria-current={active ? "page" : undefined}
            className={cn(
              "flex w-full cursor-pointer flex-col gap-0.5 rounded-xl px-2.5 py-2 text-left",
              FOCUS_RING_INSET,
              HOVER_TINT,
            )}
            onClick={onOpen}
          >
            <span className="flex min-w-0 items-center gap-2">
              <AttentionBadge attention={digest?.attention} compact />
              <span className="min-w-0 flex-1 truncate text-[13.5px] font-medium leading-5">
                {title}
              </span>
              <span className="flex shrink-0 items-center gap-1.5" aria-hidden>
                {pr && <PrGlyph pr={pr} />}
                {terminalOpen && (
                  <SquareTerminal className="size-3 text-muted-foreground" />
                )}
                {digest?.attention.state.type === "needs_you" &&
                  digest.attention.state.source === "structured" && (
                    <CircleAlert
                      className={cn("size-3", STATUS_MARK.critical)}
                    />
                  )}
              </span>
            </span>
            {density === "detailed" &&
              (visibleMeta.repoChip || visibleMeta.branch) && (
                <span className="flex min-w-0 items-center gap-1.5 pl-5 text-[11px] text-muted-foreground">
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

      {density === "detailed" && (
        <WorkspaceStateLine
          workspace={workspace}
          digest={digest}
          session={session}
          pr={pr}
          archived={archived}
          watchActive={watchActive}
          terminalOpen={terminalOpen}
          onCommand={onCommand}
          onWorkflowAction={onWorkflowAction}
        />
      )}

      {density === "detailed" &&
        (childSessions.length > 0 || (digest?.subagents?.length ?? 0) > 0) && (
          <div className="relative mr-2 mb-2 ml-5 flex flex-col gap-0.5 border-l border-border-subtle pl-2">
            {childSessions.map((child) => (
              <WorkspaceChildRow
                key={child.session}
                label={`Watch - ${watchRowLabel(child)}`}
                ariaLabel={`Watch task for ${title}: ${watchRowLabel(child)}`}
                icon={<Eye />}
                attention={child.attention}
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

function WorkspaceStateLine({
  workspace,
  digest,
  session,
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
  pr: PullRequestDigest | undefined;
  archived: boolean;
  watchActive: boolean;
  terminalOpen: boolean;
  onCommand: (command: WorkspaceCommand["id"]) => void;
  onWorkflowAction?: (action: WorkspaceWorkflowAction) => void;
}) {
  if (archived) {
    return (
      <div className="flex items-center gap-2 px-2.5 pb-2 pl-7">
        <span className="text-xs text-muted-foreground">Archived</span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="ml-auto h-6 px-2 text-[11px]"
          onClick={() => onCommand("restore")}
        >
          Restore
        </Button>
      </div>
    );
  }

  if (pr) {
    const model = workspaceWorkflowModel(null, pr);
    const primary =
      model.primary && !(watchActive && model.primary !== "open_pr")
        ? model.primary
        : undefined;
    const primaryLabel = primary
      ? workspaceWorkflowActionLabel(primary, model.stage)
      : null;
    const stateLabel = model.summary.replace(/^#\d+\s*·\s*/, "");

    return (
      <>
        <div className="flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7">
          <button
            type="button"
            className={cn(
              "flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md px-1.5 py-1 text-[11px] font-medium hover:bg-muted",
              STATUS_TEXT[model.tone],
              FOCUS_RING_INSET,
              HOVER_TINT,
            )}
            aria-label={`Open pull request #${pr.number}`}
            title={model.detail}
            onClick={() => onCommand("open-pr")}
          >
            <GitPullRequest className="size-3 shrink-0" aria-hidden />
            <span className="shrink-0 tabular-nums">#{pr.number}</span>
            <span className="min-w-0 truncate text-muted-foreground">
              {stateLabel}
            </span>
            <ExternalLink className="size-2.5 shrink-0 opacity-55" aria-hidden />
          </button>
          {primary &&
            primary !== "open_pr" &&
            primaryLabel &&
            onWorkflowAction && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="ml-auto h-6 shrink-0 bg-foreground px-2 text-[11px] text-background hover:bg-foreground/88 hover:text-background"
                title={model.detail}
                onClick={() => onWorkflowAction(primary)}
              >
                {primaryLabel}
              </Button>
            )}
        </div>
        <WorkspaceActivityLine
          workspace={workspace}
          digest={digest}
          session={session}
          terminalOpen={terminalOpen}
        />
      </>
    );
  }

  return (
    <WorkspaceActivityLine
      workspace={workspace}
      digest={digest}
      session={session}
      terminalOpen={terminalOpen}
    />
  );
}

function WorkspaceActivityLine({
  workspace,
  digest,
  session,
  terminalOpen,
}: {
  workspace: CodeWorkspaceSnapshot;
  digest: CodeSessionDigest | undefined;
  session: CodeSessionSnapshot | undefined;
  terminalOpen: boolean;
}) {
  const needsYou =
    digest?.attention.state.type === "needs_you"
      ? digest.attention.state.prompt || "Needs you"
      : null;
  const worthyDigest =
    digest && isSessionRowWorthy(digest) ? digest : undefined;
  const statusLabel = needsYou
    ? null
    : worthyDigest
      ? sessionRowLabel(worthyDigest)
      : digest
        ? LIFECYCLE_LABELS[digest.lifecycle]
        : session
          ? LIFECYCLE_LABELS[session.lifecycle]
          : null;
  const turnCount = digest?.turn_count;
  const turnLabel =
    !needsYou && turnCount !== undefined
      ? `${turnCount} ${turnCount === 1 ? "turn" : "turns"}`
      : null;
  const sessionLine = [statusLabel, turnLabel].filter(Boolean).join(" · ");
  const HarnessIcon = session ? HARNESS_ICONS[session.harness_kind] : null;
  const stamp = session?.created_at ?? workspace.created_at;
  const age = formatCompactAge(stamp);

  const terminalOnly = terminalOpen && !needsYou && !sessionLine;
  if (!needsYou && !sessionLine && !terminalOnly) return null;
  const ActivityIcon = digest ? sessionActivityIcon(digest) : null;
  const running = digestStatusTone(digest) === "running";

  return (
    <div className="flex min-w-0 items-center gap-1.5 px-2.5 pb-2 pl-7 text-[11px] text-muted-foreground">
      {needsYou ? (
        <CircleAlert
          className={cn("size-3 shrink-0", STATUS_TEXT.critical)}
          aria-hidden
        />
      ) : session && HarnessIcon ? (
        // A brand mark, so it keeps its own identity: the running tone goes
        // on the label beside it, never on the engine's logo.
        <span title={HARNESS_LABELS[session.harness_kind]}>
          <HarnessIcon className="size-3 shrink-0" aria-hidden />
        </span>
      ) : terminalOnly ? (
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
          needsYou && STATUS_TEXT.critical,
          running && STATUS_TEXT.running,
        )}
      >
        {needsYou ?? (sessionLine || "Terminal open")}
      </span>
      {age && (
        <span className="shrink-0 tabular-nums">
          {age === "now" ? "now" : age}
        </span>
      )}
    </div>
  );
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
        "flex w-full cursor-pointer items-center gap-1.5 rounded-lg px-1.5 py-1 text-left text-[11px] text-muted-foreground hover:bg-muted hover:text-foreground",
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
      className={cn("size-3 shrink-0", PR_ICON_TONE_CLASSES[tone])}
      data-pr-state={tone}
      aria-hidden
    />
  );
}

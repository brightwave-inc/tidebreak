import { useState } from "react";
import {
  Activity as ActivityIcon,
  Bot,
  ChevronDown,
  ChevronUp,
  FolderOpen,
  Globe2,
  Shapes,
  Shield,
} from "lucide-react";

import type { AgentRun } from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import { folderAccessLabel, folderReach } from "./FolderAccess";
import type { ChatFolderAccess } from "./useChatFolderAttachments";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";

/** Past this many dots the chip counts the rest rather than growing. */
const MAX_STATUS_DOTS = 3;

/**
 * Live work reads in two colours, matching how the agent rows phrase it:
 * green while a run is making progress, amber while it is parked — waiting on
 * something, or backing off before another attempt.
 */
function liveRunDotClass(status: AgentRun["status"]): string {
  switch (status) {
    case "waiting":
    case "retry_wait":
    case "cancelling":
      return "bg-warning";
    default:
      return "bg-success";
  }
}

export type ChatStatusChipProps = {
  /** Collapse the persistent card while a side panel is using the canvas. */
  compact?: boolean;
  /** How many outputs the conversation has produced. */
  outputCount: number;
  folders: readonly ChatFolderAccess[];
  /** The chat's background runs — live and settled both. */
  runs: readonly AgentRun[];
  /** Bring the Outputs panel forward. */
  onOpenOutputs: () => void;
  /** Bring the Folders panel forward. */
  onOpenFolders: () => void;
  /** Bring the Permissions panel forward. */
  onOpenPermissions: () => void;
  /** Bring the agents table forward. */
  onOpenAgents: () => void;
  /** Open the chat-scoped browser tab.  Absent when no native host is available. */
  onOpenBrowser?: () => void;
  /** How many standing approvals reach this chat, when known. */
  permissionCount?: number;
};

/**
 * What this conversation has going on, always in the header: live background
 * work first, what it has produced otherwise.
 *
 * Behind it, the places that describe only this conversation — its outputs,
 * its folders, its permissions, its background agents. The context meter sits beside this chip
 * in the header; the composer still owns the chat's settings (model, mode,
 * effort). This chip is the conversation's activity, not a second copy of
 * those controls.
 */
export function ChatStatusChip({
  compact = false,
  outputCount,
  folders,
  runs,
  onOpenOutputs,
  onOpenFolders,
  onOpenPermissions,
  onOpenAgents,
  onOpenBrowser,
  permissionCount,
}: ChatStatusChipProps) {
  const [open, setOpen] = useState(false);
  const [collapsed, setCollapsed] = useState(false);

  const liveRuns = runs.filter((run) => RUNNING_AGENT_STATUSES.has(run.status));
  const overflowRuns = Math.max(0, liveRuns.length - MAX_STATUS_DOTS);

  // The most useful thing the face can say right now: work still moving beats
  // a tally of what is done, and a chat with neither wears a quiet name.
  const faceLabel =
    liveRuns.length > 0
      ? `${liveRuns.length} running`
      : outputCount > 0
        ? outputCount === 1
          ? "1 output"
          : `${outputCount} outputs`
        : "Activity";

  const outputsSummary =
    outputCount === 0
      ? "No outputs yet"
      : outputCount === 1
        ? "1 output"
        : `${outputCount} outputs`;

  const folderSummary =
    folders.length === 0
      ? "No folders"
      : folders.length === 1
        ? `${folders[0].displayName} · ${folderAccessLabel(folderReach(folders[0].statements))}`
        : `${folders.length} folders`;

  const agentsSummary =
    runs.length === 0
      ? "None yet"
      : liveRuns.length > 0
        ? `${liveRuns.length} of ${runs.length} running`
        : runs.length === 1
          ? "1 finished"
          : `${runs.length} finished`;

  const permissionsSummary =
    permissionCount == null
      ? "Saved approvals"
      : permissionCount === 0
        ? "None saved"
        : permissionCount === 1
          ? "1 saved"
          : `${permissionCount} saved`;

  const details = (
    <ActivityDetails
      outputsSummary={outputsSummary}
      folderSummary={folderSummary}
      permissionsSummary={permissionsSummary}
      agentsSummary={agentsSummary}
      onOpenOutputs={onOpenOutputs}
      onOpenFolders={onOpenFolders}
      onOpenPermissions={onOpenPermissions}
      onOpenAgents={onOpenAgents}
      onOpenBrowser={onOpenBrowser}
      browserSummary={onOpenBrowser ? "Open shared tab" : undefined}
      onChoose={() => setOpen(false)}
    />
  );

  if (!compact) {
    if (collapsed) {
      return (
        <button
          type="button"
          className="activity-card-collapsed"
          aria-label="Expand work activity"
          onClick={() => setCollapsed(false)}
        >
          <ActivityIcon className="size-4" aria-hidden="true" />
        </button>
      );
    }

    return (
      <aside className="activity-card" aria-label="Work activity">
        <div className="activity-card-heading">
          <div>
            <p className="activity-card-kicker">Activity</p>
            <p className="activity-card-summary">{faceLabel}</p>
          </div>
          <button
            type="button"
            className="activity-card-collapse-button"
            aria-label="Collapse work activity"
            onClick={() => setCollapsed(true)}
          >
            <ChevronUp className="size-4" aria-hidden="true" />
          </button>
        </div>
        <div className="activity-card-rows">{details}</div>
      </aside>
    );
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border border-border px-2.5 text-xs whitespace-nowrap text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          aria-label={
            faceLabel === "Activity"
              ? "Work activity"
              : `Work activity: ${faceLabel}`
          }
        >
          {liveRuns.length > 0 && (
            <span className="flex items-center gap-1" aria-hidden="true">
              {liveRuns.slice(0, MAX_STATUS_DOTS).map((run) => (
                <span
                  key={run.id}
                  className={cn(
                    "size-1.5 rounded-full",
                    liveRunDotClass(run.status),
                  )}
                />
              ))}
              {overflowRuns > 0 && (
                <span className="text-2xs leading-none">{`+${overflowRuns}`}</span>
              )}
            </span>
          )}
          <span>{faceLabel}</span>
          <ChevronDown className="size-3.5 opacity-50" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-1.5">
        {details}
      </PopoverContent>
    </Popover>
  );
}

function ActivityDetails({
  outputsSummary,
  folderSummary,
  permissionsSummary,
  agentsSummary,
  onOpenOutputs,
  onOpenFolders,
  onOpenPermissions,
  onOpenAgents,
  onOpenBrowser,
  browserSummary,
  onChoose,
}: {
  outputsSummary: string;
  folderSummary: string;
  permissionsSummary: string;
  agentsSummary: string;
  browserSummary?: string;
  onOpenOutputs: () => void;
  onOpenFolders: () => void;
  onOpenPermissions: () => void;
  onOpenAgents: () => void;
  onOpenBrowser?: () => void;
  onChoose: () => void;
}) {
  const choose = (action: () => void) => {
    onChoose();
    action();
  };

  return (
    <>
      <DetailRow
        icon={<Shapes className="size-4" aria-hidden="true" />}
        label="Outputs"
        value={outputsSummary}
        onClick={() => choose(onOpenOutputs)}
      />
      <DetailRow
        icon={<FolderOpen className="size-4" aria-hidden="true" />}
        label="Folders"
        value={folderSummary}
        onClick={() => choose(onOpenFolders)}
      />
      <DetailRow
        icon={<Shield className="size-4" aria-hidden="true" />}
        label="Permissions"
        value={permissionsSummary}
        onClick={() => choose(onOpenPermissions)}
      />
      <DetailRow
        icon={<Bot className="size-4" aria-hidden="true" />}
        label="Agents"
        value={agentsSummary}
        onClick={() => choose(onOpenAgents)}
      />
      {onOpenBrowser && (
        <DetailRow
          icon={<Globe2 className="size-4" aria-hidden="true" />}
          label="Browser"
          value={browserSummary ?? "Open shared tab"}
          onClick={() => choose(onOpenBrowser)}
        />
      )}
    </>
  );
}

function DetailRow({
  icon,
  label,
  value,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  onClick: () => void;
}) {
  return (
    <button type="button" className="activity-card-row" onClick={onClick}>
      <span className="activity-card-row-label">
        {icon}
        {label}
      </span>
      <span className="activity-card-row-value">{value}</span>
    </button>
  );
}

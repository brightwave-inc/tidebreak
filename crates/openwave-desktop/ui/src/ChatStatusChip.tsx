import { useState } from "react";
import { Bot, ChevronDown, FolderOpen, Shapes } from "lucide-react";

import type { AgentRun } from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import { folderAccessLabel, folderReach } from "./FolderAccess";
import type { ChatFolderAccess } from "./useChatFolderAttachments";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
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
  /** How many outputs the conversation has produced. */
  outputCount: number;
  folders: readonly ChatFolderAccess[];
  /** The chat's background runs — live and settled both. */
  runs: readonly AgentRun[];
  /** Bring the Outputs panel forward. */
  onOpenOutputs: () => void;
  /** Bring the Folders panel forward. */
  onOpenFolders: () => void;
  /** Bring the agents table forward. */
  onOpenAgents: () => void;
};

/**
 * What this conversation has going on, always in the header: live background
 * work first, what it has produced otherwise.
 *
 * Behind it, the places that describe only this conversation — its outputs,
 * its folders, its background agents. The context meter sits beside this chip
 * in the header; the composer still owns the chat's settings (model, mode,
 * effort). This chip is the conversation's activity, not a second copy of
 * those controls.
 */
export function ChatStatusChip({
  outputCount,
  folders,
  runs,
  onOpenOutputs,
  onOpenFolders,
  onOpenAgents,
}: ChatStatusChipProps) {
  const [open, setOpen] = useState(false);

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

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border border-border px-2.5 text-xs whitespace-nowrap text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          aria-label={
            faceLabel === "Activity" ? "Chat activity" : `Chat activity: ${faceLabel}`
          }
        >
          {liveRuns.length > 0 && (
            <span className="flex items-center gap-1" aria-hidden="true">
              {liveRuns.slice(0, MAX_STATUS_DOTS).map((run) => (
                <span
                  key={run.id}
                  className={cn("size-1.5 rounded-full", liveRunDotClass(run.status))}
                />
              ))}
              {overflowRuns > 0 && (
                <span className="text-[0.65rem] leading-none">{`+${overflowRuns}`}</span>
              )}
            </span>
          )}
          <span>{faceLabel}</span>
          <ChevronDown className="size-3.5 opacity-50" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-1">
        <DetailRow
          icon={<Shapes className="size-3.5" aria-hidden="true" />}
          label="Outputs"
          value={outputsSummary}
          onClick={() => {
            setOpen(false);
            onOpenOutputs();
          }}
        />
        <DetailRow
          icon={<FolderOpen className="size-3.5" aria-hidden="true" />}
          label="Folders"
          value={folderSummary}
          onClick={() => {
            setOpen(false);
            onOpenFolders();
          }}
        />
        <DetailRow
          icon={<Bot className="size-3.5" aria-hidden="true" />}
          label="Agents"
          value={agentsSummary}
          onClick={() => {
            setOpen(false);
            onOpenAgents();
          }}
        />
      </PopoverContent>
    </Popover>
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
    <button
      type="button"
      className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent"
      onClick={onClick}
    >
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        {icon}
        {label}
      </span>
      <span className="min-w-0 truncate text-xs">{value}</span>
    </button>
  );
}

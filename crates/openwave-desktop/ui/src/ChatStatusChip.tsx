import { useEffect, useMemo, useState } from "react";
import { Bot, ChevronDown, FolderOpen, Shapes } from "lucide-react";

import type { AgentRun, ApiClient, Chat } from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import { useChatSessionStore } from "./ChatSessionStore";
import { listDeliverables } from "./deliverables";
import { folderAccessLabel, folderReach } from "./FolderAccess";
import { permissionModeOption } from "./PermissionModeMenu";
import { useRefreshSignals } from "./RefreshSignals";
import { backgroundAgentSpawnKeys, useAgentRuns } from "./useAgentRuns";
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
  client: ApiClient;
  chat: Chat;
  folders: readonly ChatFolderAccess[];
  /** Bring the Outputs panel forward. */
  onOpenOutputs: () => void;
  /** Bring the Folders panel forward. */
  onOpenFolders: () => void;
  /** Bring one background run's panel forward. */
  onOpenAgent: (runId: string) => void;
};

/**
 * The chat's standing state, always in the header: which permission mode new
 * turns run under, and whether anything is still working in the background.
 *
 * Behind it, the places that describe only this conversation — its outputs,
 * its folders, its background agents. The composer already owns changing the
 * model and the mode; repeating those controls here would leave two switches
 * for one setting, so this popover only holds what has nowhere else to live
 * now that the rail is the chat list.
 */
export function ChatStatusChip({
  client,
  chat,
  folders,
  onOpenOutputs,
  onOpenFolders,
  onOpenAgent,
}: ChatStatusChipProps) {
  const [open, setOpen] = useState(false);

  // Subscribed as a joined key rather than the message list: the header must
  // not re-render on every streamed token, and the set of spawn steps changes
  // only when one appears or resolves.
  const spawnKey = useChatSessionStore((session) =>
    backgroundAgentSpawnKeys(session.messages).join(","),
  );
  const spawnKeys = useMemo(
    () => (spawnKey ? spawnKey.split(",") : []),
    [spawnKey],
  );
  const { runs } = useAgentRuns(client, chat.id, spawnKeys);

  const matched = runs.filter(
    (run) =>
      run.tier === "background" &&
      spawnKeys.some((key) => run.id === key || run.spawn_call_id === key),
  );
  const liveRuns = matched.filter((run) => RUNNING_AGENT_STATUSES.has(run.status));
  const overflowRuns = Math.max(0, liveRuns.length - MAX_STATUS_DOTS);

  const mode = permissionModeOption(chat.permission_mode);
  const outputCount = useOutputCount(chat.id);

  const folderSummary =
    folders.length === 0
      ? "No folders"
      : folders.length === 1
        ? `${folders[0].displayName} · ${folderAccessLabel(folderReach(folders[0].statements))}`
        : `${folders.length} folders`;

  // The run a reader most likely means by "show me": the newest one still
  // working, or — once everything has settled — the newest run there is.
  const focusRun = liveRuns.at(-1) ?? matched.at(-1) ?? null;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          className={cn(
            "flex h-7 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border border-border px-2.5 text-xs whitespace-nowrap transition-colors hover:bg-accent hover:text-foreground",
            mode.elevated ? "text-warning-foreground" : "text-muted-foreground",
          )}
          aria-label={
            liveRuns.length > 0
              ? `Chat status: ${mode.label}, ${liveRuns.length} running`
              : `Chat status: ${mode.label}`
          }
        >
          <span>{mode.label}</span>
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
                <span className="text-[0.65rem] leading-none">{`+${overflowRuns}`}</span>
              )}
            </span>
          )}
          <ChevronDown className="size-3.5 opacity-50" />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-1">
        <DetailRow
          icon={<Shapes className="size-3.5" aria-hidden="true" />}
          label="Outputs"
          value={
            outputCount === 0
              ? "No outputs yet"
              : outputCount === 1
                ? "1 output"
                : `${outputCount} outputs`
          }
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
        {focusRun && (
          <DetailRow
            icon={<Bot className="size-3.5" aria-hidden="true" />}
            label="Agents"
            value={
              matched.length === 1
                ? "1 background agent"
                : `${matched.length} background agents`
            }
            onClick={() => {
              setOpen(false);
              onOpenAgent(focusRun.id);
            }}
          />
        )}
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

/**
 * How many outputs this conversation has produced. Fetched on mount and again
 * whenever a refresh signal says the number could have moved; errors are
 * swallowed, leaving the last known count, because a stale number in a summary
 * row is better than an error state in one.
 */
function useOutputCount(chatId: string): number {
  const [count, setCount] = useState(0);
  const outputWritebacks = useRefreshSignals((s) => s.outputWritebacks);

  useEffect(() => {
    let cancelled = false;
    listDeliverables(chatId).then(
      (catalog) => {
        if (!cancelled) setCount(catalog.deliverables.length);
      },
      () => {
        /* swallow — stale count is acceptable */
      },
    );
    return () => {
      cancelled = true;
    };
  }, [chatId, outputWritebacks]);

  return count;
}

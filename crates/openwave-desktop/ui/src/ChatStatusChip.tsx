import { useMemo, useState } from "react";
import { Bot, ChevronDown, FolderOpen } from "lucide-react";

import type {
  AgentRun,
  ApiClient,
  Chat,
  ModelInfo,
  ModelSelectionKey,
  PermissionMode,
} from "./api";
import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import { useChatSessionStore } from "./ChatSessionStore";
import { folderAccessLabel, folderReach } from "./FolderAccess";
import { ModelMenu } from "./ModelMenu";
import { permissionModeOption, PermissionModeMenu } from "./PermissionModeMenu";
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
  models: ModelInfo[];
  defaultModelKey: string | null;
  folders: readonly ChatFolderAccess[];
  disabled?: boolean;
  onModelChange: (key: ModelSelectionKey | null) => void | Promise<void>;
  onPermissionModeChange: (mode: PermissionMode) => void | Promise<void>;
  /** Bring the Folders panel forward. */
  onOpenFolders: () => void;
  /** Bring one background run's panel forward. */
  onOpenAgent: (runId: string) => void;
};

/**
 * The chat's standing state, always in the header: which permission mode new
 * turns run under, and whether anything is still working in the background.
 *
 * The composer keeps its own controls — this is the answer to "what is this
 * conversation set to right now" from anywhere in the transcript, plus a way
 * to change it without scrolling back down. The menus behind it are the same
 * components the composer uses, so a mode or model can only be described one
 * way in the app.
 */
export function ChatStatusChip({
  client,
  chat,
  models,
  defaultModelKey,
  folders,
  disabled,
  onModelChange,
  onPermissionModeChange,
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
      <PopoverContent
        align="end"
        className="w-80 p-1"
        // The rows open their own menus, which are portalled outside this
        // content. Dismissing on those clicks would take the popover down
        // underneath the menu the reader is still using.
        onPointerDownOutside={(event) => {
          if ((event.target as Element | null)?.closest?.("[role='menu']")) {
            event.preventDefault();
          }
        }}
      >
        <div className="flex items-center justify-between gap-2 px-2 py-1">
          <span className="text-xs text-muted-foreground">Model</span>
          <ModelMenu
            models={models}
            value={chat.model}
            defaultKey={defaultModelKey}
            disabled={disabled}
            onChange={onModelChange}
          />
        </div>
        <div className="flex items-center justify-between gap-2 px-2 py-1">
          <span className="text-xs text-muted-foreground">Permissions</span>
          <PermissionModeMenu
            value={chat.permission_mode}
            disabled={disabled}
            onChange={onPermissionModeChange}
          />
        </div>
        <button
          type="button"
          className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent"
          onClick={() => {
            setOpen(false);
            onOpenFolders();
          }}
        >
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <FolderOpen className="size-3.5" aria-hidden="true" />
            Folders
          </span>
          <span className="min-w-0 truncate text-xs">{folderSummary}</span>
        </button>
        {focusRun && (
          <button
            type="button"
            className="flex w-full cursor-pointer items-center justify-between gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent"
            onClick={() => {
              setOpen(false);
              onOpenAgent(focusRun.id);
            }}
          >
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Bot className="size-3.5" aria-hidden="true" />
              Agents
            </span>
            <span className="text-xs">
              {matched.length === 1
                ? "1 background agent"
                : `${matched.length} background agents`}
            </span>
          </button>
        )}
      </PopoverContent>
    </Popover>
  );
}

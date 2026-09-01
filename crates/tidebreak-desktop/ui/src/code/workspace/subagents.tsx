import type { ApiClient } from "../../api/client";
import { Badge } from "@/components/ui/badge";
import { Bot, CircleDotDashed } from "lucide-react";
import { Button } from "@/components/ui/button";
import type {
  CodeSubagentStatus,
  CodeSubagentSummary,
  CodeWatchSnapshot,
} from "../../api/types";
import type { CodeTranscriptItem } from "../CodeSessionReducer";
import { STATUS_MARK } from "../statusTone";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { toast } from "sonner";
import { useState } from "react";

type SubagentViewStatus = CodeSubagentStatus | "unavailable";

export function subagentSummaryFromTranscript(
  items: readonly CodeTranscriptItem[],
  callId: string,
): CodeSubagentSummary | null {
  const task = items.find(
    (item) =>
      item.kind === "tool" &&
      item.parentCallId === null &&
      item.callId === callId &&
      item.name === "Task",
  );
  if (!task || task.kind !== "tool") return null;
  return {
    call_id: callId,
    name: toolDetailSubject(task.detail) || task.name,
    status:
      task.status === "running"
        ? "running"
        : task.status === "succeeded"
          ? "done"
          : "failed",
  };
}

function toolDetailSubject(
  detail: Extract<CodeTranscriptItem, { kind: "tool" }>["detail"],
): string;
function toolDetailSubject(
  detail: Extract<CodeTranscriptItem, { kind: "tool" }>["detail"],
): string {
  switch (detail.kind) {
    case "command":
      return detail.cmd;
    case "file_read":
    case "file_edit":
      return detail.path;
    case "search":
      return detail.query;
    case "other":
      return detail.summary;
  }
}

export function subagentEmptyState(status: CodeSubagentStatus | undefined): {
  title: string;
  description: string;
} {
  switch (status) {
    case "running":
      return {
        title: "Waiting for this subagent",
        description:
          "It is still running, but it has not produced attributed transcript output yet.",
      };
    case "done":
      return {
        title: "No captured subagent output",
        description:
          "This subagent completed without leaving attributed assistant or tool activity.",
      };
    case "failed":
      return {
        title: "No captured subagent output",
        description:
          "This subagent ended before attributed assistant or tool activity was captured.",
      };
    default:
      return {
        title: "Subagent unavailable",
        description:
          "This link no longer matches a captured Task in the parent session.",
      };
  }
}

export function SubagentContextBar({
  name,
  status,
  onBack,
}: {
  name: string;
  status: SubagentViewStatus;
  onBack?: () => void;
}) {
  const label =
    status === "running"
      ? "Running"
      : status === "done"
        ? "Completed"
        : status === "failed"
          ? "Failed"
          : "Unavailable";
  const variant =
    status === "running"
      ? "info"
      : status === "done"
        ? "success"
        : status === "failed"
          ? "critical"
          : "outline";
  return (
    <div
      className="border-border-subtle bg-background/85 mx-auto mt-3 flex w-[calc(100%-2rem)] max-w-3xl items-center gap-2 rounded-lg border px-3 py-2 shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_4%,transparent)]"
      data-testid="subagent-context-bar"
    >
      <span className="grid size-7 shrink-0 place-items-center text-muted-foreground">
        <Bot className="size-3.5" aria-hidden />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate text-xs font-semibold" title={name}>
            {name}
          </span>
          <Badge variant={variant} size="sm" className="shrink-0">
            {label}
          </Badge>
        </div>
        <p className="text-muted-foreground text-xs">Read-only subagent view</p>
      </div>
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        Back to main agent
      </Button>
    </div>
  );
}

/**
 * The watch task's seat in the transcript view: the sweep drives this
 * session's turns, so instead of a composer the reader gets what the watch
 * is doing and the two decisions that are theirs — stop it, or go back.
 */
export function WatchTaskBar({
  client,
  workspaceId,
  watch,
  onBack,
  onStopped,
}: {
  client: Pick<ApiClient, "stopCodeWatch">;
  workspaceId: string;
  watch: CodeWatchSnapshot | undefined;
  onBack: () => void;
  onStopped: () => void;
}) {
  const [stopping, setStopping] = useState(false);
  const active =
    watch !== undefined &&
    (watch.state === "watching" ||
      watch.state === "fixing" ||
      watch.state === "blocked");
  const label = !watch
    ? "This watch task has finished."
    : watch.state === "fixing"
      ? `Fixing PR #${watch.pr_number}${watch.cycles > 0 ? ` · fix turn ${watch.cycles}` : ""}`
      : watch.state === "blocked"
        ? `Watch blocked${watch.detail ? `: ${watch.detail}` : ""}`
        : watch.state === "watching"
          ? `Watching PR #${watch.pr_number}${watch.detail ? ` · ${watch.detail}` : ""}`
          : `Watch ${watch.state}${watch.detail ? `: ${watch.detail}` : ""}`;
  return (
    <div
      className="border-border-subtle bg-background/80 mx-auto mb-3 flex w-full max-w-3xl items-center gap-2 rounded-md border px-3 py-2 text-xs"
      data-testid="watch-task-bar"
    >
      <CircleDotDashed
        className={cn(
          "size-3.5 shrink-0",
          watch?.state === "blocked"
            ? STATUS_MARK.warning
            : STATUS_MARK.pending,
        )}
        aria-hidden
      />
      <span className="min-w-0 flex-1 truncate" title={watch?.detail}>
        {label}
      </span>
      {active && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={stopping}
          onClick={() => {
            setStopping(true);
            client
              .stopCodeWatch(workspaceId)
              .then(() => onStopped())
              .catch((err) => {
                toast.error(
                  friendlyErrorMessage(err, "Could not stop the watch"),
                );
              })
              .finally(() => setStopping(false));
          }}
        >
          {stopping ? "Stopping…" : "Stop watching"}
        </Button>
      )}
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        Back to main task
      </Button>
    </div>
  );
}

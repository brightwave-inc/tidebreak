import { FileCode, FileSearch, Search, SquareTerminal, Wrench } from "lucide-react";

import type { CodeApprovalSnapshot, ToolDetail } from "../api/types";
import { CodeApprovalCard } from "./CodeApprovalCard";
import { Badge } from "@/components/ui/badge";
import { MessageMarkdown } from "@/MessageMarkdown";
import { ThinkingAccordion } from "@/ThinkingAccordion";
import { ToolCardShell } from "@/ToolCardShell";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { TurnReviewCard } from "./TurnReviewCard";

/**
 * The code-session transcript: markdown for assistant text, tool-card chrome
 * for engine work, and visible harness notices.
 *
 * Approvals stay out of this surface. Turn boundaries open the Diff panel.
 */

export function CodeTranscript({
  items,
  onOpenTurnDiff,
  approvals = {},
  decidingId,
  approvalError,
  onDecide,
}: {
  items: CodeTranscriptItem[];
  onOpenTurnDiff?: (turnId: string) => void;
  approvals?: Record<string, CodeApprovalSnapshot>;
  decidingId?: string | null;
  approvalError?: string;
  onDecide?: (
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) => void;
}) {
  if (items.length === 0) {
    return (
      <p className="text-muted-foreground px-4 py-8 text-sm">
        Send a message to start a turn.
      </p>
    );
  }
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-3 px-4 py-4">
      {items.map((item) => (
        <TranscriptItem
          key={item.id}
          item={item}
          onOpenTurnDiff={onOpenTurnDiff}
          approval={
            item.kind === "approval" ? approvals[item.approvalId] : undefined
          }
          deciding={item.kind === "approval" && decidingId === item.approvalId}
          approvalError={
            item.kind === "approval" && decidingId === item.approvalId
              ? approvalError
              : undefined
          }
          onDecide={onDecide}
        />
      ))}
    </div>
  );
}

function TranscriptItem({
  item,
  onOpenTurnDiff,
  approval,
  deciding,
  approvalError,
  onDecide,
}: {
  item: CodeTranscriptItem;
  onOpenTurnDiff?: (turnId: string) => void;
  approval?: CodeApprovalSnapshot;
  deciding?: boolean;
  approvalError?: string;
  onDecide?: (
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) => void;
}) {
  switch (item.kind) {
    case "user":
      return (
        <div className="ml-auto max-w-prose rounded-lg bg-muted px-3 py-2 text-sm">
          {item.text}
        </div>
      );
    case "assistant":
      return (
        <div className="max-w-prose text-sm">
          <MessageMarkdown>{item.text}</MessageMarkdown>
        </div>
      );
    case "reasoning":
      return <ThinkingAccordion text={item.text} streaming={item.streaming} />;
    case "tool":
      return (
        <CodeToolCard
          name={item.name}
          detail={item.detail}
          status={item.status}
          preview={item.preview}
        />
      );
    case "notice":
      return <HarnessNotice level={item.level} message={item.message} />;
    case "approval":
      if (!approval) {
        return (
          <p className="text-muted-foreground text-sm" role="status">
            Loading approval…
          </p>
        );
      }
      return (
        <CodeApprovalCard
          approval={approval}
          deciding={deciding}
          error={approvalError}
          onDecide={(decision, feedback) =>
            onDecide?.(item.approvalId, decision, feedback)
          }
        />
      );
    case "turn_boundary":
      return (
        <TurnReviewCard
          status={item.status}
          durationMs={item.durationMs}
          usage={item.usage}
          error={item.error}
          diffstat={item.diffstat}
          onOpenDiff={
            item.turnId && onOpenTurnDiff
              ? () => onOpenTurnDiff(item.turnId as string)
              : undefined
          }
        />
      );
  }
}

export function CodeToolCard({
  name,
  detail,
  status,
  preview,
}: {
  name: string;
  detail: ToolDetail;
  status: "running" | "succeeded" | "failed" | "denied";
  preview: string;
}) {
  const badge =
    status === "running" ? (
      <Badge variant="info" size="sm">
        Running
      </Badge>
    ) : status === "failed" ? (
      <Badge variant="critical" size="sm">
        Failed
      </Badge>
    ) : status === "denied" ? (
      <Badge variant="warning" size="sm">
        Denied
      </Badge>
    ) : (
      <Badge variant="success" size="sm">
        Done
      </Badge>
    );
  return (
    <ToolCardShell
      icon={toolIcon(detail)}
      title={toolTitle(detail, name)}
      titleClassName={detail.kind === "command" ? "font-mono" : undefined}
      badge={badge}
      defaultExpanded={status === "running"}
      label={`${name} ${status}`}
    >
      <div className="text-muted-foreground space-y-1 px-2.5 pb-2 text-xs">
        <p>{toolDetailLine(detail)}</p>
        {preview && (
          <pre className="bg-muted max-h-40 overflow-auto rounded-md p-2 font-mono whitespace-pre-wrap">
            {preview}
          </pre>
        )}
      </div>
    </ToolCardShell>
  );
}

export function HarnessNotice({
  level,
  message,
}: {
  level: "info" | "warning" | "error";
  message: string;
}) {
  const tone =
    level === "error" ? "critical" : level === "warning" ? "warning" : "info";
  return (
    <div
      role="status"
      className={cn(
        "max-w-prose rounded-md border px-3 py-2 text-sm",
        tone === "critical" &&
          "border-critical-border bg-critical-background text-critical-foreground",
        tone === "warning" &&
          "border-warning-border bg-warning-background text-warning-foreground",
        tone === "info" &&
          "border-info-border bg-info-background text-info-foreground",
      )}
    >
      {message}
    </div>
  );
}

function toolIcon(detail: ToolDetail) {
  switch (detail.kind) {
    case "command":
      return <SquareTerminal />;
    case "file_read":
      return <FileSearch />;
    case "file_edit":
      return <FileCode />;
    case "search":
      return <Search />;
    default:
      return <Wrench />;
  }
}

function toolTitle(detail: ToolDetail, name: string): string {
  switch (detail.kind) {
    case "command":
      return detail.cmd;
    case "file_read":
    case "file_edit":
      return detail.path;
    case "search":
      return detail.query;
    case "other":
      return detail.summary || name;
  }
}

function toolDetailLine(detail: ToolDetail): string {
  switch (detail.kind) {
    case "command":
      return detail.cwd ? `cwd ${detail.cwd}` : "Command";
    case "file_read":
      return "File read";
    case "file_edit":
      return "File edit";
    case "search":
      return "Search";
    case "other":
      return detail.summary;
  }
}



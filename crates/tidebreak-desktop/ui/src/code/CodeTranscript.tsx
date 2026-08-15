import { FileCode, FileSearch, Search, SquareTerminal, Wrench } from "lucide-react";

import type { CodeUsage, ToolDetail } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { MessageMarkdown } from "@/MessageMarkdown";
import { ThinkingAccordion } from "@/ThinkingAccordion";
import { ToolCardShell } from "@/ToolCardShell";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";

/**
 * The code-session transcript: markdown for assistant text, tool-card chrome
 * for engine work, and visible harness notices.
 *
 * Approvals, diffs, and files stay out of this walking skeleton. What lands
 * here is the conversation a reader can follow from the journal alone.
 */

export function CodeTranscript({ items }: { items: CodeTranscriptItem[] }) {
  if (items.length === 0) {
    return (
      <p className="text-muted-foreground px-4 py-8 text-sm">
        Send a message to start a turn.
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-3 px-4 py-4">
      {items.map((item) => (
        <TranscriptItem key={item.id} item={item} />
      ))}
    </div>
  );
}

function TranscriptItem({ item }: { item: CodeTranscriptItem }) {
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
    case "turn_boundary":
      return (
        <TurnBoundary
          status={item.status}
          durationMs={item.durationMs}
          usage={item.usage}
          error={item.error}
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

function TurnBoundary({
  status,
  durationMs,
  usage,
  error,
}: {
  status: "completed" | "failed" | "interrupted";
  durationMs: number | null;
  usage: CodeUsage | null;
  error: string | null;
}) {
  const label =
    status === "completed"
      ? "Turn completed"
      : status === "failed"
        ? "Turn failed"
        : "Turn interrupted";
  return (
    <div className="text-muted-foreground flex flex-wrap items-center gap-2 text-xs">
      <span className="font-medium">{label}</span>
      {durationMs !== null && <span>{formatDuration(durationMs)}</span>}
      {usage && (
        <span>
          {usage.input_tokens + usage.output_tokens} tokens
        </span>
      )}
      {error && <span className="text-critical-foreground">{error}</span>}
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

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

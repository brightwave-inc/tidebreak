import { FileCode, FileSearch, Search, SquareTerminal, Wrench } from "lucide-react";
import { useState, type RefCallback } from "react";

import type { CodeApprovalSnapshot, Diffstat, FileChangeKind, ToolDetail } from "../api/types";
import { CodeApprovalCard } from "./CodeApprovalCard";
import { Badge } from "@/components/ui/badge";
import { AssistantMessageBody } from "@/AssistantMessageBody";
import { AssistantWorkingIndicator } from "@/AssistantWorkingIndicator";
import { MessageFooter } from "@/MessageFooter";
import { isolatedCard } from "@/PendingCard";
import { ThinkingAccordion } from "@/ThinkingAccordion";
import { ToolCardShell } from "@/ToolCardShell";
import { ToolOutputPreview } from "@/ToolOutputPreview";
import { TranscriptSkeleton } from "@/TranscriptSkeleton";
import { UserMessage } from "@/UserMessage";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { TurnReviewCard } from "./TurnReviewCard";

/**
 * The code-session transcript: the reader's prompts, markdown for assistant
 * text, tool-card chrome for engine work, the approvals a turn is parked on,
 * and what each turn came to.
 *
 * The frame is chat's — `.messages` around a `.messages-column` — because the
 * two modes are one product and a transcript that scrolls, follows, and spaces
 * itself differently in each reads as two (decision 0030). What differs is what
 * goes in the column, not the column.
 */

export function CodeTranscript({
  items,
  approvals = {},
  decidingId,
  approvalError,
  onDecide,
  hydrated = true,
  busy = false,
  streamStalled = false,
  animateStreaming = true,
  onOpenTurnDiff,
  scrollRef,
  contentRef,
  onScroll,
}: {
  items: CodeTranscriptItem[];
  approvals?: Record<string, CodeApprovalSnapshot>;
  decidingId?: string | null;
  approvalError?: string;
  onDecide?: (
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) => void;
  /** False until the durable turn snapshot settles; drives the skeleton. */
  hydrated?: boolean;
  /** A turn is open, so the engine owes the reader something. */
  busy?: boolean;
  /** The open turn's stream has gone quiet — see `useStreamStalled`. */
  streamStalled?: boolean;
  /** False while replaying, so a reopened session does not retype itself. */
  animateStreaming?: boolean;
  /** Scope the review sidebar to one turn's changes. */
  onOpenTurnDiff?: (turnId: string) => void;
  scrollRef?: RefCallback<HTMLDivElement>;
  contentRef?: RefCallback<HTMLDivElement>;
  onScroll?: () => void;
}) {
  // Only greet a session that is genuinely empty. A session still reading its
  // turns is transiently empty too, and inviting the reader to start a turn
  // there would flash over history that is about to arrive.
  if (items.length === 0) {
    return (
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
        {hydrated ? (
          <p className="text-muted-foreground text-sm">
            Send a message to start a turn.
          </p>
        ) : (
          <div className="messages-column">
            <TranscriptSkeleton />
          </div>
        )}
      </div>
    );
  }
  return (
    <div className="messages" ref={scrollRef} onScroll={onScroll}>
      <div className="messages-column" ref={contentRef}>
        {items.map((item) =>
          isolatedCard(
            item.id,
            itemSignature(item, decidingId, approvalError),
            <TranscriptItem
              item={item}
              animateStreaming={animateStreaming}
              approval={
                item.kind === "approval" ? approvals[item.approvalId] : undefined
              }
              deciding={
                item.kind === "approval" && decidingId === item.approvalId
              }
              approvalError={
                item.kind === "approval" && decidingId === item.approvalId
                  ? approvalError
                  : undefined
              }
              onDecide={onDecide}
              onOpenTurnDiff={onOpenTurnDiff}
            />,
          ),
        )}
        {shouldShowCodeWorking(items, busy, streamStalled) && (
          <AssistantWorkingIndicator />
        )}
      </div>
    </div>
  );
}

/**
 * Whether the engine owes the reader a visible working state.
 *
 * Anything with its own live presentation speaks for itself: streaming prose, a
 * running tool card, an approval waiting on a decision. The indicator covers
 * the gaps between them — and comes back under a partial response whose stream
 * has gone quiet, so a slow engine reads differently from a hung one.
 */
export function shouldShowCodeWorking(
  items: readonly CodeTranscriptItem[],
  busy: boolean,
  streamStalled = false,
): boolean {
  if (!busy) return false;
  const last = items[items.length - 1];
  if (!last) return true;
  if (last.kind === "assistant" || last.kind === "reasoning") {
    return last.streaming ? streamStalled : true;
  }
  if (last.kind === "tool") return last.status !== "running";
  if (last.kind === "approval") return last.state !== "pending";
  return true;
}

/**
 * The data a row draws on, reduced to what decides whether it can render, so a
 * row that threw on a half-written result is retried when the result lands
 * rather than staying broken for the life of the transcript.
 */
function itemSignature(
  item: CodeTranscriptItem,
  decidingId?: string | null,
  approvalError?: string,
): string {
  switch (item.kind) {
    case "tool":
      return `${item.status}:${item.preview.length}`;
    case "approval":
      return `${item.state}:${decidingId === item.approvalId}:${approvalError ?? ""}`;
    case "turn_boundary":
      return `${item.status}:${item.diffstat?.files ?? -1}`;
    case "assistant":
    case "reasoning":
      return String(item.streaming);
    case "file_activity":
      return fileActivitySignature(item.files);
    case "steer":
      return item.text;
    default:
      return item.kind;
  }
}

function TranscriptItem({
  item,
  animateStreaming,
  approval,
  deciding,
  approvalError,
  onDecide,
  onOpenTurnDiff,
}: {
  item: CodeTranscriptItem;
  animateStreaming: boolean;
  approval?: CodeApprovalSnapshot;
  deciding?: boolean;
  approvalError?: string;
  onDecide?: (
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) => void;
  onOpenTurnDiff?: (turnId: string) => void;
}) {
  switch (item.kind) {
    case "user":
      return (
        <UserMessage
          text={item.text}
          createdAt={item.createdAt}
          anchorId={item.id}
        />
      );
    case "steer":
      return (
        <UserMessage
          text={item.text}
          anchorId={item.id}
          trailing={
            <p className="text-muted-foreground mt-1 text-[11px]">
              Steered mid-turn
            </p>
          }
        />
      );
    case "file_activity":
      return <FileActivityRow files={item.files} />;
    case "assistant":
      return (
        <article className="message message-assistant" aria-label="Assistant">
          <AssistantMessageBody
            text={item.text}
            streaming={item.streaming && animateStreaming}
          />
          <MessageFooter
            role="assistant"
            text={item.text}
            settled={!item.streaming}
          />
        </article>
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
    case "approval": {
      const card = !approval ? (
        <p className="text-muted-foreground text-sm" role="status">
          Loading approval…
        </p>
      ) : (
        <CodeApprovalCard
          approval={approval}
          deciding={deciding}
          error={approvalError}
          onDecide={(decision, feedback) =>
            onDecide?.(item.approvalId, decision, feedback)
          }
        />
      );
      return (
        <div
          data-code-approval-id={item.approvalId}
          data-code-approval-state={item.state}
        >
          {card}
        </div>
      );
    }
    case "turn_boundary":
      return <TurnReviewCard turn={item} onOpenTurnDiff={onOpenTurnDiff} />;
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
      title={
        <MiddleTruncate
          text={toolTitle(detail, name)}
          className={detail.kind === "command" ? "font-mono" : undefined}
        />
      }
      badge={badge}
      defaultExpanded={status === "running"}
      label={`${name} ${status}`}
    >
      <div className="text-muted-foreground space-y-1 px-2.5 pb-2 [overflow-anchor:none]">
        <p className="text-[11px]">{toolDetailLine(detail)}</p>
        {preview && <ToolOutputPreview text={preview} />}
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
      // An error is the reason the turn is not going anywhere, so it interrupts
      // the reader. A warning or an aside is announced when they get to it.
      role={level === "error" ? "alert" : "status"}
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

/** Keep the tail of a command or path when the header runs out of room. */
function MiddleTruncate({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  const tail = Math.min(28, Math.max(12, Math.ceil(text.length / 3)));
  if (text.length <= 40) {
    return (
      <span className={cn("block truncate", className)} title={text}>
        {text}
      </span>
    );
  }
  return (
    <span className={cn("flex min-w-0", className)} title={text}>
      <span className="truncate">{text.slice(0, -tail)}</span>
      <span className="shrink-0">{text.slice(-tail)}</span>
    </span>
  );
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

const FILE_KIND_LETTER: Record<FileChangeKind, string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
};

function fileActivitySignature(
  files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>,
): string {
  return Object.entries(files)
    .map(
      ([path, file]) =>
        `${path}:${file.kind}:${file.diffstat.insertions}:${file.diffstat.deletions}`,
    )
    .join("|");
}

function fileActivityTotals(
  files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>,
): { count: number; insertions: number; deletions: number } {
  let insertions = 0;
  let deletions = 0;
  const paths = Object.keys(files);
  for (const file of Object.values(files)) {
    insertions += file.diffstat.insertions;
    deletions += file.diffstat.deletions;
  }
  return { count: paths.length, insertions, deletions };
}

function FileActivityRow({
  files,
}: {
  files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>;
}) {
  const [open, setOpen] = useState(false);
  const { count, insertions, deletions } = fileActivityTotals(files);
  const noun = count === 1 ? "file" : "files";
  return (
    <div className="text-muted-foreground max-w-prose text-[11px]">
      <button
        type="button"
        aria-expanded={open}
        className="text-left"
        onClick={() => setOpen((current) => !current)}
      >
        {count} {noun} changed ·{" "}
        <span className="text-success-foreground-muted tabular-nums">
          +{insertions}
        </span>{" "}
        <span className="text-critical-foreground-muted tabular-nums">
          −{deletions}
        </span>
      </button>
      <div
        className={cn(
          "grid transition-[grid-template-rows] duration-[140ms] ease-out motion-reduce:transition-none",
          open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
        )}
      >
        <ul className="min-h-0 overflow-hidden">
          {Object.entries(files).map(([path, file]) => (
            <li key={path} className="flex items-baseline gap-2 pt-0.5">
              <span
                className={cn(
                  "w-3 shrink-0 font-mono",
                  file.kind === "added" && "text-success-foreground-muted",
                  file.kind === "modified" && "text-info-foreground-muted",
                  file.kind === "deleted" && "text-critical-foreground-muted",
                  file.kind === "renamed" && "text-warning-foreground-muted",
                )}
                aria-label={file.kind}
              >
                {FILE_KIND_LETTER[file.kind]}
              </span>
              <PathLabel path={path} />
              <span className="shrink-0 tabular-nums">
                +{file.diffstat.insertions} −{file.diffstat.deletions}
              </span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

function PathLabel({ path }: { path: string }) {
  const slash = path.lastIndexOf("/");
  if (slash < 0) {
    return (
      <span className="min-w-0 truncate font-mono" title={path}>
        {path}
      </span>
    );
  }
  return (
    <span className="flex min-w-0 font-mono" title={path}>
      <span className="min-w-0 truncate">{path.slice(0, slash + 1)}</span>
      <span className="shrink-0">{path.slice(slash + 1)}</span>
    </span>
  );
}



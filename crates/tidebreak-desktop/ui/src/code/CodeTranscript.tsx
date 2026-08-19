import {
  Check,
  ChevronDown,
  CircleSlash,
  FileCode,
  FileSearch,
  Loader2,
  Search,
  SquareTerminal,
  Wrench,
  X,
} from "lucide-react";
import { memo, useEffect, useId, useRef, useState, type RefCallback } from "react";

import type { CodeApprovalSnapshot, Diffstat, FileChangeKind, ToolDetail } from "../api/types";
import { CodeApprovalCard } from "./CodeApprovalCard";
import { AssistantMessageBody } from "@/AssistantMessageBody";
import { AssistantWorkingIndicator } from "@/AssistantWorkingIndicator";
import { MessageFooter } from "@/MessageFooter";
import { isolatedCard } from "@/PendingCard";
import { ThinkingAccordion } from "@/ThinkingAccordion";
import { ToolOutputPreview } from "@/ToolOutputPreview";
import { TranscriptSkeleton } from "@/TranscriptSkeleton";
import { UserMessage } from "@/UserMessage";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import {
  formatElapsedDuration,
  formatTurnDuration,
  TurnReviewCard,
} from "./TurnReviewCard";

/**
 * The code-session transcript: the reader's prompts, markdown for assistant
 * text, inline tool lines for engine work, the approvals a turn is parked on,
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
  onReveal,
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
  /**
   * The reader opened or closed something inline.
   *
   * A reveal grows the column under the row they clicked, and a transcript
   * still following the tail would answer that by scrolling — moving the row
   * out from under them. The host stops following instead.
   */
  onReveal?: () => void;
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
          // The engine, mode, and model are already chosen by the time a
          // reader gets here, so this is not a welcome screen — it is the one
          // instruction left, plus what it will produce.
          <div className="flex max-w-sm flex-col items-center gap-1 text-center text-balance">
            <p className="text-sm font-medium">Send a message to start a turn.</p>
            <p className="text-muted-foreground text-[13.5px] leading-relaxed">
              The engine's replies, the tools it runs, and the files it changes
              all land here.
            </p>
          </div>
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
              onReveal={onReveal}
            />,
          ),
        )}
        {shouldShowCodeWorking(items, busy, streamStalled) && (
          <AssistantWorkingIndicator />
        )}
        <TurnLifecycleAnnouncer text={codeTurnAnnouncement(items, busy)} />
      </div>
    </div>
  );
}

/**
 * What a screen reader hears about the turn, and nothing else.
 *
 * Streaming text, tool output, and file activity all change many times a
 * second; announcing any of them turns the transcript into a firehose that
 * drowns the one thing a supervisor has to hear — whether the engine is still
 * working, and how the turn ended. A failed turn is not announced here because
 * `TurnReviewCard` already raises it as an alert, and a fact said twice is
 * worse than a fact said once.
 */
export function codeTurnAnnouncement(
  items: readonly CodeTranscriptItem[],
  busy: boolean,
): string {
  if (busy) return "Turn running";
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item.kind !== "turn_boundary") continue;
    if (item.status === "failed") return "";
    const duration = formatTurnDuration(item.durationMs);
    const label =
      item.status === "interrupted" ? "Turn interrupted" : "Turn finished";
    return duration ? `${label} · ${duration}` : label;
  }
  return "";
}

/**
 * The one live region in the transcript.
 *
 * It is mounted empty and only fills in on a later change, so opening a
 * finished session does not read its last turn's outcome out at the reader
 * before they have asked for anything.
 */
function TurnLifecycleAnnouncer({ text }: { text: string }) {
  const [announced, setAnnounced] = useState("");
  const settled = useRef(false);

  useEffect(() => {
    if (!settled.current) {
      settled.current = true;
      return;
    }
    setAnnounced(text);
  }, [text]);

  return (
    <span className="sr-only" role="status" data-testid="code-turn-announcer">
      {announced}
    </span>
  );
}

/**
 * Whether the engine owes the reader a visible working state.
 *
 * Anything with its own live presentation speaks for itself: streaming prose, a
 * running tool line, an approval waiting on a decision. The indicator covers
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

/**
 * One transcript row.
 *
 * Memoized on its props, and the reducer keeps every untouched item object
 * identical across an update, so a streamed delta re-renders the row it
 * changed rather than all of them. That is what keeps a long session's
 * transcript honest without virtualization — but it only holds while the
 * host passes stable callbacks, which `CodeWorkspacePage` does.
 */
const TranscriptItem = memo(function TranscriptItem({
  item,
  animateStreaming,
  approval,
  deciding,
  approvalError,
  onDecide,
  onOpenTurnDiff,
  onReveal,
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
  onReveal?: () => void;
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
      return <FileActivityRow files={item.files} onReveal={onReveal} />;
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
          startedAt={item.startedAt}
          durationMs={item.durationMs}
          onReveal={onReveal}
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
          onReveal={onReveal}
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
});

/**
 * One engine action as a boxless line. The verb is a constant; the muted mono
 * subject is the command or path; the right slot is meta, one status glyph,
 * and a quiet chevron. Failed and denied open. Success stays closed. A running
 * call streams the last lines of output without growing the row.
 */
export function CodeToolCard({
  name,
  detail,
  status,
  preview,
  startedAt = null,
  durationMs = null,
  onReveal,
}: {
  name: string;
  detail: ToolDetail;
  status: "running" | "succeeded" | "failed" | "denied";
  preview: string;
  startedAt?: string | null;
  durationMs?: number | null;
  onReveal?: () => void;
}) {
  const [expanded, setExpanded] = useState(
    status === "failed" || status === "denied",
  );
  const bodyId = useId();
  const verb = toolVerb(detail, status);
  const subject = toolSubject(detail, name);
  const hasOutput = preview.trim().length > 0;
  const elapsed = useElapsedLabel(startedAt, status === "running");
  const duration = formatTurnDuration(durationMs);
  const exitCode = parseExitCode(preview);

  useEffect(() => {
    if (status === "succeeded") setExpanded(false);
    if (status === "failed" || status === "denied") setExpanded(true);
  }, [status]);

  const showTail = status === "running" && hasOutput;
  const showExpanded = expanded && status !== "running" && hasOutput;

  return (
    // Deliberately not a live region. A tool line changes on every streamed
    // byte of its own output, and a transcript of thirty of them would announce
    // each one atomically, over and over. The outcome rides the line's own name
    // instead, and the turn announcer says when the work as a whole ends.
    <div className="max-w-prose [overflow-anchor:none]">
      <button
        type="button"
        className={cn(
          "-mx-1.5 flex w-full cursor-pointer items-center gap-2 rounded-md px-1.5 py-0.5 text-left text-[13.5px] hover:bg-muted/50",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        aria-expanded={expanded || showTail}
        aria-controls={hasOutput ? bodyId : undefined}
        onClick={() => {
          onReveal?.();
          setExpanded((current) => !current);
        }}
      >
        <span className="text-muted-foreground shrink-0 [&>svg]:size-3.5">
          {toolIcon(detail)}
        </span>
        <span className="shrink-0 font-semibold">{verb}</span>
        <MiddleTruncate
          text={subject}
          // The subject is the line's whole point, so it takes the free space
          // and keeps a floor: a narrow column truncates it, never erases it.
          className="text-muted-foreground min-w-[6ch] flex-1 font-mono"
        />
        <span className="text-muted-foreground ml-auto flex shrink-0 items-center gap-1.5 text-[11px] tabular-nums">
          {status === "running" ? elapsed : duration}
          {status !== "running" && exitCode !== null && (
            <span>exit {exitCode}</span>
          )}
          {/* The glyph is the outcome for a sighted reader; this is the same
              fact for everyone else, in the same place in the line. */}
          <span className="sr-only">{STATUS_WORDS[status]}</span>
          <StatusGlyph status={status} />
          <ChevronDown
            className={cn(
              "text-muted-foreground/50 size-3.5 transition-transform duration-[140ms] ease-out motion-reduce:transition-none",
              !(expanded || showTail) && "-rotate-90",
            )}
            aria-hidden="true"
          />
        </span>
      </button>
      {showTail && (
        <div id={bodyId}>
          <StreamingTail text={preview} />
        </div>
      )}
      {showExpanded && (
        <div id={bodyId}>
          <ToolOutputPreview
            text={preview}
            collapsedLines={12}
            bare
            onToggle={onReveal}
          />
        </div>
      )}
    </div>
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

function toolVerb(
  detail: ToolDetail,
  status: "running" | "succeeded" | "failed" | "denied",
): string {
  if (detail.kind === "command" && status === "denied") return "Command denied";
  switch (detail.kind) {
    case "command":
    case "other":
      return "Command run";
    case "file_read":
      return "File read";
    case "file_edit":
      return "File edited";
    case "search":
      return "Search";
  }
}

/** The outcome word, for the readers the status glyph does not reach. */
const STATUS_WORDS: Record<
  "running" | "succeeded" | "failed" | "denied",
  string
> = {
  running: "running",
  succeeded: "succeeded",
  failed: "failed",
  denied: "denied",
};

/** Last twelve lines, height-capped so a streaming tail does not grow the row. */
function StreamingTail({ text }: { text: string }) {
  const lines = text.replace(/\n+$/, "").split("\n");
  const tail = lines.slice(-12).join("\n");
  return (
    <pre
      // `pre` is a generic element, so a bare `aria-label` on it names nothing.
      // The group role is what makes the label reach assistive tech.
      role="group"
      aria-label="Output"
      className="text-muted-foreground max-h-[17.4em] overflow-hidden font-mono text-[13.5px] break-words whitespace-pre-wrap [overflow-anchor:none]"
    >
      {tail}
    </pre>
  );
}

function useElapsedLabel(startedAt: string | null, active: boolean): string | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active || !startedAt) return;
    const id = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(id);
  }, [active, startedAt]);
  if (!startedAt) return null;
  const start = Date.parse(startedAt);
  if (!Number.isFinite(start)) return null;
  // A counter that ticks once a second has no business showing tenths.
  return formatElapsedDuration(Math.max(0, now - start));
}

function parseExitCode(preview: string): number | null {
  const match = preview.match(/(?:^|\n)\s*exit(?:ed)?(?:\s+code)?[:\s]+(-?\d+)\s*$/im);
  if (!match) return null;
  const code = Number(match[1]);
  return Number.isFinite(code) ? code : null;
}

/**
 * What the call was aimed at: the command, the path, the query.
 *
 * A harness that starts a call before its arguments have finished streaming
 * sends an empty subject, and a line reading "File read" with nothing after it
 * tells the reader less than the tool's own name does. The name is the floor.
 */
function toolSubject(detail: ToolDetail, name: string): string {
  switch (detail.kind) {
    case "command":
      return detail.cmd.trim() || name;
    case "file_read":
    case "file_edit":
      return detail.path.trim() || name;
    case "search":
      return detail.query.trim() || name;
    case "other":
      return detail.summary.trim() || name;
  }
}

function StatusGlyph({
  status,
}: {
  status: "running" | "succeeded" | "failed" | "denied";
}) {
  switch (status) {
    case "running":
      return (
        // The one animation reduced motion keeps: it is the progress signal,
        // and a frozen spinner reads as a hung call.
        <Loader2
          className="text-info-foreground size-3.5 animate-spin"
          aria-hidden="true"
        />
      );
    case "succeeded":
      return (
        <Check className="text-success-foreground size-3.5" aria-hidden="true" />
      );
    case "failed":
      return (
        <X className="text-critical-foreground size-3.5" aria-hidden="true" />
      );
    case "denied":
      return (
        <CircleSlash
          className="text-warning-foreground size-3.5"
          aria-hidden="true"
        />
      );
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
  onReveal,
}: {
  files: Record<string, { kind: FileChangeKind; diffstat: Diffstat }>;
  onReveal?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const listId = useId();
  const { count, insertions, deletions } = fileActivityTotals(files);
  const noun = count === 1 ? "file" : "files";
  return (
    <div className="text-muted-foreground max-w-prose text-[11px]">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={listId}
        className={cn(
          "-mx-1.5 cursor-pointer rounded-md px-1.5 py-0.5 text-left hover:text-foreground",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        onClick={() => {
          onReveal?.();
          setOpen((current) => !current);
        }}
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
        // A zero-height row is still in the accessibility tree: `overflow`
        // hides pixels, not semantics. Without this a screen reader reads every
        // changed file of every turn as if the summary were always open.
        aria-hidden={!open}
        inert={!open ? true : undefined}
      >
        <ul id={listId} className="min-h-0 overflow-hidden">
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
                aria-hidden
              >
                {FILE_KIND_LETTER[file.kind]}
              </span>
              {/* "A" reads as the letter A. The word is what carries the
                  change kind once the glyph is gone. */}
              <span className="sr-only">{file.kind}</span>
              <PathLabel path={path} />
              <span className="ml-auto shrink-0 tabular-nums">
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



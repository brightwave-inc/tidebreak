import {
  Check,
  CircleSlash,
  FileCode,
  FileSearch,
  Loader2,
  Search,
  SquareTerminal,
  Wrench,
  X,
} from "lucide-react";
import {
  memo,
  useEffect,
  useId,
  useRef,
  useState,
  type RefCallback,
} from "react";

import type {
  CodeApprovalSnapshot,
  Diffstat,
  FileChangeKind,
  ToolDetail,
} from "../api/types";
import { CodeApprovalCard } from "./CodeApprovalCard";
import { AssistantMessageBody } from "@/AssistantMessageBody";
import { AssistantWorkingIndicator } from "@/AssistantWorkingIndicator";
import { MessageFooter } from "@/MessageFooter";
import { isolatedCard } from "@/PendingCard";
import { ThinkingAccordion } from "@/ThinkingAccordion";
import { ToolCardShell } from "@/ToolCardShell";
import { ToolOutputPreview } from "@/ToolOutputPreview";
import { TranscriptSkeleton } from "@/TranscriptSkeleton";
import { UserMessage } from "@/UserMessage";
import { useApp } from "@/AppContext";
import { TranscriptImageAttachments } from "@/TranscriptImageAttachments";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
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
  onForkFromTurn,
  emptyState,
  sessionId,
  onReveal,
  scrollRef,
  contentRef,
  onScroll,
  recap,
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
  /** Fork the conversation at the end of one turn, from its seam row. */
  onForkFromTurn?: (turnId: string) => void;
  /** Copy for a filtered/read-only transcript with no captured rows. */
  emptyState?: { title: string; description: string };
  sessionId?: string;
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
  /**
   * Where the session stands, shown on the newest turn boundary.
   *
   * Derived after a turn completes rather than streamed with it, so it appears
   * a moment later than the boundary it lands on — which is the point: it is
   * written for the reader who left and came back, not the one watching.
   */
  recap?: string;
}) {
  // The durable turn snapshot lands before the journal replay that fills in
  // assistant text and tool activity. Hide both sources until that initial
  // replay settles; otherwise a reopened workspace shows its user prompts and
  // then visibly reconstructs every response underneath them.
  if (!hydrated) {
    return (
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
        <div className="messages-column">
          <TranscriptSkeleton />
        </div>
      </div>
    );
  }

  // Only greet a session that is genuinely empty. The engine, mode, and model
  // are already chosen, so this is the one instruction left plus what the
  // first turn will produce.
  if (items.length === 0) {
    const title = emptyState?.title ?? "Send a message to start a turn.";
    const description =
      emptyState?.description ??
      "The engine's replies, the tools it runs, and the files it changes all land here.";
    return (
      <div className="messages is-empty" ref={scrollRef} onScroll={onScroll}>
        <div className="flex max-w-sm flex-col items-center gap-1 text-center text-balance">
          <p className="text-sm font-medium">{title}</p>
          <p className="text-muted-foreground text-[13.5px] leading-relaxed">
            {description}
          </p>
          {busy && (
            <div className="mt-3">
              <AssistantWorkingIndicator />
            </div>
          )}
        </div>
      </div>
    );
  }
  // The recap belongs to the session, so only its newest boundary carries one.
  let newestBoundaryId: string | undefined;
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = items[index];
    if (item?.kind === "turn_boundary") {
      newestBoundaryId = item.id;
      break;
    }
  }
  return (
    <div className="messages" ref={scrollRef} onScroll={onScroll}>
      <div className="messages-column" ref={contentRef}>
        {groupCodeTranscriptRows(items).map((row) =>
          row.kind === "group"
            ? isolatedCard(
                row.id,
                row.signature,
                <CodeActivityGroup
                  tools={row.tools}
                  signature={row.signature}
                  onReveal={onReveal}
                />,
              )
            : isolatedCard(
                row.item.id,
                itemSignature(row.item, decidingId, approvalError),
                <TranscriptItem
                  item={row.item}
                  animateStreaming={animateStreaming}
                  approval={
                    row.item.kind === "approval"
                      ? approvals[row.item.approvalId]
                      : undefined
                  }
                  attached={row.attached}
                  deciding={
                    row.item.kind === "approval" &&
                    decidingId === row.item.approvalId
                  }
                  approvalError={
                    row.item.kind === "approval" &&
                    decidingId === row.item.approvalId
                      ? approvalError
                      : undefined
                  }
                  onDecide={onDecide}
                  onOpenTurnDiff={onOpenTurnDiff}
                  onForkFromTurn={onForkFromTurn}
                  sessionId={sessionId}
                  onReveal={onReveal}
                  recap={row.item.id === newestBoundaryId ? recap : undefined}
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
 * Whether this approval is the one the tool line directly above it is parked
 * on.
 *
 * Nothing on the wire says so — an approval carries no tool-call id — but the
 * journal only ever parks the call it just started, so an approval that lands
 * straight after a still-running tool line belongs to it. The pair is then
 * drawn as one unit, because the card repeats the command the line above
 * already shows and two separate blocks saying the same thing reads as two
 * things happening.
 */
function parksTheCallAbove(
  items: readonly CodeTranscriptItem[],
  index: number,
): boolean {
  const item = items[index];
  if (item?.kind !== "approval") return false;
  const previous = items[index - 1];
  return previous?.kind === "tool" && previous.status === "running";
}

type CodeToolItem = Extract<CodeTranscriptItem, { kind: "tool" }>;

/** One thing the column draws: an item on its own, or a run behind one line. */
export type CodeTranscriptRow =
  | {
      kind: "item";
      item: CodeTranscriptItem;
      /** This approval parks the tool line drawn directly above it. */
      attached: boolean;
    }
  | {
      kind: "group";
      id: string;
      tools: CodeToolItem[];
      /** What the group draws on, for its key and its memo. */
      signature: string;
    };

/**
 * The transcript's items, with each run of tool calls folded behind one line.
 *
 * An engine works in bursts — search, read, read, search — and drawn one row
 * per call those bursts are the whole viewport, with the prose the reader is
 * waiting for pushed off the bottom. A run collapses to a single line naming
 * its newest call, which is the one worth watching, and opens to the same rows
 * it always drew.
 *
 * Only tool calls join a run. Reasoning breaks one, because a folded "Thought"
 * line between two groups is where the engine actually paused, and hiding that
 * inside a group would put the model's own account of the work two clicks away.
 */
export function groupCodeTranscriptRows(
  items: readonly CodeTranscriptItem[],
): CodeTranscriptRow[] {
  const rows: CodeTranscriptRow[] = [];
  let run: CodeToolItem[] = [];

  // `detach` holds the run's last call out of the group: an approval hangs its
  // card off the row directly above it, and a card hanging off a collapsed
  // group would point at a line that no longer shows the command it repeats.
  function flush(detach: boolean) {
    const parked = detach ? run.pop() : undefined;
    if (run.length > 1) {
      rows.push({
        kind: "group",
        id: `group:${run[0]!.id}`,
        tools: run,
        signature: run.map(toolSignature).join("|"),
      });
    } else {
      // A group of one is just a row.
      for (const tool of run)
        rows.push({ kind: "item", item: tool, attached: false });
    }
    if (parked) rows.push({ kind: "item", item: parked, attached: false });
    run = [];
  }

  items.forEach((item, index) => {
    if (item.kind === "tool") {
      run.push(item);
      return;
    }
    const attached = parksTheCallAbove(items, index);
    flush(attached);
    rows.push({ kind: "item", item, attached });
  });
  flush(false);
  return rows;
}

/**
 * What a grouped call draws on, so a streamed byte re-renders the group it
 * changed rather than every group in the transcript. The subject is in it
 * because a harness that starts a call before its arguments finish streaming
 * fills the detail in on completion.
 */
function toolSignature(tool: CodeToolItem): string {
  return `${tool.id}:${tool.status}:${tool.preview.length}:${toolSubject(tool.detail, tool.name)}`;
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
  attached = false,
  deciding,
  approvalError,
  onDecide,
  onOpenTurnDiff,
  onForkFromTurn,
  sessionId,
  onReveal,
  recap,
}: {
  item: CodeTranscriptItem;
  animateStreaming: boolean;
  approval?: CodeApprovalSnapshot;
  /** This approval parks the tool line directly above it. */
  attached?: boolean;
  deciding?: boolean;
  approvalError?: string;
  onDecide?: (
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) => void;
  onOpenTurnDiff?: (turnId: string) => void;
  onForkFromTurn?: (turnId: string) => void;
  sessionId?: string;
  onReveal?: () => void;
  /**
   * Where the session stands, on the newest turn boundary only.
   *
   * Passed to exactly one row: the recap describes the session, not the turn,
   * so repeating it at every boundary would fill a long transcript with stale
   * copies of a line that is only true at the bottom.
   */
  recap?: string;
}) {
  switch (item.kind) {
    case "user":
      return (
        <UserMessage
          text={item.text}
          createdAt={item.createdAt}
          anchorId={item.id}
          leading={
            sessionId && item.attachments && item.attachments.length > 0 ? (
              <CodeTurnImages
                sessionId={sessionId}
                attachments={item.attachments}
              />
            ) : undefined
          }
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
      return (
        <ThinkingAccordion
          text={item.text}
          streaming={item.streaming}
          // An engine pauses to think between every pair of calls here. Left to
          // open themselves, those blocks stack until they are the whole turn.
          expandWhileStreaming={false}
        />
      );
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
          data-code-approval-attached={attached || undefined}
          className={cn(
            // A rail dropped from the tool line's own icon column, closing the
            // column gap above it. The card is the same command in full, so it
            // has to read as that line's continuation rather than as the next
            // thing the engine did.
            attached && "border-border -mt-2 ml-[7px] border-l pt-2 pl-4",
          )}
        >
          {card}
        </div>
      );
    }
    case "turn_boundary":
      return (
        <TurnReviewCard
          turn={item}
          onOpenTurnDiff={onOpenTurnDiff}
          onForkFromTurn={onForkFromTurn}
          narrative={
            recap ? (
              <span className="text-muted-foreground">{recap}</span>
            ) : undefined
          }
        />
      );
  }
});

/**
 * A run of tool calls behind one line.
 *
 * The line is the run's newest call drawn the way its own row would draw it,
 * plus a count of what came before — so a live phase still says what the engine
 * is doing right now, and a settled one says what it last did and how much work
 * that took. Opening it gives back the rows, unchanged, on a rail dropped from
 * the group's own icon column.
 *
 * Closed by default, because the reason to group at all is that a burst of
 * calls is not what the reader came for. A run holding a failure is the
 * exception: it opens itself, the same way a failed row does.
 */
export const CodeActivityGroup = memo(
  function CodeActivityGroup({
    tools,
    onReveal,
  }: {
    tools: readonly CodeToolItem[];
    /** What the group draws on. The memo compares this and nothing else. */
    signature: string;
    onReveal?: () => void;
  }) {
    const unresolved = tools.some(
      (tool) => tool.status === "failed" || tool.status === "denied",
    );
    const [expanded, setExpanded] = useState(unresolved);
    useEffect(() => {
      // A failure inside a closed group is a failure the reader has to go
      // looking for. Opening is one-way: once they close it, it stays closed.
      if (unresolved) setExpanded(true);
    }, [unresolved]);

    const lead = leadingCall(tools);
    const status = groupStatus(tools);
    const verb = toolVerb(lead.detail, lead.status);
    const subject = toolSubject(lead.detail, lead.name);
    const rest = tools.length - 1;
    const elapsed = useElapsedLabel(lead.startedAt, status === "running");
    const duration =
      status === "running"
        ? elapsed
        : formatTurnDuration(totalDurationMs(tools));

    return (
      <ToolCardShell
        icon={toolIcon(lead.detail)}
        title={
          <>
            <span className="shrink-0 font-semibold">{verb}</span>
            <MiddleTruncate
              text={subject}
              className="text-muted-foreground min-w-[6ch] flex-1 font-mono"
            />
            {/* Hidden and said again below, because "plus 2 more" lands
                mid-command when it is read out where it is drawn. */}
            <span className="text-muted-foreground shrink-0" aria-hidden="true">
              +{rest} more
            </span>
          </>
        }
        titleClassName="flex items-center gap-2"
        trailing={
          <>
            <span className="sr-only">and {rest} more</span>
            {duration}
            <span className="sr-only">{status}</span>
            <StatusGlyph status={status} />
          </>
        }
        expanded={expanded}
        onExpandedChange={(next) => {
          onReveal?.();
          setExpanded(next);
        }}
        label={`${verb} ${subject} and ${rest} more ${status}`}
        announce={false}
        bodyClassName="border-border ml-[7px] flex flex-col gap-1 border-l pl-4"
      >
        {tools.map((tool) => (
          <CodeToolCard
            key={tool.id}
            name={tool.name}
            detail={tool.detail}
            status={tool.status}
            preview={tool.preview}
            startedAt={tool.startedAt}
            durationMs={tool.durationMs}
            onReveal={onReveal}
          />
        ))}
      </ToolCardShell>
    );
  },
  (previous, next) =>
    previous.signature === next.signature &&
    previous.onReveal === next.onReveal,
);

/**
 * The call the group's one line speaks for: whatever is running now, or the
 * last thing that ran. Harnesses batch calls, so "running" is not always the
 * one at the end.
 */
function leadingCall(tools: readonly CodeToolItem[]): CodeToolItem {
  for (let index = tools.length - 1; index >= 0; index -= 1) {
    const tool = tools[index]!;
    if (tool.status === "running") return tool;
  }
  return tools[tools.length - 1]!;
}

/** The run's outcome: still working, else the worst thing in it. */
function groupStatus(
  tools: readonly CodeToolItem[],
): "running" | "succeeded" | "failed" | "denied" {
  if (tools.some((tool) => tool.status === "running")) return "running";
  if (tools.some((tool) => tool.status === "failed")) return "failed";
  if (tools.some((tool) => tool.status === "denied")) return "denied";
  return "succeeded";
}

/**
 * How long the run took, or nothing. A replayed call carries no duration, and
 * a total that silently skips those would undercount the phase.
 */
function totalDurationMs(tools: readonly CodeToolItem[]): number | null {
  let total = 0;
  for (const tool of tools) {
    if (tool.durationMs === null) return null;
    total += tool.durationMs;
  }
  return total;
}

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
    <ToolCardShell
      icon={toolIcon(detail)}
      title={
        <>
          <span className="shrink-0 font-semibold">{verb}</span>
          <MiddleTruncate
            text={subject}
            // The subject is the line's whole point, so it takes the free space
            // and keeps a floor: a narrow column truncates it, never erases it.
            className="text-muted-foreground min-w-[6ch] flex-1 font-mono"
          />
        </>
      }
      titleClassName="flex items-center gap-2"
      trailing={
        <>
          {status === "running" ? elapsed : duration}
          {status !== "running" && exitCode !== null && (
            <span>exit {exitCode}</span>
          )}
          {/* The glyph is the outcome for a sighted reader; this is the same
              fact for everyone else, in the same place in the line. */}
          <span className="sr-only">{status}</span>
          <StatusGlyph status={status} />
        </>
      }
      expanded={expanded || showTail}
      onExpandedChange={(next) => {
        onReveal?.();
        setExpanded(next);
      }}
      label={`${verb} ${subject} ${status}`}
      announce={false}
    >
      {showTail && <StreamingTail text={preview} />}
      {showExpanded && (
        <ToolOutputPreview
          text={preview}
          collapsedLines={12}
          bare
          onToggle={onReveal}
        />
      )}
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

function useElapsedLabel(
  startedAt: string | null,
  active: boolean,
): string | null {
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
  const match = preview.match(
    /(?:^|\n)\s*exit(?:ed)?(?:\s+code)?[:\s]+(-?\d+)\s*$/im,
  );
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
      return humanizeShellCommand(detail.cmd) || name;
    case "file_read":
    case "file_edit":
      return detail.path.trim() || name;
    case "search":
      return detail.query.trim() || name;
    case "other":
      return detail.summary.trim() || name;
  }
}

/**
 * Codex reports commands as the complete shell launcher. Showing that
 * serialization verbatim leaves ordinary globs looking like
 * `'"'!node_modules'"'`. Decode the outer double-quoted `-lc` argument for
 * display only; the command is never executed from this value.
 *
 * Headless engines also lead every command with `cd <worktree> && `, so a
 * session's rows all open with the same long path and the part worth reading
 * is pushed into the truncated middle. Drop that leading `cd` for display —
 * the worktree is the session's own directory, so it says nothing the row
 * does not already mean.
 */
function humanizeShellCommand(command: string): string {
  const unwrapped = unwrapShellLauncher(command.trim());
  const stripped = unwrapped.replace(
    /^cd\s+(?:"[^"]*"|'[^']*'|[^\s;&|]+)\s*&&\s*/,
    "",
  );
  // A bare `cd <path>` is the whole command; stripping it would leave nothing.
  return stripped.trim() || unwrapped;
}

function unwrapShellLauncher(trimmed: string): string {
  const wrapped = trimmed.match(
    /^\/(?:usr\/)?bin\/(?:zsh|bash|sh) -lc "([\s\S]*)"$/,
  );
  if (!wrapped) return trimmed;

  const inner = wrapped[1] ?? "";
  let decoded = "";
  for (let index = 0; index < inner.length; index += 1) {
    const char = inner[index];
    const next = inner[index + 1];
    if (char === "\\" && next && ['"', "\\", "$", "`"].includes(next)) {
      decoded += next;
      index += 1;
    } else {
      decoded += char;
    }
  }
  return decoded.replaceAll(`'"'`, "'").trim();
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
        <Check
          className="text-success-foreground size-3.5"
          aria-hidden="true"
        />
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
        {/*
          The full `-foreground` ink, not the 75% `-muted` one: at 75% over a
          white page these numerals land near 3:1, which reads as greyed-out
          rather than green in the light theme while looking fine in the dark.
        */}
        <span className="text-success-foreground tabular-nums">
          +{insertions}
        </span>{" "}
        <span className="text-critical-foreground tabular-nums">
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
                  file.kind === "added" && "text-success-foreground",
                  file.kind === "modified" && "text-info-foreground",
                  file.kind === "deleted" && "text-critical-foreground",
                  file.kind === "renamed" && "text-warning-foreground",
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

function CodeTurnImages({
  sessionId,
  attachments,
}: {
  sessionId: string;
  attachments: readonly import("../generated/wire").ImageRef[];
}) {
  const { client } = useApp();
  return (
    <TranscriptImageAttachments
      client={{
        getChatImageAttachment: (_chatId, attachmentId, signal) =>
          client.getCodeSessionImage(sessionId, attachmentId, signal),
      }}
      chatId={sessionId}
      images={attachments.map((item) => ({
        attachmentId: item.blob_id,
        mediaType: item.media_type.startsWith("image/")
          ? item.media_type
          : `image/${item.media_type}`,
        width: 0,
        height: 0,
      }))}
    />
  );
}

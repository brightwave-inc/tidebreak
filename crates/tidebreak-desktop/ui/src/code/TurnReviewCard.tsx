import type { ReactNode } from "react";
import { Check, CircleSlash, TriangleAlert } from "lucide-react";

import type { CodeUsage, Diffstat } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { WithTooltip } from "@/components/ui/tooltip";
import { formatTokenCount } from "@/ContextUsage";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";

/**
 * What a turn came to, at the seam where it ended.
 *
 * The reducer has carried the status, the duration, the usage, the failure
 * message, and the diffstat since code mode shipped, and the transcript threw
 * all of it away. A turn that failed showed the reader nothing at all, which is
 * the one outcome that must never be silent.
 *
 * The three outcomes are deliberately not the same weight. A completed turn is
 * a quiet rule between exchanges — the work above it is the content, and a
 * heavy card there would compete with it. A failed turn is a critical block,
 * because it is the reason nothing further happened. An interrupted turn sits
 * between: warning-toned, one line, no alarm.
 */

type TurnBoundary = Extract<CodeTranscriptItem, { kind: "turn_boundary" }>;

export function TurnReviewCard({
  turn,
  narrative,
  onOpenTurnDiff,
}: {
  turn: TurnBoundary;
  /**
   * The engine's own account of what it did this turn.
   *
   * No event carries one yet — this is the slot it lands in when one does, so
   * the summary is a documented gap rather than something invented from the
   * transcript.
   */
  narrative?: ReactNode;
  /** Scope the review sidebar to this turn's changes. */
  onOpenTurnDiff?: (turnId: string) => void;
}) {
  const duration = formatTurnDuration(turn.durationMs);
  const diffstat = turn.diffstat && (
    <TurnDiffstat
      stat={turn.diffstat}
      turnId={turn.turnId}
      onOpenTurnDiff={onOpenTurnDiff}
    />
  );

  if (turn.status === "failed") {
    return (
      <div
        role="alert"
        className="border-critical-border bg-critical-background text-critical-foreground flex flex-col gap-1.5 rounded-md border px-3 py-2 text-sm"
      >
        <p className="flex items-center gap-1.5 font-medium">
          <TriangleAlert size={14} aria-hidden="true" />
          Turn failed
          {duration && <span className="font-normal tabular-nums">· {duration}</span>}
        </p>
        <p>{turn.error ?? "The engine stopped without saying why."}</p>
        {narrative}
        {diffstat && <div className="flex items-center gap-2">{diffstat}</div>}
      </div>
    );
  }

  if (turn.status === "interrupted") {
    return (
      <SeamRow label="Turn interrupted" tone="warning">
        <CircleSlash size={13} aria-hidden="true" />
        <span>Turn interrupted</span>
        {duration && <span className="tabular-nums">· {duration}</span>}
        {narrative}
        {diffstat}
      </SeamRow>
    );
  }

  return (
    <SeamRow label="Turn finished" tone="quiet">
      <Check size={13} aria-hidden="true" />
      <span>Turn finished</span>
      {duration && <span className="tabular-nums">· {duration}</span>}
      {narrative}
      {diffstat}
      <TurnUsage usage={turn.usage} />
    </SeamRow>
  );
}

/** The seam itself: a rule the turn ends on, and the facts sitting on it. */
function SeamRow({
  label,
  tone,
  children,
}: {
  label: string;
  tone: "quiet" | "warning";
  children: ReactNode;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className={cn(
        "flex flex-wrap items-center gap-1.5 border-t pt-2 text-xs",
        tone === "warning" ? "text-warning-foreground" : "text-muted-foreground",
      )}
    >
      {children}
    </div>
  );
}

/**
 * The turn's token cost at the scale people quote it, with the exact counts —
 * cache reads and writes included — behind the tooltip.
 */
function TurnUsage({ usage }: { usage: CodeUsage | null }) {
  if (!usage) return null;
  const summary = `${formatTokenCount(usage.input_tokens)} in / ${formatTokenCount(usage.output_tokens)} out`;
  const detail = [
    `${usage.input_tokens.toLocaleString()} input`,
    `${usage.output_tokens.toLocaleString()} output`,
    `${usage.cache_read_input_tokens.toLocaleString()} cache read`,
    `${usage.cache_creation_input_tokens.toLocaleString()} cache write`,
  ].join(" · ");
  return (
    <WithTooltip label={detail}>
      <span className="ml-auto tabular-nums">{summary}</span>
    </WithTooltip>
  );
}

/**
 * The turn's changes, as a control rather than a label.
 *
 * Whether it opens anything is the host's call: without a handler the numbers
 * still read, so the seam never depends on a surface that is not mounted.
 */
function TurnDiffstat({
  stat,
  turnId,
  onOpenTurnDiff,
}: {
  stat: Diffstat;
  turnId: string | null;
  onOpenTurnDiff?: (turnId: string) => void;
}) {
  if (!onOpenTurnDiff || !turnId) return <DiffstatBadge stat={stat} />;
  return (
    <button
      type="button"
      className="focus-visible:ring-ring rounded-full focus-visible:ring-2 focus-visible:outline-none"
      aria-label="Review this turn's changes"
      onClick={() => onOpenTurnDiff(turnId)}
    >
      <DiffstatBadge stat={stat} />
    </button>
  );
}

export function DiffstatBadge({ stat }: { stat: Diffstat }) {
  return (
    <Badge variant="outline" size="sm" className="tabular-nums">
      {stat.files} file{stat.files === 1 ? "" : "s"} +{stat.insertions} −
      {stat.deletions}
      {stat.truncated ? " · truncated" : ""}
    </Badge>
  );
}

/** How long the turn ran, at the precision the seam can carry. */
export function formatTurnDuration(ms: number | null): string | null {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;
  const seconds = Math.round(ms / 1_000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

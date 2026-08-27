import type { ReactNode } from "react";
import {
  Check,
  CircleSlash,
  GitFork,
  MoreHorizontal,
  TriangleAlert,
} from "lucide-react";

import type { Diffstat } from "../api/types";
import { Badge } from "@/components/ui/badge";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";

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

const CODEX_REVOKED_REFRESH_TOKEN_FINGERPRINT =
  "your access token could not be refreshed because your refresh token was revoked";

/**
 * Whether Codex CLI rejected the saved refresh token for this session.
 *
 * The CLI can wrap the sentence with its own prefix or line breaks. Normalize
 * whitespace and match the full diagnostic clause so other authentication and
 * engine failures keep their original text.
 */
export function isCodexRevokedRefreshTokenError(error: string | null): boolean {
  if (!error) return false;
  const normalized = error.toLowerCase().replace(/\s+/g, " ").trim();
  return normalized.includes(CODEX_REVOKED_REFRESH_TOKEN_FINGERPRINT);
}

export function TurnReviewCard({
  turn,
  narrative,
  onOpenTurnDiff,
  onForkFromTurn,
}: {
  turn: TurnBoundary;
  /**
   * Where the session stands, on the newest boundary only.
   *
   * Not the engine's own account of the turn — no engine event carries one,
   * and that stays a documented gap. This is Tidebreak's recap, derived on the
   * utility model after the turn completes and written for a reader who left
   * and came back, which is why it sits at the bottom of the transcript rather
   * than against every turn.
   */
  narrative?: ReactNode;
  /** Scope the review sidebar to this turn's changes. */
  onOpenTurnDiff?: (turnId: string) => void;
  /** Hand everything up to this turn to a fresh agent, in a new tab. */
  onForkFromTurn?: (turnId: string) => void;
}) {
  const duration = formatTurnDuration(turn.durationMs);
  const diffstat = turn.diffstat && hasFileChanges(turn.diffstat) && (
    <TurnDiffstat
      stat={turn.diffstat}
      turnId={turn.turnId}
      onOpenTurnDiff={onOpenTurnDiff}
    />
  );
  const actions = turn.turnId && onForkFromTurn && (
    <TurnActionsMenu turnId={turn.turnId} onForkFromTurn={onForkFromTurn} />
  );

  if (turn.status === "failed") {
    const codexNeedsLogin = isCodexRevokedRefreshTokenError(turn.error);
    return (
      <div
        role="alert"
        className="border-critical-border bg-critical-background text-critical-foreground flex flex-col gap-1.5 rounded-md border px-3 py-2 text-sm"
      >
        <p className="flex items-center gap-1.5 font-medium">
          <TriangleAlert size={14} aria-hidden="true" />
          Turn failed
          {duration && (
            <span className="font-normal tabular-nums">· {duration}</span>
          )}
        </p>
        {codexNeedsLogin ? (
          <CodexLoginRecovery />
        ) : (
          <p>{turn.error ?? "The engine stopped without saying why."}</p>
        )}
        {narrative}
        {(diffstat || actions) && (
          <div className="flex items-center gap-2">
            {diffstat}
            {actions}
          </div>
        )}
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
        {actions}
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
      {actions}
    </SeamRow>
  );
}

/** Recovery for the revoked credential that belongs to Codex CLI. */
function CodexLoginRecovery() {
  return (
    <div className="flex flex-col gap-2">
      <p>
        Codex CLI rejected its saved sign-in. Tidebreak&apos;s account sign-in
        does not reset Codex CLI.
      </p>
      <div className="flex flex-col gap-1">
        <p>Run these commands in your terminal:</p>
        <ol className="list-decimal space-y-1 pl-5">
          <li>
            <code className="font-mono">codex logout</code>
          </li>
          <li>
            <code className="font-mono">codex login</code>
          </li>
        </ol>
      </div>
      <p>
        Then open Settings → Coding harnesses and select{" "}
        <strong>Re-check</strong>.
      </p>
    </div>
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
        tone === "warning"
          ? "text-warning-foreground"
          : "text-muted-foreground",
      )}
    >
      {children}
    </div>
  );
}

/**
 * What the reader can do with a finished turn, behind one quiet trigger.
 *
 * Forking is the only entry today, but the seam is where per-turn actions
 * belong, so the affordance is a menu rather than a bare fork button.
 */
function TurnActionsMenu({
  turnId,
  onForkFromTurn,
}: {
  turnId: string;
  onForkFromTurn: (turnId: string) => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-muted hover:text-foreground grid size-5 shrink-0 cursor-pointer place-items-center rounded-md",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          aria-label="Turn actions"
        >
          <MoreHorizontal className="size-3.5" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-52">
        <DropdownMenuItem onSelect={() => onForkFromTurn(turnId)}>
          <GitFork />
          Fork from here
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** A recorded zero-stat is still a diffstat; the seam only shows real changes. */
function hasFileChanges(stat: Diffstat): boolean {
  return stat.files > 0 || stat.insertions > 0 || stat.deletions > 0;
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
      className={cn(
        "hover:bg-muted cursor-pointer rounded-full",
        FOCUS_RING,
        HOVER_TINT,
      )}
      aria-label="Review this turn's changes"
      onClick={() => onOpenTurnDiff(turnId)}
    >
      <DiffstatBadge stat={stat} />
    </button>
  );
}

export function DiffstatBadge({ stat }: { stat: Diffstat }) {
  const fileLabel = `${stat.files} file${stat.files === 1 ? "" : "s"}`;
  const additionLabel = `${stat.insertions} addition${stat.insertions === 1 ? "" : "s"}`;
  const deletionLabel = `${stat.deletions} deletion${stat.deletions === 1 ? "" : "s"}`;
  return (
    <Badge
      variant="outline"
      size="sm"
      className="bg-muted/35 gap-1.5 font-mono tabular-nums"
      aria-label={`${fileLabel}, ${additionLabel}, ${deletionLabel}${stat.truncated ? ", truncated" : ""}`}
    >
      <span className="text-muted-foreground">{fileLabel}</span>
      <span className="text-success-foreground">+{stat.insertions}</span>
      <span className="text-critical-foreground">−{stat.deletions}</span>
      {stat.truncated && (
        <span className="text-warning-foreground">· truncated</span>
      )}
    </Badge>
  );
}

/**
 * How long the turn ran, at the precision the seam can carry.
 *
 * A sub-second turn rounded to "0s" reads as a broken clock rather than a fast
 * engine, so anything under a second is "<1s" and the first ten seconds carry
 * a tenth. Past that the tenth is noise and whole seconds, then minutes, say
 * it better.
 */
export function formatTurnDuration(ms: number | null): string | null {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;
  if (ms < 1_000) return "<1s";
  const tenths = Math.round(ms / 100) / 10;
  if (tenths < 10) return `${tenths.toFixed(1)}s`;
  return coarseDuration(Math.round(ms / 1_000));
}

/**
 * The same clock for a counter that ticks once a second.
 *
 * A live elapsed label reads its own tenth as jitter — it changes on a
 * schedule the reader can see — so it stays on whole seconds throughout.
 */
export function formatElapsedDuration(ms: number | null): string | null {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return null;
  if (ms < 1_000) return "<1s";
  return coarseDuration(Math.floor(ms / 1_000));
}

function coarseDuration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

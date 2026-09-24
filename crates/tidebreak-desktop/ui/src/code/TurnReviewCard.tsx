import { type ReactNode, useId } from "react";
import { useRouter } from "@tanstack/react-router";
import {
  Check,
  CircleSlash,
  GitFork,
  History,
  LogIn,
  MoreHorizontal,
  TriangleAlert,
} from "lucide-react";

import type { Diffstat } from "../api/types";
import { AssistantMessageBody } from "@/AssistantMessageBody";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import type { CodeTranscriptItem } from "./CodeSessionReducer";
import { openEngineSignIn } from "./EngineSignIn";
import { FOCUS_RING, FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { STATUS_TEXT } from "./statusTone";

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

/**
 * The engine release an error says is too old, when it says so.
 *
 * Claude Code refuses a model its build predates with "version 2.1.251 or
 * newer is required". Only the version phrase is matched, so any engine
 * that words a floor the same way lands on the same recovery, and the
 * product-specific prose around it can change without dropping the match.
 */
export function harnessVersionRequirement(error: string | null): string | null {
  if (!error) return null;
  const normalized = error.replace(/\s+/g, " ");
  const match =
    /\bversion\s+(\d+\.\d+(?:\.\d+)?(?:[-+][\w.]+)?)\s+or\s+newer\s+is\s+required\b/i.exec(
      normalized,
    );
  return match?.[1] ?? null;
}

export function TurnReviewCard({
  turn,
  recap,
  onOpenTurnDiff,
  onForkFromTurn,
  onRestoreBeforeTurn,
  undoUnavailableReason,
  onFileIssue,
}: {
  turn: TurnBoundary;
  /** A quiet reading of what the completed turn accomplished. */
  recap?: string;
  /** Scope the review sidebar to this turn's changes. */
  onOpenTurnDiff?: (turnId: string) => void;
  /** Hand everything up to this turn to a fresh agent, in a new tab. */
  onForkFromTurn?: (turnId: string) => void;
  /** Put the worktree back to how it stood before this turn began. */
  onRestoreBeforeTurn?: (turnId: string) => void;
  /**
   * Why the worktree cannot be changed right now, such as a turn running.
   * The restore stays in the menu, turned off, with this sentence under it.
   */
  undoUnavailableReason?: string;
  /** Turn a failure into a Tidebreak issue or fix, from the failure itself. */
  onFileIssue?: () => void;
}) {
  const duration = formatTurnDuration(turn.durationMs);
  const diffstat = turn.diffstat && hasFileChanges(turn.diffstat) && (
    <TurnDiffstat
      stat={turn.diffstat}
      turnId={turn.turnId}
      onOpenTurnDiff={onOpenTurnDiff}
    />
  );
  const actions = turn.turnId && (onForkFromTurn || onRestoreBeforeTurn) && (
    <TurnActionsMenu
      turnId={turn.turnId}
      onForkFromTurn={onForkFromTurn}
      onRestoreBeforeTurn={onRestoreBeforeTurn}
      undoUnavailableReason={undoUnavailableReason}
    />
  );

  if (turn.status === "failed") {
    const codexNeedsLogin = isCodexRevokedRefreshTokenError(turn.error);
    const requiredVersion = harnessVersionRequirement(turn.error);
    return (
      <div
        role="alert"
        className="notice-surface notice-critical flex flex-col gap-1.5 rounded-md border px-3 py-2 text-sm"
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
        ) : requiredVersion ? (
          <HarnessVersionRecovery required={requiredVersion} />
        ) : (
          <p>{turn.error ?? "The engine stopped without saying why."}</p>
        )}
        {recap && <TurnRecap text={recap} tone="critical" />}
        {(diffstat || actions || onFileIssue) && (
          <div className="flex items-center gap-2">
            {diffstat}
            {actions}
            {onFileIssue && <FileIssueButton onClick={onFileIssue} />}
          </div>
        )}
      </div>
    );
  }

  if (turn.status === "interrupted") {
    return (
      <SeamRow label="Turn interrupted" tone="warning" recap={recap}>
        <CircleSlash size={13} aria-hidden="true" />
        <span>Turn interrupted</span>
        {duration && <span className="tabular-nums">· {duration}</span>}
        {diffstat}
        {actions}
      </SeamRow>
    );
  }

  return (
    <SeamRow label="Turn finished" tone="quiet" recap={recap}>
      <Check size={13} aria-hidden="true" />
      <span>Turn finished</span>
      {duration && <span className="tabular-nums">· {duration}</span>}
      {diffstat}
      {actions}
    </SeamRow>
  );
}

/**
 * The way out of a failure the reader cannot fix: hand the session to Uneff
 * me, which asks what happened and files an issue or a fix.
 */
export function FileIssueButton({ onClick }: { onClick: () => void }) {
  return (
    <Button
      type="button"
      size="sm"
      variant="outline"
      className="ml-auto"
      onClick={onClick}
    >
      File an issue
    </Button>
  );
}

/**
 * Recovery for the revoked credential that belongs to Codex CLI.
 *
 * `codex login status` still reports this credential as signed in, so the
 * doctor has no Sign in to offer. The card runs the sign-in itself. Codex
 * clears the rejected credential before it starts a new login, so one
 * sign-in replaces it; nothing needs `codex logout` first, and the Codex
 * Tidebreak runs is not on the reader's own `PATH` for them to type it.
 */
function CodexLoginRecovery() {
  return (
    <div className="flex flex-col gap-2">
      <p>
        Codex CLI rejected its saved sign-in. Tidebreak&apos;s account sign-in
        does not reset Codex CLI.
      </p>
      <p>Sign in to Codex CLI again to replace it, then send the turn again.</p>
      <div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => openEngineSignIn("codex")}
        >
          <LogIn aria-hidden="true" />
          Sign in to Codex CLI
        </Button>
      </div>
    </div>
  );
}

/**
 * The engine is older than the model needs. The fix lives in Settings, not
 * in a terminal: the update channel there moves the engine past its pin.
 */
function HarnessVersionRecovery({ required }: { required: string }) {
  return (
    <div className="flex flex-col gap-2">
      <p>
        The engine is too old for this model. Version {required} or newer is
        required.
      </p>
      <p>
        Open{" "}
        <CodingHarnessesLink>Settings → Coding engines</CodingHarnessesLink>,
        set <strong>Engine versions</strong> to <strong>Latest</strong>, select{" "}
        <strong>Check for updates</strong>, then <strong>Update</strong> the
        engine.
      </p>
    </div>
  );
}

/**
 * The one door out of a failure card: the settings page that owns engine
 * setup. The card also renders in stories and tests with no router around
 * it, so a missing router leaves the words in place and the press inert
 * rather than throwing.
 */
function CodingHarnessesLink({ children }: { children: ReactNode }) {
  const router = useRouter({ warn: false }) as
    | ReturnType<typeof useRouter>
    | undefined;
  const harnessesPath: string = "/settings/coding-harnesses";
  return (
    <button
      type="button"
      className={cn(
        "cursor-pointer rounded-sm font-medium underline underline-offset-2",
        FOCUS_RING_TIGHT,
      )}
      onClick={() => void router?.navigate({ to: harnessesPath })}
    >
      {children}
    </button>
  );
}

/** The seam itself: a rule the turn ends on, and the facts sitting on it. */
function SeamRow({
  label,
  tone,
  recap,
  detail,
  children,
}: {
  label: string;
  tone: "quiet" | "warning";
  recap?: string;
  /** One plain sentence under the row, such as why something stopped. */
  detail?: string;
  children: ReactNode;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className={cn(
        "border-t pt-2 text-xs",
        tone === "warning" ? STATUS_TEXT.warning : "text-muted-foreground",
      )}
    >
      <div className="flex flex-wrap items-center gap-1.5">{children}</div>
      {detail && <p className="mt-1 break-words">{detail}</p>}
      {recap && <TurnRecap text={recap} />}
    </div>
  );
}

/** The recap stays subordinate to the turn outcome and keeps markdown links. */
function TurnRecap({
  text,
  tone = "quiet",
}: {
  text: string;
  tone?: "quiet" | "critical";
}) {
  return (
    <div
      className={cn(
        "mt-1.5 [&_.message-markdown]:text-xs",
        tone === "critical" ? STATUS_TEXT.critical : "text-muted-foreground",
      )}
    >
      <AssistantMessageBody text={text} streaming={false} />
    </div>
  );
}

/**
 * What the reader can do with a finished turn, behind one quiet trigger:
 * hand it to a fresh agent, or put the worktree back to before it.
 */
function TurnActionsMenu({
  turnId,
  onForkFromTurn,
  onRestoreBeforeTurn,
  undoUnavailableReason,
}: {
  turnId: string;
  onForkFromTurn?: (turnId: string) => void;
  onRestoreBeforeTurn?: (turnId: string) => void;
  undoUnavailableReason?: string;
}) {
  const reasonId = useId();
  const unavailable = undoUnavailableReason !== undefined;
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
      <DropdownMenuContent
        align="start"
        collisionPadding={12}
        className="w-max max-w-80"
      >
        {onForkFromTurn && (
          <DropdownMenuItem onSelect={() => onForkFromTurn(turnId)}>
            <GitFork />
            Fork from here
          </DropdownMenuItem>
        )}
        {onRestoreBeforeTurn && (
          <>
            {/*
              A turned-off item stays focusable, so a keyboard or screen
              reader user reaches it and hears why it is off.
            */}
            <DropdownMenuItem
              aria-disabled={unavailable || undefined}
              aria-describedby={unavailable ? reasonId : undefined}
              className={cn(unavailable && "cursor-not-allowed opacity-60")}
              onSelect={(event) => {
                if (unavailable) {
                  event.preventDefault();
                  return;
                }
                onRestoreBeforeTurn(turnId);
              }}
            >
              <History />
              Restore to before this turn
            </DropdownMenuItem>
            {undoUnavailableReason && (
              <p
                id={reasonId}
                className="text-muted-foreground px-2 pb-1.5 pl-10 text-xs"
              >
                {undoUnavailableReason}
              </p>
            )}
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * The seam a restore leaves in the transcript: what the worktree went back
 * to, what changed, and the way back.
 *
 * A restore runs between turns, so it reads as one of the quiet seams rather
 * than as a card. It lands as started before any file moves and changes in
 * place when the restore ends. Undo is a restore too, and it asks first, like
 * this one did. Every row keeps its Undo, whatever its status: the state the
 * restore replaced is saved before any file moves, and putting it back is
 * the way out of a restore that stopped partway or never finished. After one
 * that verifiably changed nothing, Undo finds nothing to do and says so.
 */
export function CheckpointRestoreRow({
  restore,
  onUndo,
  undoUnavailableReason,
}: {
  restore: Extract<CodeTranscriptItem, { kind: "restore" }>;
  /** Put back the state this restore replaced. */
  onUndo?: (restoreId: string) => void;
  undoUnavailableReason?: string;
}) {
  const undoing = restore.target.kind === "before_restore";
  const label = restoreLabel(restore);
  return (
    <SeamRow
      label={label}
      tone={
        restore.status === "failed" || restore.status === "partial"
          ? "warning"
          : "quiet"
      }
      detail={restore.error ?? undefined}
    >
      {restore.status === "failed" || restore.status === "partial" ? (
        <TriangleAlert size={13} aria-hidden="true" />
      ) : (
        <History size={13} aria-hidden="true" />
      )}
      <span>{label}</span>
      {restore.status !== "failed" && hasFileChanges(restore.diffstat) && (
        <DiffstatBadge stat={restore.diffstat} />
      )}
      {onUndo && (
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:text-foreground ml-1 cursor-pointer rounded-sm font-medium underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:no-underline",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          disabled={undoUnavailableReason !== undefined}
          title={undoUnavailableReason}
          aria-label={
            restore.status === "completed"
              ? undoing
                ? "Redo the restore"
                : "Undo the restore"
              : "Put back the files the restore replaced"
          }
          onClick={() => onUndo(restore.restoreId)}
        >
          Undo
        </button>
      )}
    </SeamRow>
  );
}

/** What a restore row says, by how far the restore got. */
function restoreLabel(
  restore: Extract<CodeTranscriptItem, { kind: "restore" }>,
): string {
  const undoing = restore.target.kind === "before_restore";
  const where =
    restore.turnOrdinal !== null
      ? `before turn ${restore.turnOrdinal}`
      : "before a turn";
  switch (restore.status) {
    case "started":
      return undoing
        ? "Started undoing a restore"
        : `Started restoring to ${where}`;
    case "failed":
      return undoing
        ? "Could not undo the restore. Nothing changed."
        : "Could not restore. Nothing changed.";
    case "partial":
      return undoing
        ? "The undo stopped partway"
        : "The restore stopped partway";
    case "completed":
      return undoing ? "Undid a restore" : `Restored to ${where}`;
  }
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
      <span className={STATUS_TEXT.ready}>+{stat.insertions}</span>
      <span className={STATUS_TEXT.critical}>−{stat.deletions}</span>
      {stat.truncated && (
        <span className={STATUS_TEXT.warning}>· truncated</span>
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

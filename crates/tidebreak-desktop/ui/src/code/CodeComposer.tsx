import { type KeyboardEvent, useEffect, useState } from "react";
import { Square } from "lucide-react";

import type { CodePermissionMode } from "../api/types";
import { HttpError } from "../api/client";
import type { CodeTurnSubmission } from "./parsers";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import {
  PERMISSION_MODE_LABELS,
  PERMISSION_MODE_UNAVAILABLE_REASON,
} from "./labels";

/**
 * Trimmed composer for a code session: text, send, interrupt, and the
 * permission mode the session was created under.
 *
 * A send while a turn is running parks the message in the session's single
 * queue slot rather than being refused, so a follow-up can be written while
 * the engine works. `PermissionModePicker` is the creation-time control —
 * the session's own mode is stated, not offered, because it is fixed at
 * launch.
 */

const MODES: CodePermissionMode[] = ["plan", "ask", "auto"];

const QUEUED_NOTE = "Queued — runs after the current turn.";

/** Session-scoped copy for the one refusal the queue itself produces. */
const QUEUE_FULL_NOTE =
  "A follow-up is already queued. Wait for it to run, or interrupt this turn.";

/**
 * Pick the mode a session will be created under.
 *
 * Ask is the default. Plan and Auto stay on the same scale. A mode the
 * chosen harness cannot honor is disabled with the refusal reason. This is a
 * creation-time control: the new-workspace dialog and the start-session
 * prompt use it, and an attached session shows `PermissionModeSummary`.
 */
export function PermissionModePicker({
  value,
  availableModes = MODES,
  unavailableReason = PERMISSION_MODE_UNAVAILABLE_REASON,
  onChange,
}: {
  value: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
  onChange?: (mode: CodePermissionMode) => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-1.5" data-testid="permission-modes">
      {MODES.map((mode) => {
        const available = availableModes.includes(mode);
        const selected = value === mode;
        return (
          <button
            key={mode}
            type="button"
            disabled={!available}
            title={
              available
                ? PERMISSION_MODE_LABELS[mode]
                : `${PERMISSION_MODE_LABELS[mode]}: ${unavailableReason}`
            }
            aria-pressed={selected}
            onClick={() => available && onChange?.(mode)}
            className={cn(
              "rounded-full border px-2.5 py-0.5 text-xs font-medium",
              selected && "border-foreground bg-muted",
              !available && "cursor-not-allowed opacity-50",
            )}
          >
            {PERMISSION_MODE_LABELS[mode]}
            {!available && (
              <span className="sr-only">{unavailableReason}</span>
            )}
          </button>
        );
      })}
    </div>
  );
}

/**
 * The mode a live session is running under, stated rather than offered.
 *
 * The mode is composed into the engine's launch (decisions 0033 and 0038):
 * there is no way to change it on a session that is already attached, so the
 * composer reports it. Modes this harness cannot honor stay visible, dimmed
 * with the reason, because that is part of what the session can do.
 */
export function PermissionModeSummary({
  value,
  availableModes = MODES,
  unavailableReason = PERMISSION_MODE_UNAVAILABLE_REASON,
}: {
  value: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
}) {
  return (
    <div
      className="text-muted-foreground flex flex-wrap items-center gap-1.5 text-xs"
      data-testid="permission-modes"
    >
      <span>Mode</span>
      {MODES.map((mode) => {
        const available = availableModes.includes(mode);
        const selected = value === mode;
        return (
          <span
            key={mode}
            title={
              available
                ? PERMISSION_MODE_LABELS[mode]
                : `${PERMISSION_MODE_LABELS[mode]}: ${unavailableReason}`
            }
            className={cn(
              "rounded-full px-2.5 py-0.5 font-medium",
              selected && "border-foreground text-foreground border bg-muted",
              !available && "opacity-50",
            )}
          >
            {PERMISSION_MODE_LABELS[mode]}
            {selected && <span className="sr-only"> (this session)</span>}
            {!available && <span className="sr-only">{unavailableReason}</span>}
          </span>
        );
      })}
      <span>· set when the session started</span>
    </div>
  );
}

export function CodeComposer({
  disabled,
  running,
  permissionMode,
  availableModes = MODES,
  unavailableReason = PERMISSION_MODE_UNAVAILABLE_REASON,
  onSend,
  onInterrupt,
}: {
  disabled?: boolean;
  running: boolean;
  permissionMode: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
  onSend: (message: string) => Promise<CodeTurnSubmission | void> | void;
  onInterrupt: () => Promise<void> | void;
}) {
  const [draft, setDraft] = useState("");
  const [notice, setNotice] = useState<
    { tone: "queued" | "error"; text: string } | null
  >(null);
  const canSend = draft.trim().length > 0 && !disabled;

  // The queue note describes the turn that was running when the message was
  // parked; once that turn is done the note has nothing left to say.
  useEffect(() => {
    if (!running) {
      setNotice((current) => (current?.tone === "queued" ? null : current));
    }
  }, [running]);

  async function send() {
    if (!canSend) return;
    const message = draft.trim();
    // A send that lands on an idle session is answered only when the turn
    // ends, minutes later. Clearing now keeps the box usable for the
    // follow-up that queueing exists to accept; a refusal puts the text back.
    setDraft("");
    setNotice(null);
    try {
      const outcome = await onSend(message);
      if (outcome && outcome.kind === "queued") {
        setNotice({ tone: "queued", text: QUEUED_NOTE });
      }
    } catch (err) {
      setNotice({ tone: "error", text: sendRefusal(err) });
      setDraft((current) => (current.length === 0 ? message : current));
    }
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.ctrlKey &&
      !event.altKey &&
      !event.metaKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      void send();
    }
  }

  return (
    <div className="flex shrink-0 flex-col gap-2 border-t px-4 py-3">
      <PermissionModeSummary
        value={permissionMode}
        availableModes={availableModes}
        unavailableReason={unavailableReason}
      />
      <div className="flex items-end gap-2">
        <Textarea
          rows={2}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          disabled={disabled}
          placeholder={running ? "Queue a follow-up…" : "Message the session"}
          aria-label="Message"
          className="min-h-[2.75rem] resize-none"
        />
        <Button type="button" size="sm" disabled={!canSend} onClick={() => void send()}>
          {running ? "Queue" : "Send"}
        </Button>
        {running && (
          <Button
            type="button"
            variant="destructive"
            size="sm"
            onClick={() => void onInterrupt()}
            aria-label="Interrupt"
          >
            <Square />
            Interrupt
          </Button>
        )}
      </div>
      {notice && (
        <p
          role="status"
          className={cn(
            "text-xs",
            notice.tone === "error"
              ? "text-destructive"
              : "text-muted-foreground",
          )}
        >
          {notice.text}
        </p>
      )}
    </div>
  );
}

/** The server's refusal, with the one queue-specific kind said in product terms. */
function sendRefusal(error: unknown): string {
  if (error instanceof HttpError && error.kind === "queue_full") {
    return QUEUE_FULL_NOTE;
  }
  return friendlyErrorMessage(error, "Could not send that turn");
}

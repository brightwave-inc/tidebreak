import { type KeyboardEvent, useState } from "react";
import { Square } from "lucide-react";

import type { CodePermissionMode } from "../api/types";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import {
  PERMISSION_MODE_LABELS,
  PERMISSION_MODE_UNAVAILABLE_REASON,
} from "./labels";

/**
 * Trimmed composer for a code session: text, send, interrupt, and the
 * permission mode the session was created under.
 *
 * Plan is the only mode this phase can honor. Ask and Auto stay visible so
 * the scale is honest, and they carry the server's "not yet available"
 * reason rather than disappearing.
 */

const MODES: CodePermissionMode[] = ["plan", "ask", "auto"];

export function CodeComposer({
  disabled,
  running,
  permissionMode,
  onSend,
  onInterrupt,
}: {
  disabled?: boolean;
  running: boolean;
  permissionMode: CodePermissionMode;
  onSend: (message: string) => Promise<void> | void;
  onInterrupt: () => Promise<void> | void;
}) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const canSend = draft.trim().length > 0 && !disabled && !sending && !running;

  async function send() {
    if (!canSend) return;
    const message = draft.trim();
    setSending(true);
    try {
      await onSend(message);
      setDraft("");
    } catch {
      // Leave the draft. A refused send has no turn to answer.
    } finally {
      setSending(false);
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
    <div className="flex flex-col gap-2 border-t px-4 py-3">
      <div className="flex flex-wrap items-center gap-1.5" data-testid="permission-modes">
        {MODES.map((mode) => {
          const available = mode === "plan";
          const selected = permissionMode === mode;
          return (
            <button
              key={mode}
              type="button"
              disabled={!available}
              title={
                available
                  ? PERMISSION_MODE_LABELS[mode]
                  : `${PERMISSION_MODE_LABELS[mode]}: ${PERMISSION_MODE_UNAVAILABLE_REASON}`
              }
              aria-pressed={selected}
              className={cn(
                "rounded-full border px-2.5 py-0.5 text-xs font-medium",
                selected && "border-foreground bg-muted",
                !available && "cursor-not-allowed opacity-50",
              )}
            >
              {PERMISSION_MODE_LABELS[mode]}
              {!available && (
                <span className="sr-only">
                  {PERMISSION_MODE_UNAVAILABLE_REASON}
                </span>
              )}
            </button>
          );
        })}
      </div>
      <div className="flex items-end gap-2">
        <Textarea
          rows={2}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={onKeyDown}
          disabled={disabled || sending}
          placeholder={running ? "Turn in progress…" : "Message the session"}
          aria-label="Message"
          className="min-h-[2.75rem] resize-none"
        />
        {running ? (
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
        ) : (
          <Button type="button" size="sm" disabled={!canSend} onClick={() => void send()}>
            Send
          </Button>
        )}
      </div>
    </div>
  );
}

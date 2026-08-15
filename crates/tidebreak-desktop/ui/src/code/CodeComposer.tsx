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
 * Ask is the default. Plan and Auto stay on the same scale. A mode the
 * current harness cannot honor is disabled with the refusal reason.
 */

const MODES: CodePermissionMode[] = ["plan", "ask", "auto"];

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

export function CodeComposer({
  disabled,
  running,
  permissionMode,
  availableModes = MODES,
  unavailableReason = PERMISSION_MODE_UNAVAILABLE_REASON,
  onPermissionMode,
  onSend,
  onInterrupt,
}: {
  disabled?: boolean;
  running: boolean;
  permissionMode: CodePermissionMode;
  availableModes?: readonly CodePermissionMode[];
  unavailableReason?: string;
  onPermissionMode?: (mode: CodePermissionMode) => void;
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
      <PermissionModePicker
        value={permissionMode}
        availableModes={availableModes}
        unavailableReason={unavailableReason}
        onChange={onPermissionMode}
      />
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

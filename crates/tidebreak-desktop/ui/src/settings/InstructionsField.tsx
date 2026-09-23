import { useEffect, useId, useRef, useState } from "react";

import { Textarea } from "@/components/ui/textarea";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { SettingsError, SettingsField } from "./primitives";

/**
 * Matches `MAX_INSTRUCTIONS_BYTES` on the server: the same bound a Slack
 * channel's instructions have. The cap counts UTF-8 bytes, so the count below
 * the field does too.
 */
export const MAX_INSTRUCTIONS_BYTES = 8192;

/** The share of the cap at which the count appears. */
const COUNT_FROM = 0.8;

const encoder = new TextEncoder();

export function instructionsBytes(text: string): number {
  return encoder.encode(text).length;
}

const number = new Intl.NumberFormat("en-US");

/**
 * Standing instructions as one long text field. It saves when you leave the
 * field, shows a byte count once the text nears the cap, and refuses to save
 * text over the cap.
 *
 * The field keeps its own draft, so a save that finishes while you type never
 * overwrites newer text. Saves run one at a time, newest last. A change that
 * never lost focus, because the page closed around it, saves once more on
 * the way out.
 */
export function InstructionsField({
  label,
  hint,
  placeholder,
  saved,
  disabled,
  autoFocus,
  onSave,
}: {
  label: string;
  hint: string;
  placeholder?: string;
  /** The instructions as last stored. */
  saved: string;
  disabled?: boolean;
  autoFocus?: boolean;
  /** Store the text. Reject to show why it did not save. */
  onSave: (instructions: string) => Promise<void>;
}) {
  const countId = useId();
  const [draft, setDraft] = useState(saved);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const draftRef = useRef(draft);
  const storedRef = useRef(saved);
  const onSaveRef = useRef(onSave);
  const inFlight = useRef(false);
  const queued = useRef<string | null>(null);
  const mounted = useRef(true);
  onSaveRef.current = onSave;

  // Follow the stored value while the reader has not edited away from it.
  useEffect(() => {
    if (draftRef.current === storedRef.current) {
      draftRef.current = saved;
      setDraft(saved);
    }
    storedRef.current = saved;
  }, [saved]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const pending = draftRef.current;
      if (
        pending !== storedRef.current &&
        instructionsBytes(pending) <= MAX_INSTRUCTIONS_BYTES
      ) {
        void onSaveRef.current(pending).catch(() => {});
      }
    };
  }, []);

  async function commit(text: string) {
    if (
      text === storedRef.current ||
      instructionsBytes(text) > MAX_INSTRUCTIONS_BYTES
    ) {
      return;
    }
    if (inFlight.current) {
      queued.current = text;
      return;
    }
    inFlight.current = true;
    setSaving(true);
    setError(null);
    try {
      let next: string | null = text;
      while (next !== null) {
        queued.current = null;
        await onSaveRef.current(next);
        storedRef.current = next;
        const after: string | null = queued.current;
        next = after !== null && after !== next ? after : null;
      }
    } catch (caught) {
      if (mounted.current) {
        setError(
          friendlyErrorMessage(caught, "Could not save the instructions."),
        );
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setSaving(false);
    }
  }

  const bytes = instructionsBytes(draft);
  const tooLong = bytes > MAX_INSTRUCTIONS_BYTES;
  const showCount = bytes >= MAX_INSTRUCTIONS_BYTES * COUNT_FROM;

  return (
    <>
      <SettingsField
        label={label}
        hint={hint}
        status={
          showCount ? (
            <span
              id={countId}
              className={cn(tooLong && "text-critical-foreground")}
            >
              {number.format(bytes)} of {number.format(MAX_INSTRUCTIONS_BYTES)}{" "}
              bytes
            </span>
          ) : undefined
        }
      >
        <Textarea
          className="min-h-40 resize-y"
          value={draft}
          placeholder={placeholder}
          disabled={disabled}
          autoFocus={autoFocus}
          aria-invalid={tooLong || undefined}
          aria-describedby={showCount ? countId : undefined}
          onChange={(event) => {
            draftRef.current = event.target.value;
            setDraft(event.target.value);
          }}
          onBlur={() => void commit(draftRef.current)}
        />
      </SettingsField>
      {tooLong && (
        <SettingsError>
          Shorten the instructions to {number.format(MAX_INSTRUCTIONS_BYTES)}{" "}
          bytes to save them.
        </SettingsError>
      )}
      {error && <SettingsError>{error}</SettingsError>}
      {saving && (
        <p className="text-xs text-muted-foreground" role="status">
          Saving…
        </p>
      )}
    </>
  );
}

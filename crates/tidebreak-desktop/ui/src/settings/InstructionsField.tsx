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
 * overwrites newer text. Saves run one at a time in the order you made them,
 * so an older save can never land after a newer one. A change that never lost
 * focus, because the page closed around it, saves once more on the way out.
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
  const queue = useRef<Promise<void>>(Promise.resolve());
  const pendingSaves = useRef(0);
  const mounted = useRef(true);
  onSaveRef.current = onSave;

  /** Run a save after every save already queued, skipping text already stored. */
  function enqueue(text: string): Promise<void> {
    const run = queue.current.then(async () => {
      if (text === storedRef.current) return;
      await onSaveRef.current(text);
      storedRef.current = text;
    });
    queue.current = run.catch(() => {});
    return run;
  }

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
        void enqueue(pending).catch(() => {});
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
    pendingSaves.current += 1;
    setSaving(true);
    setError(null);
    try {
      await enqueue(text);
    } catch (caught) {
      if (mounted.current) {
        setError(
          friendlyErrorMessage(caught, "Could not save the instructions."),
        );
      }
    } finally {
      pendingSaves.current -= 1;
      if (mounted.current && pendingSaves.current === 0) setSaving(false);
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
          className="min-h-40 resize-y aria-invalid:border-critical-border"
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

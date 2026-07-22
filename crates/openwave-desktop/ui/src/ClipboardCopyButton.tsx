import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";

export type ClipboardWriter = {
  writeText(text: string): Promise<void>;
};

type CopyState = "idle" | "copied" | "failed";

type TimerApi = {
  setTimeout(callback: () => void, delayMs: number): number;
  clearTimeout(id: number): void;
};

type ClipboardCopyButtonProps = {
  value: string;
  label: string;
  copiedAnnouncement: string;
  failedAnnouncement: string;
  className?: string;
};

export const COPY_STATE_RESET_MS = 3_000;

export function ClipboardCopyButton({
  value,
  label,
  copiedAnnouncement,
  failedAnnouncement,
  className,
}: ClipboardCopyButtonProps) {
  const [copyState, setCopyState] = useState<CopyState>("idle");

  useEffect(() => {
    if (copyState === "idle") return;
    return scheduleCopyStateReset(() => setCopyState("idle"));
  }, [copyState]);

  async function onCopy() {
    try {
      await copyPlainText(value);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
  }

  const visibleLabel =
    copyState === "copied"
      ? "Copied"
      : copyState === "failed"
        ? "Copy failed"
        : label;

  return (
    <>
      <button
        type="button"
        className={className}
        aria-label={visibleLabel}
        title={visibleLabel}
        onClick={() => void onCopy()}
      >
        <span aria-hidden="true" className="clipboard-copy-icon">
          {copyState === "copied" ? <Check size={13} /> : <Copy size={13} />}
        </span>
        <span>{visibleLabel}</span>
      </button>
      <span className="sr-only" role="status" aria-live="polite">
        {copyState === "copied"
          ? copiedAnnouncement
          : copyState === "failed"
            ? failedAnnouncement
            : ""}
      </span>
    </>
  );
}

export async function copyPlainText(
  text: string,
  clipboard: ClipboardWriter | undefined = globalThis.navigator?.clipboard,
): Promise<void> {
  if (!clipboard?.writeText) {
    throw new Error("Clipboard access is unavailable");
  }
  await clipboard.writeText(text);
}

export function scheduleCopyStateReset(
  reset: () => void,
  timers: TimerApi = window,
): () => void {
  const timeout = timers.setTimeout(reset, COPY_STATE_RESET_MS);
  return () => timers.clearTimeout(timeout);
}

import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";

export type ClipboardWriter = {
  writeText(text: string): Promise<void>;
  /** Present only where the richer, multi-flavour clipboard API exists. */
  write?(items: ClipboardItem[]): Promise<void>;
};

type CopyState = "idle" | "copied" | "failed";

type TimerApi = {
  setTimeout(callback: () => void, delayMs: number): number;
  clearTimeout(id: number): void;
};

type ClipboardCopyButtonProps = {
  label: string;
  copiedAnnouncement: string;
  failedAnnouncement: string;
  className?: string;
} & (
  | { value: string; copy?: never }
  /**
   * Produce and write the payload at click time — for a control whose content
   * is only known from the rendered DOM, or that writes more than plain text.
   */
  | { value?: never; copy: () => Promise<void> }
);

export const COPY_STATE_RESET_MS = 3_000;

export function ClipboardCopyButton({
  value,
  copy,
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
      if (copy) await copy();
      else await copyPlainText(value ?? "");
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
      <WithTooltip label={visibleLabel}>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className={className}
          aria-label={visibleLabel}
          onClick={() => void onCopy()}
        >
          <span aria-hidden="true" className="clipboard-copy-icon">
            {copyState === "copied" ? <Check size={13} /> : <Copy size={13} />}
          </span>
        </Button>
      </WithTooltip>
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

/**
 * Write one selection under two flavours, so the paste target picks the one it
 * understands: `text/html` for a rich editor, `text/plain` for an editor, a
 * terminal, or a spreadsheet reading tab-separated cells.
 *
 * Falls back to the plain flavour alone wherever the multi-flavour API is
 * missing or refuses the write, which is the payload every target accepts.
 */
export async function copyRichText(
  html: string,
  text: string,
  clipboard: ClipboardWriter | undefined = globalThis.navigator?.clipboard,
  item: typeof ClipboardItem | undefined = globalThis.ClipboardItem,
): Promise<void> {
  if (clipboard?.write && item) {
    try {
      await clipboard.write([
        new item({
          "text/plain": new Blob([text], { type: "text/plain" }),
          "text/html": new Blob([html], { type: "text/html" }),
        }),
      ]);
      return;
    } catch {
      // Fall through: a browser that rejects the richer write still takes text.
    }
  }
  await copyPlainText(text, clipboard);
}

export function scheduleCopyStateReset(
  reset: () => void,
  timers: TimerApi = window,
): () => void {
  const timeout = timers.setTimeout(reset, COPY_STATE_RESET_MS);
  return () => timers.clearTimeout(timeout);
}

import { useId, useState } from "react";
import { ChevronDown, ChevronRight, FileText } from "lucide-react";

import { pastedTextLineCount, pastedTextPreview } from "./PastedText";
import { ToolOutputPreview } from "./ToolOutputPreview";
import { cn } from "@/lib/utils";

/**
 * One long paste inside a sent message, folded the way the composer chip
 * folded it before send.
 *
 * The chip promised the reader that a wall of text would stay out of the way;
 * the transcript keeps that promise. Closed, it is the same card: an icon,
 * the first line, the size. Open, it is the text itself with the copy control
 * every output block has.
 */
export function PastedTextBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const lines = pastedTextLineCount(text);
  const characters = [...text].length;
  const detail = `${lines.toLocaleString()} ${lines === 1 ? "line" : "lines"} · ${characters.toLocaleString()} characters`;
  const preview = pastedTextPreview(text);
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <div
      className="border-border bg-muted/50 text-muted-foreground not-prose my-2 flex max-w-full flex-col gap-2 rounded-lg border px-2 py-1.5"
      data-testid="pasted-text-block"
    >
      <button
        type="button"
        className={cn(
          "flex min-w-0 cursor-pointer items-center gap-2 rounded-md text-left",
          "focus-visible:ring-ring focus-visible:ring-2 focus-visible:outline-none",
        )}
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((current) => !current)}
      >
        <span className="bg-background inline-flex size-9 shrink-0 items-center justify-center rounded-md">
          <FileText className="size-4" aria-hidden="true" />
        </span>
        <span className="grid min-w-0 flex-1 gap-px">
          <strong className="text-foreground text-xs font-semibold">
            Pasted text
          </strong>
          <small className="text-2xs truncate" title={preview}>
            {preview} · {detail}
          </small>
        </span>
        <Chevron className="size-4 shrink-0" aria-hidden="true" />
      </button>
      {open && (
        <div id={bodyId}>
          <ToolOutputPreview
            text={text}
            label="Pasted text"
            collapsedLines={24}
          />
        </div>
      )}
    </div>
  );
}

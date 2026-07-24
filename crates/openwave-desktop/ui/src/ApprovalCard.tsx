import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { ToolApprovalPreview } from "./api";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ToolPreviewBlock } from "./ToolPreview";

export type ApprovalDecision = "approve" | "reject";

export type ApprovalOption = {
  key: string;
  label: string;
  decision: ApprovalDecision;
  remember: boolean;
};

type ApprovalCardProps = {
  callId: string;
  /** Fixed copy naming the class of action under review. */
  summary: string;
  /** The tool's own view of the concrete action, when it projects one. */
  preview: ToolApprovalPreview | null;
  canApprove: boolean;
  canRemember: boolean;
  deciding: boolean;
  error?: string;
  onDecide: (
    callId: string,
    decision: ApprovalDecision,
    remember?: boolean,
  ) => void;
};

/**
 * The consent panel for a parked tool call.
 *
 * The options are a keyboard-navigable list rather than a row of buttons, and
 * they are ordered narrowest grant first with the decline last. Scope is easy
 * to widen by accident and hard to notice afterwards, so the cheapest keystroke
 * has to be the one that grants the least.
 */
export function ApprovalCard({
  callId,
  summary,
  preview,
  canApprove,
  canRemember,
  deciding,
  error,
  onDecide,
}: ApprovalCardProps) {
  const options = approvalOptions(canApprove, canRemember);
  const [highlight, setHighlight] = useState(0);
  const rowRefs = useRef<Array<HTMLDivElement | null>>([]);
  const safeHighlight = Math.min(highlight, options.length - 1);

  useEffect(() => {
    rowRefs.current = rowRefs.current.slice(0, options.length);
  }, [options.length]);

  const commit = (index: number) => {
    const option = options[index];
    if (!option || deciding) return;
    onDecide(callId, option.decision, option.remember);
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>, index: number) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const next = (index + delta + options.length) % options.length;
      setHighlight(next);
      rowRefs.current[next]?.focus();
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      commit(index);
      return;
    }
    const digit = Number.parseInt(event.key, 10);
    if (Number.isInteger(digit) && digit >= 1 && digit <= options.length) {
      event.preventDefault();
      setHighlight(digit - 1);
      rowRefs.current[digit - 1]?.focus();
    }
  };

  return (
    <section
      className="bg-card text-card-foreground flex w-[min(100%,38rem)] flex-col gap-3 self-start rounded-lg border p-4"
      aria-label="Approval needed"
      aria-busy={deciding}
    >
      <h3 className="text-sm font-medium break-words">{summary}</h3>
      {preview && <ToolPreviewBlock preview={preview} />}
      <div
        role="listbox"
        aria-label="What should happen"
        className="flex flex-col gap-0.5"
      >
        {options.map((option, index) => (
          <div
            key={option.key}
            ref={(node) => {
              rowRefs.current[index] = node;
            }}
            role="option"
            aria-selected={index === safeHighlight}
            tabIndex={deciding ? -1 : 0}
            onClick={() => commit(index)}
            onFocus={() => setHighlight(index)}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "focus-visible:ring-ring flex cursor-pointer items-baseline gap-2.5 rounded-md px-3 py-2 text-sm outline-hidden focus-visible:ring-2",
              index === safeHighlight ? "bg-muted" : "hover:bg-muted/60",
              deciding && "pointer-events-none opacity-60",
            )}
          >
            <span className="text-muted-foreground w-4 shrink-0 text-xs tabular-nums">
              {index + 1}.
            </span>
            <span
              className={cn(
                "flex-1",
                option.decision === "reject" && "text-muted-foreground",
              )}
            >
              {option.label}
            </span>
          </div>
        ))}
      </div>
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="text-muted-foreground">
          ↑↓ choose · 1–{options.length} jump · ↵ submit
        </span>
        <Button
          size="sm"
          disabled={deciding}
          onClick={() => commit(safeHighlight)}
        >
          Submit
        </Button>
      </div>
      {error && (
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/**
 * Narrowest grant first, decline last.
 *
 * A kind the server will not let the renderer approve offers only the decline,
 * so an unpresentable action can never be waved through from here.
 */
export function approvalOptions(
  canApprove: boolean,
  canRemember: boolean,
): ApprovalOption[] {
  const options: ApprovalOption[] = [];
  if (canApprove) {
    options.push({
      key: "once",
      label: "Yes, allow it once",
      decision: "approve",
      remember: false,
    });
    if (canRemember) {
      options.push({
        key: "chat",
        label: "Yes, and don't ask again in this chat",
        decision: "approve",
        remember: true,
      });
    }
  }
  options.push({
    key: "decline",
    label: "No, don't allow this",
    decision: "reject",
    remember: false,
  });
  return options;
}

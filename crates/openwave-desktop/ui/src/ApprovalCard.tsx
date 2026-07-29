import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { ApprovalGrantRung, ToolActionPreview } from "./api";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ScrollableContainer } from "./ScrollableContainer";
import { toolPreviewPresentation } from "./ToolPreview";

export type ApprovalDecision = "approve" | "reject";

export type ApprovalOption =
  | {
      kind: "decide";
      key: string;
      label: string;
      decision: ApprovalDecision;
      grant: ApprovalGrantRung | null;
    }
  | { kind: "more"; key: string; label: string };

type ApprovalCardProps = {
  callId: string;
  /** Fixed copy naming the class of action under review. */
  summary: string;
  /** The tool's own view of the concrete action, when it projects one. */
  preview: ToolActionPreview | null;
  canApprove: boolean;
  canRemember: boolean;
  deciding: boolean;
  error?: string;
  onDecide: (
    callId: string,
    decision: ApprovalDecision,
    grant: ApprovalGrantRung | null,
  ) => void;
};

/**
 * The consent panel for a parked tool call.
 *
 * It leads with a short question, because deciding starts with knowing what is
 * being decided; the longer sentence about what the action can reach explains
 * rather than asks, so it sits underneath. Below that, the exact action.
 *
 * The options are a keyboard-navigable list rather than a row of buttons,
 * ordered narrowest grant first with the decline last. Scope is easy to widen
 * by accident and hard to notice afterwards, so the cheapest keystroke has to
 * be the one that grants the least.
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
  const [expanded, setExpanded] = useState(false);
  const options = approvalOptions(preview, canApprove, canRemember, expanded);
  const [highlight, setHighlight] = useState(0);
  const rowRefs = useRef<Array<HTMLDivElement | null>>([]);
  const hasAutoFocused = useRef(false);
  const safeHighlight = Math.min(highlight, options.length - 1);
  const ask = approvalAsk(preview, summary);

  // Arm the shortcuts the moment the card appears, so ↑↓ / 1–9 / ↵ work
  // without a click first. The latch arms before the bail-out so this fires
  // exactly once: if focus is intentionally elsewhere we leave it there rather
  // than yanking it back on some later state change.
  useEffect(() => {
    if (hasAutoFocused.current || deciding) return;
    hasAutoFocused.current = true;
    const active = document.activeElement;
    const focusedElsewhere =
      active instanceof HTMLElement &&
      (active.isContentEditable ||
        active.tagName === "INPUT" ||
        active.tagName === "TEXTAREA" ||
        active.closest('[role="listbox"]') !== null);
    if (focusedElsewhere) return;
    // preventScroll: the point is to arm the keys, not to drag the transcript
    // to a card that may have mounted off-screen.
    rowRefs.current[0]?.focus({ preventScroll: true });
  }, [deciding]);

  const commit = (index: number) => {
    const option = options[index];
    if (!option || deciding) return;
    if (option.kind === "more") {
      setExpanded(true);
      // The revealed grants take the indices "More options" occupied, so
      // leaving the highlight here would park it on a broader grant and let
      // the next Enter commit it — the one thing this list exists to prevent.
      setHighlight(0);
      rowRefs.current[0]?.focus();
      return;
    }
    onDecide(callId, option.decision, option.grant);
  };

  const focusRow = (to: number) => {
    const wrapped = ((to % options.length) + options.length) % options.length;
    rowRefs.current[wrapped]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>, index: number) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      focusRow(index + (event.key === "ArrowDown" ? 1 : -1));
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      commit(index);
      return;
    }
    if (/^[1-9]$/.test(event.key)) {
      const target = Number(event.key) - 1;
      if (target < options.length) {
        event.preventDefault();
        focusRow(target);
      }
    }
  };

  return (
    <section
      className="bg-background flex max-w-prose flex-col gap-3 rounded-lg border p-4"
      aria-label="Approval needed"
      aria-busy={deciding}
    >
      <h3 className="font-medium break-words">{ask.title}</h3>
      {ask.summaryLine && (
        <p className="text-muted-foreground text-sm break-words">
          {ask.summaryLine}
        </p>
      )}
      {preview && (
        <ScrollableContainer className="bg-muted text-muted-foreground max-h-48 rounded-md p-3 text-xs break-words whitespace-pre-wrap">
          {toolPreviewPresentation(preview).detail}
        </ScrollableContainer>
      )}
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
            onMouseEnter={(event) => event.currentTarget.focus()}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "focus-visible:ring-ring flex cursor-pointer items-baseline gap-2.5 rounded-md px-3 py-2.5 text-sm outline-hidden focus-visible:ring-2",
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
                option.kind === "decide" &&
                  option.decision === "reject" &&
                  "text-muted-foreground",
                option.kind === "more" && "text-muted-foreground",
              )}
            >
              {option.label}
            </span>
          </div>
        ))}
      </div>
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="text-muted-foreground">
          ↑↓ choose · 1–{Math.min(options.length, 9)} jump · ↵ submit
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
 * What the card asks, and what it adds underneath.
 *
 * A tool that shows its action gets a short question about *this* action, and
 * the server's sentence about the class of egress moves to the subheading. A
 * tool with nothing to show has only that sentence, so it stays the question.
 */
export function approvalAsk(
  preview: ToolActionPreview | null,
  summary: string,
): { title: string; summaryLine: string | null } {
  if (preview?.tool === "exec") {
    return { title: "Run this command?", summaryLine: summary };
  }
  return { title: summary, summaryLine: null };
}

/** How many grants show before the rest move behind "More options". */
const INLINE_GRANTS = 1;

/**
 * Narrowest grant first, decline last.
 *
 * The broader rungs start hidden. Every option on screen is one keystroke
 * away, so a list that shows "don't ask again" beside "just this once" makes
 * the widest grant as cheap as the narrowest — and scope is easy to widen by
 * accident and hard to notice afterwards.
 *
 * A kind the server will not let the renderer approve offers only the decline,
 * so an unpresentable action can never be waved through from here.
 */
export function approvalOptions(
  preview: ToolActionPreview | null,
  canApprove: boolean,
  canRemember: boolean,
  expanded = false,
): ApprovalOption[] {
  if (!canApprove) return [declineOption()];

  const options: ApprovalOption[] = [
    {
      kind: "decide",
      key: "once",
      // Named for the act, not the abstraction: "run it once" is what the
      // person is about to do.
      label:
        preview?.tool === "exec" ? "Yes, run it once" : "Yes, allow it once",
      decision: "approve",
      grant: null,
    },
  ];

  const grants = canRemember ? grantLadder(preview) : [];
  const visible = expanded ? grants : grants.slice(0, INLINE_GRANTS);
  options.push(...visible);
  if (grants.length > visible.length) {
    options.push({ kind: "more", key: "more", label: "More options" });
  }
  options.push(declineOption());
  return options;
}

function declineOption(): ApprovalOption {
  return {
    kind: "decide",
    key: "decline",
    label: "No, don't allow this",
    decision: "reject",
    grant: null,
  };
}

/**
 * The standing grants on offer, narrowest first.
 *
 * An action that names itself can be consented to as itself, rather than as
 * the class of things its tool does — "always allow this query" rather than
 * "allow every web search in this chat". A tool with nothing to describe can
 * only be granted wholesale, because there is no narrower thing to name.
 */
export function grantLadder(preview: ToolActionPreview | null): ApprovalOption[] {
  const wholeTool: ApprovalOption = {
    kind: "decide",
    key: "whole-tool",
    label:
      preview?.tool === "exec"
        ? "Yes, and don't ask again about commands in this chat"
        : "Yes, and don't ask again in this chat",
    decision: "approve",
    grant: "whole_tool",
  };
  if (!preview) return [wholeTool];

  const exact: ApprovalOption = {
    kind: "decide",
    key: "exact",
    label: `Yes, and always allow exactly \u201c${spokenAction(preview)}\u201d`,
    decision: "approve",
    grant: "exact_action",
  };
  // A command is the one action with a rung between itself and its whole tool:
  // the executable it runs. With no arguments even that would be the same
  // grant as the exact one.
  if (preview.tool !== "exec" || preview.args.length === 0) {
    return [exact, wholeTool];
  }
  return [
    exact,
    {
      kind: "decide",
      key: "any-args",
      label: `Yes, and always allow any \u201c${preview.command}\u201d command`,
      decision: "approve",
      grant: "any_args_for_command",
    },
    wholeTool,
  ];
}

/** How much of an action fits in one row of the option list. */
const SPOKEN_ACTION_CHARS = 48;

/**
 * The action as the grant label names it.
 *
 * Bounded, because this is one row of a keyboard-driven list and a
 * natural-language query runs to hundreds of characters. The unabridged action
 * is in the block above the list, which is where someone reads it.
 */
function spokenAction(preview: ToolActionPreview): string {
  const spoken =
    preview.tool === "exec"
      ? [preview.command, ...preview.args].join(" ")
      : preview.tool === "web_extract"
        ? preview.url
        : preview.query;
  return spoken.length > SPOKEN_ACTION_CHARS
    ? `${spoken.slice(0, SPOKEN_ACTION_CHARS).trimEnd()}\u2026`
    : spoken;
}

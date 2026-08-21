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
  /** How far a remembered answer will reach, for the option labels. */
  grantScope?: GrantScopeName;
  /** Complete standing-grant ladder the server will honor for this call. */
  grantRungs?: readonly ApprovalGrantRung[];
  /** The Auto-mode judge is deciding. Advisory only: the card stays live. */
  autoJudging?: boolean;
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
 * The choices are action buttons, ordered narrowest grant first with the
 * decline last. Clicking a choice decides it. Keyboard arrows and number keys
 * only move the highlight, so browsing the list does not execute the tool or
 * persist a grant; Enter or Submit confirms the highlighted row. "More options"
 * expands the hidden rungs without deciding.
 */
export function ApprovalCard({
  callId,
  summary,
  preview,
  canApprove,
  canRemember,
  grantScope = "chat",
  grantRungs = [],
  autoJudging,
  deciding,
  error,
  onDecide,
}: ApprovalCardProps) {
  const [expanded, setExpanded] = useState(false);
  const options = approvalOptions(
    preview,
    canApprove,
    canRemember,
    expanded,
    grantScope,
    grantRungs,
  );
  const [highlight, setHighlight] = useState(0);
  const rowRefs = useRef<Array<HTMLButtonElement | null>>([]);
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
        active.closest('[aria-label="Approval choices"]') !== null);
    if (focusedElsewhere) return;
    // preventScroll: the point is to arm the keys, not to drag the transcript
    // to a card that may have mounted off-screen.
    rowRefs.current[0]?.focus({ preventScroll: true });
  }, [deciding]);

  const activate = (index: number) => {
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
    setHighlight(index);
    onDecide(callId, option.decision, option.grant);
  };

  const submitSelected = () => {
    const option = options[safeHighlight];
    if (!option || option.kind !== "decide" || deciding) return;
    onDecide(callId, option.decision, option.grant);
  };

  const focusRow = (to: number) => {
    const wrapped = ((to % options.length) + options.length) % options.length;
    if (options[wrapped]?.kind === "decide") setHighlight(wrapped);
    rowRefs.current[wrapped]?.focus();
  };

  const onKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      focusRow(index + (event.key === "ArrowDown" ? 1 : -1));
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
      {autoJudging && (
        <p className="text-muted-foreground text-sm" role="status">
          Deciding automatically… you can still answer to decide it yourself.
        </p>
      )}
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
        role="group"
        aria-label="Approval choices"
        className="flex flex-col gap-0.5"
      >
        {options.map((option, index) => (
          <button
            type="button"
            key={option.key}
            ref={(node) => {
              rowRefs.current[index] = node;
            }}
            aria-pressed={
              option.kind === "decide" ? index === safeHighlight : undefined
            }
            disabled={deciding}
            onClick={() => activate(index)}
            onFocus={() => {
              if (option.kind === "decide") setHighlight(index);
            }}
            onMouseEnter={(event) => event.currentTarget.focus()}
            onKeyDown={(event) => onKeyDown(event, index)}
            className={cn(
              "focus-visible:ring-ring flex cursor-pointer items-baseline gap-2.5 rounded-md px-3 py-2.5 text-sm outline-hidden focus-visible:ring-2",
              index === safeHighlight ? "bg-muted" : "hover:bg-muted/60",
              deciding && "opacity-60",
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
          </button>
        ))}
      </div>
      {canRemember && grantScope === "project" && (
        <p className="text-muted-foreground text-xs">
          Saved answers apply to all work in this project. Review them under
          Settings → Permissions.
        </p>
      )}
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="text-muted-foreground">
          ↑↓ choose · 1–{Math.min(options.length, 9)} jump · click or Submit
          confirms
        </span>
        <Button size="sm" disabled={deciding} onClick={submitSelected}>
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
  if (preview?.tool === "write_file") {
    return { title: "Write this file?", summaryLine: summary };
  }
  if (preview?.tool === "delegate_agent") {
    return { title: "Start this background agent?", summaryLine: summary };
  }
  return { title: summary, summaryLine: null };
}

/** How many grants show before the rest move behind "More options". */
const INLINE_GRANTS = 2;

/**
 * Narrowest grant first, decline last.
 *
 * The widest rungs start hidden. Every option on screen is one keystroke
 * away, so a list that shows "don't ask again" beside "just this once" makes
 * the widest grant as cheap as the narrowest — and scope is easy to widen by
 * accident and hard to notice afterwards.
 *
 * A kind the server will not let the renderer approve offers only the decline,
 * so an unpresentable action can never be waved through from here.
 */
/** How far a remembered answer reaches, as the label says it. */
export type GrantScopeName = "chat" | "project";

export function approvalOptions(
  preview: ToolActionPreview | null,
  canApprove: boolean,
  canRemember: boolean,
  expanded = false,
  scope: GrantScopeName = "chat",
  grantRungs: readonly ApprovalGrantRung[] = [],
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

  const grants = canRemember ? grantLadder(preview, scope, grantRungs) : [];
  // Folding a single rung costs a row to save a row: the list is no shorter for
  // hiding it, and the reader pays a keystroke to see what was already there.
  const fold = !expanded && grants.length > INLINE_GRANTS + 1;
  const visible = fold ? grants.slice(0, INLINE_GRANTS) : grants;
  options.push(...visible);
  if (fold) {
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
export function grantLadder(
  preview: ToolActionPreview | null,
  scope: GrantScopeName = "chat",
  grantRungs: readonly ApprovalGrantRung[] = [],
): ApprovalOption[] {
  // The label names the level the server will actually write. A chat filed
  // under a project grants across it, and saying "this chat" while writing
  // something wider is the one thing a consent label must never do.
  const where = scope === "project" ? "in this project" : "in this work";
  return grantRungs.flatMap((grant): ApprovalOption[] => {
    if (grant === "whole_tool") {
      return [
        {
          kind: "decide",
          key: "whole-tool",
          label:
            preview?.tool === "exec"
              ? `Yes, and don't ask again about commands ${where}`
              : `Yes, and don't ask again ${where}`,
          decision: "approve",
          grant,
        },
      ];
    }
    if (grant === "exact_action") {
      return preview
        ? [
            {
              kind: "decide",
              key: "exact",
              label: `Yes, and always allow exactly \u201c${spokenAction(preview)}\u201d`,
              decision: "approve",
              grant,
            },
          ]
        : [];
    }
    if ("path_prefix" in grant) {
      if (preview?.tool !== "write_file") return [];
      // The concrete place comes from the parked call's own path; the rung
      // only says how many segments of it were offered.
      const segments = placeSegments(preview.path);
      const count = grant.path_prefix.segments;
      if (count < 1 || count > segments.length) return [];
      const place = bounded(segments.slice(0, count).join("/"));
      return [
        {
          kind: "decide",
          key: `place-${count}`,
          label:
            count === segments.length
              ? `Yes, and always allow writing \u201c${place}\u201d`
              : `Yes, and always allow writes under \u201c${place}/\u201d`,
          decision: "approve",
          grant,
        },
      ];
    }
    if (preview?.tool !== "exec") return [];
    const argv = [preview.command, ...preview.args];
    const tokens = grant.command_prefix.tokens;
    if (tokens < 1 || tokens > argv.length) return [];
    return [
      {
        kind: "decide",
        key: `prefix-${tokens}`,
        label: `Yes, and always allow any \u201c${argv.slice(0, tokens).join(" ")}\u201d command`,
        decision: "approve",
        grant,
      },
    ];
  });
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
        : preview.tool === "write_file"
          ? preview.path
          : preview.tool === "delegate_agent"
            ? preview.task
            : preview.query;
  return bounded(spoken);
}

function bounded(spoken: string): string {
  return spoken.length > SPOKEN_ACTION_CHARS
    ? `${spoken.slice(0, SPOKEN_ACTION_CHARS).trimEnd()}\u2026`
    : spoken;
}

/**
 * The canonical segments of a workspace-relative path, matching how the
 * server names a place: empty and `.` segments dropped, so the label shows
 * the place the grant will actually cover.
 */
export function placeSegments(path: string): string[] {
  return path.split("/").filter((segment) => segment !== "" && segment !== ".");
}

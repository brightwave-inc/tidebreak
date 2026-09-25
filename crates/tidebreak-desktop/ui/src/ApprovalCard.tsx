import { useRef, useState } from "react";
import type { ApprovalGrantRung, ToolActionPreview } from "./api";
import {
  ApprovalChoiceList,
  APPROVAL_SHORTCUT_GRACE_MS,
} from "./ApprovalChoiceList";
import { ScrollableContainer } from "./ScrollableContainer";
import { toolPreviewPresentation } from "./ToolPreview";
import { Notice } from "@/components/ui/notice";

export { APPROVAL_SHORTCUT_GRACE_MS };

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
  const headingRef = useRef<HTMLHeadingElement | null>(null);
  const ask = approvalAsk(preview, summary);
  const titleId = `approval-ask-${callId}`;
  const previewId = preview ? `approval-preview-${callId}` : undefined;

  return (
    <section
      className="bg-background flex w-full min-w-0 flex-col gap-3 rounded-lg border p-4"
      aria-label="Approval needed"
      aria-busy={deciding}
    >
      <h3
        id={titleId}
        ref={headingRef}
        tabIndex={-1}
        className="font-medium break-words outline-hidden"
      >
        {ask.title}
      </h3>
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
        <ScrollableContainer
          id={previewId}
          className="bg-muted text-muted-foreground max-h-48 rounded-md p-3 text-xs break-words whitespace-pre-wrap"
        >
          {toolPreviewPresentation(preview).detail}
        </ScrollableContainer>
      )}
      <ApprovalChoiceList
        options={options.map((option) => ({
          key: option.key,
          label: option.label,
          muted:
            option.kind === "more" ||
            (option.kind === "decide" && option.decision === "reject"),
          expand: option.kind === "more",
        }))}
        disabled={deciding}
        describedBy={
          [titleId, previewId].filter(Boolean).join(" ") || undefined
        }
        headingRef={headingRef}
        note={
          canRemember && grantScope === "project" ? (
            <p className="text-muted-foreground text-xs">
              Saved answers apply to every conversation in this project. Review
              them under Settings → Permissions.
            </p>
          ) : undefined
        }
        onChoose={(index) => {
          const option = options[index];
          if (!option || option.kind !== "decide") return;
          onDecide(callId, option.decision, option.grant);
        }}
        onExpand={() => setExpanded(true)}
      />
      {error && (
        <Notice tone="critical" density="compact">
          {error}
        </Notice>
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
  if (preview?.tool === "code_session") {
    return {
      title:
        preview.operation === "create"
          ? "Start this repository work?"
          : "Send this follow-up?",
      summaryLine: summary,
    };
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
  const where =
    scope === "project" ? "in this project" : "in this conversation";
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
          : preview.tool === "delegate_agent" || preview.tool === "code_session"
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

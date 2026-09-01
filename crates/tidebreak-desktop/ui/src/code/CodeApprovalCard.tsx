import { useEffect, useId, useState, type ReactNode } from "react";

import type { CodeApprovalSnapshot } from "../api/types";
import { toolPreviewPresentation } from "../ToolPreview";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import { formatMessageTimestamp } from "@/MessageFooter";
import { cn } from "@/lib/utils";
import { ScrollableContainer } from "@/ScrollableContainer";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";

/**
 * Parked engine approval. The normalized kind leads so the reader can decide;
 * the harness payload stays behind a disclosure so the card does not paraphrase
 * what the engine asked (decision 0033). Deny opens a feedback field the model
 * will see.
 */
export function CodeApprovalCard({
  approval,
  deciding,
  error,
  onDecide,
  onReveal,
}: {
  approval: CodeApprovalSnapshot;
  deciding?: boolean;
  error?: string;
  onDecide: (decision: "approve" | "deny", feedback?: string) => void;
  /** The reader opened the payload disclosure, which grows the card. */
  onReveal?: () => void;
}) {
  const [denying, setDenying] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [payloadOpen, setPayloadOpen] = useState(false);
  const payloadId = useId();
  const decided = approval.state !== "pending";

  useEffect(() => {
    if (approval.state !== "pending") setDenying(false);
  }, [approval.state]);

  return (
    <section
      className="bg-background flex max-w-prose flex-col gap-3 rounded-lg border p-4"
      aria-label="Approval needed"
      aria-busy={deciding}
      data-testid="code-approval-card"
    >
      <div className="flex items-baseline justify-between gap-3">
        <h3 className="text-md font-medium break-words">
          {approvalTitle(approval)}
        </h3>
        <ApprovalState approval={approval} />
      </div>
      <ApprovalKindBody approval={approval} />
      <ApprovalTimes
        requestedAt={approval.requested_at}
        decidedAt={approval.decided_at}
      />
      {approval.state === "denied" && approval.feedback && (
        <p className="text-md break-words">{approval.feedback}</p>
      )}
      {approval.state === "abandoned" && (
        <p className="text-muted-foreground text-md break-words">
          The engine stopped waiting for this one, so a decision can no longer
          reach it. Whatever it asked for did not run on your say-so.
        </p>
      )}
      {/* A structured approval from the internal engine carries no verbatim
          payload; a disclosure that reveals nothing reads as a broken card. */}
      {approval.harness_raw_json.length > 0 && (
        <div>
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:text-foreground cursor-pointer rounded-sm text-xs",
              FOCUS_RING,
              HOVER_TINT,
            )}
            aria-expanded={payloadOpen}
            aria-controls={payloadOpen ? payloadId : undefined}
            onClick={() => {
              onReveal?.();
              setPayloadOpen((current) => !current);
            }}
          >
            Harness payload
          </button>
          <Reveal open={payloadOpen}>
            {/*
            One `pre`, not two: the scroll container is itself a `pre`, and a
            nested one carried the browser's own `white-space: pre`, so the
            wrapping asked for here never applied and a single long JSON line
            scrolled sideways instead.
          */}
            <ScrollableContainer
              id={payloadId}
              className="bg-muted text-muted-foreground mt-2 max-h-48 rounded-md p-3 font-mono text-xs break-words whitespace-pre-wrap"
            >
              {prettyRaw(approval.harness_raw_json)}
            </ScrollableContainer>
          </Reveal>
        </div>
      )}
      {!decided && !denying && (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            size="sm"
            disabled={deciding}
            onClick={() => onDecide("approve")}
          >
            Approve
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={deciding}
            onClick={() => setDenying(true)}
          >
            Deny
          </Button>
        </div>
      )}
      {!decided && denying && (
        <div className="flex flex-col gap-2">
          <Textarea
            rows={2}
            value={feedback}
            onChange={(event) => setFeedback(event.target.value)}
            placeholder="Tell the agent what to do instead"
            aria-label="Denial feedback"
            disabled={deciding}
          />
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              size="sm"
              variant="destructive"
              disabled={deciding}
              onClick={() => onDecide("deny", feedback.trim() || undefined)}
            >
              Deny
            </Button>
            <Button
              type="button"
              size="sm"
              variant="ghost"
              disabled={deciding}
              onClick={() => setDenying(false)}
            >
              Cancel
            </Button>
          </div>
        </div>
      )}
      {error && (
        <p
          className="text-critical-foreground text-xs break-words"
          role="alert"
        >
          {error}
        </p>
      )}
    </section>
  );
}

function ApprovalState({ approval }: { approval: CodeApprovalSnapshot }) {
  if (approval.state === "approved") {
    return <p className="text-success-foreground shrink-0 text-xs">Approved</p>;
  }
  if (approval.state === "denied") {
    return <p className="text-warning-foreground shrink-0 text-xs">Denied</p>;
  }
  if (approval.state === "abandoned") {
    return (
      <p className="text-muted-foreground shrink-0 text-xs">Not decided</p>
    );
  }
  return null;
}

function ApprovalKindBody({ approval }: { approval: CodeApprovalSnapshot }) {
  switch (approval.kind.type) {
    case "command":
      return (
        <div className="flex flex-col gap-1">
          <pre className="bg-muted overflow-x-auto rounded-md p-2 font-mono text-md break-words whitespace-pre-wrap">
            {approval.kind.cmd || "Command"}
          </pre>
          {approval.kind.cwd && (
            <p className="text-muted-foreground font-mono text-xs break-words">
              cwd {approval.kind.cwd}
            </p>
          )}
        </div>
      );
    case "file_write":
      return approval.kind.paths.length > 0 ? (
        <ul className="space-y-0.5">
          {approval.kind.paths.map((path) => (
            <li key={path}>
              <MiddleTruncate text={path} className="font-mono text-md" />
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-muted-foreground text-md">File write</p>
      );
    case "network":
    case "other":
      return (
        <p className="text-muted-foreground text-md break-words">
          {otherSummary(approval.kind.summary)}
        </p>
      );
    case "tool_use":
      // Decision 0018: the literal action, never the call's own narration.
      return (
        <pre className="bg-muted overflow-x-auto rounded-md p-2 font-mono text-md break-words whitespace-pre-wrap">
          {toolPreviewPresentation(approval.kind.preview).detail}
        </pre>
      );
    case "questions":
      return (
        <ul className="space-y-2">
          {approval.kind.questions.map((question) => (
            <li key={question.id}>
              <p className="text-md break-words">{question.question}</p>
              {question.options.length > 0 && (
                <p className="text-muted-foreground text-xs break-words">
                  {question.options.map((option) => option.label).join(" · ")}
                </p>
              )}
            </li>
          ))}
        </ul>
      );
    case "plan":
      return (
        <p className="text-muted-foreground text-md break-words">
          The engine proposed a plan. Accepting moves the session to{" "}
          <span className="font-mono">{approval.kind.proposed_mode}</span>.
        </p>
      );
  }
}

function ApprovalTimes({
  requestedAt,
  decidedAt,
}: {
  requestedAt: string;
  decidedAt?: string;
}) {
  const requested = formatMessageTimestamp(requestedAt, new Date());
  const decided = decidedAt
    ? formatMessageTimestamp(decidedAt, new Date())
    : null;
  if (!requested && !decided) return null;
  return (
    <p className="text-muted-foreground text-xs">
      {requested && (
        <WithTooltip label={requested.full}>
          <time dateTime={requestedAt}>{requested.short}</time>
        </WithTooltip>
      )}
      {requested && decided && decidedAt && " · "}
      {decided && decidedAt && (
        <WithTooltip label={decided.full}>
          <time dateTime={decidedAt}>{decided.short}</time>
        </WithTooltip>
      )}
    </p>
  );
}

function Reveal({ open, children }: { open: boolean; children: ReactNode }) {
  return (
    <div
      className={cn(
        "grid [overflow-anchor:none] transition-[grid-template-rows] duration-[140ms] ease-out motion-reduce:transition-none",
        open ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
      )}
      aria-hidden={!open}
      inert={!open ? true : undefined}
    >
      <div className="overflow-hidden">{children}</div>
    </div>
  );
}

function approvalTitle(approval: CodeApprovalSnapshot): string {
  switch (approval.kind.type) {
    case "file_write":
      return "Write this file?";
    case "command":
      return "Run this command?";
    case "network":
      return "Allow this network access?";
    case "tool_use":
      return "Run this tool?";
    case "questions":
      return "Answer these questions?";
    case "plan":
      return "Approve this plan?";
    default:
      return "Allow this?";
  }
}

function otherSummary(summary: string): string {
  const trimmed = summary.trim();
  if (!trimmed || trimmed.toLowerCase() === "unknown") {
    return "The engine needs approval";
  }
  return trimmed;
}

/** Past this, the payload is a file, not something a reader scrolls. */
export const MAX_PAYLOAD_CHARS = 20_000;

/**
 * The harness payload, pretty-printed and capped.
 *
 * An engine can attach an entire file's contents to one approval. Rendering
 * that verbatim puts megabytes of text in the transcript's DOM, which the
 * reader pays for on every later render of the card and never reads. The cap
 * is stated rather than silent, so nobody mistakes the tail for the end.
 */
function prettyRaw(raw: string): string {
  let text = raw;
  try {
    text = JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    // Not JSON: show what the engine sent, under the same cap.
  }
  if (text.length <= MAX_PAYLOAD_CHARS) return text;
  const dropped = text.length - MAX_PAYLOAD_CHARS;
  return `${text.slice(0, MAX_PAYLOAD_CHARS)}\n… ${dropped.toLocaleString()} more characters not shown.`;
}

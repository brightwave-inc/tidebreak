import { useEffect, useId, useRef, useState, type ReactNode } from "react";

import type { CodeApprovalSnapshot, CodeApprovalDecision } from "../api/types";
import { oneLine, type NeedsYouQuestion } from "../needsYou";
import { UserQuestionsCard } from "../UserQuestionsCard";
import { MessageMarkdown } from "../MessageMarkdown";
import { toolPreviewPresentation } from "../ToolPreview";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import { formatMessageTimestamp } from "@/MessageFooter";
import { cn } from "@/lib/utils";
import { ScrollableContainer } from "@/ScrollableContainer";
import { actorLabel } from "./CodeSessionReducer";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { STATUS_TEXT } from "./statusTone";

/**
 * Parked engine approval. The normalized kind leads so the reader can decide;
 * the exact action or plan stays visible and the harness envelope stays behind
 * a disclosure (decision 0033). Deny opens a feedback field the agent sees.
 */
export function CodeApprovalCard({
  approval,
  deciding,
  error,
  onDecide,
  onReveal,
  canDecide = true,
}: {
  approval: CodeApprovalSnapshot;
  deciding?: boolean;
  error?: string;
  onDecide: (decision: CodeApprovalDecision, feedback?: string) => void;
  canDecide?: boolean;
  /** The reader opened the payload disclosure, which grows the card. */
  onReveal?: () => void;
}) {
  const [denying, setDenying] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [payloadOpen, setPayloadOpen] = useState(false);
  const payloadId = useId();
  const decided = approval.state !== "pending";
  const decidedBy = actorLabel(approval.actor);

  useEffect(() => {
    if (approval.state !== "pending") setDenying(false);
  }, [approval.state]);

  if (!decided && canDecide && approval.kind.type === "questions") {
    return (
      <UserQuestionsCard
        key={approval.id}
        request={{
          callId: approval.id,
          turnId: approval.turn_id,
          askedAt: approval.requested_at,
          questions: approval.kind.questions.map((question) => ({
            ...question,
            questionType: question.question_type,
            allowFreeForm: question.allow_free_form,
          })),
        }}
        working={deciding === true}
        error={error}
        allowAdditionalContext={false}
        requireAnswer
        onAnswer={(answers) => {
          if (answers.length === 0) {
            onDecide("deny", "Questions skipped.");
            return;
          }
          onDecide({
            answers: {
              answers: answers.map((answer) => ({
                question_id: answer.questionId,
                selected_option_ids: answer.selectedOptionIds,
                ...(answer.customAnswer
                  ? { custom_answer: answer.customAnswer }
                  : {}),
              })),
            },
          });
        }}
      />
    );
  }

  return (
    <section
      className="bg-background flex min-w-0 w-full flex-col gap-3 rounded-lg border p-4"
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
      <ApprovalKindBody approval={approval} onReveal={onReveal} />
      <ApprovalTimes
        requestedAt={approval.requested_at}
        decidedAt={approval.decided_at}
      />
      {/* A shared session has several contributors, so a settled card names
          who decided (decision 0086). A card with no actor was settled by the
          owner, or by an older build that recorded no one. */}
      {decided && decidedBy && (
        <p className="text-muted-foreground text-xs">Decided by {decidedBy}</p>
      )}
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
            Engine request
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
      {!decided && canDecide && !denying && (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            size="sm"
            disabled={deciding}
            onClick={() =>
              onDecide(
                approval.kind.type === "plan"
                  ? { plan_decision: { approve: true } }
                  : "approve",
              )
            }
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
      {!decided && canDecide && denying && (
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
          className={cn(STATUS_TEXT.critical, "text-xs break-words")}
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
    return (
      <p className={cn(STATUS_TEXT.ready, "shrink-0 text-xs")}>Approved</p>
    );
  }
  if (approval.state === "denied") {
    return (
      <p className={cn(STATUS_TEXT.warning, "shrink-0 text-xs")}>Denied</p>
    );
  }
  if (approval.state === "abandoned") {
    return (
      <p className="text-muted-foreground shrink-0 text-xs">Not decided</p>
    );
  }
  return null;
}

function ApprovalKindBody({
  approval,
  onReveal,
}: {
  approval: CodeApprovalSnapshot;
  onReveal?: () => void;
}) {
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
    case "plan": {
      const proposal = planProposal(approval.harness_raw_json);
      return (
        <div className="flex min-w-0 flex-col gap-3">
          {proposal && (
            <>
              <h4 className="text-md font-medium break-words">
                {proposal.title}
              </h4>
              <PlanPreview plan={proposal.plan} onReveal={onReveal} />
            </>
          )}
          <p className="text-muted-foreground text-md break-words">
            {proposal ? "Accepting" : "The engine proposed a plan. Accepting"}{" "}
            moves the session to{" "}
            <span className="font-mono">{approval.kind.proposed_mode}</span>.
          </p>
        </div>
      );
    }
  }
}

function PlanPreview({
  plan,
  onReveal,
}: {
  plan: string;
  onReveal?: () => void;
}) {
  const region = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [overflowing, setOverflowing] = useState(false);

  useEffect(() => {
    const element = region.current;
    if (!element) return;
    const measure = () => {
      setOverflowing(element.scrollHeight > element.clientHeight);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [plan, expanded]);

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div
        ref={region}
        role="region"
        aria-label="Proposed plan"
        tabIndex={0}
        className={cn(
          "bg-muted min-w-0 overflow-auto rounded-md p-3 break-words",
          !expanded && "max-h-96",
          FOCUS_RING,
        )}
      >
        <MessageMarkdown>{plan}</MessageMarkdown>
      </div>
      {(overflowing || expanded) && (
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="self-start"
          aria-expanded={expanded}
          onClick={() => {
            onReveal?.();
            setExpanded((current) => !current);
          }}
        >
          {expanded ? "Show less" : "Show full plan"}
        </Button>
      )}
    </div>
  );
}

/** The canonical PlanProposalBody lives in the approval payload, not its kind. */
function planProposal(raw: string): { title: string; plan: string } | null {
  try {
    const value: unknown = JSON.parse(raw);
    if (
      typeof value === "object" &&
      value !== null &&
      "title" in value &&
      typeof value.title === "string" &&
      value.title.trim().length > 0 &&
      "plan" in value &&
      typeof value.plan === "string" &&
      value.plan.trim().length > 0
    ) {
      return { title: value.title, plan: value.plan };
    }
  } catch {
    // Older native approvals may carry an unstructured harness payload.
  }
  return null;
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
      if (approval.kind.preview.tool === "code_session") {
        return approval.kind.preview.operation === "create"
          ? "Start this repository work?"
          : "Send this follow-up?";
      }
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

/**
 * A parked approval as the one question it asks, for a notification or a
 * screen reader: the card's title, then the action the card shows under it.
 * A question card asks its first question, and a plan names its title.
 */
export function codeApprovalQuestion(
  approval: CodeApprovalSnapshot,
): NeedsYouQuestion {
  const title = approvalTitle(approval);
  const withAction = (action: string): NeedsYouQuestion => {
    const line = oneLine(action);
    return {
      kind: "approval",
      text: oneLine(line ? `${title} ${line}` : title),
    };
  };
  switch (approval.kind.type) {
    case "questions":
      return {
        kind: "question",
        text: oneLine(approval.kind.questions[0]?.question || title),
      };
    case "plan":
      return {
        kind: "plan",
        text: oneLine(planProposal(approval.harness_raw_json)?.title ?? title),
      };
    case "command":
      return withAction(approval.kind.cmd);
    case "file_write":
      return approval.kind.paths.length > 1
        ? {
            kind: "approval",
            text: `Write these ${approval.kind.paths.length} files?`,
          }
        : withAction(approval.kind.paths[0] ?? "");
    case "tool_use":
      return withAction(
        toolPreviewPresentation(approval.kind.preview).headline,
      );
    case "network":
    case "other":
      return withAction(otherSummary(approval.kind.summary));
  }
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

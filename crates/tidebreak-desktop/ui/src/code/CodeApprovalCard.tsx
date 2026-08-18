import { useEffect, useState, type ReactNode } from "react";

import type { CodeApprovalSnapshot } from "../api/types";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import { formatMessageTimestamp } from "@/MessageFooter";
import { cn } from "@/lib/utils";
import { ScrollableContainer } from "@/ScrollableContainer";

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
}: {
  approval: CodeApprovalSnapshot;
  deciding?: boolean;
  error?: string;
  onDecide: (decision: "approve" | "deny", feedback?: string) => void;
}) {
  const [denying, setDenying] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [payloadOpen, setPayloadOpen] = useState(false);
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
        <h3 className="text-[13.5px] font-medium break-words">
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
        <p className="text-[13.5px] break-words">{approval.feedback}</p>
      )}
      <div>
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground text-[11px]"
          aria-expanded={payloadOpen}
          onClick={() => setPayloadOpen((current) => !current)}
        >
          Harness payload
        </button>
        <Reveal open={payloadOpen}>
          <ScrollableContainer className="bg-muted text-muted-foreground mt-2 max-h-48 rounded-md p-3 text-[11px] break-words whitespace-pre-wrap">
            <pre className="font-mono">{prettyRaw(approval.harness_raw_json)}</pre>
          </ScrollableContainer>
        </Reveal>
      </div>
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
        <p className="text-critical-foreground text-[11px] break-words" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

function ApprovalState({ approval }: { approval: CodeApprovalSnapshot }) {
  if (approval.state === "approved") {
    return (
      <p className="text-success-foreground shrink-0 text-[11px]">Approved</p>
    );
  }
  if (approval.state === "denied") {
    return (
      <p className="text-warning-foreground shrink-0 text-[11px]">Denied</p>
    );
  }
  return null;
}

function ApprovalKindBody({ approval }: { approval: CodeApprovalSnapshot }) {
  switch (approval.kind.type) {
    case "command":
      return (
        <div className="flex flex-col gap-1">
          <pre className="bg-muted overflow-x-auto rounded-md p-2 font-mono text-[13.5px] break-words whitespace-pre-wrap">
            {approval.kind.cmd || "Command"}
          </pre>
          {approval.kind.cwd && (
            <p className="text-muted-foreground font-mono text-[11px] break-words">
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
              <MiddleTruncate text={path} className="font-mono text-[13.5px]" />
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-muted-foreground text-[13.5px]">File write</p>
      );
    case "network":
    case "other":
      return (
        <p className="text-muted-foreground text-[13.5px] break-words">
          {approval.kind.summary}
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
    <p className="text-muted-foreground text-[11px]">
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

function MiddleTruncate({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  const tail = Math.min(28, Math.max(12, Math.ceil(text.length / 3)));
  if (text.length <= 40) {
    return (
      <span className={cn("block truncate", className)} title={text}>
        {text}
      </span>
    );
  }
  return (
    <span className={cn("flex min-w-0", className)} title={text}>
      <span className="truncate">{text.slice(0, -tail)}</span>
      <span className="shrink-0">{text.slice(-tail)}</span>
    </span>
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
    default:
      return "Allow this?";
  }
}

function prettyRaw(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

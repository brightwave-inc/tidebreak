import { useEffect, useState } from "react";

import type { CodeApprovalSnapshot } from "../api/types";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { WithTooltip } from "@/components/ui/tooltip";
import { formatMessageTimestamp } from "@/MessageFooter";
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
      <h3 className="font-medium break-words">{approvalTitle(approval)}</h3>
      <ApprovalKindBody approval={approval} />
      <ApprovalTimes
        requestedAt={approval.requested_at}
        decidedAt={approval.decided_at}
      />
      {approval.state === "denied" && (
        <p className="text-warning-foreground text-sm break-words">
          Denied
          {approval.feedback ? `: ${approval.feedback}` : ""}
        </p>
      )}
      {approval.state === "approved" && (
        <p className="text-success-foreground text-sm">Approved</p>
      )}
      <details>
        <summary className="text-muted-foreground hover:text-foreground cursor-pointer select-none text-xs">
          Harness payload
        </summary>
        <ScrollableContainer className="bg-muted text-muted-foreground mt-2 max-h-48 rounded-md p-3 text-xs break-words whitespace-pre-wrap">
          <pre className="font-mono">{prettyRaw(approval.harness_raw_json)}</pre>
        </ScrollableContainer>
      </details>
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
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

function ApprovalKindBody({ approval }: { approval: CodeApprovalSnapshot }) {
  switch (approval.kind.type) {
    case "command":
      return (
        <div className="flex flex-col gap-1">
          <pre className="bg-muted text-muted-foreground overflow-x-auto rounded-md p-2 font-mono text-xs break-words whitespace-pre-wrap">
            {approval.kind.cmd || "Command"}
          </pre>
          {approval.kind.cwd && (
            <p className="text-muted-foreground text-xs break-words">
              cwd {approval.kind.cwd}
            </p>
          )}
        </div>
      );
    case "file_write":
      return approval.kind.paths.length > 0 ? (
        <ul className="text-muted-foreground space-y-0.5 font-mono text-xs break-words">
          {approval.kind.paths.map((path) => (
            <li key={path}>{path}</li>
          ))}
        </ul>
      ) : (
        <p className="text-muted-foreground text-sm">File write</p>
      );
    case "network":
    case "other":
      return (
        <p className="text-muted-foreground text-sm break-words">
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

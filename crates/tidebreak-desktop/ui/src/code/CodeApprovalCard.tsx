import { useEffect, useState } from "react";

import type { CodeApprovalSnapshot } from "../api/types";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { ScrollableContainer } from "@/ScrollableContainer";

/**
 * Parked engine approval. The card shows the harness payload, not a
 * Tidebreak paraphrase; deny opens a feedback field the model will see.
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
      <p className="text-muted-foreground text-sm break-words">
        {approvalSummary(approval)}
      </p>
      <ScrollableContainer className="bg-muted text-muted-foreground max-h-48 rounded-md p-3 text-xs break-words whitespace-pre-wrap">
        <pre className="font-mono">{prettyRaw(approval.harness_raw_json)}</pre>
      </ScrollableContainer>
      {approval.state === "denied" && approval.feedback && (
        <p className="text-muted-foreground text-sm">
          Denied: {approval.feedback}
        </p>
      )}
      {approval.state === "approved" && (
        <p className="text-muted-foreground text-sm">Approved</p>
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
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}
    </section>
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

function approvalSummary(approval: CodeApprovalSnapshot): string {
  switch (approval.kind.type) {
    case "file_write":
      return approval.kind.paths.join(", ") || "File write";
    case "command":
      return approval.kind.cmd || "Command";
    case "network":
      return approval.kind.summary;
    default:
      return approval.kind.summary;
  }
}

function prettyRaw(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

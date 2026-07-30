import { useState } from "react";
import { CornerDownLeft, NotebookPen, X } from "lucide-react";
import type { PendingPlanApproval, PlanDecision } from "./api";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { MessageMarkdown } from "./MessageMarkdown";

/**
 * The decision card for a plan the agent proposed from plan mode.
 *
 * Approving is the mode hand-off: the server moves the chat out of plan mode
 * and the resumed turn executes with its full tool surface, so the primary
 * action says exactly that. "Request changes" keeps the chat in plan mode and
 * sends the feedback back to be revised.
 */
export function PlanApprovalCard({
  request,
  working,
  error,
  onDecide,
  onCancel,
}: {
  request: PendingPlanApproval;
  working: boolean;
  error: string | undefined;
  onDecide: (decision: PlanDecision) => void;
  onCancel: () => void;
}) {
  const [revising, setRevising] = useState(false);
  const [feedback, setFeedback] = useState("");

  return (
    <section
      className="bg-background flex w-full max-w-prose flex-col gap-3 rounded-[20px] border p-3.5 shadow-sm"
      aria-labelledby={`plan-${request.callId}`}
      aria-busy={working}
    >
      <div className="flex items-start gap-2.5">
        <div className="min-w-0 flex-1">
          <p className="text-muted-foreground flex items-center gap-1.5 text-xs font-semibold tracking-wide uppercase">
            <NotebookPen aria-hidden="true" className="size-3.5" />
            Proposed plan
          </p>
          <h3
            id={`plan-${request.callId}`}
            className="mt-1 text-[15px] leading-5 font-semibold break-words"
          >
            {request.title}
          </h3>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          disabled={working}
          onClick={onCancel}
          aria-label="Cancel turn"
          title="Cancel turn"
          className="text-muted-foreground shrink-0"
        >
          <X aria-hidden="true" />
        </Button>
      </div>

      <div className="max-h-80 overflow-y-auto rounded-[10px] border px-3 py-2 text-sm">
        <MessageMarkdown>{request.plan}</MessageMarkdown>
      </div>

      {revising && (
        <Textarea
          maxLength={4000}
          rows={3}
          value={feedback}
          onChange={(event) => setFeedback(event.target.value)}
          disabled={working}
          aria-label="What should change"
          placeholder="What should change?"
          autoFocus
          className="min-h-16 resize-y rounded-[10px] py-2.5 text-sm focus-visible:border-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
        />
      )}

      {error && (
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}

      <div className="flex items-center justify-end gap-2 pt-0.5">
        {revising ? (
          <>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => setRevising(false)}
            >
              Back
            </Button>
            <Button
              type="button"
              size="sm"
              disabled={working}
              onClick={() =>
                onDecide(
                  feedback.trim()
                    ? { decision: "reject", feedback: feedback.trim() }
                    : { decision: "reject" },
                )
              }
            >
              {working ? "Sending…" : "Send back for changes"}
            </Button>
          </>
        ) : (
          <>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => setRevising(true)}
            >
              Request changes
            </Button>
            <Button
              type="button"
              size="sm"
              disabled={working}
              onClick={() => onDecide({ decision: "accept" })}
            >
              {working ? "Sending…" : "Approve and run"}
              {!working && <CornerDownLeft aria-hidden="true" />}
            </Button>
          </>
        )}
      </div>
    </section>
  );
}

import { useState } from "react";
import type {
  PendingPlanApproval,
  PendingUserQuestions,
  PlanDecision,
  UserQuestionAnswer,
} from "./api";
import { cn } from "@/lib/utils";
import { isolatedCard } from "./PendingCard";
import { PlanApprovalCard } from "./PlanApprovalCard";
import { UserQuestionsCard } from "./UserQuestionsCard";

/**
 * The prompts a turn parks on the reader, standing where the composer stands.
 *
 * A question or a proposed plan is not a message in the transcript; it is the
 * turn asking for one thing back, and until it gets it there is nothing else to
 * send. Rendered inline the card scrolls away under a composer that keeps
 * inviting a reply the turn will not read. Taking the composer's slot puts the
 * decision where the hands already are and makes what is being asked for the
 * only thing on offer.
 *
 * Cancelling is still reachable throughout: every card carries its own way out
 * (skip the questions, cancel the turn), which is what returns the composer.
 */
export function ComposerPrompt({
  userQuestionRequests,
  answeringQuestionCalls,
  userQuestionErrors,
  onAnswerUserQuestions,
  planApprovalRequests,
  decidingPlanCalls,
  planApprovalErrors,
  onPlanDecision,
  onPlanCancel,
}: {
  userQuestionRequests: PendingUserQuestions[];
  answeringQuestionCalls: Set<string>;
  userQuestionErrors: Record<string, string>;
  onAnswerUserQuestions: (
    callId: string,
    answers: UserQuestionAnswer[],
    additionalUserContext?: string,
  ) => void;
  planApprovalRequests: PendingPlanApproval[];
  decidingPlanCalls: Set<string>;
  planApprovalErrors: Record<string, string>;
  onPlanDecision: (callId: string, decision: PlanDecision) => void;
  onPlanCancel: (turnId: string) => void;
}) {
  // A plan card can take the whole pane; the flag lives here because the
  // wrapper that has to grow is the one this component owns.
  const [fullscreen, setFullscreen] = useState(false);
  return (
    // Wider than the composer it replaces: a plan is a document, and the extra
    // measure is what keeps it from becoming a scroller inside a scroller.
    <div className="mx-auto flex w-full max-w-4xl flex-col gap-2">
      {userQuestionRequests.map((request) =>
        isolatedCard(
          `user-questions-${request.callId}`,
          `${answeringQuestionCalls.has(request.callId)} ${userQuestionErrors[request.callId] ?? ""}`,
          <div className="border-border bg-background relative rounded-xl shadow-lg">
            <UserQuestionsCard
              request={request}
              working={answeringQuestionCalls.has(request.callId)}
              error={userQuestionErrors[request.callId]}
              onAnswer={(answers, additionalUserContext) =>
                onAnswerUserQuestions(
                  request.callId,
                  answers,
                  additionalUserContext,
                )
              }
            />
          </div>,
          request.callId,
        ),
      )}
      {planApprovalRequests.map((request) =>
        isolatedCard(
          `plan-approval-${request.callId}`,
          `${decidingPlanCalls.has(request.callId)} ${planApprovalErrors[request.callId] ?? ""}`,
          <div
            className={cn(
              "border-border bg-background relative rounded-xl shadow-lg",
              fullscreen &&
                "absolute inset-4 top-22 mx-auto max-w-4xl border shadow-none",
            )}
          >
            <div className={cn(fullscreen && "h-full")}>
              <PlanApprovalCard
                request={request}
                working={decidingPlanCalls.has(request.callId)}
                error={planApprovalErrors[request.callId]}
                onDecide={(decision) =>
                  onPlanDecision(request.callId, decision)
                }
                onCancel={() => onPlanCancel(request.turnId)}
                onFullscreenChange={setFullscreen}
              />
            </div>
          </div>,
          request.callId,
        ),
      )}
    </div>
  );
}

import type { AnsweredUserQuestion } from "./api";
import { Separator } from "@/components/ui/separator";

/**
 * What a parked question round was answered with, once it has settled.
 *
 * The turn stopped to ask, so the transcript has to say what it was told —
 * otherwise reopening a chat shows a gap where a decision was made. Labels
 * rather than option ids: this is read by the person who chose them.
 */
export function UserQuestionsResultCard({
  answers,
  additionalContext,
}: {
  answers: AnsweredUserQuestion[];
  additionalContext: string | null;
}) {
  const allSkipped = answers.every(
    (answer) => answer.selected.length === 0 && !answer.customAnswer,
  );

  return (
    <section className="bg-background rounded-lg border p-6">
      {allSkipped ? (
        <p className="text-muted-foreground text-sm">
          All questions were skipped.
        </p>
      ) : (
        <div className="space-y-2">
          {answers.map((answer, index) => {
            const chosen = [
              ...answer.selected,
              ...(answer.customAnswer ? [answer.customAnswer] : []),
            ];
            return (
              <div key={index} className="space-y-1">
                <h4 className="text-sm font-medium break-words">
                  {index + 1}. {answer.question}
                </h4>
                {chosen.length === 0 ? (
                  <p className="text-muted-foreground text-sm">Skipped</p>
                ) : (
                  <p className="text-muted-foreground text-sm break-words italic">
                    {chosen.join(", ")}
                  </p>
                )}
              </div>
            );
          })}
        </div>
      )}
      {additionalContext && (
        <>
          <Separator className="my-3" />
          <div className="space-y-1">
            <h4 className="text-sm font-medium">Additional context</h4>
            <p className="text-muted-foreground text-sm break-words italic">
              {additionalContext}
            </p>
          </div>
        </>
      )}
    </section>
  );
}

import { Compass } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { MessageMarkdown } from "./MessageMarkdown";

/**
 * The plan as it was proposed, with what the reader decided about it.
 *
 * Accepting a plan is the moment the chat left plan mode, and revising it is
 * why the next pass looks different — both are worth keeping in the transcript
 * rather than collapsing to a settled line on the rail.
 */
export function PlanDecisionResultCard({
  title,
  plan,
  accepted,
  feedback,
}: {
  title: string;
  plan: string;
  accepted: boolean;
  feedback: string | null;
}) {
  return (
    <section className="bg-background rounded-lg border p-4">
      <div className="mb-2 flex items-start gap-2">
        <div className="py-1">
          <Compass
            aria-hidden="true"
            className="text-muted-foreground size-4 shrink-0"
          />
        </div>
        <h3 className="min-w-0 font-medium break-words">{title}</h3>
        <Badge variant={accepted ? "success" : "warning"}>
          {accepted ? "Accepted" : "Revised"}
        </Badge>
      </div>

      <div className="max-h-[300px] max-w-none overflow-y-auto text-sm">
        <MessageMarkdown>{plan}</MessageMarkdown>
      </div>

      {!accepted && feedback && (
        <>
          <Separator className="my-4" />
          <div>
            <h4 className="mb-2 text-sm font-medium">Revision feedback</h4>
            <div className="text-muted-foreground text-sm">
              <MessageMarkdown>{feedback}</MessageMarkdown>
            </div>
          </div>
        </>
      )}
    </section>
  );
}

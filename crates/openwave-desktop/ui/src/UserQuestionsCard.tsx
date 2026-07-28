import { useMemo, useState } from "react";
import type { KeyboardEvent } from "react";
import type { PendingUserQuestions, UserQuestionAnswer } from "./api";
import { AttentionCard } from "./AttentionCard";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

type DraftAnswer =
  | { kind: "option"; value: string }
  | { kind: "free_form"; value: string };

export function UserQuestionsCard({
  request,
  working,
  error,
  onAnswer,
  onCancel,
}: {
  request: PendingUserQuestions;
  working: boolean;
  error: string | undefined;
  onAnswer: (answers: UserQuestionAnswer[]) => void;
  onCancel: () => void;
}) {
  const [drafts, setDrafts] = useState<Record<string, DraftAnswer>>({});
  const answers = useMemo(
    () =>
      request.questions.flatMap<UserQuestionAnswer>((question) => {
        const draft = drafts[question.id];
        if (!draft || !draft.value.trim()) return [];
        return [
          draft.kind === "option"
            ? { questionId: question.id, optionId: draft.value }
            : { questionId: question.id, freeForm: draft.value.trim() },
        ];
      }),
    [drafts, request.questions],
  );
  const complete = answers.length === request.questions.length;

  const chooseOption = (questionId: string, optionId: string) =>
    setDrafts((current) => ({
      ...current,
      [questionId]: { kind: "option", value: optionId },
    }));

  return (
    <AttentionCard
      title="A few details"
      titleId={`questions-${request.callId}`}
      busy={working}
      error={error}
    >
      <div className="flex flex-col gap-5">
        {request.questions.map((question) => {
          const draft = drafts[question.id];
          return (
            <fieldset
              className="flex min-w-0 flex-col gap-2 border-0 p-0"
              key={question.id}
              disabled={working}
            >
              <legend className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
                {question.header}
              </legend>
              <p className="break-words">{question.question}</p>
              {question.options.length > 0 && (
                <div
                  role="radiogroup"
                  aria-label={question.header}
                  className="flex flex-col gap-0.5"
                >
                  {question.options.map((option) => {
                    const selected =
                      draft?.kind === "option" && draft.value === option.id;
                    const labelId = `${request.callId}-${question.id}-${option.id}-label`;
                    const descId = option.description
                      ? `${request.callId}-${question.id}-${option.id}-desc`
                      : undefined;
                    const onKeyDown = (
                      event: KeyboardEvent<HTMLDivElement>,
                    ) => {
                      if (event.key === "Enter" || event.key === " ") {
                        event.preventDefault();
                        chooseOption(question.id, option.id);
                      }
                    };
                    return (
                      <div
                        key={option.id}
                        role="radio"
                        aria-checked={selected}
                        aria-labelledby={labelId}
                        aria-describedby={descId}
                        tabIndex={working ? -1 : 0}
                        onClick={() => chooseOption(question.id, option.id)}
                        onKeyDown={onKeyDown}
                        className={cn(
                          "focus-visible:ring-ring flex cursor-pointer flex-col gap-0.5 rounded-md px-3 py-2 text-sm outline-hidden focus-visible:ring-2",
                          selected ? "bg-muted" : "hover:bg-muted/60",
                          working && "pointer-events-none opacity-60",
                        )}
                      >
                        <span id={labelId} className="font-medium">
                          {option.label}
                        </span>
                        {option.description && (
                          <span
                            id={descId}
                            className="text-muted-foreground text-xs"
                          >
                            {option.description}
                          </span>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
              {question.allowFreeForm && (
                <label className="flex flex-col gap-1">
                  <span className="text-muted-foreground text-sm">
                    {question.options.length > 0
                      ? "Something else"
                      : "Your answer"}
                  </span>
                  <textarea
                    className="border-border bg-background ring-offset-background placeholder:text-muted-foreground focus-visible:ring-ring w-full resize-y rounded-md border px-3 py-2 text-sm outline-hidden focus-visible:ring-2 focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                    maxLength={2000}
                    rows={3}
                    value={draft?.kind === "free_form" ? draft.value : ""}
                    onChange={(event) =>
                      setDrafts((current) => ({
                        ...current,
                        [question.id]: {
                          kind: "free_form",
                          value: event.target.value,
                        },
                      }))
                    }
                  />
                </label>
              )}
            </fieldset>
          );
        })}
      </div>
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          disabled={working || !complete}
          onClick={() => onAnswer(answers)}
        >
          {working ? "Sending…" : "Continue"}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={working}
          onClick={onCancel}
        >
          Cancel turn
        </Button>
      </div>
    </AttentionCard>
  );
}

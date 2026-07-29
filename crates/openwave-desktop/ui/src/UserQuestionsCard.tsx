import { useMemo, useState } from "react";
import { CornerDownLeft, X } from "lucide-react";
import type { PendingUserQuestions, UserQuestionAnswer } from "./api";
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
  const [currentPage, setCurrentPage] = useState(0);
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
  const currentQuestion =
    request.questions[Math.min(currentPage, request.questions.length - 1)];
  if (!currentQuestion) return null;
  const currentDraft = drafts[currentQuestion.id];
  const isLastPage = currentPage === request.questions.length - 1;

  const chooseOption = (questionId: string, optionId: string) =>
    setDrafts((current) => ({
      ...current,
      [questionId]: { kind: "option", value: optionId },
    }));

  const updateFreeForm = (questionId: string, value: string) =>
    setDrafts((current) => ({
      ...current,
      [questionId]: { kind: "free_form", value },
    }));

  return (
    <section
      className="bg-background flex w-full max-w-prose flex-col gap-3 rounded-[20px] border p-3.5 shadow-sm"
      aria-labelledby={`questions-${request.callId}`}
      aria-busy={working}
    >
      <div className="flex items-start gap-2.5">
        <div className="min-w-0 flex-1">
          <p className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
            {currentQuestion.header}
          </p>
          <h3
            id={`questions-${request.callId}`}
            className="mt-1 text-[15px] leading-5 font-semibold break-words"
          >
            {currentQuestion.question}
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

      {currentQuestion.options.length > 0 && (
        <div
          role="radiogroup"
          aria-label={currentQuestion.header}
          className="grid gap-1"
        >
          {currentQuestion.options.map((option) => {
            const selected =
              currentDraft?.kind === "option" &&
              currentDraft.value === option.id;
            return (
              <label
                key={option.id}
                className={cn(
                  "hover:bg-muted/40 flex min-w-0 cursor-pointer items-start gap-2.5 rounded-[10px] border px-2.5 py-2.5 transition-colors",
                  selected && "border-foreground bg-muted",
                  working && "pointer-events-none opacity-60",
                )}
              >
                <input
                  type="radio"
                  name={`${request.callId}-${currentQuestion.id}`}
                  checked={selected}
                  disabled={working}
                  onChange={() =>
                    chooseOption(currentQuestion.id, option.id)
                  }
                  className="accent-foreground mt-0.5 size-[18px] shrink-0"
                />
                <span className="min-w-0">
                  <span className="block text-sm font-medium break-words">
                    {option.label}
                  </span>
                  {option.description && (
                    <span className="text-muted-foreground mt-0.5 block text-xs leading-4 break-words">
                      {option.description}
                    </span>
                  )}
                </span>
              </label>
            );
          })}
          {currentQuestion.allowFreeForm && (
            <label
              className={cn(
                "flex min-w-0 items-center gap-2.5 px-1 py-1.5",
                working && "pointer-events-none opacity-60",
              )}
            >
              <input
                type="radio"
                name={`${request.callId}-${currentQuestion.id}`}
                checked={currentDraft?.kind === "free_form"}
                disabled={working}
                onChange={() => updateFreeForm(currentQuestion.id, "")}
                className="accent-foreground size-[18px] shrink-0"
              />
              <input
                type="text"
                maxLength={2000}
                value={
                  currentDraft?.kind === "free_form" ? currentDraft.value : ""
                }
                onFocus={() => {
                  if (currentDraft?.kind !== "free_form") {
                    updateFreeForm(currentQuestion.id, "");
                  }
                }}
                onChange={(event) =>
                  updateFreeForm(currentQuestion.id, event.target.value)
                }
                disabled={working}
                aria-label="Other answer"
                placeholder="Other"
                className="border-border bg-background placeholder:text-muted-foreground focus-visible:border-foreground min-w-0 flex-1 rounded-lg border px-2.5 py-2 text-sm outline-hidden"
              />
            </label>
          )}
        </div>
      )}

      {currentQuestion.allowFreeForm &&
        currentQuestion.options.length === 0 && (
          <textarea
            maxLength={2000}
            rows={3}
            value={
              currentDraft?.kind === "free_form" ? currentDraft.value : ""
            }
            onChange={(event) =>
              updateFreeForm(currentQuestion.id, event.target.value)
            }
            disabled={working}
            aria-label={currentQuestion.header}
            placeholder="Your answer"
            className="border-border bg-background placeholder:text-muted-foreground focus-visible:border-foreground min-h-16 w-full resize-y rounded-[10px] border px-3 py-2.5 text-sm outline-hidden disabled:cursor-not-allowed disabled:opacity-50"
          />
        )}

      {request.questions.length > 1 && (
        <div
          className="flex items-center justify-center gap-1.5 pt-0.5"
          aria-label="Questions"
        >
          {request.questions.map((question, index) => (
            <button
              key={question.id}
              type="button"
              disabled={working}
              onClick={() => setCurrentPage(index)}
              aria-label={`Go to question ${index + 1}`}
              aria-current={index === currentPage ? "step" : undefined}
              className={cn(
                "size-1.5 rounded-full transition-opacity",
                index === currentPage
                  ? "bg-foreground"
                  : "bg-muted-foreground opacity-40 hover:opacity-70",
              )}
            />
          ))}
        </div>
      )}

      {error && (
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}

      <div className="flex items-center justify-between gap-2 pt-0.5">
        <div>
          {currentPage > 0 && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={working}
              onClick={() => setCurrentPage((page) => page - 1)}
            >
              Back
            </Button>
          )}
        </div>
        {isLastPage ? (
          <Button
            type="button"
            size="sm"
            disabled={working || !complete}
            onClick={() => onAnswer(answers)}
          >
            {working ? "Sending…" : "Continue"}
            {!working && <CornerDownLeft aria-hidden="true" />}
          </Button>
        ) : (
          <Button
            type="button"
            size="sm"
            disabled={working}
            onClick={() => setCurrentPage((page) => page + 1)}
          >
            Next
          </Button>
        )}
      </div>
    </section>
  );
}

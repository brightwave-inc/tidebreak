import { useMemo, useState } from "react";
import { CornerDownLeft, X } from "lucide-react";
import type { PendingUserQuestions, UserQuestionAnswer } from "./api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

type DraftAnswer = {
  selectedOptionIds: string[];
  customAnswer?: string;
};

export function UserQuestionsCard({
  request,
  working,
  error,
  onAnswer,
}: {
  request: PendingUserQuestions;
  working: boolean;
  error: string | undefined;
  onAnswer: (
    answers: UserQuestionAnswer[],
    additionalUserContext?: string,
  ) => void;
}) {
  const [drafts, setDrafts] = useState<Record<string, DraftAnswer>>({});
  const [currentPage, setCurrentPage] = useState(0);
  const [additionalContext, setAdditionalContext] = useState("");
  const answers = useMemo(
    () =>
      request.questions.flatMap<UserQuestionAnswer>((question) => {
        const draft = drafts[question.id];
        if (!draft) return [];
        const customAnswer = draft.customAnswer?.trim();
        if (draft.selectedOptionIds.length === 0 && !customAnswer) return [];
        return [
          {
            questionId: question.id,
            selectedOptionIds: draft.selectedOptionIds,
            ...(customAnswer ? { customAnswer } : {}),
          },
        ];
      }),
    [drafts, request.questions],
  );
  const currentQuestion =
    request.questions[Math.min(currentPage, request.questions.length - 1)];
  if (!currentQuestion) return null;
  const currentDraft = drafts[currentQuestion.id] ?? {
    selectedOptionIds: [],
  };
  const isMultiSelect = currentQuestion.questionType === "multi_select";
  const isLastPage = currentPage === request.questions.length - 1;

  const chooseSingleOption = (questionId: string, optionId: string) =>
    setDrafts((current) => ({
      ...current,
      [questionId]: { selectedOptionIds: [optionId] },
    }));

  const toggleOption = (questionId: string, optionId: string) =>
    setDrafts((current) => {
      const draft = current[questionId] ?? { selectedOptionIds: [] };
      return {
        ...current,
        [questionId]: {
          ...draft,
          selectedOptionIds: draft.selectedOptionIds.includes(optionId)
            ? draft.selectedOptionIds.filter((id) => id !== optionId)
            : [...draft.selectedOptionIds, optionId],
        },
      };
    });

  const updateCustomAnswer = (
    questionId: string,
    value: string | undefined,
  ) =>
    setDrafts((current) => {
      const draft = current[questionId] ?? { selectedOptionIds: [] };
      return {
        ...current,
        [questionId]: {
          selectedOptionIds: isMultiSelect ? draft.selectedOptionIds : [],
          ...(value === undefined ? {} : { customAnswer: value }),
        },
      };
    });

  const submitAnswers = () => {
    const context = additionalContext.trim();
    onAnswer(answers, context || undefined);
  };

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
            {isMultiSelect && " Select all that apply."}
          </h3>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          disabled={working}
          onClick={() => onAnswer([])}
          aria-label="Skip questions"
          title="Skip questions"
          className="text-muted-foreground shrink-0"
        >
          <X aria-hidden="true" />
        </Button>
      </div>

      {currentQuestion.options.length > 0 && (
        <div
          role={isMultiSelect ? "group" : "radiogroup"}
          aria-label={currentQuestion.header}
          className="grid gap-1"
        >
          {currentQuestion.options.map((option) => {
            const selected = currentDraft.selectedOptionIds.includes(option.id);
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
                  type={isMultiSelect ? "checkbox" : "radio"}
                  name={`${request.callId}-${currentQuestion.id}`}
                  checked={selected}
                  disabled={working}
                  onChange={() =>
                    isMultiSelect
                      ? toggleOption(currentQuestion.id, option.id)
                      : chooseSingleOption(currentQuestion.id, option.id)
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
                type={isMultiSelect ? "checkbox" : "radio"}
                name={`${request.callId}-${currentQuestion.id}`}
                checked={currentDraft.customAnswer !== undefined}
                disabled={working}
                onChange={(event) =>
                  updateCustomAnswer(
                    currentQuestion.id,
                    event.target.checked
                      ? (currentDraft.customAnswer ?? "")
                      : undefined,
                  )
                }
                className="accent-foreground size-[18px] shrink-0"
              />
              <Input
                type="text"
                maxLength={2000}
                value={currentDraft.customAnswer ?? ""}
                onFocus={() => {
                  if (currentDraft.customAnswer === undefined) {
                    updateCustomAnswer(currentQuestion.id, "");
                  }
                }}
                onChange={(event) =>
                  updateCustomAnswer(currentQuestion.id, event.target.value)
                }
                disabled={working}
                aria-label="Other answer"
                placeholder="Other"
                className="h-auto min-w-0 flex-1 rounded-lg px-2.5 py-2 text-sm focus-visible:border-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
              />
            </label>
          )}
        </div>
      )}

      {currentQuestion.allowFreeForm &&
        currentQuestion.options.length === 0 && (
          <Textarea
            maxLength={2000}
            rows={3}
            value={currentDraft.customAnswer ?? ""}
            onChange={(event) =>
              updateCustomAnswer(currentQuestion.id, event.target.value)
            }
            disabled={working}
            aria-label={currentQuestion.header}
            placeholder="Your answer"
            className="min-h-16 resize-y rounded-[10px] py-2.5 text-sm focus-visible:border-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
          />
        )}

      <Textarea
        maxLength={2000}
        rows={2}
        value={additionalContext}
        onChange={(event) => setAdditionalContext(event.target.value)}
        disabled={working}
        aria-label="Additional context"
        placeholder="Additional context (optional)"
        className="min-h-14 resize-y rounded-[10px] py-2.5 text-sm focus-visible:border-foreground focus-visible:ring-0 focus-visible:ring-offset-0"
      />

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
            disabled={working}
            onClick={submitAnswers}
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

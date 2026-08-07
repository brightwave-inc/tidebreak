import { useCallback, useMemo, useState } from "react";
import { ChevronDown, CornerDownLeft, X } from "lucide-react";
import type { PendingUserQuestions, UserQuestionAnswer } from "./api";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";

type DraftAnswer = {
  selectedOptionIds: string[];
  /** Undefined means "Other" is not chosen; "" means chosen but still empty. */
  customAnswer?: string;
};

function QuestionOption({
  label,
  description,
  selected,
  disabled,
  onSelect,
  children,
}: {
  label: string;
  description: string | null;
  selected: boolean;
  disabled: boolean;
  onSelect: () => void;
  children: React.ReactNode;
}) {
  return (
    <div
      className={cn(
        "hover:bg-accent/50 flex cursor-pointer items-start space-x-3 rounded-md p-2 transition-colors",
        selected && "bg-accent",
        disabled && "pointer-events-none opacity-60",
      )}
      role="button"
      tabIndex={0}
      onClick={onSelect}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect();
        }
      }}
    >
      {children}
      <div className="grid min-w-0 flex-1 gap-1.5 leading-none">
        <div className="cursor-pointer text-sm leading-none font-medium break-words">
          {label}
        </div>
        {description && (
          <p className="text-muted-foreground cursor-pointer text-sm break-words">
            {description}
          </p>
        )}
      </div>
    </div>
  );
}

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
  const [showContextForm, setShowContextForm] = useState(false);
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

  const submit = useCallback(
    (context?: string) => {
      const trimmed = context?.trim();
      onAnswer(answers, trimmed || undefined);
    },
    [answers, onAnswer],
  );
  const skipAll = useCallback(() => onAnswer([]), [onAnswer]);

  const questions = request.questions;
  const currentQuestion =
    questions[Math.min(currentPage, questions.length - 1)];
  if (!currentQuestion) return null;

  const question = currentQuestion;
  const draft = drafts[question.id] ?? { selectedOptionIds: [] };
  const isMultiSelect = question.questionType === "multi_select";
  const isLastPage = currentPage === questions.length - 1;
  const otherChosen = draft.customAnswer !== undefined;

  const update = (next: DraftAnswer) =>
    setDrafts((current) => ({ ...current, [question.id]: next }));

  const chooseOption = (optionId: string) => {
    if (working) return;
    if (isMultiSelect) {
      update({
        ...draft,
        selectedOptionIds: draft.selectedOptionIds.includes(optionId)
          ? draft.selectedOptionIds.filter((id) => id !== optionId)
          : [...draft.selectedOptionIds, optionId],
      });
      return;
    }
    // Picking a listed option in a single-select drops any "Other" text, so the
    // two can never both count as the answer.
    update({ selectedOptionIds: [optionId] });
  };

  const chooseOther = () => {
    if (working) return;
    update({
      selectedOptionIds: isMultiSelect ? draft.selectedOptionIds : [],
      customAnswer: draft.customAnswer ?? "",
    });
  };

  const setCustomAnswer = (value: string) =>
    update({
      selectedOptionIds: isMultiSelect ? draft.selectedOptionIds : [],
      customAnswer: value,
    });

  const toggleOther = (checked: boolean) =>
    update({
      selectedOptionIds: draft.selectedOptionIds,
      ...(checked ? { customAnswer: draft.customAnswer ?? "" } : {}),
    });

  const errorNotice = error ? (
    <p className="text-destructive text-xs break-words" role="alert">
      {error}
    </p>
  ) : null;

  if (showContextForm) {
    return (
      <section
        className="bg-background rounded-lg border p-4"
        aria-labelledby={`questions-${request.callId}`}
        aria-busy={working}
      >
        <div className="mb-6 flex items-start justify-between gap-3">
          <h3
            id={`questions-${request.callId}`}
            className="min-w-0 font-medium"
          >
            What would you like to add or clarify?
          </h3>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            className="text-muted-foreground shrink-0"
            disabled={working}
            onClick={skipAll}
            aria-label="Skip questions"
            title="Skip questions"
          >
            <X aria-hidden="true" />
          </Button>
        </div>

        <div className="space-y-6">
          <Input
            maxLength={2000}
            placeholder="Add additional context"
            aria-label="Additional context"
            value={additionalContext}
            disabled={working}
            onChange={(event) => setAdditionalContext(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !working) {
                submit(additionalContext);
              }
            }}
            autoFocus
          />

          {errorNotice}

          <Separator />

          <div className="flex items-center justify-between">
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() => setShowContextForm(false)}
            >
              Back
            </Button>
            <Button
              type="button"
              disabled={working}
              onClick={() => submit(additionalContext)}
            >
              {working ? "Sending…" : "Continue"}
              {!working && <CornerDownLeft aria-hidden="true" />}
            </Button>
          </div>
        </div>
      </section>
    );
  }

  const otherRow = question.allowFreeForm ? (
    <div className="flex items-center space-x-3 p-2">
      {isMultiSelect ? (
        <Checkbox
          checked={otherChosen}
          disabled={working}
          onCheckedChange={(checked) => toggleOther(checked === true)}
          aria-label="Other"
        />
      ) : (
        <RadioGroupItem value="other" aria-label="Other" disabled={working} />
      )}
      <Input
        maxLength={2000}
        placeholder="Other"
        aria-label="Other answer"
        value={draft.customAnswer ?? ""}
        disabled={working}
        onChange={(event) => setCustomAnswer(event.target.value)}
        onClick={() => {
          if (!isMultiSelect && !otherChosen) chooseOther();
        }}
        className="flex-1"
      />
    </div>
  ) : null;

  return (
    <section
      className="bg-background rounded-lg border p-4"
      aria-labelledby={`questions-${request.callId}`}
      aria-busy={working}
    >
      <div className="mb-2 flex items-start justify-between gap-3">
        <h3 id={`questions-${request.callId}`} className="min-w-0 font-medium">
          {question.question}
          {isMultiSelect ? " Select all that apply." : ""}
        </h3>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="text-muted-foreground shrink-0"
          disabled={working}
          onClick={skipAll}
          aria-label="Skip questions"
          title="Skip questions"
        >
          <X aria-hidden="true" />
        </Button>
      </div>

      <div className="space-y-4">
        <div>
          {isMultiSelect ? (
            <div
              role="group"
              aria-label={question.header}
              className="flex flex-col gap-1"
            >
              {question.options.map((option) => {
                const selected = draft.selectedOptionIds.includes(option.id);
                return (
                  <QuestionOption
                    key={option.id}
                    label={option.label}
                    description={option.description}
                    selected={selected}
                    disabled={working}
                    onSelect={() => chooseOption(option.id)}
                  >
                    <Checkbox
                      checked={selected}
                      disabled={working}
                      onCheckedChange={() => chooseOption(option.id)}
                      aria-label={option.label}
                    />
                  </QuestionOption>
                );
              })}
              {otherRow}
            </div>
          ) : (
            <RadioGroup
              className="gap-1"
              aria-label={question.header}
              value={draft.selectedOptionIds[0] ?? (otherChosen ? "other" : "")}
              onValueChange={(value) =>
                value === "other" ? chooseOther() : chooseOption(value)
              }
            >
              {question.options.map((option) => (
                <QuestionOption
                  key={option.id}
                  label={option.label}
                  description={option.description}
                  selected={draft.selectedOptionIds[0] === option.id}
                  disabled={working}
                  onSelect={() => chooseOption(option.id)}
                >
                  <RadioGroupItem
                    value={option.id}
                    disabled={working}
                    aria-label={option.label}
                  />
                </QuestionOption>
              ))}
              {otherRow}
            </RadioGroup>
          )}
        </div>

        {errorNotice}

        <Separator />

        <div className="flex items-center justify-between gap-2">
          {currentPage === 0 ? (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button type="button" variant="outline" disabled={working}>
                  {questions.length === 1 ? "Skip" : "Skip all"}
                  <ChevronDown aria-hidden="true" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start">
                <DropdownMenuItem onClick={() => setShowContextForm(true)}>
                  Add your own context
                </DropdownMenuItem>
                <DropdownMenuItem onClick={skipAll}>
                  {questions.length === 1
                    ? "Skip question"
                    : "Skip these questions"}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          ) : (
            <Button
              type="button"
              variant="outline"
              disabled={working}
              onClick={() => setCurrentPage((page) => page - 1)}
            >
              Back
            </Button>
          )}

          {questions.length > 1 && (
            <div className="flex items-center gap-2">
              {questions.map((entry, index) => (
                <button
                  key={entry.id}
                  type="button"
                  disabled={working}
                  onClick={() => setCurrentPage(index)}
                  className={cn(
                    "h-2 w-2 rounded-full transition-colors",
                    index === currentPage
                      ? "bg-primary"
                      : "bg-muted-foreground/30",
                  )}
                  aria-label={`Go to question ${index + 1}`}
                  aria-current={index === currentPage ? "step" : undefined}
                />
              ))}
            </div>
          )}

          <div className="flex gap-2">
            {isLastPage ? (
              <>
                <Button
                  type="button"
                  variant="outline"
                  disabled={working}
                  onClick={() => setShowContextForm(true)}
                >
                  Continue and add context
                </Button>
                <Button
                  type="button"
                  disabled={working}
                  onClick={() => submit()}
                >
                  {working ? "Sending…" : "Continue"}
                  {!working && <CornerDownLeft aria-hidden="true" />}
                </Button>
              </>
            ) : (
              <Button
                type="button"
                variant="outline"
                disabled={working}
                onClick={() => setCurrentPage((page) => page + 1)}
              >
                Next
              </Button>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

import { useMemo, useState } from "react";
import type {
  PendingUserQuestions,
  UserQuestionAnswer,
} from "./api";

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

  return (
    <section
      className="user-questions"
      aria-labelledby={`questions-${request.callId}`}
      aria-busy={working}
    >
      <div className="folder-consent-heading">
        <div>
          <h2 id={`questions-${request.callId}`}>A few details</h2>
          <span className="status">waiting for your answer</span>
        </div>
      </div>
      <div className="user-question-list">
        {request.questions.map((question) => {
          const draft = drafts[question.id];
          return (
            <fieldset
              className="user-question"
              key={question.id}
              disabled={working}
            >
              <legend>{question.header}</legend>
              <p>{question.question}</p>
              {question.options.length > 0 && (
                <div className="user-question-options">
                  {question.options.map((option) => {
                    const inputId = `${request.callId}-${question.id}-${option.id}`;
                    return (
                      <label className="user-question-option" htmlFor={inputId} key={option.id}>
                        <input
                          id={inputId}
                          type="radio"
                          name={`${request.callId}-${question.id}`}
                          checked={
                            draft?.kind === "option" &&
                            draft.value === option.id
                          }
                          onChange={() =>
                            setDrafts((current) => ({
                              ...current,
                              [question.id]: {
                                kind: "option",
                                value: option.id,
                              },
                            }))
                          }
                        />
                        <span>
                          <strong>{option.label}</strong>
                          <small>{option.description}</small>
                        </span>
                      </label>
                    );
                  })}
                </div>
              )}
              {question.allowFreeForm && (
                <label className="user-question-free-form">
                  <span>
                    {question.options.length > 0
                      ? "Something else"
                      : "Your answer"}
                  </span>
                  <textarea
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
      <div className="folder-consent-actions">
        <button
          type="button"
          className="btn btn-primary"
          disabled={working || !complete}
          onClick={() => onAnswer(answers)}
        >
          {working ? "Sending…" : "Continue"}
        </button>
        <button type="button" className="btn" disabled={working} onClick={onCancel}>
          Cancel turn
        </button>
      </div>
      {error && (
        <p className="folder-consent-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

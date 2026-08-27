import { useMemo, useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import {
  mobileApprovalQuestion,
  mobileApprovalSummary,
  mobileToolPreviewDetail,
  type MobilePendingPlanApproval,
  type MobilePendingToolApproval,
  type MobilePendingUserQuestions,
  type MobilePlanDecision,
  type MobileUserQuestionAnswer,
} from "../lib/chatPrompts";
import { Button, Field, SectionLabel } from "./Controls";
import { ErrorText } from "./Screen";

export function ChatToolApprovalCard({
  approval,
  onDecide,
}: {
  approval: MobilePendingToolApproval;
  onDecide: (
    decision:
      | { decision: "approve" }
      | { decision: "reject"; feedback: string },
  ) => Promise<void>;
}) {
  const [rejecting, setRejecting] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [busy, setBusy] = useState<"approve" | "reject" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function decide(kind: "approve" | "reject") {
    const reason = feedback.trim();
    if (kind === "reject" && !reason) {
      setError("Tell the agent what to change before rejecting.");
      return;
    }
    setBusy(kind);
    setError(null);
    try {
      await onDecide(
        kind === "approve"
          ? { decision: "approve" }
          : { decision: "reject", feedback: reason },
      );
    } catch (reasonCaught) {
      setError(
        reasonCaught instanceof Error
          ? reasonCaught.message
          : "The approval decision could not be sent.",
      );
    } finally {
      setBusy(null);
    }
  }

  return (
    <View className="gap-3 rounded-xl border border-warning-border bg-background p-4">
      <View className="gap-1">
        <SectionLabel>Approval needed</SectionLabel>
        <Text className="text-base font-semibold text-foreground">
          {mobileApprovalQuestion(approval)}
        </Text>
        <Text className="text-sm text-muted-foreground">
          {mobileApprovalSummary(approval.approval)}
        </Text>
      </View>

      {approval.autoJudgeStatus === "judging" ? (
        <Text className="text-xs text-info-foreground">
          Tidebreak is deciding automatically. You can still decide now.
        </Text>
      ) : null}

      {approval.preview ? (
        <ScrollView
          className="max-h-48 rounded-lg border border-border bg-muted p-3"
          nestedScrollEnabled
        >
          <Text className="font-mono text-xs text-foreground" selectable>
            {mobileToolPreviewDetail(approval.preview)}
          </Text>
        </ScrollView>
      ) : (
        <View className="rounded-lg border border-border bg-muted p-3">
          <Text className="text-sm text-muted-foreground">
            No action preview is available for this request.
          </Text>
        </View>
      )}

      {rejecting ? (
        <Field
          label="Feedback for the agent"
          hint="The agent receives this with the rejection."
          value={feedback}
          onChangeText={(value) => {
            setFeedback(value);
            setError(null);
          }}
          placeholder="Explain what should change"
          maxLength={512}
          editable={busy === null}
        />
      ) : null}

      {error ? <ErrorText>{error}</ErrorText> : null}

      {rejecting ? (
        <View className="gap-2 sm:flex-row">
          <View className="flex-1">
            <Button
              label="Reject with feedback"
              variant="destructive"
              busy={busy === "reject"}
              disabled={busy !== null}
              onPress={() => void decide("reject")}
            />
          </View>
          <View className="flex-1">
            <Button
              label="Cancel"
              variant="secondary"
              disabled={busy !== null}
              onPress={() => {
                setRejecting(false);
                setError(null);
              }}
            />
          </View>
        </View>
      ) : (
        <View className="gap-2 sm:flex-row">
          {approval.canApprove ? (
            <View className="flex-1">
              <Button
                label="Approve once"
                busy={busy === "approve"}
                disabled={busy !== null}
                onPress={() => void decide("approve")}
              />
            </View>
          ) : null}
          <View className="flex-1">
            <Button
              label="Reject…"
              variant="secondary"
              disabled={busy !== null}
              onPress={() => setRejecting(true)}
            />
          </View>
        </View>
      )}
    </View>
  );
}

type DraftAnswer = {
  selectedOptionIds: string[];
  customAnswer?: string;
};

export function ChatUserQuestionsCard({
  request,
  onAnswer,
}: {
  request: MobilePendingUserQuestions;
  onAnswer: (
    answers: MobileUserQuestionAnswer[],
    additionalUserContext?: string,
  ) => Promise<void>;
}) {
  const [drafts, setDrafts] = useState<Record<string, DraftAnswer>>({});
  const [page, setPage] = useState(0);
  const [additionalContext, setAdditionalContext] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const question =
    request.questions[Math.min(page, request.questions.length - 1)];

  const answers = useMemo(
    () =>
      request.questions.flatMap<MobileUserQuestionAnswer>((entry) => {
        const draft = drafts[entry.id];
        if (!draft) return [];
        const customAnswer = draft.customAnswer?.trim();
        if (draft.selectedOptionIds.length === 0 && !customAnswer) return [];
        return [
          {
            questionId: entry.id,
            selectedOptionIds: draft.selectedOptionIds,
            ...(customAnswer ? { customAnswer } : {}),
          },
        ];
      }),
    [drafts, request.questions],
  );

  if (!question) return null;

  const questionId = question.id;
  const draft = drafts[question.id] ?? { selectedOptionIds: [] };
  const multi = question.questionType === "multi_select";
  const last = page === request.questions.length - 1;

  function update(next: DraftAnswer) {
    setDrafts((current) => ({ ...current, [questionId]: next }));
    setError(null);
  }

  function chooseOption(optionId: string) {
    if (busy) return;
    if (multi) {
      update({
        ...draft,
        selectedOptionIds: draft.selectedOptionIds.includes(optionId)
          ? draft.selectedOptionIds.filter((id) => id !== optionId)
          : [...draft.selectedOptionIds, optionId],
      });
      return;
    }
    update({ selectedOptionIds: [optionId] });
  }

  function setCustomAnswer(value: string) {
    update({
      selectedOptionIds: multi ? draft.selectedOptionIds : [],
      customAnswer: value,
    });
  }

  async function submit(skipAll = false) {
    setBusy(true);
    setError(null);
    try {
      const context = additionalContext.trim();
      await onAnswer(
        skipAll ? [] : answers,
        skipAll || !context ? undefined : context,
      );
    } catch (reason) {
      setError(
        reason instanceof Error
          ? reason.message
          : "The answers could not be sent.",
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <View className="gap-3 rounded-xl border border-border bg-background p-4">
      <View className="gap-1">
        <View className="flex-row items-center justify-between gap-3">
          <SectionLabel>{question.header}</SectionLabel>
          {request.questions.length > 1 ? (
            <Text className="text-xs text-muted-foreground">
              {page + 1} of {request.questions.length}
            </Text>
          ) : null}
        </View>
        <Text className="text-base font-semibold text-foreground">
          {question.question}
        </Text>
        {multi ? (
          <Text className="text-xs text-muted-foreground">
            Select all that apply.
          </Text>
        ) : null}
      </View>

      <View className="gap-2">
        {question.options.map((option) => {
          const selected = draft.selectedOptionIds.includes(option.id);
          return (
            <Pressable
              key={option.id}
              accessibilityRole={multi ? "checkbox" : "radio"}
              accessibilityState={{ checked: selected, disabled: busy }}
              disabled={busy}
              className={`min-h-16 flex-row items-start gap-3 rounded-xl border p-3 disabled:opacity-50 ${
                selected
                  ? "border-primary bg-muted"
                  : "border-border bg-page-background"
              }`}
              onPress={() => chooseOption(option.id)}
            >
              <Text className="w-5 text-base text-foreground">
                {multi ? (selected ? "■" : "□") : selected ? "●" : "○"}
              </Text>
              <View className="min-w-0 flex-1 gap-1">
                <Text className="text-sm font-medium text-foreground">
                  {option.label}
                </Text>
                <Text className="text-xs text-muted-foreground">
                  {option.description}
                </Text>
              </View>
            </Pressable>
          );
        })}
      </View>

      {question.allowFreeForm ? (
        <Field
          label={multi ? "Other answer (optional)" : "Other answer"}
          multiline
          value={draft.customAnswer ?? ""}
          onChangeText={setCustomAnswer}
          placeholder="Write your own answer"
          maxLength={2_000}
          editable={!busy}
        />
      ) : null}

      {last ? (
        <Field
          label="Additional context (optional)"
          multiline
          value={additionalContext}
          onChangeText={(value) => {
            setAdditionalContext(value);
            setError(null);
          }}
          placeholder="Add context that applies to all answers"
          maxLength={2_000}
          editable={!busy}
        />
      ) : null}

      {error ? <ErrorText>{error}</ErrorText> : null}

      <View className="gap-2">
        <View className="flex-row gap-2">
          {page > 0 ? (
            <View className="flex-1">
              <Button
                label="Back"
                compact
                variant="secondary"
                disabled={busy}
                onPress={() => setPage((current) => current - 1)}
              />
            </View>
          ) : null}
          <View className="flex-1">
            <Button
              label={last ? "Continue" : "Next"}
              compact
              busy={last && busy}
              disabled={busy}
              onPress={() => {
                if (last) void submit();
                else setPage((current) => current + 1);
              }}
            />
          </View>
        </View>
        <Button
          label={request.questions.length === 1 ? "Skip question" : "Skip all"}
          compact
          variant="secondary"
          disabled={busy}
          onPress={() => void submit(true)}
        />
      </View>
    </View>
  );
}

export function ChatPlanApprovalCard({
  request,
  onDecide,
}: {
  request: MobilePendingPlanApproval;
  onDecide: (decision: MobilePlanDecision) => Promise<void>;
}) {
  const [revising, setRevising] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [busy, setBusy] = useState<"accept" | "reject" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function decide(kind: "accept" | "reject") {
    const reason = feedback.trim();
    if (kind === "reject" && !reason) {
      setError("Explain what the agent should change in the plan.");
      return;
    }
    setBusy(kind);
    setError(null);
    try {
      await onDecide(
        kind === "accept"
          ? { decision: "accept" }
          : { decision: "reject", feedback: reason },
      );
    } catch (reasonCaught) {
      setError(
        reasonCaught instanceof Error
          ? reasonCaught.message
          : "The plan decision could not be sent.",
      );
    } finally {
      setBusy(null);
    }
  }

  return (
    <View className="gap-3 rounded-xl border border-border bg-background p-4">
      <View className="gap-1">
        <SectionLabel>Plan ready</SectionLabel>
        <Text className="text-base font-semibold text-foreground">
          {request.title}
        </Text>
      </View>

      <ScrollView
        className="max-h-72 rounded-lg border border-border bg-page-background p-3"
        nestedScrollEnabled
      >
        <Text className="text-sm leading-6 text-foreground" selectable>
          {request.plan}
        </Text>
      </ScrollView>

      {revising ? (
        <Field
          label="Plan feedback"
          hint="The agent revises the plan before asking again."
          multiline
          value={feedback}
          onChangeText={(value) => {
            setFeedback(value);
            setError(null);
          }}
          placeholder="Explain what should change"
          maxLength={4_000}
          editable={busy === null}
        />
      ) : null}

      {error ? <ErrorText>{error}</ErrorText> : null}

      {revising ? (
        <View className="gap-2 sm:flex-row">
          <View className="flex-1">
            <Button
              label="Send back for changes"
              busy={busy === "reject"}
              disabled={busy !== null}
              onPress={() => void decide("reject")}
            />
          </View>
          <View className="flex-1">
            <Button
              label="Cancel"
              variant="secondary"
              disabled={busy !== null}
              onPress={() => {
                setRevising(false);
                setError(null);
              }}
            />
          </View>
        </View>
      ) : (
        <View className="gap-2 sm:flex-row">
          <View className="flex-1">
            <Button
              label="Execute plan"
              busy={busy === "accept"}
              disabled={busy !== null}
              onPress={() => void decide("accept")}
            />
          </View>
          <View className="flex-1">
            <Button
              label="Request changes…"
              variant="secondary"
              disabled={busy !== null}
              onPress={() => setRevising(true)}
            />
          </View>
        </View>
      )}
    </View>
  );
}

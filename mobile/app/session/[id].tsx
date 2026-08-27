import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useRef, useState } from "react";
import {
  KeyboardAvoidingView,
  NativeScrollEvent,
  NativeSyntheticEvent,
  Platform,
  Pressable,
  ScrollView,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { CodeApprovalCard } from "../../src/components/CodeApprovalCard";
import { Button, LoadingState, StatusPill } from "../../src/components/Controls";
import { ErrorText } from "../../src/components/Screen";
import {
  decideCodeApproval,
  interruptCodeSession,
  listCodeApprovals,
  listCodeQueuedTurns,
  listCodeTurns,
  steerCodeSession,
  submitCodeTurn,
} from "../../src/lib/api";
import { pendingApprovals } from "../../src/lib/approvals";
import {
  clearDeliveredDraft,
  codeSubmissionWasAccepted,
  restoreUndeliveredDraft,
  sessionActionAvailability,
  submissionFailure,
  type AmbiguousCodeSubmission,
} from "../../src/lib/submission";
import type { TimelineItem } from "../../src/lib/transcript";
import { useSessionStore } from "../../src/session/store";
import { useMachineClient } from "../../src/session/useMachineClient";
import { useSessionEvents } from "../../src/session/useSessionEvents";

const PIN_THRESHOLD = 80;
type SendMode = "steer" | "followup";

export default function SessionDetailScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{
    id?: string;
    title?: string;
    workspace?: string;
  }>();
  const session = useSessionStore((state) => state.session);
  const client = useMachineClient();
  const queryClient = useQueryClient();
  const [refreshVersion, setRefreshVersion] = useState(0);
  const transcript = useSessionEvents(client, params.id, refreshVersion);
  const approvalsQuery = useQuery({
    queryKey: ["code-approvals", client, params.id],
    enabled: !!client && !!params.id,
    queryFn: () => listCodeApprovals(client!, params.id!),
    refetchInterval: 5_000,
  });
  const approvals = pendingApprovals(approvalsQuery.data ?? []);
  const approvalListKey = approvals.map((approval) => approval.id).join(":");
  const scrollRef = useRef<ScrollView>(null);
  const submittingTurnRef = useRef(false);
  const steeringRef = useRef(false);
  const interruptingRef = useRef(false);
  const refreshingRef = useRef(false);
  const [pinned, setPinned] = useState(true);
  const [showJump, setShowJump] = useState(false);
  const [message, setMessage] = useState("");
  const [mode, setMode] = useState<SendMode>("followup");
  const [submittingTurn, setSubmittingTurn] = useState(false);
  const [steering, setSteering] = useState(false);
  const [interrupting, setInterrupting] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [deliveryUnknown, setDeliveryUnknown] = useState(false);
  const [ambiguousTurn, setAmbiguousTurn] =
    useState<AmbiguousCodeSubmission | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);
  const availability = sessionActionAvailability({
    submittingTurn,
    steering,
    interrupting,
    refreshing,
    deliveryUnknown,
  });

  useEffect(() => {
    if (pinned) {
      scrollRef.current?.scrollToEnd({ animated: true });
    } else {
      setShowJump(true);
    }
  }, [approvalListKey, transcript.items, pinned]);

  useEffect(() => {
    if (transcript.approvalRevision === 0) return;
    void queryClient.invalidateQueries({
      queryKey: ["code-approvals", client, params.id],
    });
  }, [client, params.id, queryClient, transcript.approvalRevision]);

  useEffect(() => {
    if (!transcript.activeTurnId) setMode("followup");
  }, [transcript.activeTurnId]);

  function onScroll(event: NativeSyntheticEvent<NativeScrollEvent>) {
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    const fromBottom =
      contentSize.height - layoutMeasurement.height - contentOffset.y;
    const atBottom = fromBottom < PIN_THRESHOLD;
    setPinned(atBottom);
    if (atBottom) setShowJump(false);
  }

  async function send() {
    const content = message.trim();
    const steeringCurrent = mode === "steer" && !!transcript.activeTurnId;
    const controlsBlocked =
      steeringRef.current ||
      interruptingRef.current ||
      refreshingRef.current ||
      deliveryUnknown;
    if (
      !client ||
      !params.id ||
      !content ||
      controlsBlocked ||
      (!steeringCurrent && submittingTurnRef.current) ||
      (steeringCurrent
        ? !availability.canSteer
        : !availability.canFollowUp)
    ) {
      return;
    }
    if (steeringCurrent) {
      steeringRef.current = true;
      setSteering(true);
    } else {
      submittingTurnRef.current = true;
      setSubmittingTurn(true);
    }
    setSendError(null);
    setReceipt(null);
    let requestDispatched = false;
    let turnAttempt: AmbiguousCodeSubmission | null = null;
    try {
      if (steeringCurrent && transcript.activeTurnId) {
        requestDispatched = true;
        await steerCodeSession(
          client,
          params.id,
          transcript.activeTurnId,
          content,
        );
        setReceipt("Guidance sent to the current turn.");
      } else {
        const [turns, queue] = await Promise.all([
          listCodeTurns(client, params.id),
          listCodeQueuedTurns(client, params.id),
        ]);
        turnAttempt = {
          message: content,
          knownTurnIds: new Set(turns.map((turn) => turn.id)),
          knownQueuedIds: new Set(queue.queued.map((turn) => turn.id)),
        };
        requestDispatched = true;
        setMessage("");
        const result = await submitCodeTurn(client, params.id, content);
        setReceipt(
          result.kind === "queued"
            ? "Follow-up queued behind the current turn."
            : "Turn accepted.",
        );
      }
      if (steeringCurrent) {
        setMessage((current) => clearDeliveredDraft(current, content));
      }
      setAmbiguousTurn(null);
      setRefreshVersion((value) => value + 1);
    } catch (reason) {
      const failure = submissionFailure(reason, requestDispatched);
      setSendError(failure.message);
      setDeliveryUnknown(failure.deliveryUnknown);
      setAmbiguousTurn(failure.deliveryUnknown ? turnAttempt : null);
      if (!steeringCurrent) {
        setMessage((current) => restoreUndeliveredDraft(current, content));
      }
    } finally {
      if (steeringCurrent) {
        steeringRef.current = false;
        setSteering(false);
      } else {
        submittingTurnRef.current = false;
        setSubmittingTurn(false);
      }
    }
  }

  async function interrupt() {
    if (
      !client ||
      !params.id ||
      steeringRef.current ||
      interruptingRef.current ||
      refreshingRef.current ||
      !availability.canInterrupt
    ) {
      return;
    }
    interruptingRef.current = true;
    setInterrupting(true);
    setSendError(null);
    setReceipt(null);
    try {
      await interruptCodeSession(client, params.id);
      setReceipt("Interrupt requested.");
      setRefreshVersion((value) => value + 1);
    } catch (reason) {
      const failure = submissionFailure(reason, true);
      setSendError(failure.message);
      setDeliveryUnknown(failure.deliveryUnknown);
      setAmbiguousTurn(null);
    } finally {
      interruptingRef.current = false;
      setInterrupting(false);
    }
  }

  async function decideApproval(
    approvalId: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) {
    if (!client) return;
    try {
      await decideCodeApproval(client, approvalId, decision, feedback);
    } finally {
      // The decision can commit even when its response is lost. Replace every
      // cached approval list before the reader can submit the decision again.
      await queryClient.invalidateQueries({ queryKey: ["code-approvals"] });
    }
  }

  async function refreshAfterAmbiguousAction() {
    if (
      !client ||
      !params.id ||
      refreshingRef.current ||
      steeringRef.current ||
      interruptingRef.current
    ) {
      return;
    }
    refreshingRef.current = true;
    setRefreshing(true);
    setSendError(null);
    try {
      if (ambiguousTurn) {
        const [turns, queue] = await Promise.all([
          listCodeTurns(client, params.id),
          listCodeQueuedTurns(client, params.id),
        ]);
        if (codeSubmissionWasAccepted(ambiguousTurn, turns, queue.queued)) {
          setMessage((current) =>
            clearDeliveredDraft(current, ambiguousTurn.message),
          );
          setReceipt("The turn was accepted before the connection ended.");
        } else {
          setReceipt("No matching turn was found. You can send again.");
        }
      } else {
        setReceipt("Session refreshed. Verify the latest event before retrying.");
      }
      setDeliveryUnknown(false);
      setAmbiguousTurn(null);
      setRefreshVersion((value) => value + 1);
    } catch (reason) {
      setSendError(
        reason instanceof Error
          ? reason.message
          : "Could not verify the latest session state.",
      );
    } finally {
      refreshingRef.current = false;
      setRefreshing(false);
    }
  }

  if (!session?.machine || !params.id) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <ErrorText>Attach a machine to supervise this session.</ErrorText>
        <View className="mt-4">
          <Button label="Go back" onPress={() => router.replace("/")} />
        </View>
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <KeyboardAvoidingView
        className="flex-1"
        behavior={Platform.OS === "ios" ? "padding" : undefined}
        keyboardVerticalOffset={Platform.OS === "ios" ? 74 : 0}
      >
        <View className="border-b border-border px-5 py-3">
          <View className="flex-row items-start justify-between gap-3">
            <View className="flex-1 gap-0.5">
              <Text
                className="text-lg font-semibold text-foreground"
                numberOfLines={2}
              >
                {params.title || "Session"}
              </Text>
              <Text
                className="font-mono text-xs text-muted-foreground"
                numberOfLines={1}
              >
                {params.workspace || params.id}
              </Text>
            </View>
            <StatusPill tone={transcript.live ? "live" : "info"}>
              {transcript.live ? "Live" : "Reconnecting"}
            </StatusPill>
          </View>
        </View>

        <View className="flex-1">
          <ScrollView
            ref={scrollRef}
            className="flex-1"
            contentContainerClassName="gap-3 px-5 py-4"
            keyboardShouldPersistTaps="handled"
            onScroll={onScroll}
            scrollEventThrottle={16}
          >
            {transcript.items.length === 0 ? (
              <LoadingState label="Waiting for session events…" />
            ) : (
              transcript.items.map((item) => (
                <TimelineRow key={item.id} item={item} />
              ))
            )}
            {approvalsQuery.isError ? (
              <ErrorText>
                {approvalsQuery.error instanceof Error
                  ? approvalsQuery.error.message
                  : "Could not load approvals for this session."}
              </ErrorText>
            ) : null}
            {approvals.map((approval) => (
              <CodeApprovalCard
                key={approval.id}
                approval={approval}
                onApprove={() => decideApproval(approval.id, "approve")}
                onDeny={(feedback) =>
                  decideApproval(approval.id, "deny", feedback)
                }
              />
            ))}
          </ScrollView>
          {showJump && !pinned ? (
            <Pressable
              accessibilityRole="button"
              className="absolute bottom-4 self-center rounded-full bg-primary px-4 py-2"
              onPress={() => {
                setPinned(true);
                setShowJump(false);
                scrollRef.current?.scrollToEnd({ animated: true });
              }}
            >
              <Text className="text-sm font-medium text-primary-foreground">
                Jump to latest
              </Text>
            </Pressable>
          ) : null}
        </View>

        <View className="gap-2 border-t border-border bg-background px-4 py-3">
          {transcript.activeTurnId ? (
            <View className="flex-row gap-2">
              <View className="flex-1">
                <Button
                  label="Steer current"
                  compact
                  variant={mode === "steer" ? "primary" : "secondary"}
                  disabled={!availability.canChangeMode}
                  onPress={() => setMode("steer")}
                />
              </View>
              <View className="flex-1">
                <Button
                  label="Follow-up"
                  compact
                  variant={mode === "followup" ? "primary" : "secondary"}
                  disabled={!availability.canChangeMode}
                  onPress={() => setMode("followup")}
                />
              </View>
            </View>
          ) : null}
          {receipt ? (
            <Text className="text-xs text-success-foreground">{receipt}</Text>
          ) : null}
          {sendError ? <ErrorText>{sendError}</ErrorText> : null}
          {deliveryUnknown ? (
            <Button
              label="Refresh before sending again"
              variant="secondary"
              busy={refreshing}
              disabled={steering || interrupting}
              onPress={() => void refreshAfterAmbiguousAction()}
            />
          ) : null}
          <TextInput
            multiline
            maxLength={32_000}
            placeholder={
              transcript.activeTurnId && mode === "steer"
                ? "Guide the current turn"
                : transcript.activeTurnId
                  ? "Queue a follow-up turn"
                  : "Send a follow-up turn"
            }
            placeholderTextColor="#697386"
            value={message}
            onChangeText={setMessage}
            editable={
              !deliveryUnknown && !interrupting && !steering && !refreshing
            }
            className="max-h-40 min-h-12 rounded-lg border border-border bg-background px-3 py-3 text-base text-foreground disabled:opacity-50"
          />
          <View className="flex-row gap-2">
            <View className="flex-1">
              <Button
                label={
                  transcript.activeTurnId && mode === "steer"
                    ? "Send guidance"
                    : transcript.activeTurnId
                      ? "Queue follow-up"
                      : "Send turn"
                }
                busy={
                  transcript.activeTurnId && mode === "steer"
                    ? steering
                    : submittingTurn
                }
                disabled={
                  message.trim().length === 0 ||
                  deliveryUnknown ||
                  interrupting ||
                  refreshing ||
                  (transcript.activeTurnId && mode === "steer"
                    ? !availability.canSteer
                    : !availability.canFollowUp)
                }
                onPress={() => void send()}
              />
            </View>
            {transcript.activeTurnId ? (
              <View className="flex-1">
                <Button
                  label="Interrupt"
                  variant="destructive"
                  busy={interrupting}
                  disabled={!availability.canInterrupt}
                  onPress={() => void interrupt()}
                />
              </View>
            ) : null}
          </View>
        </View>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

function TimelineRow({ item }: { item: TimelineItem }) {
  if (item.kind === "user") {
    return (
      <View className="max-w-[90%] self-end rounded-xl bg-primary px-3 py-2.5">
        <Text className="text-sm text-primary-foreground" selectable>
          {item.text}
        </Text>
      </View>
    );
  }
  if (item.kind === "assistant") {
    return (
      <View className="max-w-[94%] self-start rounded-xl border border-border bg-background px-3 py-2.5">
        <Text className="text-sm text-foreground" selectable>
          {item.text || (item.streaming ? "Working…" : "")}
        </Text>
      </View>
    );
  }
  if (item.kind === "tool") {
    return (
      <View className="rounded-lg border border-border bg-muted px-3 py-2">
        <Text className="text-sm text-foreground" numberOfLines={2}>
          {item.name}
          {item.summary ? ` · ${item.summary}` : ""}
        </Text>
      </View>
    );
  }
  return (
    <Text className="text-center text-xs text-muted-foreground">{item.text}</Text>
  );
}

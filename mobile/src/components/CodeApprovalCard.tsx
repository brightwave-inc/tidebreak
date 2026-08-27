import { useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import type { CodeApprovalSnapshot } from "../generated/wire";
import {
  approvalSummary,
  approvalTitle,
  formatApprovalPayload,
  validDenialFeedback,
} from "../lib/approvals";
import { Button, Field, SectionLabel } from "./Controls";
import { ErrorText } from "./Screen";

export function CodeApprovalCard({
  approval,
  onApprove,
  onDeny,
}: {
  approval: CodeApprovalSnapshot;
  onApprove: () => Promise<void>;
  onDeny: (feedback: string) => Promise<void>;
}) {
  const [denying, setDenying] = useState(false);
  const [feedback, setFeedback] = useState("");
  const [payloadOpen, setPayloadOpen] = useState(false);
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function decide(kind: "approve" | "deny") {
    const denial = validDenialFeedback(feedback);
    if (kind === "deny" && !denial) {
      setError("Tell the agent what to change before denying.");
      return;
    }
    setBusy(kind);
    setError(null);
    try {
      if (kind === "approve") await onApprove();
      else await onDeny(denial!);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Decision failed.");
    } finally {
      setBusy(null);
    }
  }

  return (
    <View className="gap-3 rounded-xl border border-warning-border bg-background p-4">
      <View className="gap-1">
        <SectionLabel>Approval needed</SectionLabel>
        <Text className="text-base font-semibold text-foreground">
          {approvalTitle(approval.kind)}?
        </Text>
      </View>
      <View className="rounded-lg border border-border bg-muted p-3">
        <Text className="font-mono text-sm text-foreground" selectable>
          {approvalSummary(approval.kind)}
        </Text>
      </View>
      <Pressable
        accessibilityRole="button"
        accessibilityState={{ expanded: payloadOpen }}
        onPress={() => setPayloadOpen((current) => !current)}
      >
        <Text className="text-xs font-medium text-muted-foreground">
          {payloadOpen ? "Hide" : "Show"} harness payload
        </Text>
      </Pressable>
      {payloadOpen ? (
        <ScrollView
          className="max-h-48 rounded-lg border border-border bg-muted p-3"
          nestedScrollEnabled
        >
          <Text className="font-mono text-xs text-muted-foreground" selectable>
            {formatApprovalPayload(approval.harness_raw_json)}
          </Text>
        </ScrollView>
      ) : null}
      {denying ? (
        <Field
          label="Feedback for the agent"
          hint="The engine receives this with the denial."
          multiline
          value={feedback}
          onChangeText={setFeedback}
          placeholder="Explain what should change"
          maxLength={2_000}
        />
      ) : null}
      {error ? <ErrorText>{error}</ErrorText> : null}
      <View className="gap-2 sm:flex-row">
        {denying ? (
          <>
            <View className="flex-1">
              <Button
                label="Deny with feedback"
                variant="destructive"
                busy={busy === "deny"}
                disabled={busy !== null}
                onPress={() => void decide("deny")}
              />
            </View>
            <View className="flex-1">
              <Button
                label="Cancel"
                variant="secondary"
                disabled={busy !== null}
                onPress={() => {
                  setDenying(false);
                  setError(null);
                }}
              />
            </View>
          </>
        ) : (
          <>
            <View className="flex-1">
              <Button
                label="Approve"
                busy={busy === "approve"}
                disabled={busy !== null}
                onPress={() => void decide("approve")}
              />
            </View>
            <View className="flex-1">
              <Button
                label="Deny…"
                variant="secondary"
                disabled={busy !== null}
                onPress={() => setDenying(true)}
              />
            </View>
          </>
        )}
      </View>
    </View>
  );
}

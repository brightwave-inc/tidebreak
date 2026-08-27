import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRouter } from "expo-router";
import { Pressable, RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { CodeApprovalCard } from "../src/components/CodeApprovalCard";
import { LoadingState, StatusPill } from "../src/components/Controls";
import { ErrorText } from "../src/components/Screen";
import { decideCodeApproval, listCodeApprovals } from "../src/lib/api";
import { pendingApprovals } from "../src/lib/approvals";
import { useMachineClient } from "../src/session/useMachineClient";

export default function ApprovalsScreen() {
  const router = useRouter();
  const client = useMachineClient();
  const queryClient = useQueryClient();
  const approvalsQuery = useQuery({
    queryKey: ["code-approvals", client],
    enabled: !!client,
    queryFn: () => listCodeApprovals(client!),
    refetchInterval: 5_000,
  });
  const approvals = pendingApprovals(approvalsQuery.data ?? []);

  async function decide(
    id: string,
    decision: "approve" | "deny",
    feedback?: string,
  ) {
    if (!client) return;
    try {
      await decideCodeApproval(client, id, decision, feedback);
    } finally {
      // A response can be lost after the decision commits. Refresh the
      // authoritative list before the reader can accidentally retry it.
      await queryClient.invalidateQueries({ queryKey: ["code-approvals"] });
    }
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView
        contentContainerClassName="gap-4 px-5 py-6"
        refreshControl={
          <RefreshControl
            refreshing={approvalsQuery.isRefetching}
            onRefresh={() => void approvalsQuery.refetch()}
          />
        }
      >
        <View className="flex-row items-start justify-between gap-3">
          <View className="flex-1 gap-1">
            <Text className="text-2xl font-semibold text-foreground">
              Approvals
            </Text>
            <Text className="text-sm text-muted-foreground">
              Oldest requests appear first. Denials require feedback for the
              agent.
            </Text>
          </View>
          <StatusPill tone={approvals.length > 0 ? "warning" : "neutral"}>
            {approvals.length}
          </StatusPill>
        </View>
        {!client ? (
          <ErrorText>Attach a machine to review approvals.</ErrorText>
        ) : null}
        {approvalsQuery.isLoading ? (
          <LoadingState label="Loading approvals…" />
        ) : null}
        {approvalsQuery.isError ? (
          <ErrorText>
            {approvalsQuery.error instanceof Error
              ? approvalsQuery.error.message
              : "Could not load approvals."}
          </ErrorText>
        ) : null}
        {client &&
        !approvalsQuery.isLoading &&
        !approvalsQuery.isError &&
        approvals.length === 0 ? (
          <View className="rounded-xl border border-border bg-background p-5">
            <Text className="text-base font-medium text-foreground">
              Nothing is waiting
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground">
              Sensitive code actions appear here when an agent pauses for a
              decision.
            </Text>
          </View>
        ) : null}
        {approvals.map((approval) => (
          <View key={approval.id} className="gap-2">
            <CodeApprovalCard
              approval={approval}
              onApprove={() => decide(approval.id, "approve")}
              onDeny={(feedback) => decide(approval.id, "deny", feedback)}
            />
            <Pressable
              accessibilityRole="link"
              onPress={() =>
                router.push({
                  pathname: "/session/[id]",
                  params: { id: approval.session_id },
                })
              }
            >
              <Text className="text-sm font-medium text-foreground">
                Open session
              </Text>
            </Pressable>
          </View>
        ))}
      </ScrollView>
    </SafeAreaView>
  );
}

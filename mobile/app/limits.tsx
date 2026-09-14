import { useQuery } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, ChipPill, EmptyState, Meter } from "../src/components/Console";
import { limitScopeLabel, limitWindowLabel } from "../src/lib/consoleLabels";
import { stamp } from "../src/lib/consoleTime";
import { formatMicroUsd } from "../src/lib/consoleTypes";
import type { CostLimitView } from "../src/lib/consoleTypes";
import { limitQueries } from "../src/session/consoleQueries";

/**
 * One cap. Reaching a bound the installation configured is not a malfunction,
 * so both the warned and the exceeded state warn rather than alarm — what
 * separates them is the words, and `resets_at` says when it ends by itself.
 */
function LimitCard({ policy }: { policy: CostLimitView }) {
  const scope = limitScopeLabel(policy.scope_type);
  const named =
    policy.scope_label && policy.scope_label !== scope
      ? policy.scope_label
      : null;
  return (
    <Card className="gap-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {named ? `${scope} · ${named}` : scope}
        </Text>
        {policy.exceeded ? (
          <ChipPill chip={{ label: "Limit reached", tone: "warning" }} />
        ) : policy.warning_reached ? (
          <ChipPill chip={{ label: "Near limit", tone: "warning" }} />
        ) : null}
      </View>
      <Text className="text-xs text-muted-foreground">
        {limitWindowLabel(policy.window)} ·{" "}
        {formatMicroUsd(policy.limit_microusd)} limit
        {policy.applies_to === "sandbox" ? " · sandbox spend only" : ""}
        {policy.enabled ? "" : " · not enforced"}
      </Text>
      <Meter
        fraction={policy.used_fraction}
        warn={policy.exceeded || policy.warning_reached}
      />
      <View className="flex-row justify-between">
        <Text className="text-xs text-muted-foreground">
          {formatMicroUsd(policy.current_spend_microusd)} this window
        </Text>
        <Text className="text-xs text-muted-foreground">
          resets {stamp(policy.resets_at)}
        </Text>
      </View>
      {policy.limit_microusd === 0 && policy.enabled ? (
        <Text className="text-xs text-warning-foreground">
          A zero cap denies this scope’s metered inference outright.
        </Text>
      ) : null}
    </Card>
  );
}

/**
 * The caps that can refuse this account: its own, its teams', those on models
 * granted to it, and the installation-wide ones. Every caller may read this —
 * asking why you are being refused is not an administrator's question.
 */
export default function LimitsScreen() {
  const query = useQuery(limitQueries.mine());
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  if (query.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load your limits"
          {...(query.error instanceof Error
            ? { detail: query.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }

  const policies = query.data?.data ?? [];

  if (policies.length === 0 && !query.isLoading) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="No budgets apply to you"
          detail="Nothing on this gateway caps your metered inference right now."
        />
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-3 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <Text className="text-xs text-muted-foreground">
          Spend counted against these caps is metered, installation-billed
          inference. Subscription-covered, provisioned-capacity, and
          prepaid-credit traffic use different payers and do not draw them down.
        </Text>
        {policies.map((policy) => (
          <LimitCard key={policy.id} policy={policy} />
        ))}
      </ScrollView>
    </SafeAreaView>
  );
}

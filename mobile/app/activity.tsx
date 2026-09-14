import { useQuery } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Card,
  ChipPill,
  EmptyState,
  ExpandableSection,
  StatTile,
} from "../src/components/Console";
import { SectionLabel } from "../src/components/Controls";
import {
  executionKindLabel,
  formatCount,
} from "../src/lib/consoleLabels";
import { formatMicroUsd } from "../src/lib/consoleTypes";
import type { GroupedUsageRow, UsageResponse } from "../src/lib/consoleTypes";
import {
  meQueries,
  usageQueries,
} from "../src/session/consoleQueries";

function keyLabel(row: GroupedUsageRow, dimension: string): string {
  const value = row.key[dimension];
  if (value == null) {
    return dimension === "client" ? "Unidentified client" : "(none)";
  }
  return dimension === "sandbox" ? value.slice(0, 8) : value;
}

/** What the covered billing classes came to, or nothing when none did. */
function coveredCostDetail(row: GroupedUsageRow): string | undefined {
  const parts: string[] = [];
  if (row.subscription_cost_microusd > 0) {
    parts.push(`${formatMicroUsd(row.subscription_cost_microusd)} subscription`);
  }
  if ((row.provisioned_cost_microusd ?? 0) > 0) {
    parts.push(
      `${formatMicroUsd(row.provisioned_cost_microusd ?? 0)} provisioned`,
    );
  }
  if ((row.credits_cost_microusd ?? 0) > 0) {
    parts.push(`${formatMicroUsd(row.credits_cost_microusd ?? 0)} credits`);
  }
  return parts.length > 0 ? parts.join(" · ") : undefined;
}

/** The zero fields a compat row does not carry, so one row shape renders both. */
const GROUPED_ZEROES = {
  pending_rate_requests: 0,
  provisional_requests: 0,
  unpriceable_requests: 0,
  estimated_cost_microusd: 0,
  subscription_cost_microusd: 0,
  provisioned_cost_microusd: 0,
  credits_cost_microusd: 0,
  subscription_requests: 0,
  attributed_requests: 0,
  unattributed_requests: 0,
} as const;

/**
 * The compatibility view's per-dimension arrays, lifted into the one row shape
 * the sections render. The two modes are the same events cut differently; only
 * the envelope differs, so the rendering should not.
 */
function compatRows(
  summary: UsageResponse | undefined,
  dimension: "model" | "client" | "sandbox" | "execution_kind",
): GroupedUsageRow[] {
  if (dimension === "model") {
    return (summary?.by_model ?? []).map((row) => ({
      ...GROUPED_ZEROES,
      key: { model: row.gateway_model_id },
      inference_requests: row.inference_requests,
      input_tokens: row.input_tokens,
      output_tokens: row.output_tokens,
      priced_requests: row.priced_requests,
      pending_rate_requests: row.pending_rate_requests,
      provisional_requests: row.provisional_requests,
      unpriceable_requests: row.unpriceable_requests,
      estimated_cost_microusd: row.estimated_cost_microusd,
      subscription_cost_microusd: row.subscription_cost_microusd ?? 0,
      provisioned_cost_microusd: row.provisioned_cost_microusd ?? 0,
      credits_cost_microusd: row.credits_cost_microusd ?? 0,
      subscription_requests: row.subscription_requests ?? 0,
    }));
  }
  if (dimension === "client") {
    return (summary?.by_client ?? []).map((row) => ({
      ...GROUPED_ZEROES,
      key: { client: row.client_name },
      inference_requests: row.inference_requests,
      input_tokens: row.input_tokens,
      output_tokens: row.output_tokens,
      priced_requests: row.priced_requests,
      pending_rate_requests: row.pending_rate_requests,
      attributed_requests: row.attributed_requests,
      unattributed_requests: row.unattributed_requests,
    }));
  }
  if (dimension === "sandbox") {
    return (summary?.by_sandbox ?? []).map((row) => ({
      ...GROUPED_ZEROES,
      key: { sandbox: row.sandbox_id },
      inference_requests: row.inference_requests,
      input_tokens: row.input_tokens,
      output_tokens: row.output_tokens,
      priced_requests: row.priced_requests,
      estimated_cost_microusd: row.estimated_cost_microusd,
      subscription_cost_microusd: row.subscription_cost_microusd,
      provisioned_cost_microusd: row.provisioned_cost_microusd ?? 0,
      credits_cost_microusd: row.credits_cost_microusd ?? 0,
      ...(row.last_activity_at ? { last_activity_at: row.last_activity_at } : {}),
    }));
  }
  return (summary?.by_execution_kind ?? []).map((row) => ({
    ...GROUPED_ZEROES,
    key: { execution_kind: row.execution_kind },
    inference_requests: row.inference_requests,
    input_tokens: row.input_tokens,
    output_tokens: row.output_tokens,
    priced_requests: row.priced_requests,
    pending_rate_requests: row.pending_rate_requests,
    provisional_requests: row.provisional_requests,
    unpriceable_requests: row.unpriceable_requests,
    estimated_cost_microusd: row.estimated_cost_microusd,
    subscription_cost_microusd: row.subscription_cost_microusd,
    provisioned_cost_microusd: row.provisioned_cost_microusd ?? 0,
    credits_cost_microusd: row.credits_cost_microusd ?? 0,
  }));
}

function CostRow({
  title,
  row,
  showCost = true,
}: {
  title: string;
  row: GroupedUsageRow;
  showCost?: boolean;
}) {
  const covered = coveredCostDetail(row);
  return (
    <View className="gap-0.5">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {title}
        </Text>
        <Text className="text-sm text-foreground">
          {showCost
            ? formatMicroUsd(row.estimated_cost_microusd)
            : `${formatCount(row.inference_requests)} reqs`}
        </Text>
      </View>
      <Text className="text-xs text-muted-foreground">
        {formatCount(row.inference_requests)} reqs ·{" "}
        {formatCount(row.input_tokens)} in / {formatCount(row.output_tokens)} out
      </Text>
      {covered ? (
        <Text className="text-xs text-muted-foreground">{covered}</Text>
      ) : null}
    </View>
  );
}

/**
 * Strictly the signed-in account's usage: stat tiles, then expandable rollups.
 *
 * Members get self-narrowed reads from the server. An administrator's reads are
 * installation-wide, so their rollups are re-asked in filtered query mode pinned
 * to their own id — this screen is about *your* activity either way, and the
 * account switcher Tidewatch carried is an administrator affordance that belongs
 * with the rest of the admin console (#3402).
 */
export default function ActivityScreen() {
  const [refreshing, setRefreshing] = useState(false);
  const me = useQuery(meQueries.identity());
  const summary = useQuery(usageQueries.summary());
  const scope = summary.data?.scope;
  const selfScoped = scope === "self";
  const userId = me.data?.user_id;

  // Installation-wide reads have to be narrowed back to the caller; a
  // self-scoped read already is.
  const needsFiltered = scope !== undefined && !selfScoped && Boolean(userId);
  const byModel = useQuery({
    ...usageQueries.mine(userId ?? "", "model"),
    enabled: needsFiltered,
  });
  const byClient = useQuery({
    ...usageQueries.mine(userId ?? "", "client"),
    enabled: needsFiltered,
  });
  const bySandbox = useQuery({
    ...usageQueries.mine(userId ?? "", "sandbox"),
    enabled: needsFiltered,
  });
  const byExecutionKind = useQuery({
    ...usageQueries.mine(userId ?? "", "execution_kind"),
    enabled: needsFiltered,
  });

  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void Promise.allSettled([
      summary.refetch(),
      ...(needsFiltered
        ? [
            byModel.refetch(),
            byClient.refetch(),
            bySandbox.refetch(),
            byExecutionKind.refetch(),
          ]
        : []),
    ]).finally(() => setRefreshing(false));
  }, [summary, needsFiltered, byModel, byClient, bySandbox, byExecutionKind]);

  if (summary.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load usage"
          {...(summary.error instanceof Error
            ? { detail: summary.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }

  const mine = selfScoped
    ? summary.data?.by_user?.[0]
    : summary.data?.by_user?.find((row) => row.user_id === userId);

  const modelRows = selfScoped
    ? compatRows(summary.data, "model")
    : (byModel.data?.grouped ?? []);
  const clientRows = selfScoped
    ? compatRows(summary.data, "client")
    : (byClient.data?.grouped ?? []);
  const executionRows = selfScoped
    ? compatRows(summary.data, "execution_kind")
    : (byExecutionKind.data?.grouped ?? []);
  const sandboxRows = (
    selfScoped ? compatRows(summary.data, "sandbox") : (bySandbox.data?.grouped ?? [])
  ).filter((row) => row.key.sandbox != null);

  const unattributed = clientRows.reduce(
    (sum, row) => sum + row.unattributed_requests,
    0,
  );

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-3 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <Text className="text-xs text-muted-foreground">
          Your usage on this gateway. Costs are billed spend only —
          subscription-covered, provisioned-capacity, and prepaid-credit traffic
          are different payers and are never summed in.
        </Text>

        {mine ? (
          <View className="gap-2">
            <View className="flex-row gap-2">
              <StatTile
                label="Requests"
                value={formatCount(mine.inference_requests)}
                {...(mine.priced_requests < mine.inference_requests
                  ? { detail: "cost is a floor — some unpriced" }
                  : {})}
              />
              <StatTile
                label="Est. spend"
                value={formatMicroUsd(mine.estimated_cost_microusd)}
              />
            </View>
            <View className="flex-row gap-2">
              <StatTile
                label="Tokens"
                value={`${formatCount(mine.input_tokens)} in`}
                detail={`${formatCount(mine.output_tokens)} out`}
              />
              <StatTile
                label="Subscription"
                value={formatMicroUsd(mine.subscription_cost_microusd)}
                {...(mine.subscription_requests > 0
                  ? {
                      detail: `${formatCount(mine.subscription_requests)} reqs absorbed`,
                    }
                  : {})}
              />
            </View>
            <View className="flex-row gap-2">
              <StatTile
                label="Provisioned"
                value={formatMicroUsd(mine.provisioned_cost_microusd ?? 0)}
                detail="absorbed by provider capacity"
              />
              <StatTile
                label="Credits"
                value={formatMicroUsd(mine.credits_cost_microusd)}
                detail="prepaid balance"
              />
            </View>
            <View className="flex-row gap-2">
              <StatTile
                label="Tool calls"
                value={formatCount(mine.tool_calls)}
              />
              <StatTile
                label="App requests"
                value={formatCount(mine.app_requests)}
              />
            </View>
          </View>
        ) : summary.isLoading ? null : (
          <Card>
            <Text className="text-sm text-muted-foreground">
              No inference recorded for this account yet.
            </Text>
          </Card>
        )}

        <ExpandableSection
          title="Models"
          rows={modelRows}
          rowKey={(row) => keyLabel(row, "model")}
          empty="No inference recorded."
          renderRow={(row) => (
            <CostRow title={keyLabel(row, "model")} row={row} />
          )}
        />

        <ExpandableSection
          title="Clients"
          supporting="Which harnesses and tools sent your traffic. Requests without a conversation identity routed fine — they are just absent from conversation views."
          rows={clientRows}
          rowKey={(row) => keyLabel(row, "client")}
          empty="No client-attributed traffic."
          renderRow={(row) => (
            <View className="gap-0.5">
              <CostRow
                title={keyLabel(row, "client")}
                row={row}
                showCost={false}
              />
              {row.unattributed_requests > 0 ? (
                <Text className="text-xs text-muted-foreground">
                  {formatCount(row.attributed_requests)} in conversations ·{" "}
                  {formatCount(row.unattributed_requests)} unattributed
                </Text>
              ) : null}
            </View>
          )}
        />

        {unattributed > 0 ? (
          <Card>
            <Text className="text-sm text-muted-foreground">
              {formatCount(unattributed)} of your requests carried no
              conversation identity. They are counted above, never omitted —
              displayed usage always sums to total usage.
            </Text>
          </Card>
        ) : null}

        <ExpandableSection
          title="Execution"
          supporting="Your workstation and detached-agent inference, kept as separate execution kinds."
          rows={executionRows}
          rowKey={(row) => keyLabel(row, "execution_kind")}
          empty="No execution-kind usage recorded."
          renderRow={(row) => (
            <CostRow
              title={executionKindLabel(keyLabel(row, "execution_kind"))}
              row={row}
            />
          )}
        />

        <ExpandableSection
          title="Sandbox spend"
          supporting="Your detached-agent inference, kept apart from workstation clients."
          rows={sandboxRows}
          rowKey={(row) => keyLabel(row, "sandbox")}
          empty="No sandbox inference recorded."
          renderRow={(row) => (
            <CostRow title={keyLabel(row, "sandbox")} row={row} />
          )}
        />

        {selfScoped ? (
          <ExpandableSection
            title="Connected apps"
            supporting="Your governed app traffic, rolled up by app."
            rows={summary.data?.by_app ?? []}
            rowKey={(row) => row.app_id}
            empty="No governed app traffic recorded."
            renderRow={(row) => (
              <View className="flex-row items-center justify-between gap-2">
                <Text
                  className="flex-1 text-sm font-medium text-foreground"
                  numberOfLines={1}
                >
                  {row.app_name}
                </Text>
                <Text className="text-xs text-muted-foreground">
                  {formatCount(row.tool_calls)} tool calls ·{" "}
                  {formatCount(row.app_requests)} requests
                </Text>
              </View>
            )}
          />
        ) : (
          <Card>
            <Text className="text-xs text-muted-foreground">
              Your app-traffic totals are in the tiles above. A per-app
              breakdown for one account is not served by the gateway yet — the
              usage read only cross-tabs inference.
            </Text>
          </Card>
        )}

        <View className="gap-2">
          <SectionLabel>Scope</SectionLabel>
          <Card>
            <View className="flex-row items-center justify-between gap-2">
              <Text className="flex-1 text-xs text-muted-foreground">
                {selfScoped
                  ? "The gateway narrows these reads to your account."
                  : "Your reads are installation-wide; the figures above are pinned to your own account."}
              </Text>
              <ChipPill
                chip={{
                  label: selfScoped ? "Self" : "Installation",
                  tone: selfScoped ? "neutral" : "info",
                }}
              />
            </View>
          </Card>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

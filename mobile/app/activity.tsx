import { useQuery } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { Pressable, RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Card,
  ChipPill,
  EmptyState,
  ExpandableSection,
  StatTile,
} from "../src/components/Console";
import { ConsoleLink } from "../src/components/Admin";
import { SectionLabel } from "../src/components/Controls";
import {
  executionKindLabel,
  formatCount,
} from "../src/lib/consoleLabels";
import { formatMicroUsd } from "../src/lib/consoleTypes";
import type {
  GroupedUsageRow,
  UsageResponse,
  UserUsageRow,
} from "../src/lib/consoleTypes";
import { administers } from "../src/lib/sections";
import {
  meQueries,
  usageQueries,
} from "../src/session/consoleQueries";
import { useActiveConnection } from "../src/session/store";
import { useLearnedAdminRole } from "../src/session/useAdminRole";

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
 * Which account this screen is reporting on.
 *
 * Offered only where the gateway's reads are installation-wide: those already
 * carry every account's row, so switching costs one filtered re-read rather
 * than any widening of authority. The credential is untouched — what changes
 * is which account's row is being displayed, never who is asking.
 */
function AccountPicker({
  users,
  selectedId,
  onPick,
}: {
  users: UserUsageRow[];
  selectedId: string | undefined;
  onPick: (userId: string) => void;
}) {
  return (
    <Card className="gap-2">
      <SectionLabel>Show usage for</SectionLabel>
      <Text className="text-xs text-muted-foreground">
        Your reads are installation-wide, so this view can show any account’s
        usage. It changes what is displayed, not what you are signed in as.
      </Text>
      {users.map((user) => (
        <Pressable
          key={user.user_id}
          accessibilityRole="button"
          accessibilityState={{ selected: user.user_id === selectedId }}
          onPress={() => onPick(user.user_id)}
          className={`min-h-11 justify-center rounded-lg border px-3 py-2.5 ${
            user.user_id === selectedId
              ? "border-primary bg-background"
              : "border-border bg-background"
          }`}
        >
          <Text className="text-sm font-medium text-foreground">
            {user.display_name || user.email}
          </Text>
          <Text className="text-xs text-muted-foreground">{user.email}</Text>
        </Pressable>
      ))}
    </Card>
  );
}

/**
 * One account's usage: stat tiles, then expandable rollups.
 *
 * Members get self-narrowed reads from the server. An administrator's reads are
 * installation-wide, so the rollups are re-asked in filtered query mode pinned
 * to one account — their own by default, and any account the switcher names.
 */
export default function ActivityScreen() {
  const [refreshing, setRefreshing] = useState(false);
  const [switching, setSwitching] = useState(false);
  // Which account the view is reporting on, when it is not the caller's own.
  // Deliberately screen state rather than anything persisted: it is a lens on
  // one screen, and it must not survive into what the rest of the app thinks
  // the signed-in account is.
  const [viewing, setViewing] = useState<string | null>(null);
  const connection = useActiveConnection();
  const me = useQuery(meQueries.identity());
  const summary = useQuery(usageQueries.summary());
  const scope = summary.data?.scope;
  const selfScoped = scope === "self";
  useLearnedAdminRole(scope);
  const isAdmin = administers(connection);
  const userId = (isAdmin ? viewing : null) ?? me.data?.user_id;

  // Installation-wide reads have to be narrowed to one account; a self-scoped
  // read already is.
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

  const viewedRow = selfScoped
    ? summary.data?.by_user?.[0]
    : summary.data?.by_user?.find((row) => row.user_id === userId);
  const mine = viewedRow;
  const viewingOther = isAdmin && viewing != null && viewing !== me.data?.user_id;

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
        {/* The account switcher: an administrator's reads already carry every
            account's row, so this changes which one is displayed rather than
            widening anything. A member has one account and taps nothing. */}
        {switching && isAdmin ? (
          <AccountPicker
            users={summary.data?.by_user ?? []}
            selectedId={userId}
            onPick={(picked) => {
              setViewing(picked);
              setSwitching(false);
            }}
          />
        ) : null}

        <Pressable
          accessibilityRole={isAdmin ? "button" : undefined}
          accessibilityLabel={
            isAdmin ? "Change which account this usage is for" : undefined
          }
          disabled={!isAdmin}
          onPress={() => setSwitching((current) => !current)}
        >
          <Text className="text-xs text-muted-foreground">
            {mine ? (
              <>
                Usage for{" "}
                <Text className="font-medium text-foreground">
                  {mine.display_name || mine.email}
                </Text>
                {isAdmin ? " (tap to switch)" : ""}.{" "}
              </>
            ) : (
              "Your usage on this gateway. "
            )}
            Costs are billed spend only — subscription-covered,
            provisioned-capacity, and prepaid-credit traffic are different
            payers and are never summed in.
          </Text>
        </Pressable>

        {viewingOther ? (
          <Card>
            <Text className="text-xs text-muted-foreground">
              You are reading another account’s usage. Nothing about your own
              session changed — the switcher only picks whose row is shown.
            </Text>
          </Card>
        ) : null}

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
                  : viewingOther
                    ? "Your reads are installation-wide; the figures above are pinned to the account the switcher named."
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

        {/* The console's usage page is administrator-only, so the hand-off is
            offered only to one — a member would be redirected away from it.
            This screen is one account's slice of the same subject; the
            installation-wide figures and every write live there. */}
        {isAdmin ? <ConsoleLink webPath="/usage" /> : null}
      </ScrollView>
    </SafeAreaView>
  );
}

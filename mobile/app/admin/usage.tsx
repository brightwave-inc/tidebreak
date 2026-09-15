import { useQuery } from "@tanstack/react-query";
import { Text, View } from "react-native";
import {
  AdminScreen,
  LoadError,
  RowGroup,
} from "../../src/components/Admin";
import { StatTile } from "../../src/components/Console";
import { formatCount } from "../../src/lib/consoleLabels";
import { formatMicroUsd } from "../../src/lib/consoleTypes";
import type { UsageResponse } from "../../src/lib/consoleTypes";
import type { InstallationUsage } from "../../src/lib/gatewayAdmin";
import { usageQueries } from "../../src/session/consoleQueries";

type InstallationUsageResponse = UsageResponse & Partial<InstallationUsage>;

/** What the covered billing classes came to, or nothing when none did. */
function coveredCost(
  subscription: number | undefined,
  provisioned: number | undefined,
  credits: number | undefined,
): string | undefined {
  const parts: string[] = [];
  if ((subscription ?? 0) > 0) {
    parts.push(`${formatMicroUsd(subscription ?? 0)} subscription`);
  }
  if ((provisioned ?? 0) > 0) {
    parts.push(`${formatMicroUsd(provisioned ?? 0)} provisioned`);
  }
  if ((credits ?? 0) > 0) {
    parts.push(`${formatMicroUsd(credits ?? 0)} credits`);
  }
  return parts.length > 0 ? parts.join(" · ") : undefined;
}

/**
 * Installation-wide totals. `summary` is the authoritative pair when the
 * gateway sends it; summing `by_user` is the fallback, and a floor rather than
 * a total — the rollups are cuts of the same events, and a row can be absent
 * from one without being absent from the ledger.
 */
function totals(data: InstallationUsageResponse | undefined): {
  requests: number;
  spend: number;
  exact: boolean;
} {
  if (data?.summary) {
    return {
      requests: data.summary.inference_requests,
      spend: data.summary.estimated_cost_microusd,
      exact: true,
    };
  }
  const users = data?.by_user ?? [];
  return {
    requests: users.reduce((sum, row) => sum + row.inference_requests, 0),
    spend: users.reduce((sum, row) => sum + row.estimated_cost_microusd, 0),
    exact: false,
  };
}

/** Name, spend, then volume — the row shape the member activity screen uses. */
function UsageRow({
  title,
  amount,
  detail,
  covered,
}: {
  title: string;
  amount: string;
  detail: string;
  covered?: string;
}) {
  return (
    <View className="gap-0.5">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {title}
        </Text>
        <Text className="text-sm font-medium text-foreground">{amount}</Text>
      </View>
      <Text className="text-xs text-muted-foreground">{detail}</Text>
      {covered ? (
        <Text className="text-xs text-muted-foreground">{covered}</Text>
      ) : null}
    </View>
  );
}

/**
 * Everything this installation has spent, by account, team, and model.
 *
 * The unfiltered read is the compatibility view — the same one the hub uses to
 * learn whether this account administers the gateway — so no filter or
 * `group_by` is sent and the `by_*` rollups arrive populated.
 */
export default function AdminUsageScreen() {
  const summary = useQuery(usageQueries.summary());
  const data = summary.data;
  const { requests, spend, exact } = totals(data);

  return (
    <AdminScreen refresh={() => summary.refetch()} consolePath="/usage">
      {summary.isError ? (
        <LoadError what="usage" error={summary.error} />
      ) : null}

      <Text className="text-xs text-muted-foreground">
        Installation-wide inference. Costs are billed spend only —
        subscription-covered, provisioned-capacity, and prepaid-credit traffic
        are different payers and are never summed in.
      </Text>

      <View className="flex-row gap-2">
        <StatTile
          label="Requests"
          value={formatCount(requests)}
          detail={exact ? undefined : "summed across accounts"}
        />
        <StatTile
          label="Est. spend"
          value={formatMicroUsd(spend)}
          detail={
            data?.summary && data.summary.priced_requests < requests
              ? "a floor — some unpriced"
              : undefined
          }
        />
      </View>

      <RowGroup
        title="By user"
        rows={data?.by_user ?? []}
        rowKey={(row) => row.user_id}
        empty={summary.isLoading ? "Loading…" : "No inference recorded."}
        renderRow={(row) => (
          <UsageRow
            title={row.display_name || row.email}
            amount={formatMicroUsd(row.estimated_cost_microusd)}
            detail={`${formatCount(row.inference_requests)} reqs · ${formatCount(
              row.input_tokens,
            )} in / ${formatCount(row.output_tokens)} out`}
            covered={coveredCost(
              row.subscription_cost_microusd,
              row.provisioned_cost_microusd,
              row.credits_cost_microusd,
            )}
          />
        )}
      />

      <Text className="text-xs text-muted-foreground">
        Team rows are the team attributed when each request was made, which is
        what the spend was authorized under — not who is in the team now.
      </Text>

      <RowGroup
        title="By team"
        rows={data?.by_team ?? []}
        rowKey={(row) => row.team_slug}
        empty={
          summary.isLoading ? "Loading…" : "No team-attributed inference."
        }
        renderRow={(row) => (
          <UsageRow
            title={row.team_name}
            amount={formatMicroUsd(row.estimated_cost_microusd)}
            detail={`${formatCount(row.inference_requests)} reqs · ${formatCount(
              row.input_tokens,
            )} in / ${formatCount(row.output_tokens)} out`}
          />
        )}
      />

      <RowGroup
        title="By model"
        rows={data?.by_model ?? []}
        rowKey={(row) => row.gateway_model_id}
        empty={summary.isLoading ? "Loading…" : "No inference recorded."}
        renderRow={(row) => (
          <UsageRow
            title={row.gateway_model_id}
            amount={formatMicroUsd(row.estimated_cost_microusd)}
            detail={`${row.provider_name} · ${formatCount(
              row.inference_requests,
            )} reqs`}
            covered={coveredCost(
              row.subscription_cost_microusd,
              row.provisioned_cost_microusd,
              row.credits_cost_microusd,
            )}
          />
        )}
      />
    </AdminScreen>
  );
}

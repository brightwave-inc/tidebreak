import { useQuery } from "@tanstack/react-query";
import { Text, View } from "react-native";
import {
  AdminLimitCard,
  AdminScreen,
  LoadError,
} from "../../src/components/Admin";
import { StatTile } from "../../src/components/Console";
import { SectionLabel } from "../../src/components/Controls";
import { spacedSlug } from "../../src/lib/consoleLabels";
import type { CostLimit } from "../../src/lib/gatewayAdmin";
import {
  concurrencyOccupancy,
  schedulingPressureNote,
} from "../../src/lib/sandboxStatus";
import {
  adminQueries,
  isSandboxesNotEnabled,
  sandboxQueries,
} from "../../src/session/consoleQueries";

/**
 * Cost-limit scopes as an administrator reads them. Deliberately not the
 * member vocabulary in `consoleLabels.ts`, where `user` is "You": on an
 * installation-wide list every row's scope is somebody, and "You" would be
 * wrong on all but one of them.
 */
const SCOPE_LABEL: Record<string, string> = {
  installation: "Installation",
  user: "Account",
  team: "Team",
  gateway_model: "Model",
  connected_app: "App",
};

/** Broadest scope first: the cap that can refuse everyone leads the page. */
const SCOPE_ORDER = [
  "installation",
  "team",
  "user",
  "gateway_model",
  "connected_app",
];

function scopeRank(scope: string): number {
  const index = SCOPE_ORDER.indexOf(scope);
  // A scope this build has never seen sorts after the ones it knows rather
  // than silently ahead of the installation-wide cap.
  return index === -1 ? SCOPE_ORDER.length : index;
}

/**
 * Every cap on the installation, grouped by what it limits, plus the two
 * bounds no administrator session can move: the cost-control calendar and the
 * sandbox concurrency allowance.
 *
 * Both the database policies and the deployment's configured bounds belong
 * here — a reader asking what can refuse a request needs the ones nobody can
 * edit too, and pointing at the wrong knob is how a ceiling gets raised
 * against a cluster that was never the constraint.
 */
export default function AdminLimitsScreen() {
  const limits = useQuery(adminQueries.costLimits());
  const settings = useQuery(adminQueries.costControlSettings());
  const concurrency = useQuery(sandboxQueries.concurrency());

  const policies = limits.data?.data ?? [];
  const exceeded = policies.filter((policy) => policy.exceeded).length;
  const warned = policies.filter(
    (policy) => policy.warning_reached && !policy.exceeded,
  ).length;

  const groups = new Map<string, CostLimit[]>();
  for (const policy of policies) {
    const rows = groups.get(policy.scope_type) ?? [];
    rows.push(policy);
    groups.set(policy.scope_type, rows);
  }
  const ordered = [...groups.entries()].sort(
    ([left], [right]) => scopeRank(left) - scopeRank(right),
  );
  const occupancy = concurrency.data?.data;

  return (
    <AdminScreen
      refresh={() =>
        Promise.all([
          limits.refetch(),
          settings.refetch(),
          concurrency.refetch(),
        ])
      }
      consolePath="/limits"
    >
      {limits.isError ? (
        <LoadError what="cost limits" error={limits.error} />
      ) : null}

      <View className="flex-row gap-2">
        <StatTile label="Caps" value={policies.length.toLocaleString()} />
        <StatTile label="Near limit" value={warned.toLocaleString()} />
        <StatTile label="Reached" value={exceeded.toLocaleString()} />
      </View>

      <Text className="text-xs text-muted-foreground">
        Only metered, installation-billed inference draws these down.
        {settings.data
          ? ` Windows open and close on the ${settings.data.time_zone} calendar.`
          : ""}
      </Text>

      {occupancy ? (
        <View className="gap-1">
          <SectionLabel>Sandbox concurrency</SectionLabel>
          <Text className="text-sm text-foreground">
            {concurrencyOccupancy(occupancy)}
          </Text>
          {/* Raising a ceiling is the wrong move when the slots are held by
              pods the cluster never placed, so say which kind of full this is
              before pointing at the knob. */}
          {schedulingPressureNote(occupancy) ? (
            <Text className="text-sm text-foreground">
              {schedulingPressureNote(occupancy)}
            </Text>
          ) : null}
          <Text className="text-xs text-muted-foreground">
            Raise either cap in deployment configuration (`sandboxes.bounds`)
            and redeploy. No administrator session can change it.
          </Text>
        </View>
      ) : concurrency.isError && !isSandboxesNotEnabled(concurrency.error) ? (
        <LoadError what="sandbox concurrency" error={concurrency.error} />
      ) : null}

      {ordered.length === 0 ? (
        <Text className="text-sm text-muted-foreground">
          {limits.isLoading
            ? "Loading…"
            : "No cost limits are configured on this gateway."}
        </Text>
      ) : (
        ordered.map(([scope, rows]) => (
          <View className="gap-2" key={scope}>
            <SectionLabel>
              {SCOPE_LABEL[scope] ?? spacedSlug(scope)}
            </SectionLabel>
            {rows.map((policy) => (
              <AdminLimitCard
                key={policy.id}
                policy={policy}
                scopeLabel={SCOPE_LABEL[scope] ?? spacedSlug(scope)}
              />
            ))}
          </View>
        ))
      )}
    </AdminScreen>
  );
}

import { useQuery } from "@tanstack/react-query";
import { Text, View } from "react-native";
import { AdminScreen, LoadError } from "../../src/components/Admin";
import { Card, StatTile } from "../../src/components/Console";
import { SectionLabel, StatusPill } from "../../src/components/Controls";
import { spacedSlug } from "../../src/lib/consoleLabels";
import type { GuardrailPolicy } from "../../src/lib/gatewayAdmin";
import { adminQueries } from "../../src/session/consoleQueries";

/** The window the activity tiles describe, sent explicitly so it can be named. */
const ACTIVITY_DAYS = 7;

const STANCE_LABEL: Record<string, string> = {
  fail_open: "fails open",
  fail_closed: "fails closed",
};

/**
 * One policy.
 *
 * `mode` is what an administrator configured; the surfaces where the gateway
 * can only monitor are reported separately, so an `enforce` policy whose every
 * effective mode is monitor is stated rather than left to be inferred from a
 * pill that promises enforcement.
 */
function PolicyCard({ policy }: { policy: GuardrailPolicy }) {
  const monitorOnly =
    policy.mode === "enforce" &&
    policy.effective_modes.length > 0 &&
    policy.effective_modes.every((entry) => entry.mode === "monitor");
  const attachments = policy.attachments.length;
  return (
    <Card className="gap-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {policy.name}
        </Text>
        <View className="flex-row gap-1.5">
          <StatusPill tone={policy.mode === "enforce" ? "live" : "neutral"}>
            {spacedSlug(policy.mode)}
          </StatusPill>
          {policy.enabled ? null : <StatusPill tone="warning">off</StatusPill>}
        </View>
      </View>
      <Text className="text-xs text-muted-foreground">
        {attachments === 0
          ? "Attached to nothing"
          : `${attachments} attachment${attachments === 1 ? "" : "s"}`}
        {" · "}
        {STANCE_LABEL[policy.failure_stance] ??
          spacedSlug(policy.failure_stance)}
        {policy.engines.length > 0
          ? ` · ${policy.engines.map((engine) => engine.display_name).join(", ")}`
          : ""}
      </Text>
      {monitorOnly ? (
        <Text className="text-xs text-warning-foreground">
          Configured to enforce, but every surface it runs on is monitor-only on
          this gateway.
        </Text>
      ) : null}
    </Card>
  );
}

/**
 * What the guardrail engines have seen lately, then what is configured to see
 * it.
 *
 * Activity leads because it is the question a phone gets asked — whether
 * anything is being flagged — and the policy list is the context for the
 * answer. No scanned content appears here: the activity read is counts only,
 * and which requests were flagged stays in the console.
 */
export default function AdminGuardrailsScreen() {
  const activity = useQuery(adminQueries.guardrailActivity(ACTIVITY_DAYS));
  const policies = useQuery(adminQueries.guardrailPolicies());

  const rows = activity.data?.data ?? [];
  const totals = rows.reduce(
    (sum, row) => ({
      evaluations: sum.evaluations + row.evaluations,
      flagged: sum.flagged + row.flagged,
      blocked: sum.blocked + row.blocked,
      degraded: sum.degraded + row.degraded,
    }),
    { evaluations: 0, flagged: 0, blocked: 0, degraded: 0 },
  );
  const policyRows = policies.data?.data ?? [];

  return (
    <AdminScreen
      refresh={() => Promise.all([activity.refetch(), policies.refetch()])}
      consolePath="/guardrails"
    >
      {activity.isError ? (
        <LoadError what="guardrail activity" error={activity.error} />
      ) : null}

      <Text className="text-xs text-muted-foreground">
        Evaluations over the last {ACTIVITY_DAYS} days, counted across every
        brokered surface. Which requests were flagged is in the console — this
        read carries counts only.
      </Text>

      <View className="flex-row gap-2">
        <StatTile
          label="Evaluations"
          value={totals.evaluations.toLocaleString()}
        />
        <StatTile label="Flagged" value={totals.flagged.toLocaleString()} />
      </View>
      <View className="flex-row gap-2">
        <StatTile label="Blocked" value={totals.blocked.toLocaleString()} />
        <StatTile
          label="Degraded"
          value={totals.degraded.toLocaleString()}
          detail="engine could not answer"
        />
      </View>

      {policies.isError ? (
        <LoadError what="guardrail policies" error={policies.error} />
      ) : null}

      <View className="gap-2">
        <SectionLabel>Policies</SectionLabel>
        {policyRows.length === 0 ? (
          <Text className="text-sm text-muted-foreground">
            {policies.isLoading
              ? "Loading…"
              : "No guardrail policies are configured."}
          </Text>
        ) : (
          policyRows.map((policy) => (
            <PolicyCard key={policy.id} policy={policy} />
          ))
        )}
      </View>
    </AdminScreen>
  );
}

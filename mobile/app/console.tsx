import { useQuery } from "@tanstack/react-query";
import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import {
  Pressable,
  RefreshControl,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Card,
  ChipPill,
  ConsoleRow,
  SpendMeter,
  StatCell,
} from "../src/components/Console";
import { SectionLabel } from "../src/components/Controls";
import { Body, ErrorText, Screen } from "../src/components/Screen";
import { formatCompact, phaseChip } from "../src/lib/consoleLabels";
import { formatMicroUsd, isTerminal } from "../src/lib/consoleTypes";
import type { SandboxView, UserUsageRow } from "../src/lib/consoleTypes";
import { consoleSectionsFor } from "../src/lib/sections";
import type { ConsoleSectionId } from "../src/lib/sections";
import {
  isSandboxesNotEnabled,
  meQueries,
  sandboxQueries,
  usageQueries,
} from "../src/session/consoleQueries";
import { useActiveConnection } from "../src/session/store";

const SECTION_ROUTES: Record<ConsoleSectionId, { label: string; href: string }> =
  {
    sandboxes: { label: "Sandboxes", href: "/sandboxes" },
    activity: { label: "Activity", href: "/activity" },
    limits: { label: "My limits", href: "/limits" },
    catalog: { label: "Models & apps", href: "/catalog" },
    subscriptions: { label: "Subscriptions", href: "/subscriptions" },
    "shared-apps": { label: "Shared apps", href: "/shared-apps" },
  };

/** One live run in the Now strip: what it is doing and what it has cost. */
function NowCard({
  sandbox,
  onPress,
}: {
  sandbox: SandboxView;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={`Open sandbox: ${sandbox.task_prompt}`}
      onPress={onPress}
    >
      <Card className="w-64 gap-2.5">
        <Text
          className="text-sm font-medium text-foreground"
          numberOfLines={2}
        >
          {sandbox.task_prompt}
        </Text>
        <ChipPill chip={phaseChip(sandbox.phase ?? sandbox.state)} />
        <SpendMeter
          spendMicroUsd={sandbox.spend_microusd}
          ceilingMicroUsd={sandbox.spend_ceiling_microusd}
        />
      </Card>
    </Pressable>
  );
}

/**
 * The account picture, two rows of three. Every figure is this caller's own
 * row from the usage summary, and the billing classes stay apart in one cell
 * rather than being summed — adding them would invent a number nobody is
 * charged.
 */
function StatGrid({ me }: { me: UserUsageRow }) {
  const spendRows = [
    { label: "Metered", value: formatMicroUsd(me.estimated_cost_microusd) },
    ...(me.subscription_requests > 0
      ? [
          {
            label: "Covered",
            value: formatMicroUsd(me.subscription_cost_microusd),
          },
        ]
      : []),
    ...((me.provisioned_cost_microusd ?? 0) > 0
      ? [
          {
            label: "Provisioned",
            value: formatMicroUsd(me.provisioned_cost_microusd ?? 0),
          },
        ]
      : []),
    ...(me.credits_cost_microusd > 0
      ? [
          {
            label: "Credits",
            value: formatMicroUsd(me.credits_cost_microusd),
          },
        ]
      : []),
  ];
  return (
    <View className="flex-row flex-wrap gap-2">
      <StatCell
        label="Requests"
        value={me.inference_requests.toLocaleString()}
        {...(me.priced_requests < me.inference_requests
          ? { sub: "some requests unpriced" }
          : {})}
      />
      <StatCell label="Input tokens" value={formatCompact(me.input_tokens)} />
      <StatCell label="Output tokens" value={formatCompact(me.output_tokens)} />
      <StatCell label="Spend" rows={spendRows} />
      <StatCell label="Tool calls" value={me.tool_calls.toLocaleString()} />
      <StatCell label="App requests" value={me.app_requests.toLocaleString()} />
    </View>
  );
}

/**
 * The gateway hub: who you are on which installation, what is running right
 * now, what it has cost, and the console surfaces this session can reach.
 *
 * A separate hub from `/home` on purpose. Home is the machine's — sessions,
 * approvals, delivery — and this is the gateway's; both are reachable from
 * each other and neither has to win the first screen while #3314's design pass
 * is still ahead.
 */
export default function ConsoleScreen() {
  const router = useRouter();
  const connection = useActiveConnection();
  const sections = consoleSectionsFor(connection);
  const consoleGranted = sections.includes("sandboxes");
  const [refreshing, setRefreshing] = useState(false);

  const me = useQuery({ ...meQueries.identity(), enabled: !!connection });
  const viewerId = me.data?.user_id ?? connection?.identity?.user_id;

  // The Now strip is "your detached runs", so it always names the caller. An
  // administrator's unscoped read would answer the whole installation, which
  // is a different screen (#3402).
  const sandboxes = useQuery({
    ...sandboxQueries.list(viewerId ? { ownerId: viewerId } : {}),
    enabled: consoleGranted && !!viewerId,
  });
  const summary = useQuery({
    ...usageQueries.summary(),
    enabled: consoleGranted,
  });

  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void Promise.allSettled([
      me.refetch(),
      sandboxes.refetch(),
      summary.refetch(),
    ]).finally(() => setRefreshing(false));
  }, [me, sandboxes, summary]);

  if (!connection) {
    return (
      <Screen title="Gateway">
        <Body>Pair a gateway to reach its console.</Body>
      </Screen>
    );
  }

  const host = connection.gatewayUrl
    .replace(/^https?:\/\//, "")
    .replace(/\/+$/, "");
  const identity = me.data ?? connection.identity;
  const live = (sandboxes.data?.data ?? []).filter(
    (sandbox) => !isTerminal(sandbox.state),
  );
  // Not a load failure: the installation does not run sandboxes. The strip and
  // the row drop rather than rendering a card that blames the network.
  const sandboxesDisabled = isSandboxesNotEnabled(sandboxes.error);
  const scope = summary.data?.scope;
  // A member's self-scoped read carries exactly one `by_user` row; an
  // administrator's is installation-wide and matches on id.
  const mine =
    scope === "self"
      ? summary.data?.by_user?.[0]
      : summary.data?.by_user?.find((row) => row.user_id === viewerId);

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView
        contentContainerClassName="gap-4 px-5 py-6"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <Card className="gap-0.5">
          <Text className="text-base font-medium text-foreground" numberOfLines={1}>
            {host}
          </Text>
          <Text className="text-xs text-muted-foreground" numberOfLines={1}>
            {identity
              ? `Signed in as ${identity.display_name || identity.email || identity.user_id}`
              : "Reading your identity…"}
          </Text>
        </Card>

        {!consoleGranted ? (
          <Card className="gap-1">
            <Text className="text-sm font-medium text-foreground">
              Read-only member surfaces
            </Text>
            <Text className="text-xs text-muted-foreground">
              This pairing was granted the self-service surfaces but not the
              gateway console. Sandboxes, activity, and limits appear after
              signing in again to a gateway that offers them.
            </Text>
          </Card>
        ) : sandboxesDisabled ? null : (
          <View className="gap-2">
            <SectionLabel>Now</SectionLabel>
            {sandboxes.isError ? (
              <ErrorText>
                {sandboxes.error instanceof Error
                  ? sandboxes.error.message
                  : "Could not load your sandboxes."}
              </ErrorText>
            ) : live.length > 0 ? (
              <ScrollView
                horizontal
                showsHorizontalScrollIndicator={false}
                contentContainerClassName="gap-3"
              >
                {live.map((sandbox) => (
                  <NowCard
                    key={sandbox.id}
                    sandbox={sandbox}
                    onPress={() =>
                      router.push({
                        pathname: "/sandbox/[id]",
                        params: { id: sandbox.id },
                      })
                    }
                  />
                ))}
              </ScrollView>
            ) : sandboxes.isLoading ? null : (
              <Text className="text-sm text-muted-foreground">
                No sandboxes running.
              </Text>
            )}
          </View>
        )}

        {mine ? <StatGrid me={mine} /> : null}

        <View className="gap-2">
          <SectionLabel>Gateway</SectionLabel>
          <View className="rounded-xl border border-border bg-background px-4">
            {sections
              .filter(
                (section) => !(section === "sandboxes" && sandboxesDisabled),
              )
              .map((section, index) => (
                <ConsoleRow
                  key={section}
                  label={SECTION_ROUTES[section].label}
                  first={index === 0}
                  onPress={() => router.push(SECTION_ROUTES[section].href)}
                />
              ))}
          </View>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

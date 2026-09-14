import { useQuery } from "@tanstack/react-query";
import { useRouter } from "expo-router";
import { useCallback, useMemo, useState } from "react";
import {
  FlatList,
  Pressable,
  RefreshControl,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Card,
  ChipPill,
  EmptyState,
  SpendMeter,
} from "../src/components/Console";
import { phaseChip } from "../src/lib/consoleLabels";
import { relative } from "../src/lib/consoleTime";
import type { SandboxView } from "../src/lib/consoleTypes";
import {
  ALL_STATUS_GROUP_KEYS,
  groupOfState,
  matchesFilter,
  matchesStatusSelection,
  needsAttention,
  SANDBOX_STATUS_GROUPS,
  concurrencyOccupancy,
  schedulingPressureNote,
  statesForGroups,
} from "../src/lib/sandboxStatus";
import type { SandboxStatusGroupKey } from "../src/lib/sandboxStatus";
import {
  isSandboxesNotEnabled,
  meQueries,
  sandboxQueries,
} from "../src/session/consoleQueries";

function StatusChips({
  selected,
  counts,
  onChange,
}: {
  selected: SandboxStatusGroupKey[];
  counts: Partial<Record<SandboxStatusGroupKey, number>>;
  onChange: (next: SandboxStatusGroupKey[]) => void;
}) {
  return (
    <View className="flex-row flex-wrap gap-2">
      {SANDBOX_STATUS_GROUPS.map((group) => {
        const active = selected.includes(group.key);
        const count = counts[group.key];
        return (
          <Pressable
            key={group.key}
            accessibilityRole="button"
            accessibilityState={{ selected: active }}
            // Never empty the selection: an empty one fetches nothing and
            // reads as "no sandboxes" rather than as a filter nobody chose.
            onPress={() => {
              const next = active
                ? selected.filter((key) => key !== group.key)
                : [...selected, group.key];
              onChange(next.length === 0 ? [group.key] : next);
            }}
            className={`rounded-full border px-3 py-1.5 ${
              active
                ? "border-primary bg-primary"
                : "border-border bg-background"
            }`}
          >
            <Text
              className={`text-xs font-medium ${
                active ? "text-primary-foreground" : "text-muted-foreground"
              }`}
            >
              {group.label}
              {active && count !== undefined ? ` ${count}` : ""}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}

function SandboxRow({
  sandbox,
  attention,
  onPress,
}: {
  sandbox: SandboxView;
  attention: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={sandbox.task_prompt}
      onPress={onPress}
    >
      <Card
        className={`gap-2.5 ${attention ? "border-warning-border bg-warning-background" : ""}`}
      >
        {attention ? (
          <Text className="text-xs font-medium uppercase tracking-wide text-warning-foreground">
            Waiting on you
          </Text>
        ) : null}
        <Text className="text-base font-medium text-foreground" numberOfLines={2}>
          {sandbox.task_prompt}
        </Text>
        <View className="flex-row items-center justify-between gap-2">
          <ChipPill chip={phaseChip(sandbox.phase ?? sandbox.state)} />
          <Text className="text-xs text-muted-foreground" numberOfLines={1}>
            {sandbox.harness}
            {sandbox.repository_url
              ? ` · ${sandbox.repository_url.split("/").slice(-1)[0]}`
              : " · research"}
          </Text>
        </View>
        {sandbox.phase_detail ? (
          <Text className="text-sm text-muted-foreground" numberOfLines={1}>
            {sandbox.phase_detail}
          </Text>
        ) : null}
        <SpendMeter
          spendMicroUsd={sandbox.spend_microusd}
          ceilingMicroUsd={sandbox.spend_ceiling_microusd}
        />
        <Text className="text-xs text-muted-foreground">
          started {relative(sandbox.created_at)}
        </Text>
      </Card>
    </Pressable>
  );
}

/**
 * The caller's own runs.
 *
 * Deliberately not a fleet view: the read is pinned to the signed-in account,
 * so an administrator sees their own runs here exactly as a member does. The
 * installation-wide list, its owner attribution, and the administrator cancel
 * belong to slice #3402.
 */
export default function SandboxesScreen() {
  const router = useRouter();
  const [selected, setSelected] = useState<SandboxStatusGroupKey[]>([
    ...ALL_STATUS_GROUP_KEYS,
  ]);
  const [filter, setFilter] = useState("");
  const [refreshing, setRefreshing] = useState(false);

  const me = useQuery(meQueries.identity());
  const viewerId = me.data?.user_id;

  const query = useQuery({
    ...sandboxQueries.list({
      ...(viewerId ? { ownerId: viewerId } : {}),
      states: statesForGroups(selected),
    }),
    // Before the identity resolves there is no id to narrow with, and asking
    // unscoped would answer an administrator's whole installation under a
    // screen that says otherwise.
    enabled: !!viewerId,
  });
  const concurrency = useQuery(sandboxQueries.concurrency());

  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void Promise.allSettled([query.refetch(), concurrency.refetch()]).finally(
      () => setRefreshing(false),
    );
  }, [query, concurrency]);

  const fetched = query.data?.data ?? [];

  // Re-applied locally because the server-side `states` filter is additive: a
  // gateway older than it ignores the parameter and answers unfiltered, and
  // the chips would then be decoration over an unfiltered list.
  const inSelection = useMemo(
    () => fetched.filter((s) => matchesStatusSelection(s.state, selected)),
    [fetched, selected],
  );

  const counts = useMemo(() => {
    const tally: Partial<Record<SandboxStatusGroupKey, number>> = {};
    for (const key of selected) {
      tally[key] = 0;
    }
    for (const sandbox of inSelection) {
      // A state no group covers is shown but not counted: attributing it to
      // one of them would be a guess.
      const key = groupOfState(sandbox.state);
      if (key !== undefined && key in tally) {
        tally[key] = (tally[key] ?? 0) + 1;
      }
    }
    return tally;
  }, [inSelection, selected]);

  const sandboxes = useMemo(() => {
    const matched = inSelection.filter((sandbox) =>
      matchesFilter(
        filter,
        sandbox.task_prompt,
        sandbox.profile_name,
        sandbox.harness,
        sandbox.repository_url,
      ),
    );
    // Parked runs first, newest-first order preserved inside each half: the
    // rest of the list is a record, these are the ones nothing advances until
    // this reader steers them.
    return [
      ...matched.filter((s) => needsAttention(s, viewerId)),
      ...matched.filter((s) => !needsAttention(s, viewerId)),
    ];
  }, [inSelection, filter, viewerId]);

  if (isSandboxesNotEnabled(query.error)) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Sandboxes are not enabled here"
          detail="This installation does not run detached agents. An administrator turns them on in deployment configuration."
        />
      </SafeAreaView>
    );
  }

  const searching = filter.trim().length > 0;
  const allStatuses = selected.length >= ALL_STATUS_GROUP_KEYS.length;
  const statusWords = SANDBOX_STATUS_GROUPS.filter((group) =>
    selected.includes(group.key),
  ).map((group) => group.label.toLowerCase());
  const statusPhrase =
    statusWords.length > 1
      ? `${statusWords.slice(0, -1).join(", ")} or ${statusWords[statusWords.length - 1]}`
      : statusWords.join("");
  const occupancy = concurrency.data?.data;

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <FlatList
        className="flex-1"
        contentContainerClassName="gap-3 px-5 py-4"
        data={sandboxes}
        keyExtractor={(sandbox) => sandbox.id}
        keyboardShouldPersistTaps="handled"
        renderItem={({ item }) => (
          <SandboxRow
            sandbox={item}
            attention={needsAttention(item, viewerId)}
            onPress={() =>
              router.push({ pathname: "/sandbox/[id]", params: { id: item.id } })
            }
          />
        )}
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
        ListHeaderComponent={
          <View className="gap-3 pb-1">
            <StatusChips
              selected={selected}
              counts={counts}
              onChange={setSelected}
            />
            <TextInput
              value={filter}
              onChangeText={setFilter}
              placeholder="Filter by task, repo, or profile"
              placeholderTextColor="#697386"
              accessibilityLabel="Filter sandboxes"
              className="min-h-11 rounded-lg border border-border bg-background px-3 py-2.5 text-base text-foreground"
            />
            {occupancy ? (
              <View className="gap-1">
                <Text className="text-xs text-muted-foreground">
                  {concurrencyOccupancy(occupancy)}
                </Text>
                {/* A slot held by a pod no node would take frees when the
                    cluster finds capacity, not when a task finishes — so the
                    counts above are not the whole answer to "why was I
                    refused". */}
                {schedulingPressureNote(occupancy) ? (
                  <Text className="text-xs text-muted-foreground">
                    {schedulingPressureNote(occupancy)}
                  </Text>
                ) : null}
              </View>
            ) : null}
          </View>
        }
        ListEmptyComponent={
          query.isLoading || !viewerId ? null : query.isError ? (
            <EmptyState
              title="Couldn’t load your sandboxes"
              {...(query.error instanceof Error
                ? { detail: query.error.message }
                : {})}
            />
          ) : searching ? (
            <EmptyState
              title="Nothing matches this search"
              detail={`No run’s task, repository, or profile matches “${filter.trim()}”.`}
            />
          ) : allStatuses ? (
            <EmptyState
              title="No sandboxes"
              detail="Hand a task to a sandbox from a harness and it appears here."
            />
          ) : (
            // Only the selected statuses were fetched, so the copy reports
            // what is absent and points at the control rather than claiming
            // that widening would reveal anything.
            <EmptyState
              title={`No ${statusPhrase} runs`}
              detail="Other statuses are behind the chips above."
            />
          )
        }
      />
    </SafeAreaView>
  );
}

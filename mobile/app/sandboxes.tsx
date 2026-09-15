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
import { ConsoleLink } from "../src/components/Admin";
import { ownerDirectory, ownerLabel } from "../src/lib/admin";
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
import { administers } from "../src/lib/sections";
import {
  adminQueries,
  isSandboxesNotEnabled,
  meQueries,
  sandboxQueries,
} from "../src/session/consoleQueries";
import { useActiveConnection } from "../src/session/store";

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
  owner,
  attention,
  onPress,
}: {
  sandbox: SandboxView;
  owner?: string | undefined;
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
          {owner ? `${owner} · ` : ""}started {relative(sandbox.created_at)}
        </Text>
      </Card>
    </Pressable>
  );
}

/** Whose runs the list is showing. Administrators only: a member has one scope. */
function ScopeTab({
  label,
  active,
  onPress,
}: {
  label: string;
  active: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="tab"
      accessibilityState={{ selected: active }}
      onPress={onPress}
      className={`min-h-11 flex-1 items-center justify-center rounded-lg border ${
        active ? "border-primary bg-primary" : "border-border bg-background"
      }`}
    >
      <Text
        className={`text-sm font-medium ${
          active ? "text-primary-foreground" : "text-muted-foreground"
        }`}
      >
        {label}
      </Text>
    </Pressable>
  );
}

/**
 * The run list: one's own by default, the whole installation for an
 * administrator who asks.
 *
 * An administrator's unscoped read is installation-wide — the server widens
 * it, not the client — which is the point of the Everyone tab but also means
 * the rows are not all the reader's own. Hence the owner line: a list of other
 * people's runs that never says whose is a list that quietly invites
 * misreading.
 */
export default function SandboxesScreen() {
  const router = useRouter();
  const connection = useActiveConnection();
  const isAdmin = administers(connection);
  const [selected, setSelected] = useState<SandboxStatusGroupKey[]>([
    ...ALL_STATUS_GROUP_KEYS,
  ]);
  const [mineOnly, setMineOnly] = useState(true);
  const [filter, setFilter] = useState("");
  const [refreshing, setRefreshing] = useState(false);

  const me = useQuery(meQueries.identity());
  const viewerId = me.data?.user_id;

  // A member is always narrowed to themself; "Everyone" drops the parameter
  // and lets the server's installation-wide widening answer.
  const scopedToSelf = !isAdmin || mineOnly;
  const query = useQuery({
    ...sandboxQueries.list({
      ...(scopedToSelf && viewerId ? { ownerId: viewerId } : {}),
      states: statesForGroups(selected),
    }),
    // Narrowing to self before the identity resolves has no id to narrow with,
    // and asking unscoped would answer an administrator's whole installation
    // under a control that says otherwise. The fleet read needs no id.
    enabled: !scopedToSelf || !!viewerId,
  });
  const concurrency = useQuery(sandboxQueries.concurrency());
  // Names for the owner line. The directory read is refused for a member, and
  // a member's rows are all their own, so there is nothing to disambiguate.
  const people = useQuery({ ...adminQueries.people(), enabled: isAdmin });

  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void Promise.allSettled([query.refetch(), concurrency.refetch()]).finally(
      () => setRefreshing(false),
    );
  }, [query, concurrency]);

  const ownerNames = useMemo(
    () => ownerDirectory(people.data?.data ?? []),
    [people.data],
  );

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
        // The rendered label rather than the directory lookup, so a row that
        // fell back to an id prefix is still reachable by typing it.
        ownerLabel(sandbox.user_id, ownerNames, viewerId),
      ),
    );
    // Parked runs first, newest-first order preserved inside each half: the
    // rest of the list is a record, these are the ones nothing advances until
    // this reader steers them.
    return [
      ...matched.filter((s) => needsAttention(s, viewerId)),
      ...matched.filter((s) => !needsAttention(s, viewerId)),
    ];
  }, [inSelection, filter, ownerNames, viewerId]);

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
  // Which scope the empty copy is talking about. An administrator's tab is
  // easy to lose track of, and a bare "No sandboxes" under Mine would read as
  // a claim about the whole installation. A member has only one scope, so
  // their copy names none.
  const scopePhrase = !isAdmin
    ? ""
    : mineOnly
      ? " of yours"
      : " in this installation";
  const scopedNoun = scopedToSelf
    ? "your sandboxes"
    : "the installation’s sandboxes";

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
            owner={ownerLabel(item.user_id, ownerNames, viewerId)}
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
              placeholder={
                isAdmin && !mineOnly
                  ? "Filter by task, repo, profile, or owner"
                  : "Filter by task, repo, or profile"
              }
              placeholderTextColor="#697386"
              accessibilityLabel="Filter sandboxes"
              className="min-h-11 rounded-lg border border-border bg-background px-3 py-2.5 text-base text-foreground"
            />
            {isAdmin ? (
              <View className="flex-row gap-2">
                <ScopeTab
                  label="Mine"
                  active={mineOnly}
                  onPress={() => setMineOnly(true)}
                />
                <ScopeTab
                  label="Everyone"
                  active={!mineOnly}
                  onPress={() => setMineOnly(false)}
                />
              </View>
            ) : null}
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
          query.isLoading || (scopedToSelf && !viewerId) ? null : query.isError ? (
            <EmptyState
              title={`Couldn’t load ${scopedNoun}`}
              {...(query.error instanceof Error
                ? { detail: query.error.message }
                : {})}
            />
          ) : searching ? (
            <EmptyState
              title="Nothing matches this search"
              detail={`No run’s task, repository, profile, or owner matches “${filter.trim()}”.`}
            />
          ) : allStatuses ? (
            <EmptyState
              title={`No sandboxes${scopePhrase}`}
              detail="Hand a task to a sandbox from a harness and it appears here."
            />
          ) : (
            // Only the selected statuses were fetched, so the copy reports
            // what is absent and points at the control rather than claiming
            // that widening would reveal anything.
            <EmptyState
              title={`No ${statusPhrase} runs${scopePhrase}`}
              detail="Other statuses are behind the chips above."
            />
          )
        }
        ListFooterComponent={
          /* The console's sandbox index is administrator-only, so the hand-off
             is offered only to one. Spawning, and every fleet verb beyond the
             cancel on a run's own page, lives there. */
          isAdmin ? (
            <View className="pt-1">
              <ConsoleLink webPath="/sandboxes" />
            </View>
          ) : null
        }
      />
    </SafeAreaView>
  );
}

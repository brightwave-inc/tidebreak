import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Text, View } from "react-native";
import {
  AdminScreen,
  FilterField,
  LoadError,
  RowGroup,
} from "../../src/components/Admin";
import { StatTile } from "../../src/components/Console";
import { StatusPill } from "../../src/components/Controls";
import type { Team } from "../../src/lib/gatewayAdmin";
import { matchesFilter } from "../../src/lib/sandboxStatus";
import { adminQueries } from "../../src/session/consoleQueries";

/** How many matches are rendered; the filter is how you reach the rest. */
const SHOWN = 25;

function count(value: number, singular: string): string {
  return `${value.toLocaleString()} ${singular}${value === 1 ? "" : "s"}`;
}

function TeamRow({ team }: { team: Team }) {
  return (
    <View className="gap-1">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {team.name}
        </Text>
        {team.enabled ? null : (
          <StatusPill tone="warning">disabled</StatusPill>
        )}
      </View>
      <Text className="text-xs text-muted-foreground" numberOfLines={1}>
        {count(team.member_count, "member")} · {count(team.model_count, "model")}{" "}
        · {count(team.app_count, "app")}
      </Text>
    </View>
  );
}

/**
 * Teams and the reach they carry.
 *
 * Disabling a team suspends every grant it holds without deleting them, so a
 * disabled team with grants is a live fact about who cannot route right now —
 * which is why the pill warns rather than reading as a neutral state.
 */
export default function AdminTeamsScreen() {
  const query = useQuery(adminQueries.teams());
  const [filter, setFilter] = useState("");

  const teams = query.data?.data ?? [];
  const matches = teams.filter((team) =>
    matchesFilter(filter, team.name, team.slug),
  );
  const disabled = teams.filter((team) => !team.enabled).length;
  const members = teams.reduce((sum, team) => sum + team.member_count, 0);

  return (
    <AdminScreen refresh={() => query.refetch()} consolePath="/teams">
      {query.isError ? <LoadError what="teams" error={query.error} /> : null}

      <View className="flex-row gap-2">
        <StatTile label="Teams" value={teams.length.toLocaleString()} />
        <StatTile
          label="Memberships"
          value={members.toLocaleString()}
          detail="one account can hold several"
        />
        <StatTile label="Disabled" value={disabled.toLocaleString()} />
      </View>

      <FilterField
        value={filter}
        onChange={setFilter}
        placeholder="Filter by name or slug"
      />

      <RowGroup
        title="Teams"
        rows={matches.slice(0, SHOWN)}
        rowKey={(team) => team.id}
        empty={query.isLoading ? "Loading…" : "No teams match."}
        renderRow={(team) => <TeamRow team={team} />}
      />

      {matches.length > SHOWN ? (
        <Text className="text-xs text-muted-foreground">
          Showing {SHOWN} of {matches.length} matches — narrow the filter to
          reach the rest.
        </Text>
      ) : null}
    </AdminScreen>
  );
}

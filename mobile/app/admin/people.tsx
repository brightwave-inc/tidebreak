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
import { PEOPLE_PAGE_SIZE } from "../../src/lib/admin";
import type { Person } from "../../src/lib/gatewayAdmin";
import { matchesFilter } from "../../src/lib/sandboxStatus";
import { adminQueries } from "../../src/session/consoleQueries";

/** How many matches are rendered; the filter is how you reach the rest. */
const SHOWN = 25;

function PersonRow({ person }: { person: Person }) {
  const name = person.display_name || person.email || person.username;
  const secondary = person.display_name ? person.email : null;
  return (
    <View className="gap-1">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {name || "Unnamed account"}
        </Text>
        <View className="flex-row gap-1.5">
          {person.kind === "service" ? (
            <StatusPill tone="info">service</StatusPill>
          ) : null}
          {person.is_admin ? <StatusPill tone="live">admin</StatusPill> : null}
          {person.is_active ? null : (
            <StatusPill tone="warning">inactive</StatusPill>
          )}
        </View>
      </View>
      {secondary ? (
        <Text className="text-xs text-muted-foreground" numberOfLines={1}>
          {secondary}
        </Text>
      ) : null}
    </View>
  );
}

/**
 * The installation's user directory. The whole list arrives in one read and
 * narrowing happens on the device: the endpoint's `search` would cost a round
 * trip per keystroke to answer a question the loaded rows already answer.
 */
export default function AdminPeopleScreen() {
  const query = useQuery(adminQueries.people());
  const [filter, setFilter] = useState("");

  const people = query.data?.data ?? [];
  const matches = people.filter((person) =>
    matchesFilter(filter, person.display_name, person.email, person.username),
  );
  const admins = people.filter((person) => person.is_admin).length;
  const inactive = people.filter((person) => !person.is_active).length;

  return (
    <AdminScreen refresh={() => query.refetch()} consolePath="/people">
      {query.isError ? (
        <LoadError what="the directory" error={query.error} />
      ) : null}

      <View className="flex-row gap-2">
        <StatTile
          label="Accounts"
          value={people.length.toLocaleString()}
          detail={
            people.length >= PEOPLE_PAGE_SIZE ? "first page only" : undefined
          }
        />
        <StatTile label="Administrators" value={admins.toLocaleString()} />
        <StatTile label="Inactive" value={inactive.toLocaleString()} />
      </View>

      <FilterField
        value={filter}
        onChange={setFilter}
        placeholder="Filter by name or email"
      />

      <RowGroup
        title="People"
        rows={matches.slice(0, SHOWN)}
        rowKey={(person) => person.id}
        empty={query.isLoading ? "Loading…" : "No accounts match."}
        renderRow={(person) => <PersonRow person={person} />}
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

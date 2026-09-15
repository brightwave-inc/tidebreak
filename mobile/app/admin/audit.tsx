import { useQueries } from "@tanstack/react-query";
import { useState } from "react";
import { Platform, Pressable, Text, View } from "react-native";
import {
  AdminScreen,
  LoadError,
  RowGroup,
} from "../../src/components/Admin";
import { StatTile } from "../../src/components/Console";
import { StatusPill } from "../../src/components/Controls";
import {
  isAuditFailure,
  ledgerCursor,
  recentAuditFailures,
  withNextAuditPage,
} from "../../src/lib/admin";
import { clockTime, dayLabel } from "../../src/lib/consoleTime";
import type { AuditEvent } from "../../src/lib/gatewayAdmin";
import { adminQueries } from "../../src/session/consoleQueries";

/** Action slugs are identifiers; a proportional font makes them hard to scan. */
const MONO = Platform.select({ ios: "Menlo", default: "monospace" });

const DAY_MS = 24 * 60 * 60 * 1000;

const OUTCOME_TONE: Record<string, "critical" | "warning"> = {
  failed: "critical",
  denied: "warning",
};

/** Today's rows are worth a clock; older ones only need the day. */
function when(iso: string): string {
  const day = dayLabel(iso);
  return day === "Today" ? clockTime(iso) : day;
}

function EventRow({ event }: { event: AuditEvent }) {
  return (
    <View className="gap-1">
      <View className="flex-row items-center gap-2">
        <Text className="text-xs text-muted-foreground">
          {when(event.occurred_at)}
        </Text>
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {event.actor}
        </Text>
        {event.occurrence_count > 1 ? (
          <StatusPill>×{event.occurrence_count}</StatusPill>
        ) : null}
        {isAuditFailure(event) ? (
          <StatusPill tone={OUTCOME_TONE[event.outcome] ?? "warning"}>
            {event.outcome}
          </StatusPill>
        ) : null}
      </View>
      <Text
        className="text-xs text-muted-foreground"
        numberOfLines={1}
        style={{ fontFamily: MONO }}
      >
        {event.action}
      </Text>
    </View>
  );
}

function FilterChip({
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
      accessibilityRole="button"
      accessibilityState={{ selected: active }}
      onPress={onPress}
      className={`rounded-full border px-3 py-1.5 ${
        active ? "border-primary bg-primary" : "border-border bg-background"
      }`}
    >
      <Text
        className={`text-xs font-medium ${
          active ? "text-primary-foreground" : "text-muted-foreground"
        }`}
      >
        {label}
      </Text>
    </Pressable>
  );
}

/**
 * The ledger, read for triage rather than inventory: what has been refused or
 * has broken recently, and who was acting.
 *
 * Paging is keyset — each loaded page is its own query keyed by the cursor that
 * produced it — so the counts below describe exactly the pages loaded and say
 * so, instead of implying a window nobody asked the gateway for. A pull
 * refreshes every loaded page rather than dropping back to the first, which
 * would silently discard what was already read.
 */
export default function AdminAuditScreen() {
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  const [failedOnly, setFailedOnly] = useState(false);

  const pages = useQueries({
    queries: cursors.map((cursor) => adminQueries.auditEvents(cursor)),
  });
  const events = pages.flatMap((page) => page.data?.data ?? []);
  const nextCursor = ledgerCursor(pages.map((page) => page.data));
  const failure = pages.find((page) => page.isError);
  const failures = recentAuditFailures(events, DAY_MS);
  const shown = failedOnly ? events.filter(isAuditFailure) : events;

  return (
    <AdminScreen
      refresh={() => Promise.all(pages.map((page) => page.refetch()))}
      consolePath="/audit"
    >
      {failure ? <LoadError what="the ledger" error={failure.error} /> : null}

      <View className="flex-row gap-2">
        <StatTile
          label="Failures, 24h"
          value={failures.toLocaleString()}
          detail={`of the last ${events.length} events loaded`}
        />
      </View>

      <View className="flex-row gap-2">
        <FilterChip
          label="All"
          active={!failedOnly}
          onPress={() => setFailedOnly(false)}
        />
        <FilterChip
          label="Failed"
          active={failedOnly}
          onPress={() => setFailedOnly(true)}
        />
      </View>

      <RowGroup
        title="Recent activity"
        rows={shown}
        rowKey={(event) => event.id}
        empty={
          pages.some((page) => page.isLoading)
            ? "Loading…"
            : failedOnly
              ? "Nothing failed in the pages loaded."
              : "The ledger is empty."
        }
        renderRow={(event) => <EventRow event={event} />}
      />

      {nextCursor ? (
        <Pressable
          accessibilityRole="button"
          onPress={() =>
            setCursors((current) => withNextAuditPage(current, nextCursor))
          }
          className="min-h-11 items-center justify-center rounded-lg border border-border bg-background px-4 py-3"
        >
          <Text className="text-sm font-medium text-foreground">Load more</Text>
        </Pressable>
      ) : events.length > 0 ? (
        <Text className="text-center text-xs text-muted-foreground">
          End of the ledger.
        </Text>
      ) : null}
    </AdminScreen>
  );
}

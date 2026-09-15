/**
 * The administration screens' shared furniture.
 *
 * Built on the console's own primitives (`Console.tsx`) and this repo's tokens
 * rather than on a second visual language: the administration pages are
 * deliberately thinner than the gateway's web console — a phone is where an
 * administrator checks something, not where they configure it — so what they
 * need is scaffolding, not chrome.
 */

import { useCallback, useState, type ReactNode } from "react";
import {
  Linking,
  Pressable,
  RefreshControl,
  ScrollView,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, EmptyState, Meter } from "./Console";
import { StatusPill } from "./Controls";
import { stamp } from "../lib/consoleTime";
import { formatMicroUsd } from "../lib/consoleTypes";
import type { CostLimit } from "../lib/gatewayAdmin";
import { administers } from "../lib/sections";
import { useActiveConnection } from "../session/store";

/**
 * Wraps an administration page. The gateway is the real gate — every read
 * behind these screens is refused for a member — so this exists for product
 * coherence: somebody who reaches one of these routes should read why it is
 * empty rather than a stack of refusals.
 */
export function AdminGate({ children }: { children: ReactNode }) {
  const connection = useActiveConnection();
  if (!administers(connection)) {
    return (
      <EmptyState
        title="Administrators only"
        detail="This account doesn’t administer this gateway, so there is nothing here to show."
      />
    );
  }
  return <>{children}</>;
}

/**
 * The shape every administration page shares: gated, pull-to-refresh, and
 * ending in the console hand-off.
 *
 * `consolePath` is omitted by a page that carries a hand-off per section
 * instead of one for the whole screen.
 */
export function AdminScreen({
  refresh,
  consolePath,
  children,
}: {
  refresh: () => Promise<unknown>;
  consolePath?: string;
  children: ReactNode;
}) {
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void refresh().finally(() => setRefreshing(false));
  }, [refresh]);

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <AdminGate>
        <ScrollView
          className="flex-1"
          contentContainerClassName="gap-3 px-5 py-4"
          keyboardShouldPersistTaps="handled"
          refreshControl={
            <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
          }
        >
          {children}
          {consolePath ? <ConsoleLink webPath={consolePath} /> : null}
        </ScrollView>
      </AdminGate>
    </SafeAreaView>
  );
}

/**
 * A read that failed, named.
 *
 * An administration page runs several reads at once and one refusal must not
 * blank the others: an empty section and a section that could not be read are
 * different facts, and only one of them is worth acting on.
 */
export function LoadError({ what, error }: { what: string; error: unknown }) {
  return (
    <View className="gap-1 rounded-xl border border-border bg-muted p-3">
      <Text className="text-sm font-medium text-foreground">
        Couldn’t load {what}
      </Text>
      {error instanceof Error ? (
        <Text className="text-xs text-muted-foreground">{error.message}</Text>
      ) : null}
    </View>
  );
}

/**
 * The hand-off an administration page ends with.
 *
 * These screens are read-only summaries; the gateway's own console is where
 * the same subject is administered, so the row says where to go rather than
 * leaving the page looking like the whole story.
 *
 * Every caller owns the other half: `webPath` must be a destination this
 * reader can actually open. Most console paths are administrator-only, so a
 * screen a member can reach has to gate the row on `administers` rather than
 * hand them a redirect — which is why `AdminScreen` may render it
 * unconditionally and the member screens may not.
 *
 * `writesHere` suppresses the supporting line on a surface that carries its
 * own controls, so a page with a working button directly above does not tell
 * the reader to go elsewhere for it.
 */
export function ConsoleLink({
  webPath,
  writesHere = false,
}: {
  webPath: string;
  writesHere?: boolean;
}) {
  const connection = useActiveConnection();
  const baseUrl = connection?.gatewayUrl;
  if (!baseUrl) {
    return null;
  }
  const url = `${baseUrl.replace(/\/+$/, "")}${webPath}`;
  return (
    <View className="gap-1.5">
      <Pressable
        accessibilityRole="link"
        accessibilityLabel="Open in gateway console"
        accessibilityHint="Opens the gateway console in your browser, where you’ll sign in."
        onPress={() => void Linking.openURL(url)}
        className="min-h-11 flex-row items-center justify-center gap-2 rounded-lg border border-border bg-background px-4 py-3"
      >
        <Text className="text-sm font-medium text-muted-foreground">
          Open in gateway console ↗
        </Text>
      </Pressable>
      {writesHere ? null : (
        <Text className="text-center text-xs text-muted-foreground">
          These screens read only — changes are made in the console.
        </Text>
      )}
    </View>
  );
}

/** The one-line narrowing control every directory-shaped page carries. */
export function FilterField({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (next: string) => void;
  placeholder: string;
}) {
  return (
    <TextInput
      value={value}
      onChangeText={onChange}
      placeholder={placeholder}
      placeholderTextColor="#697386"
      accessibilityLabel={placeholder}
      autoCapitalize="none"
      autoCorrect={false}
      className="min-h-11 rounded-lg border border-border bg-background px-3 py-2.5 text-base text-foreground"
    />
  );
}

/** A group of rows under a heading, rendered as one bordered card. */
export function RowGroup<T>({
  title,
  rows,
  rowKey,
  renderRow,
  empty,
}: {
  title: string;
  rows: readonly T[];
  rowKey: (row: T, index: number) => string;
  renderRow: (row: T, index: number) => ReactNode;
  empty: string;
}) {
  return (
    <View className="gap-2">
      <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {title}
      </Text>
      <Card className="py-0">
        {rows.length === 0 ? (
          <Text className="py-3 text-sm text-muted-foreground">{empty}</Text>
        ) : (
          rows.map((row, index) => (
            <View
              key={rowKey(row, index)}
              className={
                index === 0 ? "gap-1 py-3" : "gap-1 border-t border-border py-3"
              }
            >
              {renderRow(row, index)}
            </View>
          ))
        )}
      </Card>
    </View>
  );
}

/**
 * One cap on the installation.
 *
 * Reaching a bound an administrator configured is not a malfunction, so both
 * the warned and the exceeded state warn rather than alarm; what separates
 * them is the words, and `resets_at` says when it ends by itself.
 */
export function AdminLimitCard({
  policy,
  scopeLabel,
}: {
  policy: CostLimit;
  scopeLabel: string;
}) {
  const named =
    policy.scope_label && policy.scope_label !== scopeLabel
      ? policy.scope_label
      : null;
  return (
    <Card className="gap-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {named ?? scopeLabel}
        </Text>
        {policy.exceeded ? (
          <StatusPill tone="warning">Limit reached</StatusPill>
        ) : policy.warning_reached ? (
          <StatusPill tone="warning">Near limit</StatusPill>
        ) : null}
      </View>
      <Text className="text-xs text-muted-foreground">
        {formatMicroUsd(policy.limit_microusd)} limit
        {policy.applies_to === "sandbox" ? " · sandbox spend only" : ""}
        {policy.enabled ? "" : " · not enforced"}
      </Text>
      <Meter
        fraction={policy.used_fraction}
        warn={policy.exceeded || policy.warning_reached}
      />
      <View className="flex-row justify-between">
        <Text className="text-xs text-muted-foreground">
          {formatMicroUsd(policy.current_spend_microusd)} this window
        </Text>
        <Text className="text-xs text-muted-foreground">
          resets {stamp(policy.resets_at)}
        </Text>
      </View>
      {policy.limit_microusd === 0 && policy.enabled ? (
        <Text className="text-xs text-warning-foreground">
          A zero cap denies this scope’s metered inference outright.
        </Text>
      ) : null}
    </Card>
  );
}

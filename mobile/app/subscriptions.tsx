import { useQuery } from "@tanstack/react-query";
import * as WebBrowser from "expo-web-browser";
import { useCallback, useState } from "react";
import { RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, ChipPill, EmptyState, Meter } from "../src/components/Console";
import { Button, SectionLabel } from "../src/components/Controls";
import { subscriptionLimitChip } from "../src/lib/consoleLabels";
import { stampUnixSeconds } from "../src/lib/consoleTime";
import type {
  SubscriptionBinding,
  SubscriptionProvider,
  SubscriptionUsageWindow,
} from "../src/lib/consoleTypes";
import { subscriptionQueries } from "../src/session/consoleQueries";
import { useActiveConnection } from "../src/session/store";

/**
 * One quota window. `used_percent` arrives clamped, and `rejected` is the
 * provider's own verdict about which window refused a request — worth showing
 * even when the percentage has not caught up with it.
 */
function UsageWindowRow({ window }: { window: SubscriptionUsageWindow }) {
  return (
    <View className="gap-1">
      <View className="flex-row items-center justify-between gap-2">
        <Text className="flex-1 text-xs text-muted-foreground" numberOfLines={1}>
          {window.label || window.key}
          {window.model_scope ? ` · ${window.model_scope}` : ""}
        </Text>
        <Text className="text-xs text-muted-foreground">
          {Math.round(window.used_percent)}% used
        </Text>
      </View>
      <Meter
        fraction={window.used_percent / 100}
        warn={window.status === "rejected"}
      />
      {window.resets_at_unix_seconds != null ? (
        <Text className="text-xs text-muted-foreground">
          resets {stampUnixSeconds(window.resets_at_unix_seconds)}
        </Text>
      ) : null}
    </View>
  );
}

/**
 * One reachable account. Quota comes from the listing's stored snapshot; for an
 * account the live re-read can serve — the owner's own OpenAI binding — a lazy
 * per-card query refines it. Every other kind would be answered 422 or 404, so
 * it is never asked.
 */
function BindingCard({
  provider,
  binding,
}: {
  provider: SubscriptionProvider;
  binding: SubscriptionBinding;
}) {
  const liveUsageServable =
    binding.is_own &&
    binding.usage_supported &&
    provider.provider_kind === "openai";
  const live = useQuery({
    ...subscriptionQueries.usage(binding.binding_id),
    enabled: liveUsageServable,
  });

  const windows = live.data?.usage_windows ?? binding.usage_windows;
  const observedAt =
    live.data?.usage_updated_at_unix_seconds ??
    binding.usage_updated_at_unix_seconds;

  return (
    <Card className="gap-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {provider.name}
        </Text>
        {binding.reauthorization_required ? (
          <ChipPill chip={{ label: "Reconnect needed", tone: "warning" }} />
        ) : binding.limit_state ? (
          <ChipPill chip={subscriptionLimitChip(binding.limit_state)} />
        ) : null}
      </View>

      <Text className="text-sm text-foreground" numberOfLines={1}>
        {binding.label}
        {binding.account_hint && binding.account_hint !== binding.label
          ? ` · ${binding.account_hint}`
          : ""}
      </Text>

      <Text className="text-xs text-muted-foreground">
        {binding.is_own
          ? "your account"
          : `shared with you${binding.owner_email ? ` by ${binding.owner_email}` : ""}${
              binding.shared_via_team_name
                ? ` via ${binding.shared_via_team_name}`
                : ""
            }`}
        {binding.plan_hint ? ` · ${binding.plan_hint}` : ""}
      </Text>

      {windows.length > 0 ? (
        <View className="gap-2 pt-1">
          {windows.map((window) => (
            <UsageWindowRow key={window.key} window={window} />
          ))}
          {observedAt != null ? (
            <Text className="text-xs text-muted-foreground">
              observed {stampUnixSeconds(observedAt)}
            </Text>
          ) : null}
        </View>
      ) : binding.usage_supported ? (
        <Text className="text-xs text-muted-foreground">
          No quota reading yet — one is recorded the next time this account
          serves a request.
        </Text>
      ) : (
        <Text className="text-xs text-muted-foreground">
          This provider publishes no account quota the gateway can read.
        </Text>
      )}

      {binding.fallback_to_metered ? (
        <Text className="text-xs text-muted-foreground">
          Falls back to metered billing when this quota is exhausted.
        </Text>
      ) : null}
    </Card>
  );
}

/**
 * The provider accounts this member can actually use, with whatever quota the
 * gateway can see. Renaming, sharing, and resetting credits are owner actions
 * the account page keeps; connecting one needs a browser OAuth flow this client
 * cannot host.
 */
export default function SubscriptionsScreen() {
  const query = useQuery(subscriptionQueries.list());
  const connection = useActiveConnection();
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  if (query.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load your subscriptions"
          {...(query.error instanceof Error
            ? { detail: query.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }

  const providers = query.data?.providers ?? [];
  const accounts = providers.flatMap((provider) =>
    provider.bindings.map((binding) => ({ provider, binding })),
  );
  const connectUrl = connection
    ? `${connection.gatewayUrl.replace(/\/+$/, "")}/account/subscriptions`
    : null;

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-3 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        {accounts.length === 0 && !query.isLoading ? (
          <EmptyState
            title="No provider subscriptions connected"
            detail="Connecting one is a browser sign-in with the provider."
          />
        ) : (
          <>
            <SectionLabel>Accounts</SectionLabel>
            {accounts.map(({ provider, binding }) => (
              <BindingCard
                key={binding.binding_id}
                provider={provider}
                binding={binding}
              />
            ))}
          </>
        )}

        {connectUrl ? (
          <View className="gap-2 pt-1">
            <Button
              label="Connect in browser"
              variant="secondary"
              onPress={() => void WebBrowser.openBrowserAsync(connectUrl)}
            />
            <Text className="text-xs text-muted-foreground">
              Only the provider sign-in needs a browser — everything above reads
              here.
            </Text>
          </View>
        ) : null}
      </ScrollView>
    </SafeAreaView>
  );
}

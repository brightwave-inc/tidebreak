import { useRouter } from "expo-router";
import { useMemo, useState } from "react";
import {
  ActivityIndicator,
  Pressable,
  RefreshControl,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Body } from "../src/components/Screen";
import {
  attentionBadgeLabel,
  harnessLabel,
  lifecycleLabel,
} from "../src/lib/updates";
import { useMachineClient } from "../src/session/useMachineClient";
import { useHasSnapshot, useListedSessions } from "../src/session/updatesStore";
import { useUpdatesFeed } from "../src/session/useUpdatesFeed";
import { useSessionStore } from "../src/session/store";

export default function SessionsScreen() {
  const router = useRouter();
  const session = useSessionStore((state) => state.session);
  const client = useMachineClient();
  const { live, refresh } = useUpdatesFeed(client);
  const rows = useListedSessions();
  const hasSnapshot = useHasSnapshot();
  const [refreshing, setRefreshing] = useState(false);

  const signedOut = !session;
  const notAttached = !session?.machine;

  // An empty list is trustworthy only after a snapshot landed; a live socket
  // with no snapshot yet still means loading.
  const empty = useMemo(
    () => !signedOut && !notAttached && hasSnapshot && rows.length === 0,
    [hasSnapshot, notAttached, rows.length, signedOut],
  );

  async function onRefresh() {
    setRefreshing(true);
    refresh();
    setTimeout(() => setRefreshing(false), 600);
  }

  if (signedOut) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <Text className="text-2xl font-semibold text-foreground">Sessions</Text>
        <Body>Sign in to a gateway and attach a machine to supervise work.</Body>
        <Pressable
          className="rounded-lg bg-primary px-4 py-3 mt-4"
          onPress={() => router.replace("/")}
        >
          <Text className="text-center text-primary-foreground">Pair a gateway</Text>
        </Pressable>
      </SafeAreaView>
    );
  }

  if (notAttached) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <Text className="text-2xl font-semibold text-foreground">Sessions</Text>
        <Body>Attach a Tidebreak machine to see live sessions.</Body>
        <Pressable
          className="rounded-lg bg-primary px-4 py-3 mt-4"
          onPress={() => router.replace("/attach")}
        >
          <Text className="text-center text-primary-foreground">Attach</Text>
        </Pressable>
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView
        contentContainerClassName="px-5 py-6 gap-3"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={() => void onRefresh()} />
        }
      >
        <View className="flex-row items-baseline justify-between">
          <Text className="text-2xl font-semibold text-foreground">Sessions</Text>
          <Text className="text-xs text-muted-foreground">
            {live ? "Live" : "Reconnecting…"}
          </Text>
        </View>
        {!hasSnapshot && rows.length === 0 ? (
          <View className="py-12 items-center gap-2">
            <ActivityIndicator />
            <Text className="text-sm text-muted-foreground">Loading sessions…</Text>
          </View>
        ) : null}
        {empty ? (
          <View className="rounded-xl border border-border bg-background p-4">
            <Text className="text-base text-foreground">No sessions yet</Text>
            <Text className="text-sm text-muted-foreground mt-1">
              When a code session starts on this machine, it will appear here.
            </Text>
          </View>
        ) : null}
        {rows.map((row) => {
          const badge = attentionBadgeLabel(row.attention);
          return (
            <Pressable
              key={row.session}
              className="rounded-xl border border-border bg-background p-4 gap-1"
              onPress={() =>
                router.push({
                  pathname: "/session/[id]",
                  params: {
                    id: row.session,
                    title: row.title,
                    workspace: row.workspace,
                  },
                })
              }
            >
              <View className="flex-row items-start justify-between gap-2">
                <Text className="flex-1 text-base font-medium text-foreground">
                  {row.title || "Untitled session"}
                </Text>
                {badge ? (
                  <View className="rounded-full bg-warning px-2 py-0.5">
                    <Text className="text-xs text-warning-foreground">{badge}</Text>
                  </View>
                ) : null}
              </View>
              <Text className="text-sm text-muted-foreground">{row.workspace}</Text>
              <Text className="text-sm text-muted-foreground">
                {harnessLabel(row.harness_kind)} · {lifecycleLabel(row.lifecycle)}
              </Text>
            </Pressable>
          );
        })}
        <Pressable
          className="mt-2 rounded-lg border border-border bg-background px-4 py-3"
          onPress={() => router.push("/approvals")}
        >
          <Text className="text-center text-base text-foreground">Approvals</Text>
        </Pressable>
        <Pressable
          className="rounded-lg border border-border bg-background px-4 py-3"
          onPress={() => router.push("/settings")}
        >
          <Text className="text-center text-base text-foreground">Settings</Text>
        </Pressable>
      </ScrollView>
    </SafeAreaView>
  );
}

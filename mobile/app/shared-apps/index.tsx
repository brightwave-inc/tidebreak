import { useQuery } from "@tanstack/react-query";
import { useRouter } from "expo-router";
import { useCallback, useState } from "react";
import { FlatList, Pressable, RefreshControl, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, ChipPill, EmptyState } from "../../src/components/Console";
import { sharedAppStatusChip } from "../../src/lib/consoleLabels";
import type { SharedAppDescription } from "../../src/lib/consoleTypes";
import {
  meQueries,
  sharedAppQueries,
} from "../../src/session/consoleQueries";

function SharedAppRow({
  app,
  mine,
  onPress,
}: {
  app: SharedAppDescription;
  mine: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={app.name}
      onPress={onPress}
    >
      <Card className="gap-2">
        <View className="flex-row items-center justify-between gap-2">
          <Text
            className="flex-1 text-base font-medium text-foreground"
            numberOfLines={2}
          >
            {app.name}
          </Text>
          <ChipPill chip={sharedAppStatusChip(app.status)} />
        </View>
        <Text className="text-xs text-muted-foreground">
          {mine ? "by you" : "shared with you"}
          {app.current_revision != null ? ` · rev ${app.current_revision}` : ""}
        </Text>
      </Card>
    </Pressable>
  );
}

/**
 * Team tools shared with this account, plus its own drafts. Authoring and
 * lifecycle are deliberately absent: those are control-plane writes this client
 * holds no grant for, and running an app happens in the browser for now.
 */
export default function SharedAppsScreen() {
  const router = useRouter();
  const query = useQuery(sharedAppQueries.list());
  const me = useQuery(meQueries.identity());
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  const apps = query.data?.shared_apps ?? [];

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <FlatList
        className="flex-1"
        contentContainerClassName="gap-3 px-5 py-4"
        data={apps}
        keyExtractor={(app) => app.id}
        renderItem={({ item }) => (
          <SharedAppRow
            app={item}
            mine={item.owner_user_id === me.data?.user_id}
            onPress={() =>
              router.push({
                pathname: "/shared-apps/[id]",
                params: { id: item.id },
              })
            }
          />
        )}
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
        ListFooterComponent={
          query.data?.truncated ? (
            <Text className="pt-2 text-xs text-muted-foreground">
              More apps are reachable than this listing returned.
            </Text>
          ) : null
        }
        ListEmptyComponent={
          query.isLoading ? null : query.isError ? (
            <EmptyState
              title="Couldn’t load shared apps"
              {...(query.error instanceof Error
                ? { detail: query.error.message }
                : {})}
            />
          ) : (
            <EmptyState
              title="No shared apps"
              detail="Apps a teammate publishes to a team you belong to appear here."
            />
          )
        }
      />
    </SafeAreaView>
  );
}

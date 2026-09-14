import { useMutation, useQuery } from "@tanstack/react-query";
import { useLocalSearchParams } from "expo-router";
import * as WebBrowser from "expo-web-browser";
import { useCallback, useState } from "react";
import { Alert, RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, ChipPill, EmptyState } from "../../src/components/Console";
import { Button, SectionLabel } from "../../src/components/Controls";
import { sharedAppStatusChip } from "../../src/lib/consoleLabels";
import type { SharedAppBindingView } from "../../src/lib/consoleTypes";
import {
  meQueries,
  sharedAppMutations,
  sharedAppQueries,
} from "../../src/session/consoleQueries";
import { useActiveConnection } from "../../src/session/store";

/**
 * One binding, as names rather than addresses: what the app may call, and
 * whether this viewer reaches it. A shared app confers reachability to itself,
 * never to what it calls — so an unreachable binding means every call through
 * it will be refused for them, which is worth saying before they try.
 */
function BindingRow({
  binding,
  first,
}: {
  binding: SharedAppBindingView;
  first: boolean;
}) {
  const count = binding.operation_ids.length;
  return (
    <View
      className={first ? "gap-1 py-3" : "gap-1 border-t border-border py-3"}
    >
      <Text className="text-sm font-medium text-foreground">
        {binding.display_name ?? "Unavailable app"}
      </Text>
      <Text className="text-xs text-muted-foreground">
        {count} operation{count === 1 ? "" : "s"}
      </Text>
      {binding.viewer_reachable ? null : (
        <Text className="text-xs text-warning-foreground">
          Connect this app on the gateway first
        </Text>
      )}
    </View>
  );
}

/**
 * One shared app: what it may call as you, whether you have accepted that, and
 * a way out to the gateway to run it. Author lifecycle actions are absent on
 * purpose — publishing, stopping, and deleting are control-plane writes this
 * client holds no grant for.
 */
export default function SharedAppScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const connection = useActiveConnection();
  const query = useQuery(sharedAppQueries.detail(id));
  const me = useQuery(meQueries.identity());
  const app = query.data;
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  const accept = useMutation({
    // The revision the sheet was rendered from, not "whatever is current": the
    // gateway answers `revision_moved` when the app was revised since, rather
    // than recording consent to a manifest this viewer never read.
    mutationFn: () =>
      sharedAppMutations.consent(
        id,
        app?.consent.revision_id ?? app?.manifest_revision_id,
      ),
    onSuccess: () => void query.refetch(),
    onError: (error) => {
      Alert.alert(
        "Couldn’t record consent",
        error instanceof Error ? error.message : "Unknown error",
      );
      // The refusal is usually `revision_moved`, so re-read: the sheet the
      // viewer is looking at is the stale half.
      void query.refetch();
    },
  });

  if (query.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load this app"
          {...(query.error instanceof Error
            ? { detail: query.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }
  if (!app) {
    return <SafeAreaView className="flex-1 bg-page-background" />;
  }

  const gatewayUrl = connection
    ? `${connection.gatewayUrl.replace(/\/+$/, "")}/shared-apps/${app.id}`
    : null;

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-4 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <Card className="gap-2">
          <Text className="text-lg font-semibold text-foreground">
            {app.title ?? app.name}
          </Text>
          <View className="flex-row items-center gap-2">
            <ChipPill chip={sharedAppStatusChip(app.status)} />
            {app.owner_user_id === me.data?.user_id ? (
              <Text className="text-xs text-muted-foreground">by you</Text>
            ) : null}
          </View>
          {/* The revision the manifest below came from — not
              `current_revision`, which a republish between the two reads can
              already have moved. */}
          {app.manifest_revision != null ? (
            <Text className="text-xs text-muted-foreground">
              Revision {app.manifest_revision}
            </Text>
          ) : null}
        </Card>

        <Card className="gap-2">
          <SectionLabel>Consent</SectionLabel>
          {app.consent.required ? (
            <>
              <Text className="text-sm text-muted-foreground">
                This app calls the apps below as you. Nothing runs until you
                accept what it may call.
              </Text>
              <Button
                label={accept.isPending ? "Recording…" : "Allow this app"}
                onPress={() => accept.mutate()}
                disabled={accept.isPending}
              />
            </>
          ) : (
            <Text className="text-sm text-muted-foreground">
              You have accepted what this revision may call as you. A new
              revision asks again.
            </Text>
          )}
        </Card>

        <View className="gap-2">
          <SectionLabel>What it may call</SectionLabel>
          <Card className="py-0">
            {app.bindings.length === 0 ? (
              <Text className="py-3 text-sm text-muted-foreground">
                This app calls nothing on your behalf.
              </Text>
            ) : (
              app.bindings.map((binding, index) => (
                <BindingRow
                  key={binding.connected_app_id}
                  binding={binding}
                  first={index === 0}
                />
              ))
            )}
          </Card>
        </View>

        {gatewayUrl ? (
          <View className="gap-2">
            <Button
              label="Open in gateway"
              variant="secondary"
              onPress={() => void WebBrowser.openBrowserAsync(gatewayUrl)}
            />
            <Text className="text-xs text-muted-foreground">
              Running a shared app happens in the browser for now.
            </Text>
          </View>
        ) : null}
      </ScrollView>
    </SafeAreaView>
  );
}

import { useQuery } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Card, ChipPill, EmptyState } from "../src/components/Console";
import { SectionLabel } from "../src/components/Controls";
import {
  appConnectionChip,
  protocolLabel,
  spacedSlug,
} from "../src/lib/consoleLabels";
import type { CatalogApp, CatalogModel } from "../src/lib/consoleTypes";
import { catalogQueries } from "../src/session/consoleQueries";

function ModelRow({ model, first }: { model: CatalogModel; first: boolean }) {
  return (
    <View
      className={first ? "gap-1.5 py-3" : "gap-1.5 border-t border-border py-3"}
    >
      <Text className="text-base font-medium text-foreground" numberOfLines={1}>
        {model.name || model.id}
      </Text>
      <Text className="text-xs text-muted-foreground" numberOfLines={1}>
        {model.provider_name}
        {model.context_window != null
          ? ` · ${model.context_window.toLocaleString()} ctx`
          : ""}
      </Text>
      <View className="flex-row flex-wrap gap-1.5">
        {model.protocols.map((protocol) => (
          <ChipPill
            key={protocol}
            chip={{ label: protocolLabel(protocol), tone: "neutral" }}
          />
        ))}
        {model.supports_tools ? (
          <ChipPill chip={{ label: "tools", tone: "neutral" }} />
        ) : null}
        {model.supports_vision ? (
          <ChipPill chip={{ label: "vision", tone: "neutral" }} />
        ) : null}
      </View>
    </View>
  );
}

function AppRow({ app, first }: { app: CatalogApp; first: boolean }) {
  return (
    <View
      className={first ? "gap-1.5 py-3" : "gap-1.5 border-t border-border py-3"}
    >
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {app.name}
        </Text>
        <ChipPill chip={appConnectionChip(app.connection)} />
      </View>
      <Text className="text-xs text-muted-foreground">
        {spacedSlug(app.app_kind)}
      </Text>
    </View>
  );
}

/**
 * What this account may invoke, and how its connected apps stand — the member
 * catalog, not the installation's inventory. Reachability is the gateway's own
 * answer, so nothing here is filtered client-side.
 */
export default function CatalogScreen() {
  const query = useQuery(catalogQueries.mine());
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  if (query.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load the catalog"
          {...(query.error instanceof Error
            ? { detail: query.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }

  const models = query.data?.models ?? [];
  const apps = query.data?.apps ?? [];

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-4 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <View className="gap-2">
          <SectionLabel>Models</SectionLabel>
          <Card className="py-0">
            {models.length === 0 ? (
              <Text className="py-3 text-sm text-muted-foreground">
                {query.isLoading
                  ? "Loading…"
                  : "No models are granted to you on this gateway."}
              </Text>
            ) : (
              models.map((model, index) => (
                <ModelRow key={model.id} model={model} first={index === 0} />
              ))
            )}
          </Card>
        </View>

        <View className="gap-2">
          <SectionLabel>Connected apps</SectionLabel>
          <Card className="py-0">
            {apps.length === 0 ? (
              <Text className="py-3 text-sm text-muted-foreground">
                {query.isLoading ? "Loading…" : "No apps are granted to you."}
              </Text>
            ) : (
              apps.map((app, index) => (
                <AppRow key={app.id} app={app} first={index === 0} />
              ))
            )}
          </Card>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}

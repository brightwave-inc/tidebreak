import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Text, View } from "react-native";
import {
  AdminScreen,
  FilterField,
  LoadError,
} from "../../src/components/Admin";
import { Card, StatTile } from "../../src/components/Console";
import { SectionLabel, StatusPill } from "../../src/components/Controls";
import { spacedSlug } from "../../src/lib/consoleLabels";
import type {
  ModelProvider,
  ProviderModel,
  ProvisionedCapacityMode,
} from "../../src/lib/gatewayAdmin";
import { matchesFilter } from "../../src/lib/sandboxStatus";
import { adminQueries } from "../../src/session/consoleQueries";

/**
 * What a capacity binding means, which depends on the provider kind: the same
 * slug is a different arrangement on `OpenAI` and on Bedrock, and naming the
 * slug alone would leave the reader to know that.
 */
function capacityLabel(
  providerKind: string,
  mode: ProvisionedCapacityMode,
): string {
  if (providerKind === "openai") {
    if (mode === "provider_observed") return "Scale when available";
    if (mode === "metered_only") return "Metered processing only";
    if (mode === "none") return "Follow the request";
  }
  if (providerKind === "bedrock") {
    if (mode === "route_resource") return "Provisioned Throughput resource";
    if (mode === "metered_only") return "On-demand models only";
    if (mode === "none") return "Use the configured resource";
  }
  if (providerKind === "google_vertex") {
    if (mode === "dedicated_only") return "Provisioned Throughput only";
    if (mode === "metered_only") return "Pay-as-you-go only";
    if (mode === "none") return "Use provider defaults";
  }
  if (mode === "none") return "No capacity binding";
  return "Unsupported capacity binding";
}

function ModelRow({
  model,
  providerKind,
  first,
}: {
  model: ProviderModel;
  providerKind: string;
  first: boolean;
}) {
  return (
    <View
      className={first ? "gap-1 py-3" : "gap-1 border-t border-border py-3"}
    >
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-base font-medium text-foreground"
          numberOfLines={1}
        >
          {model.display_name}
        </Text>
        {model.enabled ? null : (
          <StatusPill tone="warning">disabled</StatusPill>
        )}
      </View>
      <Text className="text-xs text-muted-foreground" numberOfLines={1}>
        {model.gateway_id}
        {model.context_window != null
          ? ` · ${model.context_window.toLocaleString()} ctx`
          : ""}
      </Text>
      <Text className="text-xs text-muted-foreground" numberOfLines={2}>
        Capacity · {capacityLabel(providerKind, model.provisioned_capacity_mode)}
      </Text>
      {/* Zero grants on both counts means nobody can route to the model: it is
          in the catalog but no grant reaches it, which is otherwise only
          discovered request by request. */}
      {model.team_grant_count === 0 && model.user_grant_count === 0 ? (
        <Text className="text-xs text-warning-foreground">
          No team or account holds a grant to this model.
        </Text>
      ) : null}
    </View>
  );
}

/**
 * One provider and the catalog rows it owns. A disabled provider stops every
 * model under it regardless of the models' own state, so its pill is the one
 * that explains a silent catalog.
 */
function ProviderCard({
  provider,
  models,
}: {
  provider: ModelProvider;
  models: ProviderModel[];
}) {
  return (
    <View className="gap-2">
      <View className="flex-row items-center justify-between gap-2">
        <SectionLabel>{provider.name}</SectionLabel>
        <View className="flex-row gap-1.5">
          <StatusPill>{spacedSlug(provider.provider_kind)}</StatusPill>
          {provider.enabled ? null : (
            <StatusPill tone="warning">disabled</StatusPill>
          )}
        </View>
      </View>
      <Card className="py-0">
        {models.length === 0 ? (
          <Text className="py-3 text-sm text-muted-foreground">
            No catalog models under this provider.
          </Text>
        ) : (
          models.map((model, index) => (
            <ModelRow
              key={model.id}
              model={model}
              providerKind={provider.provider_kind}
              first={index === 0}
            />
          ))
        )}
      </Card>
    </View>
  );
}

/**
 * The installation's model catalog, grouped by the provider that serves each
 * row. This is the inventory — what exists and what may serve traffic — not
 * what any one account may invoke; that question is the member catalog's.
 */
export default function AdminModelsScreen() {
  const providers = useQuery(adminQueries.modelProviders());
  const models = useQuery(adminQueries.providerModels());
  const [filter, setFilter] = useState("");

  const providerRows = providers.data?.data ?? [];
  const modelRows = models.data?.data ?? [];
  const matches = modelRows.filter((model) =>
    matchesFilter(filter, model.display_name, model.gateway_id, model.upstream_id),
  );
  // A provider with no matching model drops out while filtering, but is always
  // shown when nothing is being asked for — an empty provider is a real state.
  const shownProviders = filter.trim()
    ? providerRows.filter((provider) =>
        matches.some((model) => model.provider_id === provider.id),
      )
    : providerRows;

  const disabledModels = modelRows.filter((model) => !model.enabled).length;

  return (
    <AdminScreen
      refresh={() => Promise.all([providers.refetch(), models.refetch()])}
      consolePath="/models"
    >
      {providers.isError ? (
        <LoadError what="providers" error={providers.error} />
      ) : null}
      {models.isError ? (
        <LoadError what="the model catalog" error={models.error} />
      ) : null}

      <View className="flex-row gap-2">
        <StatTile label="Providers" value={providerRows.length.toLocaleString()} />
        <StatTile label="Models" value={modelRows.length.toLocaleString()} />
        <StatTile label="Disabled" value={disabledModels.toLocaleString()} />
      </View>

      <FilterField
        value={filter}
        onChange={setFilter}
        placeholder="Filter models by name or id"
      />

      {shownProviders.length === 0 ? (
        <Text className="text-sm text-muted-foreground">
          {providers.isLoading || models.isLoading
            ? "Loading…"
            : "No providers match."}
        </Text>
      ) : (
        shownProviders.map((provider) => (
          <ProviderCard
            key={provider.id}
            provider={provider}
            models={matches.filter((model) => model.provider_id === provider.id)}
          />
        ))
      )}
    </AdminScreen>
  );
}

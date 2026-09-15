import { useQuery } from "@tanstack/react-query";
import { Text, View } from "react-native";
import {
  AdminScreen,
  ConsoleLink,
  LoadError,
} from "../../src/components/Admin";
import { Card, ExpandableSection } from "../../src/components/Console";
import { SectionLabel, StatusPill } from "../../src/components/Controls";
import { spacedSlug } from "../../src/lib/consoleLabels";
import { stamp } from "../../src/lib/consoleTime";
import type {
  ConnectedApp,
  IdentityProvider,
  McpEndpoint,
  ScimConnector,
} from "../../src/lib/gatewayAdmin";
import { adminQueries } from "../../src/session/consoleQueries";

/** What the installation accepts at sign-in. */
const LOGIN_MODE: Record<string, string> = {
  local_only: "Password sign-in only",
  local_or_sso: "Password or federated sign-in",
  sso_required: "Federated sign-in required",
};

/** The two MCP trust tiers, neither of which is a fault state. */
const EXECUTION_MODE: Record<string, string> = {
  direct: "Direct",
  gateway_attested: "Attested only",
};

/**
 * Verification state. Only a verified provider is offered at sign-in, and an
 * `error` provider is one whose last verification failed — a real malfunction,
 * unlike a draft nobody has finished yet.
 */
const PROVIDER_STATUS: Record<
  string,
  { label: string; tone: "success" | "neutral" | "critical" }
> = {
  verified: { label: "verified", tone: "success" },
  draft: { label: "draft", tone: "neutral" },
  error: { label: "error", tone: "critical" },
};

function AppRow({ app }: { app: ConnectedApp }) {
  return (
    <View className="gap-1">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {app.name}
        </Text>
        <View className="flex-row gap-1.5">
          {app.built_in ? <StatusPill>built in</StatusPill> : null}
          {app.enabled ? null : (
            <StatusPill tone="warning">disabled</StatusPill>
          )}
        </View>
      </View>
      <Text className="text-xs text-muted-foreground">
        {spacedSlug(app.kind)}
      </Text>
    </View>
  );
}

function EndpointRow({ endpoint }: { endpoint: McpEndpoint }) {
  const apps = endpoint.app_ids.length;
  return (
    <View className="gap-1">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {endpoint.slug}
        </Text>
        <View className="flex-row gap-1.5">
          <StatusPill>
            {EXECUTION_MODE[endpoint.execution_mode] ??
              spacedSlug(endpoint.execution_mode)}
          </StatusPill>
          {endpoint.enabled ? null : (
            <StatusPill tone="warning">disabled</StatusPill>
          )}
        </View>
      </View>
      <Text className="text-xs text-muted-foreground">
        {endpoint.name} · {apps} app{apps === 1 ? "" : "s"}
        {endpoint.owner_type === "user" ? " · user-owned" : ""}
      </Text>
    </View>
  );
}

function ProviderRow({ provider }: { provider: IdentityProvider }) {
  const status = PROVIDER_STATUS[provider.status] ?? {
    label: spacedSlug(provider.status),
    tone: "neutral" as const,
  };
  return (
    <View className="gap-1 border-t border-border py-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {provider.name}
        </Text>
        <View className="flex-row gap-1.5">
          <StatusPill tone={status.tone}>{status.label}</StatusPill>
          {provider.enabled ? null : <StatusPill tone="warning">off</StatusPill>}
        </View>
      </View>
      <Text className="text-xs text-muted-foreground" numberOfLines={1}>
        {provider.issuer}
      </Text>
    </View>
  );
}

function ConnectorRow({ connector }: { connector: ScimConnector }) {
  return (
    <View className="gap-1 border-t border-border py-2">
      <View className="flex-row items-center justify-between gap-2">
        <Text
          className="flex-1 text-sm font-medium text-foreground"
          numberOfLines={1}
        >
          {connector.name}
        </Text>
        {connector.enabled ? null : <StatusPill>retired</StatusPill>}
      </View>
      <Text className="text-xs text-muted-foreground">
        {connector.token_prefix}… ·{" "}
        {connector.last_used_at
          ? `last used ${stamp(connector.last_used_at)}`
          : "never used"}
      </Text>
    </View>
  );
}

/**
 * The static configuration surfaces, on one screen.
 *
 * Apps, MCP endpoints, and identity are separate pages in the gateway's own
 * console because each is administered separately; on a phone nobody triages
 * them apart, and three near-empty screens read worse than one that answers
 * "how is this gateway set up".
 */
export default function AdminConfigurationScreen() {
  const apps = useQuery(adminQueries.connectedApps());
  const endpoints = useQuery(adminQueries.mcpEndpoints());
  const policy = useQuery(adminQueries.authenticationPolicy());
  const providers = useQuery(adminQueries.identityProviders());
  const connectors = useQuery(adminQueries.scimConnectors());

  const accounts = policy.data?.accounts;
  const providerRows = providers.data?.data ?? [];
  const connectorRows = connectors.data?.data ?? [];

  return (
    <AdminScreen
      refresh={() =>
        Promise.all([
          apps.refetch(),
          endpoints.refetch(),
          policy.refetch(),
          providers.refetch(),
          connectors.refetch(),
        ])
      }
    >
      {apps.isError ? (
        <LoadError what="connected apps" error={apps.error} />
      ) : null}
      <ExpandableSection
        title="Connected apps"
        supporting="What the gateway can call on a caller's behalf."
        rows={apps.data?.data ?? []}
        rowKey={(app) => app.id}
        empty={apps.isLoading ? "Loading…" : "No connected apps."}
        renderRow={(app) => <AppRow app={app} />}
      />
      <ConsoleLink webPath="/apps" />

      {endpoints.isError ? (
        <LoadError what="MCP endpoints" error={endpoints.error} />
      ) : null}
      <ExpandableSection
        title="MCP endpoints"
        supporting="Each is one URL clients connect to. Attested-only endpoints admit tool calls this gateway itself vouched for; direct ones admit any authorized client."
        rows={endpoints.data?.data ?? []}
        rowKey={(endpoint) => endpoint.id}
        empty={endpoints.isLoading ? "Loading…" : "No MCP endpoints."}
        renderRow={(endpoint) => <EndpointRow endpoint={endpoint} />}
      />
      <ConsoleLink webPath="/mcp-endpoints" />

      {policy.isError ? (
        <LoadError what="the sign-in policy" error={policy.error} />
      ) : null}
      {providers.isError ? (
        <LoadError what="identity providers" error={providers.error} />
      ) : null}
      {connectors.isError ? (
        <LoadError what="SCIM connectors" error={connectors.error} />
      ) : null}
      <Card className="gap-2">
        <SectionLabel>Identity</SectionLabel>
        {policy.data ? (
          <View className="gap-0.5">
            <Text className="text-sm font-medium text-foreground">
              {LOGIN_MODE[policy.data.login_mode] ??
                spacedSlug(policy.data.login_mode)}
            </Text>
            {accounts ? (
              <Text className="text-xs text-muted-foreground">
                {accounts.active.toLocaleString()} active accounts ·{" "}
                {accounts.sso.toLocaleString()} federated ·{" "}
                {accounts.passwordless.toLocaleString()} with no password
              </Text>
            ) : null}
          </View>
        ) : (
          <Text className="text-sm text-muted-foreground">
            {policy.isLoading ? "Loading…" : "Sign-in policy unavailable."}
          </Text>
        )}

        <Text className="pt-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Providers
        </Text>
        {providerRows.length === 0 ? (
          <Text className="text-sm text-muted-foreground">
            No federated providers configured.
          </Text>
        ) : (
          providerRows.map((provider) => (
            <ProviderRow key={provider.id} provider={provider} />
          ))
        )}

        <Text className="pt-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          SCIM
        </Text>
        {connectorRows.length === 0 ? (
          <Text className="text-sm text-muted-foreground">
            No SCIM connectors.
          </Text>
        ) : (
          connectorRows.map((connector) => (
            <ConnectorRow key={connector.id} connector={connector} />
          ))
        )}
      </Card>
      <ConsoleLink webPath="/identity" />
    </AdminScreen>
  );
}

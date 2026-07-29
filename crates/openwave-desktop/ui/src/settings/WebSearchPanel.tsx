import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
  WebSearchProviderKind,
} from "../api";
import { Button } from "@/components/ui/button";
import {
  ActiveProviderField,
  ProviderCredentialField,
  TimeoutSecondsField,
  timeoutMsFromSeconds,
} from "./ProviderFields";
import { Input } from "@/components/ui/input";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const MIN_WEB_SEARCH_TIMEOUT_SECONDS = 1;
const MAX_WEB_SEARCH_TIMEOUT_SECONDS = 60;

/**
 * SearXNG is self-hosted: the operator runs the instance, so it needs an
 * address instead of a key and never appears in the credential list.
 */
const SEARXNG_PROVIDER: WebSearchProviderKind = "searxng";

export function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutSeconds, setTimeoutSeconds] = useState("");
  const [searxngBaseUrl, setSearxngBaseUrl] = useState("");
  // One draft key per provider: a pass can add Exa's key and Tavily's key
  // together, and switching the active provider must not discard either.
  const [apiKeys, setApiKeys] = useState<
    Partial<Record<WebSearchProviderKind, string>>
  >({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [removing, setRemoving] = useState<WebSearchProviderKind | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const [nextConfig, nextCredentials] = await Promise.all([
          client.getWebSearchConfig(),
          client.listWebSearchCredentials(),
        ]);
        if (cancelled) return;
        setConfig(nextConfig);
        setCredentials(nextCredentials.credentials);
        setProvider(nextConfig.provider ?? "");
        setTimeoutSeconds(String(nextConfig.timeout_ms / 1000));
        setSearxngBaseUrl(nextConfig.searxng_base_url ?? "");
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  const working = saving || removing !== null;
  const state = webSearchState(config);

  async function save() {
    const timeout = timeoutMsFromSeconds(
      timeoutSeconds,
      MIN_WEB_SEARCH_TIMEOUT_SECONDS,
      MAX_WEB_SEARCH_TIMEOUT_SECONDS,
    );
    if ("error" in timeout) {
      setError(timeout.error);
      return;
    }

    setSaving(true);
    setError(null);
    try {
      // Keys go first so the newly active provider never lands
      // selected-but-unusable when the caller supplied both in one pass.
      for (const credential of credentials) {
        const key = apiKeys[credential.provider]?.trim();
        if (!key) continue;
        await client.putWebSearchCredential(credential.provider, key);
        setApiKeys((current) => ({ ...current, [credential.provider]: "" }));
      }
      const nextConfig = await client.putWebSearchConfig({
        provider: provider || null,
        timeout_ms: timeout.timeoutMs,
        // An empty field clears the stored address rather than leaving a
        // stale one behind an emptied box.
        searxng_base_url: searxngBaseUrl.trim() || null,
      });
      const nextCredentials = await client.listWebSearchCredentials();
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      setTimeoutSeconds(String(nextConfig.timeout_ms / 1000));
      setSearxngBaseUrl(nextConfig.searxng_base_url ?? "");
      toast.success("Saved web-search settings");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function removeCredential(target: WebSearchProviderKind) {
    setRemoving(target);
    setError(null);
    try {
      await client.deleteWebSearchCredential(target);
      const [nextConfig, nextCredentials] = await Promise.all([
        client.getWebSearchConfig(),
        client.listWebSearchCredentials(),
      ]);
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      toast.success(`Removed the saved ${providerLabel(target)} API key`);
    } catch (err) {
      setError(String(err));
    } finally {
      setRemoving(null);
    }
  }

  return (
    <SettingsPanel
      title="Web search"
      description="Configure as many providers as you like, choose the one agents search through, and bound every request. Saved keys are never shown here."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">
          Loading web-search settings…
        </p>
      ) : !config ? (
        <p className="text-sm text-muted-foreground">
          Web-search settings are unavailable.
        </p>
      ) : (
        <>
          <SettingsStatus
            tone={state.kind}
            label={state.label}
            description={state.description}
          />

          <SettingsSection
            title="Providers"
            description="Give a key to every provider you want available. Brave Search has a free tier; Exa and Tavily are paid. Each key is stored in the system keychain and never shown again."
          >
            {credentials.map((credential) => (
              <ProviderCredentialField
                key={credential.provider}
                provider={providerLabel(credential.provider)}
                hasCredential={credential.has_credential}
                value={apiKeys[credential.provider] ?? ""}
                disabled={working}
                removing={removing === credential.provider}
                onChange={(value) =>
                  setApiKeys((current) => ({
                    ...current,
                    [credential.provider]: value,
                  }))
                }
                onRemove={() => void removeCredential(credential.provider)}
              />
            ))}
          </SettingsSection>

          <SettingsSection
            title="Self-hosted instance"
            description="SearXNG needs no key — it needs the address of the instance you run. Enable the JSON output format on that instance, which is off by default."
          >
            <SettingsField
              label="SearXNG instance URL"
              hint="For example http://localhost:8888. A loopback or private address is expected here; leave blank to take SearXNG out of service."
            >
              <Input
                type="url"
                inputMode="url"
                placeholder="http://localhost:8888"
                value={searxngBaseUrl}
                disabled={working}
                onChange={(event) => setSearxngBaseUrl(event.target.value)}
              />
            </SettingsField>
          </SettingsSection>

          <SettingsSection
            title="Active provider"
            description="Agents search and open pages through this one provider. The others stay configured and idle."
          >
            <ActiveProviderField
              value={provider}
              disabled={working}
              onChange={setProvider}
              options={[
                ...credentials.map((credential) => ({
                  kind: credential.provider,
                  label: providerLabel(credential.provider),
                })),
                {
                  kind: SEARXNG_PROVIDER,
                  label: providerLabel(SEARXNG_PROVIDER),
                },
              ]}
            />

            <TimeoutSecondsField
              label="Request timeout"
              minSeconds={MIN_WEB_SEARCH_TIMEOUT_SECONDS}
              maxSeconds={MAX_WEB_SEARCH_TIMEOUT_SECONDS}
              value={timeoutSeconds}
              disabled={working}
              onChange={setTimeoutSeconds}
            />
          </SettingsSection>

          {/* One save for the whole surface: it stores every key typed above
              and the selection together, so a provider cannot go active in a
              pass that failed to save its key. */}
          <div className="flex flex-wrap gap-2">
            <Button type="button" disabled={working} onClick={() => void save()}>
              {saving ? "Saving…" : "Save settings"}
            </Button>
          </div>

          <p className="text-sm leading-relaxed text-muted-foreground">
            Foreground and background agents can request configured search.
            Foreground requests ask for approval before the query leaves
            OpenWave.
          </p>

          <p className="text-sm leading-relaxed text-muted-foreground">
            {config.provider
              ? `Agents also open single pages through ${providerLabel(config.provider)}. If a page comes back empty or ${providerLabel(config.provider)} is unavailable, OpenWave reads it directly instead.`
              : "Agents can still open single pages without a provider: OpenWave reads them directly."}
          </p>
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

function providerLabel(provider: WebSearchProviderKind): string {
  switch (provider) {
    case "exa":
      return "Exa";
    case "tavily":
      return "Tavily";
    case "brave":
      return "Brave Search";
    case "searxng":
      return "SearXNG";
    default:
      return provider;
  }
}

function webSearchState(config: WebSearchConfigInfo | null): {
  kind: "disabled" | "ready" | "not-configured";
  label: string;
  description: string;
} {
  if (!config?.provider) {
    return {
      kind: "disabled",
      label: "Disabled",
      description: "No web-search provider is selected.",
    };
  }
  if (config.available) {
    return {
      kind: "ready",
      label: "Ready",
      description:
        config.provider === SEARXNG_PROVIDER
          ? `${providerLabel(config.provider)} is selected and pointed at ${config.searxng_base_url}.`
          : `${providerLabel(config.provider)} is selected and has a saved key.`,
    };
  }
  return {
    kind: "not-configured",
    label: "Not configured",
    description:
      config.provider === SEARXNG_PROVIDER
        ? `${providerLabel(config.provider)} is selected but needs an instance URL.`
        : `${providerLabel(config.provider)} is selected but needs an API key.`,
  };
}

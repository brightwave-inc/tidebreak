import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
  WebSearchProviderKind,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  ActiveProviderField,
  ProviderCredentialField,
} from "./ProviderFields";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const MIN_WEB_SEARCH_TIMEOUT_SECONDS = 1;
const MAX_WEB_SEARCH_TIMEOUT_SECONDS = 60;

export function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutSeconds, setTimeoutSeconds] = useState("");
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
    const seconds = Number(timeoutSeconds);
    if (
      !Number.isFinite(seconds) ||
      timeoutSeconds.trim() === "" ||
      seconds < MIN_WEB_SEARCH_TIMEOUT_SECONDS ||
      seconds > MAX_WEB_SEARCH_TIMEOUT_SECONDS
    ) {
      setError(
        `Timeout must be between ${MIN_WEB_SEARCH_TIMEOUT_SECONDS} and ${MAX_WEB_SEARCH_TIMEOUT_SECONDS} seconds.`,
      );
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
        timeout_ms: Math.round(seconds * 1000),
      });
      const nextCredentials = await client.listWebSearchCredentials();
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      setTimeoutSeconds(String(nextConfig.timeout_ms / 1000));
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
            description="Give a key to every provider you want available. Each key is stored in the system keychain and never shown again."
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
            title="Active provider"
            description="Agents search through this one provider. The others stay configured and idle."
          >
            <ActiveProviderField
              value={provider}
              disabled={working}
              onChange={setProvider}
              options={credentials.map((credential) => ({
                kind: credential.provider,
                label: providerLabel(credential.provider),
              }))}
            />

            <SettingsField
              label="Request timeout (seconds)"
              hint={`Between ${MIN_WEB_SEARCH_TIMEOUT_SECONDS} and ${MAX_WEB_SEARCH_TIMEOUT_SECONDS} seconds.`}
            >
              <Input
                type="number"
                inputMode="numeric"
                min={MIN_WEB_SEARCH_TIMEOUT_SECONDS}
                max={MAX_WEB_SEARCH_TIMEOUT_SECONDS}
                step="1"
                value={timeoutSeconds}
                disabled={working}
                onChange={(event) => setTimeoutSeconds(event.target.value)}
              />
            </SettingsField>
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
  if (config.has_credential) {
    return {
      kind: "ready",
      label: "Ready",
      description: `${providerLabel(config.provider)} is selected and has a saved key.`,
    };
  }
  return {
    kind: "not-configured",
    label: "Not configured",
    description: `${providerLabel(config.provider)} is selected but needs an API key.`,
  };
}

import { useEffect, useState } from "react";
import type {
  ApiClient,
  WebSearchConfigInfo,
  WebSearchCredentialReadiness,
  WebSearchProviderKind,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  SETTINGS_SELECT_CLASS,
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

const MIN_WEB_SEARCH_TIMEOUT_MS = 1_000;
const MAX_WEB_SEARCH_TIMEOUT_MS = 60_000;

export function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [savingConfig, setSavingConfig] = useState(false);
  const [savingCredential, setSavingCredential] = useState(false);
  const [removingCredential, setRemovingCredential] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    const [nextConfig, nextCredentials] = await Promise.all([
      client.getWebSearchConfig(),
      client.listWebSearchCredentials(),
    ]);
    setConfig(nextConfig);
    setCredentials(nextCredentials.credentials);
    setProvider(nextConfig.provider ?? "");
    setTimeoutMs(String(nextConfig.timeout_ms));
  }

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
        setTimeoutMs(String(nextConfig.timeout_ms));
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

  const activeProvider = config?.provider;
  const selectedCredential = activeProvider
    ? credentials.find((credential) => credential.provider === activeProvider)
    : undefined;
  const selectedHasCredential = selectedCredential?.has_credential ?? false;
  const working = savingConfig || savingCredential || removingCredential;
  const state = webSearchState(config);

  async function saveConfig() {
    const parsedTimeout = Number(timeoutMs);
    if (
      !Number.isInteger(parsedTimeout) ||
      parsedTimeout < MIN_WEB_SEARCH_TIMEOUT_MS ||
      parsedTimeout > MAX_WEB_SEARCH_TIMEOUT_MS
    ) {
      setError(
        `Timeout must be a whole number between ${MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and ${MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.`,
      );
      return;
    }

    setSavingConfig(true);
    setError(null);
    try {
      const nextConfig = await client.putWebSearchConfig({
        provider: provider || null,
        timeout_ms: parsedTimeout,
      });
      setConfig(nextConfig);
      setTimeoutMs(String(nextConfig.timeout_ms));
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingConfig(false);
    }
  }

  async function saveCredential() {
    if (!activeProvider || !apiKey.trim()) return;
    setSavingCredential(true);
    setError(null);
    try {
      await client.putWebSearchCredential(activeProvider, apiKey.trim());
      setApiKey("");
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingCredential(false);
    }
  }

  async function removeCredential() {
    if (!activeProvider) return;
    setRemovingCredential(true);
    setError(null);
    try {
      await client.deleteWebSearchCredential(activeProvider);
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingCredential(false);
    }
  }

  return (
    <SettingsPanel
      title="Web search"
      description="Choose the provider agents may use and bound every request. Existing keys are never shown here."
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
          <div className={`web-search-state is-${state.kind}`} role="status">
            <strong>{state.label}</strong>
            <span>{state.description}</span>
          </div>

          <SettingsSection>
            <SettingsField label="Provider">
              <select
                className={SETTINGS_SELECT_CLASS}
                value={provider}
                disabled={working}
                onChange={(event) =>
                  setProvider(event.target.value as WebSearchProviderKind | "")
                }
              >
                <option value="">Disabled</option>
                <option value="exa">Exa</option>
                <option value="tavily">Tavily</option>
              </select>
            </SettingsField>

            <SettingsField
              label="Request timeout (ms)"
              hint={`Between ${MIN_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} and ${MAX_WEB_SEARCH_TIMEOUT_MS.toLocaleString()} ms.`}
            >
              <Input
                type="number"
                inputMode="numeric"
                min={MIN_WEB_SEARCH_TIMEOUT_MS}
                max={MAX_WEB_SEARCH_TIMEOUT_MS}
                step="1000"
                value={timeoutMs}
                disabled={working}
                onChange={(event) => setTimeoutMs(event.target.value)}
              />
            </SettingsField>
            <Button
              type="button"
              className="self-start"
              disabled={working}
              onClick={() => void saveConfig()}
            >
              {savingConfig ? "Saving…" : "Save configuration"}
            </Button>
          </SettingsSection>

          {activeProvider && (
            <SettingsSection title={`${activeProvider} credential`}>
              <span className="text-xs text-muted-foreground">
                {selectedHasCredential
                  ? "credential saved"
                  : "no credential saved"}
              </span>
              <SettingsField
                label={selectedHasCredential ? "Replace API key" : "API key"}
              >
                <Input
                  type="password"
                  placeholder="Paste a new API key"
                  value={apiKey}
                  maxLength={8_192}
                  autoComplete="new-password"
                  disabled={working}
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </SettingsField>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  disabled={working || !apiKey.trim()}
                  onClick={() => void saveCredential()}
                >
                  {savingCredential
                    ? "Saving…"
                    : selectedHasCredential
                      ? "Update key"
                      : "Save key"}
                </Button>
                {selectedHasCredential && (
                  <Button
                    type="button"
                    variant="destructive"
                    disabled={working}
                    onClick={() => void removeCredential()}
                  >
                    {removingCredential ? "Removing…" : "Remove saved key"}
                  </Button>
                )}
              </div>
            </SettingsSection>
          )}

          {provider !== (activeProvider ?? "") && (
            <p className="text-xs text-muted-foreground">
              Save the provider configuration before managing that provider’s
              key.
            </p>
          )}

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
      description: `${config.provider} is selected and has a saved credential.`,
    };
  }
  return {
    kind: "not-configured",
    label: "Not configured",
    description: `${config.provider} is selected but needs an API key.`,
  };
}

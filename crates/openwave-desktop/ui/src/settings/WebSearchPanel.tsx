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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

const MIN_WEB_SEARCH_TIMEOUT_SECONDS = 1;
const MAX_WEB_SEARCH_TIMEOUT_SECONDS = 60;

// Radix Select reserves the empty string, so "Disabled" (no provider) rides on
// a sentinel value the wire never carries.
const NO_PROVIDER = "__disabled__";

export function WebSearchPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<WebSearchConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<WebSearchCredentialReadiness[]>([]);
  const [provider, setProvider] = useState<WebSearchProviderKind | "">("");
  const [timeoutSeconds, setTimeoutSeconds] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [removingCredential, setRemovingCredential] = useState(false);
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

  // Keyed to the provider in the dropdown, not the saved one: picking a
  // provider has to offer its key field in the same pass that selects it.
  const selectedHasCredential = provider
    ? (credentials.find((credential) => credential.provider === provider)
        ?.has_credential ?? false)
    : false;
  const working = saving || removingCredential;
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
      // The key goes first so a provider never lands selected-but-unusable
      // when the caller supplied both in one pass.
      if (provider && apiKey.trim()) {
        await client.putWebSearchCredential(provider, apiKey.trim());
        setApiKey("");
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

  async function removeCredential() {
    if (!provider) return;
    setRemovingCredential(true);
    setError(null);
    try {
      await client.deleteWebSearchCredential(provider);
      const [nextConfig, nextCredentials] = await Promise.all([
        client.getWebSearchConfig(),
        client.listWebSearchCredentials(),
      ]);
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      toast.success("Removed the saved API key");
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingCredential(false);
    }
  }

  return (
    <SettingsPanel
      title="Web search"
      description="Choose the provider agents may use, give it a key, and bound every request. Saved keys are never shown here."
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
              <Select
                value={provider === "" ? NO_PROVIDER : provider}
                disabled={working}
                onValueChange={(value) => {
                  setProvider(
                    value === NO_PROVIDER
                      ? ""
                      : (value as WebSearchProviderKind),
                  );
                  setApiKey("");
                }}
              >
                <SelectTrigger aria-label="Provider">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={NO_PROVIDER}>Disabled</SelectItem>
                  <SelectItem value="exa">Exa</SelectItem>
                  <SelectItem value="tavily">Tavily</SelectItem>
                </SelectContent>
              </Select>
            </SettingsField>

            {provider && (
              <SettingsField
                label="API key"
                hint={
                  selectedHasCredential
                    ? "A key is already saved. Type a new one to replace it."
                    : "Stored in the system keychain, never shown again."
                }
              >
                <Input
                  type="password"
                  placeholder={
                    selectedHasCredential
                      ? "Saved — leave blank to keep it"
                      : `Paste your ${providerLabel(provider)} API key`
                  }
                  value={apiKey}
                  maxLength={8_192}
                  autoComplete="new-password"
                  disabled={working}
                  onChange={(event) => setApiKey(event.target.value)}
                />
              </SettingsField>
            )}

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

            <div className="flex flex-wrap gap-2">
              <Button
                type="button"
                disabled={working}
                onClick={() => void save()}
              >
                {saving ? "Saving…" : "Save settings"}
              </Button>
              {provider && selectedHasCredential && (
                <Button
                  type="button"
                  variant="outline"
                  disabled={working}
                  onClick={() => void removeCredential()}
                >
                  {removingCredential ? "Removing…" : "Remove saved key"}
                </Button>
              )}
            </div>
          </SettingsSection>

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

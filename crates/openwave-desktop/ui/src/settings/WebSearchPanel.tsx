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
          <div className={`web-search-state is-${state.kind}`} role="status">
            <strong>{state.label}</strong>
            <span>{state.description}</span>
          </div>

          <SettingsSection
            title="Providers"
            description="Give a key to every provider you want available. Each key is stored in the system keychain and never shown again."
          >
            {credentials.map((credential) => (
              <div className="flex flex-col gap-1.5" key={credential.provider}>
                <SettingsField
                  label={`${providerLabel(credential.provider)} API key`}
                  hint={
                    credential.has_credential
                      ? "A key is already saved. Type a new one to replace it."
                      : undefined
                  }
                >
                  <Input
                    type="password"
                    placeholder={
                      credential.has_credential
                        ? "Saved — leave blank to keep it"
                        : `Paste your ${providerLabel(credential.provider)} API key`
                    }
                    value={apiKeys[credential.provider] ?? ""}
                    maxLength={8_192}
                    autoComplete="new-password"
                    disabled={working}
                    onChange={(event) =>
                      setApiKeys((current) => ({
                        ...current,
                        [credential.provider]: event.target.value,
                      }))
                    }
                  />
                </SettingsField>
                {credential.has_credential && (
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="self-start"
                    disabled={working}
                    onClick={() => void removeCredential(credential.provider)}
                  >
                    {removing === credential.provider
                      ? "Removing…"
                      : `Remove saved ${providerLabel(credential.provider)} key`}
                  </Button>
                )}
              </div>
            ))}
          </SettingsSection>

          <SettingsSection
            title="Active provider"
            description="Agents search through this one provider. The others stay configured and idle."
          >
            <SettingsField label="Provider">
              <Select
                value={provider === "" ? NO_PROVIDER : provider}
                disabled={working}
                onValueChange={(value) =>
                  setProvider(
                    value === NO_PROVIDER
                      ? ""
                      : (value as WebSearchProviderKind),
                  )
                }
              >
                <SelectTrigger aria-label="Provider">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={NO_PROVIDER}>Disabled</SelectItem>
                  {credentials.map((credential) => (
                    <SelectItem
                      key={credential.provider}
                      value={credential.provider}
                    >
                      {providerLabel(credential.provider)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </SettingsField>

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

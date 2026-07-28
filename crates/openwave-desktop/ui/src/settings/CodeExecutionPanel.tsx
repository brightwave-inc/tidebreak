import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
  CodeExecutionProviderKind,
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

const MIN_CODE_EXECUTION_TIMEOUT_MS = 1_000;
const MAX_CODE_EXECUTION_TIMEOUT_MS = 120_000;

// Radix Select reserves the empty string, so "Disabled" (no provider) rides on
// a sentinel value the wire never carries.
const NO_PROVIDER = "__disabled__";

export function CodeExecutionPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<CodeExecutionConfigInfo | null>(null);
  const [provider, setProvider] = useState<CodeExecutionProviderKind | "">("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [loading, setLoading] = useState(true);
  const [savingConfig, setSavingConfig] = useState(false);
  const [savingCredential, setSavingCredential] = useState(false);
  const [removingCredential, setRemovingCredential] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    const nextConfig = await client.getCodeExecutionConfig();
    setConfig(nextConfig);
    setProvider(nextConfig.provider ?? "");
    setTimeoutMs(String(nextConfig.timeout_ms));
  }

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .getCodeExecutionConfig()
      .then((nextConfig) => {
        if (cancelled) return;
        setConfig(nextConfig);
        setProvider(nextConfig.provider ?? "");
        setTimeoutMs(String(nextConfig.timeout_ms));
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  const state = codeExecutionState(config);
  const activeProvider = config?.provider;
  const working = savingConfig || savingCredential || removingCredential;

  async function saveConfig() {
    const parsedTimeout = Number(timeoutMs);
    if (
      !Number.isInteger(parsedTimeout) ||
      parsedTimeout < MIN_CODE_EXECUTION_TIMEOUT_MS ||
      parsedTimeout > MAX_CODE_EXECUTION_TIMEOUT_MS
    ) {
      setError(
        `Timeout must be a whole number between ${MIN_CODE_EXECUTION_TIMEOUT_MS.toLocaleString()} and ${MAX_CODE_EXECUTION_TIMEOUT_MS.toLocaleString()} ms.`,
      );
      return;
    }

    setSavingConfig(true);
    setError(null);
    try {
      const nextConfig = await client.putCodeExecutionConfig({
        provider: provider || null,
        timeout_ms: parsedTimeout,
      });
      setConfig(nextConfig);
      setProvider(nextConfig.provider ?? "");
      setTimeoutMs(String(nextConfig.timeout_ms));
      toast.success("Saved code-execution configuration");
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingConfig(false);
    }
  }

  async function saveCredential() {
    if (activeProvider !== "e2b" || !apiKey.trim()) return;
    setSavingCredential(true);
    setError(null);
    try {
      await client.putCodeExecutionCredential("e2b", apiKey.trim());
      setApiKey("");
      await refresh();
      toast.success("Saved the E2B API key");
    } catch (err) {
      setError(String(err));
    } finally {
      setSavingCredential(false);
    }
  }

  async function removeCredential() {
    if (activeProvider !== "e2b") return;
    setRemovingCredential(true);
    setError(null);
    try {
      await client.deleteCodeExecutionCredential("e2b");
      await refresh();
      toast.success("Removed the saved E2B API key");
    } catch (err) {
      setError(String(err));
    } finally {
      setRemovingCredential(false);
    }
  }

  return (
    <SettingsPanel
      title="Code execution"
      description="Choose an isolated execution provider and a host-enforced timeout."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">
          Loading code-execution settings…
        </p>
      ) : !config ? (
        <p className="text-sm text-muted-foreground">
          Code-execution settings are unavailable.
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
                onValueChange={(value) =>
                  setProvider(
                    value === NO_PROVIDER
                      ? ""
                      : (value as CodeExecutionProviderKind),
                  )
                }
              >
                <SelectTrigger aria-label="Provider">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={NO_PROVIDER}>Disabled</SelectItem>
                  <SelectItem value="local">Local native sandbox</SelectItem>
                  <SelectItem value="e2b">E2B cloud sandbox</SelectItem>
                </SelectContent>
              </Select>
            </SettingsField>

            <SettingsField
              label="Execution timeout (ms)"
              hint={`Between ${MIN_CODE_EXECUTION_TIMEOUT_MS.toLocaleString()} and ${MAX_CODE_EXECUTION_TIMEOUT_MS.toLocaleString()} ms.`}
            >
              <Input
                type="number"
                inputMode="numeric"
                min={MIN_CODE_EXECUTION_TIMEOUT_MS}
                max={MAX_CODE_EXECUTION_TIMEOUT_MS}
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

          {activeProvider === "e2b" && (
            <SettingsSection title="E2B credential">
              <span className="text-xs text-muted-foreground">
                {config.has_credential
                  ? "credential saved"
                  : "no credential saved"}
              </span>
              <SettingsField
                label={config.has_credential ? "Replace API key" : "API key"}
              >
                <Input
                  type="password"
                  placeholder="Paste a new E2B API key"
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
                    : config.has_credential
                      ? "Update key"
                      : "Save key"}
                </Button>
                {config.has_credential && (
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
            Local execution blocks network and confines writes to private chat
            scratch. E2B runs commands in a managed cloud sandbox, reuses its
            workspace while the sandbox is alive, and allows internet access.
            Both providers retain the execution consent boundary.
          </p>
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

function codeExecutionState(config: CodeExecutionConfigInfo | null): {
  kind: "disabled" | "ready" | "not-configured";
  label: string;
  description: string;
} {
  if (!config?.provider) {
    return {
      kind: "disabled",
      label: "Disabled",
      description: "No code-execution provider is selected.",
    };
  }
  if (config.available) {
    return {
      kind: "ready",
      label: "Ready",
      description:
        config.provider === "e2b"
          ? "E2B is selected and has a saved credential."
          : "The local native sandbox is available.",
    };
  }
  if (config.provider === "e2b" && !config.has_credential) {
    return {
      kind: "not-configured",
      label: "Not configured",
      description: "E2B is selected but needs an API key.",
    };
  }
  return {
    kind: "not-configured",
    label: "Unavailable",
    description: "The selected execution provider is unavailable.",
  };
}

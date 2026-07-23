import { useEffect, useState } from "react";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
  CodeExecutionProviderKind,
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

const MIN_CODE_EXECUTION_TIMEOUT_MS = 1_000;
const MAX_CODE_EXECUTION_TIMEOUT_MS = 120_000;

export function CodeExecutionPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<CodeExecutionConfigInfo | null>(null);
  const [provider, setProvider] = useState<CodeExecutionProviderKind | "">("");
  const [timeoutMs, setTimeoutMs] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

    setSaving(true);
    setError(null);
    try {
      const nextConfig = await client.putCodeExecutionConfig({
        provider: provider || null,
        timeout_ms: parsedTimeout,
      });
      setConfig(nextConfig);
      setProvider(nextConfig.provider ?? "");
      setTimeoutMs(String(nextConfig.timeout_ms));
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
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
              <select
                className={SETTINGS_SELECT_CLASS}
                value={provider}
                disabled={saving}
                onChange={(event) =>
                  setProvider(
                    event.target.value as CodeExecutionProviderKind | "",
                  )
                }
              >
                <option value="">Disabled</option>
                <option value="local">Local native sandbox</option>
              </select>
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
                disabled={saving}
                onChange={(event) => setTimeoutMs(event.target.value)}
              />
            </SettingsField>

            <Button
              type="button"
              className="self-start"
              disabled={saving}
              onClick={() => void saveConfig()}
            >
              {saving ? "Saving…" : "Save configuration"}
            </Button>
          </SettingsSection>

          <p className="text-sm leading-relaxed text-muted-foreground">
            Local execution uses the host’s native sandbox, blocks network,
            clears inherited environment variables, and confines writes to
            private chat scratch. Commands still cross the existing execution
            consent boundary.
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
      description: "The local native sandbox is available.",
    };
  }
  return {
    kind: "not-configured",
    label: "Unavailable",
    description: "The selected native sandbox is unavailable on this host.",
  };
}

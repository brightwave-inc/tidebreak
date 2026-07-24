import { useEffect, useMemo, useState } from "react";
import type {
  ApiClient,
  ModelInfo,
  ModelSelectionKey,
  ProviderKind,
} from "../api";
import {
  canonicalModelSelection,
  modelForSelection,
  providerLabel,
} from "../ModelSelection";
import {
  SETTINGS_SELECT_CLASS,
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

export function ModelsPanel({
  client,
  models,
}: {
  client: ApiClient;
  models: ModelInfo[];
}) {
  const [defaultModel, setDefaultModel] = useState<string | null>(null);
  const [provider, setProvider] = useState<ProviderKind | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const providers = useMemo(
    () =>
      [...new Set(models.map((model) => model.provider))] as ProviderKind[],
    [models],
  );
  const selected = modelForSelection(models, defaultModel);
  const providerModels = provider
    ? models.filter((model) => model.provider === provider)
    : [];

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const settings = await client.getSettings();
        if (cancelled) return;
        setDefaultModel(settings.model);
        const resolved = modelForSelection(models, settings.model);
        setProvider(
          resolved?.provider ??
            providers.find((kind) =>
              models.some((model) => model.provider === kind && model.available),
            ) ??
            providers[0] ??
            null,
        );
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, models, providers]);

  async function save(model: ModelSelectionKey | null) {
    setSaving(true);
    setError(null);
    try {
      const next = await client.putSettings({ model });
      setDefaultModel(next.model);
      const resolved = modelForSelection(models, next.model);
      if (resolved) setProvider(resolved.provider);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  const canonical = canonicalModelSelection(models, defaultModel);
  const selectedValue =
    selected?.provider === provider ? (canonical ?? "") : "";
  const legacyUnavailable = defaultModel !== null && selected === null;

  return (
    <SettingsPanel
      title="Models"
      description="Choose the provider first, then a model that provider is configured to serve. New chats inherit this default; each conversation can still override it."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading model settings…</p>
      ) : (
        <>
          <SettingsSection title="Default model">
            <SettingsField label="Provider">
              <select
                className={SETTINGS_SELECT_CLASS}
                value={provider ?? ""}
                disabled={saving || providers.length === 0}
                onChange={(event) =>
                  setProvider((event.target.value || null) as ProviderKind | null)
                }
              >
                {providers.length === 0 && (
                  <option value="">No providers configured</option>
                )}
                {providers.map((kind) => {
                  const usable = models.some(
                    (model) => model.provider === kind && model.available,
                  );
                  return (
                    <option key={kind} value={kind}>
                      {providerLabel(kind)}
                      {usable ? "" : " — unavailable"}
                    </option>
                  );
                })}
              </select>
            </SettingsField>
            <SettingsField
              label="Model"
              hint="Unavailable models remain visible for clarity but cannot be selected until their provider is enabled and credentialed."
            >
              <select
                className={SETTINGS_SELECT_CLASS}
                value={selectedValue}
                disabled={saving || provider === null}
                onChange={(event) => {
                  const value = event.target.value as ModelSelectionKey | "";
                  void save(value || null);
                }}
              >
                <option value="">Server default</option>
                {providerModels.map((model) => (
                  <option
                    key={model.key}
                    value={model.key}
                    disabled={!model.available}
                  >
                    {model.display_name}
                    {model.available ? "" : " — unavailable"}
                  </option>
                ))}
              </select>
            </SettingsField>
            {legacyUnavailable && (
              <SettingsError>
                The saved legacy model “{defaultModel}” is not uniquely registered.
                Add it under the OpenAI-compatible provider, then choose it here.
              </SettingsError>
            )}
          </SettingsSection>
          {models.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No models are registered yet. Configure a provider first.
            </p>
          )}
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

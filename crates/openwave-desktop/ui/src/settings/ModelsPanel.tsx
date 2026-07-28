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

// Radix Select reserves the empty string for its placeholder, so the
// "Server default" choice needs a sentinel that no catalog key can collide
// with (keys are always `provider::id`).
const SERVER_DEFAULT = "__server_default__";

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
              <Select
                value={provider ?? ""}
                disabled={saving || providers.length === 0}
                onValueChange={(value) =>
                  setProvider((value || null) as ProviderKind | null)
                }
              >
                <SelectTrigger aria-label="Provider">
                  <SelectValue placeholder="No providers configured" />
                </SelectTrigger>
                <SelectContent>
                  {providers.map((kind) => {
                    const usable = models.some(
                      (model) => model.provider === kind && model.available,
                    );
                    return (
                      <SelectItem key={kind} value={kind}>
                        {providerLabel(kind)}
                        {usable ? "" : " — unavailable"}
                      </SelectItem>
                    );
                  })}
                </SelectContent>
              </Select>
            </SettingsField>
            <SettingsField
              label="Model"
              hint="Unavailable models remain visible for clarity but cannot be selected until their provider is enabled and credentialed."
            >
              <Select
                value={selectedValue === "" ? SERVER_DEFAULT : selectedValue}
                disabled={saving || provider === null}
                onValueChange={(value) => {
                  void save(
                    value === SERVER_DEFAULT
                      ? null
                      : (value as ModelSelectionKey),
                  );
                }}
              >
                <SelectTrigger aria-label="Model">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={SERVER_DEFAULT}>Server default</SelectItem>
                  {providerModels.map((model) => (
                    <SelectItem
                      key={model.key}
                      value={model.key}
                      disabled={!model.available}
                    >
                      {model.display_name}
                      {model.available ? "" : " — unavailable"}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
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

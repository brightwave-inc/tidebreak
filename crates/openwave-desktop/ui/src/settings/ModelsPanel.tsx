import { useEffect, useState } from "react";
import type { ApiClient, ModelInfo } from "../api";
import { Input } from "@/components/ui/input";
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
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const settings = await client.getSettings();
        if (cancelled) return;
        setDefaultModel(settings.model);
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

  async function save(model: string | null) {
    setSaving(true);
    setError(null);
    try {
      const next = await client.putSettings({ model });
      setDefaultModel(next.model);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  const isCustom = Boolean(
    defaultModel && !models.some((m) => m.id === defaultModel),
  );

  return (
    <SettingsPanel
      title="Models"
      description="Pick the default model for new chats. Any chat left on “Default” uses this. Individual chats can override it from the model menu in the message bar."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading model settings…</p>
      ) : (
        <>
          <SettingsSection title="Default model">
            <SettingsField label="Model">
              <select
                className={SETTINGS_SELECT_CLASS}
                value={defaultModel ?? ""}
                disabled={saving}
                onChange={(e) => void save(e.target.value || null)}
              >
                <option value="">Server default</option>
                {models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id} ({m.provider})
                  </option>
                ))}
                {isCustom && defaultModel && (
                  <option value={defaultModel}>{defaultModel} (custom)</option>
                )}
              </select>
            </SettingsField>
            <SettingsField
              label="Custom model ID"
              hint="Select a listed model above, or type any model ID your enabled providers support."
            >
              <Input
                type="text"
                placeholder="e.g. claude-sonnet-4-20250514"
                defaultValue={isCustom && defaultModel ? defaultModel : ""}
                key={defaultModel ?? "none"}
                disabled={saving}
                onBlur={(e) => {
                  const next = e.target.value.trim();
                  if (next && next !== (defaultModel ?? "")) void save(next);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
              />
            </SettingsField>
          </SettingsSection>
          {models.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No models are available yet. Add a provider credential on the
              Providers page to populate the catalog.
            </p>
          )}
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

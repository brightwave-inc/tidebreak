import { useState } from "react";
import type {
  ApiClient,
  CustomModelConfig,
  ProviderInfo,
  ProviderKind,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

export function ProvidersPanel({
  providers,
  client,
  onChanged,
}: {
  providers: ProviderInfo[];
  client: ApiClient;
  onChanged: () => void;
}) {
  return (
    <SettingsPanel
      title="Providers"
      description="Keys stay on this machine. Enable a provider, then save a credential."
    >
      {providers.map((p) => (
        <ProviderRow key={p.kind} info={p} client={client} onChanged={onChanged} />
      ))}
    </SettingsPanel>
  );
}

function ProviderRow({
  info,
  client,
  onChanged,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [models, setModels] = useState<CustomModelConfig[]>(info.models);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
        models?: CustomModelConfig[];
      } = { enabled };
      if (info.kind === "openai_compatible") {
        body.base_url = baseUrl.trim() || null;
        body.models = models.map((model) => ({
          ...model,
          id: model.id.trim(),
          display_name: model.display_name?.trim() || null,
        }));
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function clearCredential() {
    setSaving(true);
    setError(null);
    try {
      await client.deleteCredential(info.kind as ProviderKind);
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsSection title={info.kind.replaceAll("_", " ")}>
      <div className="flex items-center justify-between gap-3">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="size-4 accent-[var(--primary)]"
            checked={info.enabled}
            disabled={saving}
            onChange={(e) => void save(e.target.checked)}
          />
          Enabled
        </label>
        <span className="text-xs text-muted-foreground">
          {info.has_credential ? "credential set" : "no credential"}
        </span>
      </div>
      {info.kind === "openai_compatible" && (
        <>
          <Input
            type="text"
            placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <span className="text-sm font-medium">Models</span>
              <Button
                type="button"
                variant="outline"
                disabled={saving}
                onClick={() =>
                  setModels((current) => [
                    ...current,
                    {
                      id: "",
                      display_name: null,
                      context_window: 32_768,
                      max_output_tokens: 4_096,
                    },
                  ])
                }
              >
                Add model
              </Button>
            </div>
            {models.length === 0 && (
              <p className="text-xs text-muted-foreground">
                Add each model this endpoint serves. Custom models start with
                conservative text-only, non-reasoning capabilities.
              </p>
            )}
            {models.map((model, index) => (
              <div
                className="grid gap-2 rounded-md border border-border p-3"
                key={index}
              >
                <Input
                  type="text"
                  aria-label={`Custom model ${index + 1} ID`}
                  placeholder="model ID"
                  value={model.id}
                  onChange={(event) =>
                    setModels((current) =>
                      current.map((item, itemIndex) =>
                        itemIndex === index
                          ? { ...item, id: event.target.value }
                          : item,
                      ),
                    )
                  }
                />
                <Input
                  type="text"
                  aria-label={`Custom model ${index + 1} display name`}
                  placeholder="display name (optional)"
                  value={model.display_name ?? ""}
                  onChange={(event) =>
                    setModels((current) =>
                      current.map((item, itemIndex) =>
                        itemIndex === index
                          ? {
                              ...item,
                              display_name: event.target.value || null,
                            }
                          : item,
                      ),
                    )
                  }
                />
                <div className="grid grid-cols-2 gap-2">
                  <label className="grid gap-1 text-xs text-muted-foreground">
                    Context tokens
                    <Input
                      type="number"
                      min={1024}
                      aria-label={`Custom model ${index + 1} context tokens`}
                      value={model.context_window}
                      onChange={(event) =>
                        setModels((current) =>
                          current.map((item, itemIndex) =>
                            itemIndex === index
                              ? {
                                  ...item,
                                  context_window: Number(event.target.value),
                                }
                              : item,
                          ),
                        )
                      }
                    />
                  </label>
                  <label className="grid gap-1 text-xs text-muted-foreground">
                    Max output
                    <Input
                      type="number"
                      min={1}
                      aria-label={`Custom model ${index + 1} max output`}
                      value={model.max_output_tokens}
                      onChange={(event) =>
                        setModels((current) =>
                          current.map((item, itemIndex) =>
                            itemIndex === index
                              ? {
                                  ...item,
                                  max_output_tokens: Number(event.target.value),
                                }
                              : item,
                          ),
                        )
                      }
                    />
                  </label>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  disabled={saving}
                  onClick={() =>
                    setModels((current) =>
                      current.filter((_, itemIndex) => itemIndex !== index),
                    )
                  }
                >
                  Remove model
                </Button>
              </div>
            ))}
          </div>
        </>
      )}
      <Input
        type="password"
        placeholder="API key"
        value={key}
        onChange={(e) => setKey(e.target.value)}
        autoComplete="off"
      />
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={
            saving ||
            (info.kind !== "openai_compatible" && !key.trim()) ||
            (info.kind === "openai_compatible" &&
              models.some((model) => !model.id.trim()))
          }
          onClick={() => void save(true)}
        >
          Save configuration
        </Button>
        {info.has_credential && (
          <Button
            type="button"
            variant="outline"
            disabled={saving}
            onClick={() => void clearCredential()}
          >
            Clear
          </Button>
        )}
      </div>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsSection>
  );
}
